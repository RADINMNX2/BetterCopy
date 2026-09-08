use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, FILETIME};
use windows::Win32::Storage::FileSystem::{
    CopyFileExW, CreateFileW, MoveFileExW, SetFileAttributesW, SetFileTime,
    FILE_ATTRIBUTE_FLAGS, LPPROGRESS_ROUTINE_CALLBACK_REASON, MOVEFILE_COPY_ALLOWED,
    MOVEFILE_WRITE_THROUGH, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};
use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};

use crate::profiler::profile_device;
use crate::walker::build_work_list;

// Win32 copy flag constants
const COPY_FILE_NO_BUFFERING: u32 = 0x00001000;
const COPY_FILE_FAIL_IF_EXISTS: u32 = 0x00000001;

// Win32 file attribute constants + mask of attributes we replicate onto copies.
const FILE_ATTRIBUTE_READONLY: u32 = 0x00000001;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x00000002;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x00000004;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x00000020;
const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x00000100;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x00001000;
const FILE_ATTRIBUTE_NOT_CONTENT_INDEXED: u32 = 0x00002000;
const FILE_METADATA_MASK: u32 = FILE_ATTRIBUTE_READONLY
    | FILE_ATTRIBUTE_HIDDEN
    | FILE_ATTRIBUTE_SYSTEM
    | FILE_ATTRIBUTE_ARCHIVE
    | FILE_ATTRIBUTE_TEMPORARY
    | FILE_ATTRIBUTE_OFFLINE
    | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED;

const WRITE_ATTRIBUTES: u32 = 0x00000100;

/// How long transfer workers wait for a detached volume to come back before failing.
pub const DEFAULT_RESILIENCE_TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Global progress tracking state.
pub struct ProgressState {
    pub total_files: usize,
    pub total_bytes: u64,
    pub files_completed: AtomicUsize,
    pub bytes_completed: AtomicU64,
    pub cancel_flag: Arc<AtomicBool>,
    pub pause_flag: Arc<AtomicBool>,
    pub resilience_timeout: Duration,
    pub failed_files: Arc<Mutex<Vec<(PathBuf, String)>>>,
}

/// Thread-local state for CopyFileExW callback.
struct FileProgressState {
    global_state: *const ProgressState,
    last_bytes: Cell<u64>,
}

/// Safe Win32 CopyFileExW progress callback. Also honors pause (blocks in place,
/// freezing the in-flight copy) and cancellation while paused.
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

    // Pause: block inside the callback so the in-flight CopyFileExW freezes in place.
    while global.pause_flag.load(Ordering::Relaxed) && !global.cancel_flag.load(Ordering::Relaxed) {
        std::thread::sleep(PAUSE_POLL_INTERVAL);
    }

    if global.cancel_flag.load(Ordering::Relaxed) {
        return 1; // PROGRESS_CANCEL (cancels copy and deletes destination file)
    }

    let current = total_bytes_transferred as u64;
    let delta = current.saturating_sub(state.last_bytes.get());
    state.last_bytes.set(current);
    global.bytes_completed.fetch_add(delta, Ordering::SeqCst);

    0 // PROGRESS_CONTINUE
}

/// Returns true if `path` metadata can be read (used to detect detached volumes).
pub fn path_accessible(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}

/// Waits until `parent` becomes accessible again, honoring cancellation and pause.
/// Gives up after `timeout` of active waiting (time spent paused does not consume budget).
pub fn wait_for_device(
    parent: &Path,
    cancel: &AtomicBool,
    pause: &AtomicBool,
    timeout: Duration,
) -> bool {
    let mut budget = timeout;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        if !pause.load(Ordering::Relaxed) {
            if path_accessible(parent) {
                return true;
            }
            let wait = DEVICE_POLL_INTERVAL.min(budget);
            if wait.is_zero() {
                return false;
            }
            thread::sleep(wait);
            budget = budget.saturating_sub(wait);
        } else {
            // Paused: hold until resumed or cancelled; do not consume the resilience budget.
            while pause.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
                thread::sleep(PAUSE_POLL_INTERVAL);
            }
        }
    }
}

/// Fast, deterministic content fingerprint (FNV-1a 64-bit). Used for verified moves.
pub fn file_hash_fnv1a64(path: &Path) -> std::io::Result<u64> {
    let mut file = fs::File::open(path)?;
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut i = 0;
        while i < n {
            hash ^= buf[i] as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
            i += 1;
        }
    }
    Ok(hash)
}

/// Verifies that the destination matches the source before a two-phase move delete.
/// When `verify` is true a content hash comparison is performed; otherwise only size.
pub fn files_match_verified(src: &Path, dest: &Path, src_size: u64, verify: bool) -> bool {
    if verify {
        match (file_hash_fnv1a64(src), file_hash_fnv1a64(dest)) {
            (Ok(s), Ok(d)) => s == d,
            _ => false,
        }
    } else {
        fs::metadata(dest).map(|m| m.len() == src_size).unwrap_or(false)
    }
}

/// Applies the source timestamps to a path (used for both files and directories).
fn set_file_times(path: &Path, creation: u64, access: u64, write_time: u64) -> Result<(), String> {
    let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            WRITE_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        );
        if let Ok(h) = handle {
            if h != INVALID_HANDLE_VALUE {
                let ft_create = FILETIME {
                    dwLowDateTime: (creation & 0xFFFFFFFF) as u32,
                    dwHighDateTime: (creation >> 32) as u32,
                };
                let ft_access = FILETIME {
                    dwLowDateTime: (access & 0xFFFFFFFF) as u32,
                    dwHighDateTime: (access >> 32) as u32,
                };
                let ft_write = FILETIME {
                    dwLowDateTime: (write_time & 0xFFFFFFFF) as u32,
                    dwHighDateTime: (write_time >> 32) as u32,
                };
                let res = SetFileTime(h, Some(&ft_create), Some(&ft_access), Some(&ft_write));
                let _ = CloseHandle(h);
                return res.map_err(|e| e.to_string());
            }
            return Err("Invalid handle when applying file times".to_string());
        }
        Err(format!("Failed to open destination for timestamp preservation: {}", path.display()))
    }
}

/// Replicates source file attributes + timestamps onto the destination copy.
fn apply_file_metadata(item: &crate::walker::CopyItem) -> Result<(), String> {
    let attrs = item.attributes & FILE_METADATA_MASK;
    unsafe {
        let _ = SetFileAttributesW(PCWSTR(item.dest_wide.as_ptr()), FILE_ATTRIBUTE_FLAGS(attrs));
    }
    set_file_times(&item.dest_path, item.creation_time, item.last_access_time, item.last_write_time)
}

/// Apply directory timestamps after a copy pass.
fn apply_directory_timestamps(dirs: &[crate::walker::CopyItem]) -> Result<(), String> {
    for dir in dirs {
        let _ = set_file_times(&dir.dest_path, dir.creation_time, dir.last_access_time, dir.last_write_time);
    }
    Ok(())
}

/// Actual CopyFileExW invocation. Does not do failure accounting or resilience.
fn do_copy(
    src_wide: &[u16],
    dest_wide: &[u16],
    size: u64,
    global_state: &ProgressState,
) -> Result<(), String> {
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
        res.map_err(|e| e.to_string())
    }
}

/// Copy a single file through Win32 CopyFileExW, preserving metadata on success,
/// and retrying once after waiting for a detached volume when applicable.
fn copy_file_win32(
    item: &crate::walker::CopyItem,
    global_state: &ProgressState,
) -> Result<(), String> {
    let result = do_copy(&item.src_wide, &item.dest_wide, item.size, global_state);

    if result.is_ok() {
        let _ = apply_file_metadata(item);
        return Ok(());
    }

    // Device-gone resiliency: if the destination volume is unreachable, wait for it
    // to come back (honoring pause/cancel) and retry the copy once.
    if let Some(parent) = item.dest_path.parent() {
        if !path_accessible(parent) {
            if wait_for_device(
                parent,
                &global_state.cancel_flag,
                &global_state.pause_flag,
                global_state.resilience_timeout,
            ) {
                return do_copy(&item.src_wide, &item.dest_wide, item.size, global_state);
            }
            return Err(format!(
                "Destination volume unavailable and did not return within {}s: {}",
                global_state.resilience_timeout.as_secs(),
                item.dest_path.display()
            ));
        }
    }
    result
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

/// Blocks while the transfer is paused (used between items so workers do not claim
/// more work while paused).
fn wait_while_paused(cancel: &AtomicBool, pause: &AtomicBool) {
    while pause.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
        thread::sleep(PAUSE_POLL_INTERVAL);
    }
}

/// Run a multi-threaded copy/move operation based on auto-tuned concurrency.
#[allow(clippy::too_many_arguments)]
pub fn run_engine(
    sources: &[PathBuf],
    dest: &Path,
    is_move: bool,
    custom_concurrency: Option<usize>,
    cancel_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    verify: bool,
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
        pause_flag,
        verify,
        progress_callback,
    )
}

/// Run a multi-threaded copy/move operation with a pre-built work list.
#[allow(clippy::too_many_arguments)]
pub fn run_engine_with_work_list(
    work_list: crate::walker::WorkList,
    sources: &[PathBuf],
    dest: &Path,
    is_move: bool,
    custom_concurrency: Option<usize>,
    cancel_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    verify: bool,
    progress_callback: Option<Box<dyn Fn(usize, u64) + Send + Sync>>,
) -> EngineSummary {
    let start_time = Instant::now();

    // Prevent system from sleeping during transfer
    unsafe {
        let _ = SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_CONTINUOUS);
    }

    let profile = profile_device(dest);
    let concurrency = custom_concurrency.unwrap_or(profile.concurrency);
    let resilience_timeout = DEFAULT_RESILIENCE_TIMEOUT;

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
        pause_flag: pause_flag.clone(),
        resilience_timeout,
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
        wait_while_paused(&cancel_flag, &pause_flag);
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
                wait_while_paused(&cancel_flag, &pause_flag);
                unsafe {
                    let res = MoveFileExW(
                        PCWSTR(item.src_wide.as_ptr()),
                        PCWSTR(item.dest_wide.as_ptr()),
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
            wait_while_paused(&cancel_flag, &pause_flag);
            unsafe {
                let res = MoveFileExW(
                    PCWSTR(item.src_wide.as_ptr()),
                    PCWSTR(item.dest_wide.as_ptr()),
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
                wait_while_paused(&cancel_flag, &pause_flag);
                if let Err(e) = copy_file_win32(item, &global_state) {
                    global_state.failed_files.lock().unwrap_or_else(|e| e.into_inner()).push((item.src_path.clone(), e));
                }
            }
            for dir in small_dirs.iter() {
                for item in dir {
                    if cancel_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    wait_while_paused(&cancel_flag, &pause_flag);
                    if let Err(e) = copy_file_win32(item, &global_state) {
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
                let p_flag = pause_flag.clone();

                large_threads.push(thread::spawn(move || {
                    loop {
                        if c_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        wait_while_paused(&c_flag, &p_flag);
                        let current = idx.fetch_add(1, Ordering::SeqCst);
                        if current >= items.len() {
                            break;
                        }
                        let item = &items[current];
                        if let Err(e) = copy_file_win32(item, &state) {
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
                let p_flag = pause_flag.clone();

                small_threads.push(thread::spawn(move || {
                    loop {
                        if c_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        wait_while_paused(&c_flag, &p_flag);
                        let current = idx.fetch_add(1, Ordering::SeqCst);
                        if current >= dirs.len() {
                            break;
                        }
                        let items = &dirs[current];
                        for item in items {
                            if c_flag.load(Ordering::Relaxed) {
                                break;
                            }
                            wait_while_paused(&c_flag, &p_flag);
                            if let Err(e) = copy_file_win32(item, &state) {
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
                    if files_match_verified(&file.src_path, &file.dest_path, file.size, verify) {
                        if let Err(e) = fs::remove_file(&file.src_path) {
                            failures.push((file.src_path.clone(), format!("Move cleanup failed: {}", e)));
                        }
                    } else {
                        failures.push((file.src_path.clone(), "Move verification failed: destination file mismatch or missing".to_string()));
                    }
                }
            }
            for file in large_items.iter() {
                if files_match_verified(&file.src_path, &file.dest_path, file.size, verify) {
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
    pause_flag: Arc<AtomicBool>,
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
        pause_flag,
        progress_callback,
    )
}

/// Run a multi-threaded parallel delete operation with a pre-built delete list.
pub fn run_delete_engine_with_delete_list(
    delete_list: crate::walker::DeleteList,
    _sources: &[PathBuf],
    concurrency: usize,
    cancel_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
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
        pause_flag: pause_flag.clone(),
        resilience_timeout: DEFAULT_RESILIENCE_TIMEOUT,
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
            let p_flag = pause_flag.clone();

            threads.push(thread::spawn(move || {
                loop {
                    if c_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    wait_while_paused(&c_flag, &p_flag);
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