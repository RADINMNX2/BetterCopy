use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

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
}

#[derive(Debug, Default, Clone)]
pub struct WorkList {
    pub dirs: Vec<CopyItem>,
    pub small_files: Vec<CopyItem>,
    pub large_files: Vec<CopyItem>,
    pub total_files: usize,
    pub total_bytes: u64,
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
) -> std::io::Result<()> {
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let src_path = ensure_long_path(&entry.path());
        let file_name = entry.file_name();
        let dest_path = dest_dir.join(&file_name);

        let metadata = fs::symlink_metadata(&src_path)?;
        let file_attr = metadata.file_attributes();
        
        // Skip reparse points (junction loops, symlinks)
        if (file_attr & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            continue;
        }

        let src_wide = encode_wide_path(&src_path);
        let dest_wide = encode_wide_path(&dest_path);

        if metadata.is_dir() {
            work_list.dirs.push(CopyItem {
                src_path: src_path.clone(),
                dest_path: dest_path.clone(),
                src_wide,
                dest_wide,
                size: 0,
                is_dir: true,
            });
            walk_dir(&src_path, &dest_path, work_list)?;
        } else {
            let size = metadata.len();
            let item = CopyItem {
                src_path,
                dest_path,
                src_wide,
                dest_wide,
                size,
                is_dir: false,
            };
            work_list.total_files += 1;
            work_list.total_bytes += size;

            if size < 1_000_000 {
                work_list.small_files.push(item);
            } else {
                work_list.large_files.push(item);
            }
        }
    }
    Ok(())
}

/// Builds the flat work list of directories and files partitioned into queues.
pub fn build_work_list(sources: &[PathBuf], dest_root: &Path) -> std::io::Result<WorkList> {
    let mut work_list = WorkList::default();
    let dest_root_long = ensure_long_path(dest_root);

    for source in sources {
        let src_long = ensure_long_path(source);
        let metadata = fs::symlink_metadata(&src_long)?;
        let file_attr = metadata.file_attributes();

        // Skip reparse points at root
        if (file_attr & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            continue;
        }

        let file_name = match source.file_name() {
            Some(name) => name,
            None => continue, // Skip root drive paths like C:\ which don't have a filename component
        };
        let target_dest = dest_root_long.join(file_name);
        
        let src_wide = encode_wide_path(&src_long);
        let dest_wide = encode_wide_path(&target_dest);

        if metadata.is_dir() {
            work_list.dirs.push(CopyItem {
                src_path: src_long.clone(),
                dest_path: target_dest.clone(),
                src_wide,
                dest_wide,
                size: 0,
                is_dir: true,
            });
            walk_dir(&src_long, &target_dest, &mut work_list)?;
        } else {
            let size = metadata.len();
            let item = CopyItem {
                src_path: src_long,
                dest_path: target_dest,
                src_wide,
                dest_wide,
                size,
                is_dir: false,
            };
            work_list.total_files += 1;
            work_list.total_bytes += size;

            if size < 1_000_000 {
                work_list.small_files.push(item);
            } else {
                work_list.large_files.push(item);
            }
        }
    }

    Ok(work_list)
}
