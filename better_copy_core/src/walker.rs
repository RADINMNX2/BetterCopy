use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};

// Win32 file attribute for reparse point (junctions/symlinks)
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

#[derive(Debug, Clone)]
pub struct CopyItem {
    pub src_path: PathBuf,
    pub dest_path: PathBuf,
    pub src_wide: Vec<u16>,
    pub dest_wide: Vec<u16>,
    pub size: u64,
    pub is_dir: bool,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
}

#[derive(Debug, Default, Clone)]
pub struct WorkList {
    pub dirs: Vec<CopyItem>,
    pub small_files: Vec<CopyItem>,
    pub large_files: Vec<CopyItem>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub skipped_links: Vec<(PathBuf, String)>,
}

/// Helper to convert a path to a Windows long path format (prefixed with \\?\)
/// to bypass the MAX_PATH (260 chars) limitation. Doesn't require path to exist.
pub fn ensure_long_path(path: &Path) -> PathBuf {
    let path_str = path.to_string_lossy().replace('/', "\\");
    if path_str.starts_with(r"\\?\") {
        return PathBuf::from(path_str);
    }
    
    // Resolve relative paths based on current directory
    let abs_path = if Path::new(&path_str).is_absolute() {
        PathBuf::from(&path_str)
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(&path_str)
    };
    
    let abs_str = abs_path.to_string_lossy().replace('/', "\\");
    
    if abs_str.starts_with(r"\\") {
        // UNC path: \\server\share\path -> \\?\UNC\server\share\path
        let unc_part = &abs_str[2..];
        PathBuf::from(format!(r"\\?\UNC\{}", unc_part))
    } else {
        PathBuf::from(format!(r"\\?\{}", abs_str))
    }
}

/// Helper to encode a path into wide string format including the null-terminator.
fn encode_wide_path(path: &Path) -> Vec<u16> {
    path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect()
}

/// Recursively traverses a directory.
fn walk_dir(
    src_dir: &Path,
    dest_dir: &Path,
    work_list: &mut WorkList,
    cancel_flag: Option<&Arc<AtomicBool>>,
    progress_callback: Option<&dyn Fn(usize)>,
) -> std::io::Result<()> {
    if let Some(cancel) = cancel_flag {
        if cancel.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Operation cancelled"));
        }
    }
    for entry in fs::read_dir(src_dir)? {
        if let Some(cancel) = cancel_flag {
            if cancel.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Operation cancelled"));
            }
        }
        let entry = entry?;
        let src_path = ensure_long_path(&entry.path());
        let file_name = entry.file_name();
        let dest_path = dest_dir.join(&file_name);

        let metadata = fs::symlink_metadata(&src_path)?;
        let file_attr = metadata.file_attributes();
        
        // Skip reparse points (junction loops, symlinks) and log them
        if (file_attr & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            work_list.skipped_links.push((src_path.clone(), "Skipped symlink/junction".to_string()));
            continue;
        }

        let src_wide = encode_wide_path(&src_path);
        let dest_wide = encode_wide_path(&dest_path);
        
        let creation_time = metadata.creation_time();
        let last_access_time = metadata.last_access_time();
        let last_write_time = metadata.last_write_time();

        if metadata.is_dir() {
            // Guard against recursive directory copying (e.g. copying a folder into itself)
            if is_self_copy(&src_path, dest_dir) {
                continue;
            }
            work_list.dirs.push(CopyItem {
                src_path: src_path.clone(),
                dest_path: dest_path.clone(),
                src_wide,
                dest_wide,
                size: 0,
                is_dir: true,
                creation_time,
                last_access_time,
                last_write_time,
            });
            if let Some(cb) = progress_callback {
                cb(work_list.total_files);
            }
            walk_dir(&src_path, &dest_path, work_list, cancel_flag, progress_callback)?;
        } else {
            let size = metadata.len();
            let item = CopyItem {
                src_path,
                dest_path,
                src_wide,
                dest_wide,
                size,
                is_dir: false,
                creation_time,
                last_access_time,
                last_write_time,
            };
            work_list.total_files += 1;
            work_list.total_bytes += size;
            if let Some(cb) = progress_callback {
                cb(work_list.total_files);
            }

            if size < 1_048_576 {
                work_list.small_files.push(item);
            } else {
                work_list.large_files.push(item);
            }
        }
    }
    Ok(())
}

/// Builds the flat work list of directories and files partitioned into queues.
pub fn build_work_list(
    sources: &[PathBuf],
    dest_root: &Path,
    cancel_flag: Option<&Arc<AtomicBool>>,
    progress_callback: Option<&dyn Fn(usize)>,
) -> std::io::Result<WorkList> {
    let mut work_list = WorkList::default();
    let dest_root_long = ensure_long_path(dest_root);
    let mut planned_dests = std::collections::HashSet::new();

    for source in sources {
        if let Some(cancel) = cancel_flag {
            if cancel.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Operation cancelled"));
            }
        }
        let src_long = ensure_long_path(source);
        let metadata = fs::symlink_metadata(&src_long)?;
        let file_attr = metadata.file_attributes();

        // Skip reparse points at root and log them
        if (file_attr & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            work_list.skipped_links.push((src_long.clone(), "Skipped symlink/junction at root".to_string()));
            continue;
        }

        let file_name = match source.file_name() {
            Some(name) => name,
            None => continue, // Skip root drive paths like C:\ which don't have a filename component
        };
        let mut target_dest = dest_root_long.join(file_name);

        // Resolve self-copy and name collisions with already planned destinations
        let mut counter = 1;
        let mut check_dest = target_dest.clone();
        while is_self_copy(&src_long, &check_dest) || planned_dests.contains(&check_dest) {
            let stem = target_dest.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let ext = target_dest.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let suffix = if counter == 1 {
                " - Copy".to_string()
            } else {
                format!(" - Copy ({})", counter)
            };
            let new_name = format!("{}{}{}", stem, suffix, ext);
            check_dest = dest_root_long.join(new_name);
            counter += 1;
        }
        target_dest = check_dest;
        planned_dests.insert(target_dest.clone());
        
        let src_wide = encode_wide_path(&src_long);
        let dest_wide = encode_wide_path(&target_dest);
        
        let creation_time = metadata.creation_time();
        let last_access_time = metadata.last_access_time();
        let last_write_time = metadata.last_write_time();

        if metadata.is_dir() {
            work_list.dirs.push(CopyItem {
                src_path: src_long.clone(),
                dest_path: target_dest.clone(),
                src_wide,
                dest_wide,
                size: 0,
                is_dir: true,
                creation_time,
                last_access_time,
                last_write_time,
            });
            if let Some(cb) = progress_callback {
                cb(work_list.total_files);
            }
            walk_dir(&src_long, &target_dest, &mut work_list, cancel_flag, progress_callback)?;
        } else {
            let size = metadata.len();
            let item = CopyItem {
                src_path: src_long,
                dest_path: target_dest,
                src_wide,
                dest_wide,
                size,
                is_dir: false,
                creation_time,
                last_access_time,
                last_write_time,
            };
            work_list.total_files += 1;
            work_list.total_bytes += size;
            if let Some(cb) = progress_callback {
                cb(work_list.total_files);
            }

            if size < 1_048_576 {
                work_list.small_files.push(item);
            } else {
                work_list.large_files.push(item);
            }
        }
    }

    Ok(work_list)
}

#[derive(Debug, Default, Clone)]
pub struct DeleteList {
    pub files: Vec<PathBuf>,
    pub dirs: Vec<PathBuf>,
}

/// Recursively traverses directory for building a deletion list.
fn walk_dir_for_delete(
    dir: &Path,
    delete_list: &mut DeleteList,
    cancel_flag: Option<&Arc<AtomicBool>>,
    progress_callback: Option<&dyn Fn(usize)>,
) -> std::io::Result<()> {
    if let Some(cancel) = cancel_flag {
        if cancel.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Operation cancelled"));
        }
    }
    for entry in fs::read_dir(dir)? {
        if let Some(cancel) = cancel_flag {
            if cancel.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Operation cancelled"));
            }
        }
        let entry = entry?;
        let path = ensure_long_path(&entry.path());
        let metadata = fs::symlink_metadata(&path)?;
        
        let file_attr = metadata.file_attributes();
        if (file_attr & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            // Do not traverse reparse points. Just treat them as files to delete the link itself.
            delete_list.files.push(path);
            if let Some(cb) = progress_callback {
                cb(delete_list.files.len());
            }
            continue;
        }

        if metadata.is_dir() {
            delete_list.dirs.push(path.clone());
            walk_dir_for_delete(&path, delete_list, cancel_flag, progress_callback)?;
        } else {
            delete_list.files.push(path);
            if let Some(cb) = progress_callback {
                cb(delete_list.files.len());
            }
        }
    }
    Ok(())
}

/// Builds the flat delete list of directories and files.
pub fn build_delete_list(
    sources: &[PathBuf],
    cancel_flag: Option<&Arc<AtomicBool>>,
    progress_callback: Option<&dyn Fn(usize)>,
) -> std::io::Result<DeleteList> {
    let mut delete_list = DeleteList::default();

    for source in sources {
        if let Some(cancel) = cancel_flag {
            if cancel.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Operation cancelled"));
            }
        }
        let src_long = ensure_long_path(source);
        let metadata = fs::symlink_metadata(&src_long)?;
        let file_attr = metadata.file_attributes();

        if (file_attr & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            delete_list.files.push(src_long);
            if let Some(cb) = progress_callback {
                cb(delete_list.files.len());
            }
            continue;
        }

        if metadata.is_dir() {
            delete_list.dirs.push(src_long.clone());
            walk_dir_for_delete(&src_long, &mut delete_list, cancel_flag, progress_callback)?;
        } else {
            delete_list.files.push(src_long);
            if let Some(cb) = progress_callback {
                cb(delete_list.files.len());
            }
        }
    }

    Ok(delete_list)
}

pub fn get_file_identity(path: &Path) -> Option<(u32, u64)> {
    let path_wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let handle = CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        );
        
        if let Ok(h) = handle {
            if h != INVALID_HANDLE_VALUE {
                let mut info = BY_HANDLE_FILE_INFORMATION::default();
                let res = GetFileInformationByHandle(h, &mut info);
                let _ = CloseHandle(h);
                if res.is_ok() {
                    let file_index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
                    return Some((info.dwVolumeSerialNumber, file_index));
                }
            }
        }
    }
    None
}

pub fn is_self_copy(src: &Path, dest: &Path) -> bool {
    // 1. Compare volume serial and file index directly
    if let (Some(src_id), Some(dest_id)) = (get_file_identity(src), get_file_identity(dest)) {
        if src_id == dest_id {
            return true;
        }
    }

    // 2. Compare parent identities and filenames case-insensitively
    if let (Some(src_parent), Some(dest_parent)) = (src.parent(), dest.parent()) {
        if let (Some(src_p_id), Some(dest_p_id)) = (get_file_identity(src_parent), get_file_identity(dest_parent)) {
            if src_p_id == dest_p_id {
                if let (Some(src_name), Some(dest_name)) = (src.file_name(), dest.file_name()) {
                    let src_str = src_name.to_string_lossy().to_lowercase();
                    let dest_str = dest_name.to_string_lossy().to_lowercase();
                    if src_str == dest_str {
                        return true;
                    }
                }
            }
        }
    }

    false
}

