use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, FILETIME};
use windows::Win32::Storage::FileSystem::{
    CopyFileExW, CreateFileW, MoveFileExW, SetFileTime,
    LPPROGRESS_ROUTINE_CALLBACK_REASON, MOVEFILE_COPY_ALLOWED,
    MOVEFILE_WRITE_THROUGH, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};
use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};

use crate::profiler::profile_device;
use crate::walker::build_work_list;

// Win32 copy flag constant
const COPY_FILE_NO_BUFFERING: u32 = 0x00001000;
const COPY_FILE_FAIL_IF_EXISTS: u32 = 0x00000001;

/// Global progress tracking state
pub struct ProgressState {
    pub total_files: usize,
    pub total_bytes: u64,
    pub files_completed: AtomicUsize,
    pub bytes_completed: AtomicU64,
    pub cancel_flag: Arc<AtomicBool>,
    pub failed_files: Arc<Mutex<Vec<(PathBuf, String)>>>,
}

/// Thread-local state for CopyFileExW callback
struct FileProgressState {
    global_state: *const ProgressState,
    last_bytes: Cell<u64>,
}

/// Safe Win32 CopyFileExW progress callback.
unsafe extern "system" fn copy_progress_routine(
    _total_file_size: i64,
    total_bytes_transferred: i64,
    _stream_size: i64,
    _stream_bytes_transferred: i64,
    _dw_stream_number: u32,
    _dw_callback_reason: LPPROGRESS_ROUTINE_CALLBACK_REASON,
    _h_source_file: HANDLE,
    _h_destination_file: HANDLE,
    lp_data: *const std::ffi::c_void,
) -> u32 {
    let state = unsafe { &*(lp_data as *const FileProgressState) };
    let global = unsafe { &*state.global_state };

    if global.cancel_flag.load(Ordering::Relaxed) {
        return 1; // PROGRESS_CANCEL (cancels copy and deletes destination file)
    }

    let current = total_bytes_transferred as u64;
    let delta = current - state.last_bytes.get();
    state.last_bytes.set(current);
    global.bytes_completed.fetch_add(delta, Ordering::SeqCst);

    0 // PROGRESS_CONTINUE
}



/// Copy a single file using Win32 CopyFileExW with unbuffered write options for large files.
fn copy_file_win32(
    src: &Path,
    dest: &Path,
    size: u64,
    global_state: &ProgressState,
) -> Result<(), String> {
    let src_wide: Vec<u16> = src.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let dest_wide: Vec<u16> = dest.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();

    // Fail if exists to prevent silent overwrite, and bypass caching for large files.
    let mut flags = COPY_FILE_FAIL_IF_EXISTS;
    if size >= 256 * 1024 * 1024 {
        flags |= COPY_FILE_NO_BUFFERING;
    }

    let file_state = FileProgressState {
        global_state,
        last_bytes: Cell::new(0),
    };

    unsafe {
        let res = CopyFileExW(
            PCWSTR(src_wide.as_ptr()),
            PCWSTR(dest_wide.as_ptr()),
            Some(copy_progress_routine),
            Some(&file_state as *const _ as *const std::ffi::c_void),
            None,
            flags,
        );

        if res.is_ok() {
            global_state.files_completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        } else {
            let err = res.err().map(|e| e.to_string()).unwrap_or_else(|| "Unknown Win32 error".to_string());
            if global_state.cancel_flag.load(Ordering::Relaxed) {
                Err("Cancelled".to_string())
            } else {
                global_state.files_completed.fetch_add(1, Ordering::SeqCst);
                Err(err)
            }
        }
    }
}

fn apply_directory_timestamps(dirs: &[crate::walker::CopyItem]) -> Result<(), String> {
    for dir in dirs {
        let dest_wide: Vec<u16> = dir.dest_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let handle = CreateFileW(
                PCWSTR(dest_wide.as_ptr()),
                0x00000100, // WRITE_ATTRIBUTES
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            );
            if let Ok(h) = handle {
                if h != INVALID_HANDLE_VALUE {
                    let ft_create = FILETIME {
                        dwLowDateTime: (dir.creation_time & 0xFFFFFFFF) as u32,
                        dwHighDateTime: (dir.creation_time >> 32) as u32,
                    };
                    let ft_access = FILETIME {
                        dwLowDateTime: (dir.last_access_time & 0xFFFFFFFF) as u32,
                        dwHighDateTime: (dir.last_access_time >> 32) as u32,
                    };
                    let ft_write = FILETIME {
                        dwLowDateTime: (dir.last_write_time & 0xFFFFFFFF) as u32,
                        dwHighDateTime: (dir.last_write_time >> 32) as u32,
                    };
                    let _ = SetFileTime(
                        h,
                        Some(&ft_create),
                        Some(&ft_access),
                        Some(&ft_write),
                    );
                    let _ = CloseHandle(h);
                }
            }
        }
    }
    Ok(())
}


/// Helper to clear the Windows clipboard after a successful move operation.
pub fn clear_clipboard() -> bool {
    unsafe {
        if OpenClipboard(None).is_ok() {
            let res = EmptyClipboard();
            let _ = CloseClipboard();
            res.is_ok()
        } else {
            false
        }
    }
}

/// Represents the final execution statistics.
#[derive(Debug, Clone)]
pub struct EngineSummary {
    pub files_copied: usize,
    pub bytes_copied: u64,
    pub elapsed: Duration,
    pub failures: Vec<(PathBuf, String)>,
    pub was_cancelled: bool,
}

/// Run a multi-threaded copy/move operation based on auto-tuned concurrency.
pub fn run_engine(
    sources: &[PathBuf],
    dest: &Path,
    is_move: bool,
    custom_concurrency: Option<usize>,
    cancel_flag: Arc<AtomicBool>,
    progress_callback: Option<Box<dyn Fn(usize, u64) + Send + Sync>>,
) -> EngineSummary {
    let start_time = Instant::now();

    // Prevent system from sleeping during transfer
    unsafe {
        let _ = SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_CONTINUOUS);
    }

    // Build the flat work list
    let work_list = match build_work_list(sources, dest, Some(&cancel_flag), None) {
        Ok(wl) => wl,
        Err(e) => {
            unsafe {
                let _ = SetThreadExecutionState(ES_CONTINUOUS);
            }
            return EngineSummary {
                files_copied: 0,
                bytes_copied: 0,
                elapsed: start_time.elapsed(),
                failures: vec![(dest.to_path_buf(), format!("Failed to build work list: {}", e))],
                was_cancelled: e.kind() == std::io::ErrorKind::Interrupted || cancel_flag.load(Ordering::Relaxed),
            };
        }
    };

    run_engine_with_work_list(
        work_list,
        sources,
        dest,
        is_move,
        custom_concurrency,
        cancel_flag,
        progress_callback,
    )
}

/// Run a multi-threaded copy/move operation with a pre-built work list.
pub fn run_engine_with_work_list(
    work_list: crate::walker::WorkList,
    sources: &[PathBuf],
    dest: &Path,
    is_move: bool,
    custom_concurrency: Option<usize>,
    cancel_flag: Arc<AtomicBool>,
    progress_callback: Option<Box<dyn Fn(usize, u64) + Send + Sync>>,
) -> EngineSummary {
    let start_time = Instant::now();

    // Prevent system from sleeping during transfer
    unsafe {
        let _ = SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_CONTINUOUS);
    }

    let profile = profile_device(dest);
    let concurrency = custom_concurrency.unwrap_or(profile.concurrency);

    // Ensure destination directory itself exists
    if let Err(e) = fs::create_dir_all(dest) {
        unsafe {
            let _ = SetThreadExecutionState(ES_CONTINUOUS);
        }
        return EngineSummary {
            files_copied: 0,
            bytes_copied: 0,
            elapsed: start_time.elapsed(),
            failures: vec![(dest.to_path_buf(), format!("Failed to create destination directory: {}", e))],
            was_cancelled: false,
        };
    }

    let global_state = Arc::new(ProgressState {
        total_files: work_list.total_files,
        total_bytes: work_list.total_bytes,
        files_completed: AtomicUsize::new(0),
        bytes_completed: AtomicU64::new(0),
        cancel_flag: cancel_flag.clone(),
        failed_files: Arc::new(Mutex::new(Vec::new())),
    });

    // Add preflight skipped links to failures
    if !work_list.skipped_links.is_empty() {
        let mut failed = global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner());
        for (path, err) in &work_list.skipped_links {
            failed.push((path.clone(), err.clone()));
        }
    }

    // 1. Recreate folder structure
    for dir in &work_list.dirs {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        if let Err(e) = fs::create_dir_all(&dir.dest_path) {
            global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((
                dir.src_path.clone(),
                format!("Failed to create directory {}: {}", dir.dest_path.display(), e),
            ));
        }
    }

    // Determine same-volume fast rename eligibility for moves
    let is_same_volume = if is_move {
        let dest_profile = profile_device(dest);
        let mut same = true;
        for src in sources {
            let src_profile = profile_device(src);
            if src_profile.physical_device_number != dest_profile.physical_device_number
                || src_profile.physical_device_number.is_none()
            {
                same = false;
                break;
            }
        }
        same
    } else {
        false
    };

    // Group small files by destination parent directory to prevent NTFS directory index locks
    let mut dir_groups: HashMap<PathBuf, Vec<crate::walker::CopyItem>> = HashMap::new();
    for item in work_list.small_files {
        if let Some(parent) = item.dest_path.parent() {
            dir_groups.entry(parent.to_path_buf()).or_default().push(item);
        }
    }
    let dir_works: Vec<Vec<crate::walker::CopyItem>> = dir_groups.into_values().collect();
    
    let small_dirs = Arc::new(dir_works);
    let large_items = Arc::new(work_list.large_files);

    // If same volume move, execute renaming instantly (single thread is fastest, avoid queuing)
    if is_move && is_same_volume {
        for dir in small_dirs.iter() {
            for item in dir {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                let src_wide: Vec<u16> = item.src_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
                let dest_wide: Vec<u16> = item.dest_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
                unsafe {
                    let res = MoveFileExW(
                        PCWSTR(src_wide.as_ptr()),
                        PCWSTR(dest_wide.as_ptr()),
                        MOVEFILE_WRITE_THROUGH | MOVEFILE_COPY_ALLOWED,
                    );
                    if res.is_ok() {
                        global_state.files_completed.fetch_add(1, Ordering::SeqCst);
                        global_state.bytes_completed.fetch_add(item.size, Ordering::SeqCst);
                    } else {
                        let err = res.err().map(|e| e.to_string()).unwrap_or_else(|| "Move error".to_string());
                        global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((item.src_path.clone(), err));
                        global_state.files_completed.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }

        for item in large_items.iter() {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let src_wide: Vec<u16> = item.src_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            let dest_wide: Vec<u16> = item.dest_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let res = MoveFileExW(
                    PCWSTR(src_wide.as_ptr()),
                    PCWSTR(dest_wide.as_ptr()),
                    MOVEFILE_WRITE_THROUGH | MOVEFILE_COPY_ALLOWED,
                );
                if res.is_ok() {
                    global_state.files_completed.fetch_add(1, Ordering::SeqCst);
                    global_state.bytes_completed.fetch_add(item.size, Ordering::SeqCst);
                } else {
                    let err = res.err().map(|e| e.to_string()).unwrap_or_else(|| "Move error".to_string());
                    global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((item.src_path.clone(), err));
                    global_state.files_completed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    } else {
        // Multi-threaded copying/moving (two-phase for moves)
        let small_idx = Arc::new(AtomicUsize::new(0));
        let large_idx = Arc::new(AtomicUsize::new(0));

        let done_flag = Arc::new(AtomicBool::new(false));
        let done_flag_clone = done_flag.clone();

        // Spawn progress reporting side-thread
        let progress_state = global_state.clone();
        let progress_cancel = cancel_flag.clone();
        let progress_handle = thread::spawn(move || {
            while !progress_cancel.load(Ordering::Relaxed) && !done_flag_clone.load(Ordering::Relaxed) {
                let completed = progress_state.files_completed.load(Ordering::Relaxed);
                let bytes = progress_state.bytes_completed.load(Ordering::Relaxed);
                if let Some(ref cb) = progress_callback {
                    cb(completed, bytes);
                }
                thread::sleep(Duration::from_millis(100));
            }
            // Send one last progress update when done/cancelled
            let completed = progress_state.files_completed.load(Ordering::Relaxed);
            let bytes = progress_state.bytes_completed.load(Ordering::Relaxed);
            if let Some(ref cb) = progress_callback {
                cb(completed, bytes);
            }
        });

        if concurrency == 1 {
            // HDD Profile / single-thread: copy all files completely sequentially on the main thread
            // to eliminate concurrent disk head seeks.
            for item in large_items.iter() {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = copy_file_win32(&item.src_path, &item.dest_path, item.size, &global_state) {
                    global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((item.src_path.clone(), e));
                }
            }
            for dir in small_dirs.iter() {
                for item in dir {
                    if cancel_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Err(e) = copy_file_win32(&item.src_path, &item.dest_path, item.size, &global_state) {
                        global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((item.src_path.clone(), e));
                    }
                }
            }
        } else {
            // 1. Large-file pool: Capped at min(2, concurrency) workers to prevent I/O seek contention
            let large_workers = std::cmp::min(2, concurrency);
            let mut large_threads = Vec::new();
            for _ in 0..large_workers {
                let idx = large_idx.clone();
                let items = large_items.clone();
                let state = global_state.clone();
                let c_flag = cancel_flag.clone();

                large_threads.push(thread::spawn(move || {
                    loop {
                        if c_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        let current = idx.fetch_add(1, Ordering::SeqCst);
                        if current >= items.len() {
                            break;
                        }
                        let item = &items[current];
                        if let Err(e) = copy_file_win32(&item.src_path, &item.dest_path, item.size, &state) {
                            state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((item.src_path.clone(), e));
                        }
                    }
                }));
            }

            // 2. Small-file pool: sharded by directory to prevent NTFS index lock contention
            let mut small_threads = Vec::new();
            for _ in 0..concurrency {
                let idx = small_idx.clone();
                let dirs = small_dirs.clone();
                let state = global_state.clone();
                let c_flag = cancel_flag.clone();

                small_threads.push(thread::spawn(move || {
                    loop {
                        if c_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        let current = idx.fetch_add(1, Ordering::SeqCst);
                        if current >= dirs.len() {
                            break;
                        }
                        let items = &dirs[current];
                        for item in items {
                            if c_flag.load(Ordering::Relaxed) {
                                break;
                            }
                            if let Err(e) = copy_file_win32(&item.src_path, &item.dest_path, item.size, &state) {
                                state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((item.src_path.clone(), e));
                            }
                        }
                    }
                }));
            }

            // Wait for workers to complete
            for t in large_threads {
                let _ = t.join();
            }
            for t in small_threads {
                let _ = t.join();
            }
        }
        done_flag.store(true, Ordering::Relaxed);
        let _ = progress_handle.join();
    }

    // Post-copy pass: Apply original directory timestamps
    let _ = apply_directory_timestamps(&work_list.dirs);

    // Two-Phase Move: Delete sources ONLY if everything copied without errors
    let mut failures = global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let was_cancelled = cancel_flag.load(Ordering::Relaxed);

    if is_move && !was_cancelled {
        // Delete all small and large source files (unless same volume move)
        if !is_same_volume {
            for dir in small_dirs.iter() {
                for file in dir {
                    // Verification check: verify destination existence and size
                    let mut success = false;
                    if let Ok(dest_meta) = fs::metadata(&file.dest_path) {
                        if dest_meta.len() == file.size {
                            success = true;
                        }
                    }
                    if success {
                        if let Err(e) = fs::remove_file(&file.src_path) {
                            failures.push((file.src_path.clone(), format!("Move cleanup failed: {}", e)));
                        }
                    } else {
                        failures.push((file.src_path.clone(), "Move verification failed: destination file mismatch or missing".to_string()));
                    }
                }
            }
            for file in large_items.iter() {
                let mut success = false;
                if let Ok(dest_meta) = fs::metadata(&file.dest_path) {
                    if dest_meta.len() == file.size {
                        success = true;
                    }
                }
                if success {
                    if let Err(e) = fs::remove_file(&file.src_path) {
                        failures.push((file.src_path.clone(), format!("Move cleanup failed: {}", e)));
                    }
                } else {
                    failures.push((file.src_path.clone(), "Move verification failed: destination file mismatch or missing".to_string()));
                }
            }
        }
        // Delete source directories in reverse depth-first order
        let mut sorted_dirs = work_list.dirs.clone();
        sorted_dirs.sort_by(|a, b| b.src_path.to_string_lossy().len().cmp(&a.src_path.to_string_lossy().len()));
        for dir in sorted_dirs {
            let _ = fs::remove_dir(&dir.src_path); // Ignore failure if directories are not empty
        }

        // Clear clipboard ONLY if everything succeeded
        if failures.is_empty() {
            let _ = clear_clipboard();
        }
    }

    // Restore sleep states
    unsafe {
        let _ = SetThreadExecutionState(ES_CONTINUOUS);
    }

    let completed = global_state.files_completed.load(Ordering::Relaxed);
    let total_failed = failures.len();
    let files_copied = if completed >= total_failed { completed - total_failed } else { 0 };

    EngineSummary {
        files_copied,
        bytes_copied: global_state.bytes_completed.load(Ordering::Relaxed),
        elapsed: start_time.elapsed(),
        failures,
        was_cancelled,
    }
}

/// Run a multi-threaded parallel delete operation.
pub fn run_delete_engine(
    sources: &[PathBuf],
    concurrency: usize,
    cancel_flag: Arc<AtomicBool>,
    progress_callback: Option<Box<dyn Fn(usize, u64) + Send + Sync + 'static>>,
) -> EngineSummary {
    let start_time = Instant::now();
    
    // Prevent system sleep during operation
    unsafe {
        let _ = SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_CONTINUOUS);
    }

    fn mut_or_empty_path(sources: &[PathBuf]) -> PathBuf {
        sources.first().cloned().unwrap_or_default()
    }

    // Build delete list using walker
    let delete_list = match crate::walker::build_delete_list(sources, Some(&cancel_flag), None) {
        Ok(dl) => dl,
        Err(e) => {
            unsafe {
                let _ = SetThreadExecutionState(ES_CONTINUOUS);
            }
            return EngineSummary {
                files_copied: 0,
                bytes_copied: 0,
                elapsed: start_time.elapsed(),
                failures: vec![(mut_or_empty_path(sources), format!("Failed to build delete list: {}", e))],
                was_cancelled: e.kind() == std::io::ErrorKind::Interrupted || cancel_flag.load(Ordering::Relaxed),
            };
        }
    };
    
    run_delete_engine_with_delete_list(
        delete_list,
        sources,
        concurrency,
        cancel_flag,
        progress_callback,
    )
}

/// Run a multi-threaded parallel delete operation with a pre-built delete list.
pub fn run_delete_engine_with_delete_list(
    delete_list: crate::walker::DeleteList,
    _sources: &[PathBuf],
    concurrency: usize,
    cancel_flag: Arc<AtomicBool>,
    progress_callback: Option<Box<dyn Fn(usize, u64) + Send + Sync + 'static>>,
) -> EngineSummary {
    let start_time = Instant::now();
    
    // Prevent system sleep during operation
    unsafe {
        let _ = SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_CONTINUOUS);
    }

    let mut failures = Vec::new();
    let total_files = delete_list.files.len();
    let global_state = Arc::new(ProgressState {
        total_files,
        total_bytes: total_files as u64,
        files_completed: AtomicUsize::new(0),
        bytes_completed: AtomicU64::new(0),
        cancel_flag: cancel_flag.clone(),
        failed_files: Arc::new(Mutex::new(Vec::new())),
    });

    if total_files > 0 {
        // Multi-threaded deletion
        let files = Arc::new(delete_list.files);
        let file_idx = Arc::new(AtomicUsize::new(0));

        let done_flag = Arc::new(AtomicBool::new(false));
        let done_flag_clone = done_flag.clone();

        // Spawn progress reporting side-thread
        let progress_state = global_state.clone();
        let progress_cancel = cancel_flag.clone();
        let progress_handle = thread::spawn(move || {
            while !progress_cancel.load(Ordering::Relaxed) && !done_flag_clone.load(Ordering::Relaxed) {
                let completed = progress_state.files_completed.load(Ordering::Relaxed);
                if let Some(ref cb) = progress_callback {
                    cb(completed, completed as u64);
                }
                thread::sleep(Duration::from_millis(100));
            }
            let completed = progress_state.files_completed.load(Ordering::Relaxed);
            if let Some(ref cb) = progress_callback {
                cb(completed, completed as u64);
            }
        });

        // Spawn worker threads
        let mut threads = Vec::new();
        for _ in 0..concurrency {
            let idx = file_idx.clone();
            let items = files.clone();
            let state = global_state.clone();
            let c_flag = cancel_flag.clone();

            threads.push(thread::spawn(move || {
                loop {
                    if c_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let current = idx.fetch_add(1, Ordering::SeqCst);
                    if current >= items.len() {
                        break;
                    }
                    let path = &items[current];
                    
                    // Note: Check if metadata is directory for directory symlinks/junctions
                    let res = if path.is_dir() {
                        fs::remove_dir(path)
                    } else {
                        fs::remove_file(path)
                    };
                    
                    if let Err(e) = res {
                        state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((path.clone(), e.to_string()));
                    }
                    
                    state.files_completed.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        // Wait for workers
        for t in threads {
            let _ = t.join();
        }
        done_flag.store(true, Ordering::Relaxed);
        let _ = progress_handle.join();
    }

    // Now delete directories in reverse depth-first order
    let was_cancelled = cancel_flag.load(Ordering::Relaxed);
    if !was_cancelled {
        let mut sorted_dirs = delete_list.dirs;
        // Sort by path length descending (so child directories are deleted before parents)
        sorted_dirs.sort_by(|a, b| b.to_string_lossy().len().cmp(&a.to_string_lossy().len()));
        
        for dir in sorted_dirs {
            if let Err(e) = fs::remove_dir(&dir) {
                failures.push((dir, format!("Failed to delete directory: {}", e)));
            }
        }
    }

    // Merge failures
    let mut worker_failures = global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).clone();
    failures.append(&mut worker_failures);

    // Restore sleep states
    unsafe {
        let _ = SetThreadExecutionState(ES_CONTINUOUS);
    }

    let completed = global_state.files_completed.load(Ordering::Relaxed);
    let total_failed = failures.len();
    let files_copied = if completed >= total_failed { completed - total_failed } else { 0 };

    EngineSummary {
        files_copied,
        bytes_copied: files_copied as u64,
        elapsed: start_time.elapsed(),
        failures,
        was_cancelled,
    }
}

