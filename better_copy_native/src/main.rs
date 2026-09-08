#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod overlay;
mod settings;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4, ShellExecuteW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetCursorPos,
    AppendMenuW,
    GetMessageW, HMENU, HWND_MESSAGE, LoadIconW, MF_CHECKED, MF_SEPARATOR, MF_STRING,
    MF_UNCHECKED, MSG, PostQuitMessage,
    RegisterClassW, SW_SHOWNORMAL, TrackPopupMenu, TranslateMessage,
    WNDCLASSW,
};

use crate::app::{APP, AppState, JobSpec, UiMode, UiView};
use crate::overlay::OverlayAction;
use crate::settings::Settings;

const TRAY_ID: u32 = 1001;

const TRAY_SHOW: usize = 1;
const TRAY_PAUSE: usize = 2;
const TRAY_CANCEL: usize = 3;
const TRAY_HOTKEYS: usize = 4;
const TRAY_AUTOSTART: usize = 5;
const TRAY_AUTOCLOSE: usize = 6;
const TRAY_LOG_DIR: usize = 7;
const TRAY_QUIT: usize = 8;

const ELEVATED_JOB_FILE: &str = "better_copy_elevated_job.json";
const ELEVATED_RESULT_FILE: &str = "better_copy_elevated_result.json";

type TriggerSender = mpsc::Sender<better_copy_core::trigger::HotkeyEvent>;
static EVENT_TX: OnceLock<Mutex<TriggerSender>> = OnceLock::new();

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn module_instance() -> HINSTANCE {
    unsafe {
        match GetModuleHandleW(None) {
            Ok(h) => HINSTANCE(h.0),
            Err(_) => HINSTANCE(std::ptr::null_mut()),
        }
    }
}

fn show_dashboard(app: &Arc<AppState>) {
    if let Ok(lock) = app.overlay_hwnd.lock() {
        if let Some(hwnd) = *lock {
            crate::overlay::show(hwnd.0);
        }
    }
}

fn hide_dashboard(app: &Arc<AppState>) {
    if let Ok(lock) = app.overlay_hwnd.lock() {
        if let Some(hwnd) = *lock {
            crate::overlay::hide(hwnd.0);
        }
    }
}

fn toggle_dashboard(app: &Arc<AppState>) {
    if app.overlay_visible.load(Ordering::SeqCst) {
        hide_dashboard(app);
    } else {
        show_dashboard(app);
    }
}

fn set_ui(app: &Arc<AppState>, f: impl FnOnce(&mut UiView)) {
    if let Ok(mut ui) = app.ui.lock() {
        f(&mut ui);
    }
    app.refresh();
}

pub(crate) fn format_eta(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return String::from("--");
    }
    let total = secs as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

fn now_stamp() -> String {
    let t = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(t);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

fn civil_from_unix(ts: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = ts / 86_400;
    let rem = ts % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    (y as i64, mo, d, h as u32, mi as u32, s as u32)
}

fn civil_from_days(z: u64) -> (i64, u32, u32) {
    let z = z as i64 - 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

fn persist_log(text: &str) -> String {
    let dir = Settings::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let name = format!("job_{}.log", now_stamp().replace(':', "-"));
    let path = dir.join(name);
    let _ = std::fs::write(&path, text);
    path.to_string_lossy().into_owned()
}

fn open_folder(folder: &str) {
    let wide_dir = wide(folder);
    unsafe {
        let _ = ShellExecuteW(
            HWND(std::ptr::null_mut()),
            w!("explore"),
            PCWSTR(wide_dir.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn open_file(path: &str) {
    let wide_path = wide(path);
    unsafe {
        let _ = ShellExecuteW(
            HWND(std::ptr::null_mut()),
            w!("open"),
            PCWSTR(wide_path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn copy_job_title(is_move: bool) -> String {
    if is_move {
        "Moving items…".to_string()
    } else {
        "Copying items…".to_string()
    }
}

// ---------------------------------------------------------------------------
// Job execution
// ---------------------------------------------------------------------------

fn start_copy_job(spec: JobSpec) {
    let app = APP.get().unwrap().clone();
    if app.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    app.reset_job_flags();
    {
        let _ = app.last_job.lock().map(|mut j| *j = Some(spec.clone()));
    }
    set_ui(&app, |ui| {
        ui.mode = UiMode::Analyzing;
        ui.title = copy_job_title(spec.is_move);
        ui.detail = format!("To {}", spec.dest.display());
        ui.files_done = 0;
        ui.bytes_done = 0;
        ui.total_files = 0;
        ui.total_bytes = 0;
        ui.failures.clear();
        ui.cancelled = false;
        ui.is_delete = false;
        ui.log_path = String::new();
    });
    show_dashboard(&app);
    let worker = app.clone();
    let _ = std::thread::Builder::new()
        .name("copy-job".to_string())
        .spawn(move || run_copy_worker(worker, spec));
}

fn run_copy_worker(app: Arc<AppState>, spec: JobSpec) {
    use better_copy_core::{engine, walker};

    let existing: Vec<PathBuf> = spec
        .sources
        .iter()
        .filter(|p| p.exists())
        .cloned()
        .collect();

    let wl_result = walker::build_work_list(&existing, &spec.dest, Some(&app.cancel), None);
    let work_list = match wl_result {
        Ok(w) => w,
        Err(e) => {
            set_ui(&app, |ui| {
                ui.mode = UiMode::Done;
                ui.cancelled = true;
                ui.failures = vec![(
                    spec.dest.clone(),
                    format!("Failed to analyze: {}", e),
                )];
                ui.detail = "Could not read the destination folder.".to_string();
            });
            app.busy.store(false, Ordering::SeqCst);
            return;
        }
    };

    let total_files = work_list.total_files;
    let total_bytes = work_list.total_bytes;

    if app.cancel.load(Ordering::SeqCst) {
        finish_cancelled(app);
        return;
    }

    if total_files == 0 {
        set_ui(&app, |ui| {
            ui.mode = UiMode::Done;
            ui.title = "Nothing to copy".to_string();
            ui.detail = "No files were found in the selection.".to_string();
            ui.cancelled = false;
        });
        app.busy.store(false, Ordering::SeqCst);
        maybe_auto_close_dashboard(&app);
        app.refresh();
        return;
    }

    set_ui(&app, |ui| {
        ui.mode = UiMode::Running;
        ui.title = copy_job_title(spec.is_move);
        ui.detail = format!(
            "To {} · {} files, {:.1} MB",
            spec.dest.display(),
            total_files,
            total_bytes as f64 / (1024.0 * 1024.0)
        );
        ui.total_files = total_files;
        ui.total_bytes = total_bytes;
    });

    let start = Instant::now();
    let cb_app = app.clone();
    let cb = Box::new(move |files: usize, bytes: u64| {
        let elapsed = start.elapsed().as_secs_f64();
        if let Ok(mut ui) = cb_app.ui.lock() {
            ui.files_done = files;
            ui.bytes_done = bytes;
            if elapsed > 0.001 {
                let bps = bytes as f64 / elapsed;
                ui.speed_mbps = bps / (1024.0 * 1024.0);
                ui.eta_secs = if bps > 0.0 && bytes < total_bytes {
                    (total_bytes - bytes) as f64 / bps
                } else {
                    0.0
                };
            }
        }
        cb_app.refresh();
    });

    let summary = engine::run_engine_with_work_list(
        work_list,
        &existing,
        &spec.dest,
        spec.is_move,
        None,
        app.cancel.clone(),
        app.pause.clone(),
        spec.verify,
        Some(cb),
    );

    finish_job(app, summary, false, &spec, start.elapsed());
}

fn start_delete_job(sources: Vec<PathBuf>) {
    let app = APP.get().unwrap().clone();
    if app.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    app.reset_job_flags();
    {
        let spec = JobSpec {
            sources: sources.clone(),
            dest: PathBuf::new(),
            is_move: false,
            verify: false,
        };
        let _ = app.last_job.lock().map(|mut j| *j = Some(spec));
    }
    set_ui(&app, |ui| {
        ui.mode = UiMode::Analyzing;
        ui.title = "Deleting items…".to_string();
        ui.detail = format!("{} item(s) to delete", sources.len());
        ui.files_done = 0;
        ui.bytes_done = 0;
        ui.total_files = 0;
        ui.total_bytes = 0;
        ui.failures.clear();
        ui.cancelled = false;
        ui.is_delete = true;
        ui.log_path = String::new();
    });
    show_dashboard(&app);
    let worker = app.clone();
    let _ = std::thread::Builder::new()
        .name("delete-job".to_string())
        .spawn(move || run_delete_worker(worker, sources));
}

fn run_delete_worker(app: Arc<AppState>, sources: Vec<PathBuf>) {
    use better_copy_core::{engine, walker};

    let dl_result = walker::build_delete_list(&sources, Some(&app.cancel), None);
    let delete_list = match dl_result {
        Ok(d) => d,
        Err(e) => {
            set_ui(&app, |ui| {
                ui.mode = UiMode::Done;
                ui.cancelled = true;
                ui.failures = vec![(
                    sources.first().cloned().unwrap_or_default(),
                    format!("Failed to analyze: {}", e),
                )];
            });
            app.busy.store(false, Ordering::SeqCst);
            return;
        }
    };

    let total = delete_list.files.len() + delete_list.dirs.len();
    if total == 0 {
        set_ui(&app, |ui| {
            ui.mode = UiMode::Done;
            ui.title = "Nothing to delete".to_string();
            ui.detail = "The selection is already empty.".to_string();
        });
        app.busy.store(false, Ordering::SeqCst);
        maybe_auto_close_dashboard(&app);
        app.refresh();
        return;
    }

    set_ui(&app, |ui| {
        ui.mode = UiMode::Running;
        ui.total_files = total;
        ui.detail = format!("{} item(s) queued", total);
    });

    let start = Instant::now();
    let cb_app = app.clone();
    let cb = Box::new(move |files: usize, _bytes: u64| {
        if let Ok(mut ui) = cb_app.ui.lock() {
            ui.files_done = files;
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.001 && files > 0 {
                ui.speed_mbps = files as f64 / elapsed;
                ui.eta_secs = if ui.total_files > files {
                    (ui.total_files - files) as f64 / ui.speed_mbps
                } else {
                    0.0
                };
            }
        }
        cb_app.refresh();
    });

    let summary = engine::run_delete_engine_with_delete_list(
        delete_list,
        &sources,
        8,
        app.cancel.clone(),
        app.pause.clone(),
        Some(cb),
    );

    finish_job(app, summary, true, &JobSpec {
        sources,
        dest: PathBuf::new(),
        is_move: false,
        verify: false,
    }, start.elapsed());
}

fn finish_cancelled(app: Arc<AppState>) {
    set_ui(&app, |ui| {
        ui.mode = UiMode::Done;
        ui.cancelled = true;
        ui.title = "Cancelled".to_string();
    });
    app.busy.store(false, Ordering::SeqCst);
    app.refresh();
}

fn build_log(app: &AppState, op: &str, sources: &[PathBuf], dest: &PathBuf, is_move: bool, verify: bool, files_done: usize, bytes_done: u64, elapsed: Duration, was_cancelled: bool, failures: &[(PathBuf, String)]) -> String {
    let mut out = String::new();
    out.push_str("BetterCopy job log\n====================\n");
    out.push_str(&format!("Started:    {}\n", now_stamp()));
    out.push_str(&format!("Operation:  {} ({})\n", op, if is_move { "move" } else { "copy" }));
    out.push_str(&format!("Verify:     {}\n", verify));
    out.push_str("Sources:\n");
    for s in sources {
        out.push_str(&format!("  - {}\n", s.display()));
    }
    out.push_str(&format!("Destination: {}\n", dest.display()));
    out.push_str(&format!("Files:      {} copied\n", files_done));
    out.push_str(&format!("Bytes:      {}\n", bytes_done));
    out.push_str(&format!("Elapsed:    {:.2}s\n", elapsed.as_secs_f64()));
    out.push_str(&format!("Cancelled:  {}\n", was_cancelled));
    out.push_str("\nFailures:\n");
    if failures.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for (p, e) in failures {
            out.push_str(&format!("  - {} : {}\n", p.display(), e));
        }
    }
    out
}

fn finish_job(
    app: Arc<AppState>,
    summary: better_copy_core::engine::EngineSummary,
    is_delete: bool,
    spec: &JobSpec,
    elapsed: Duration,
) {
    let op = if is_delete {
        "Delete"
    } else if spec.is_move {
        "Move"
    } else {
        "Copy"
    };
    let log = build_log(
        &app,
        op,
        &spec.sources,
        &spec.dest,
        spec.is_move,
        spec.verify,
        summary.files_copied,
        summary.bytes_copied,
        elapsed,
        summary.was_cancelled,
        &summary.failures,
    );
    let log_path = persist_log(&log);

    set_ui(&app, |ui| {
        ui.mode = UiMode::Done;
        ui.files_done = summary.files_copied;
        ui.bytes_done = summary.bytes_copied;
        ui.failures = summary.failures;
        ui.cancelled = summary.was_cancelled;
        ui.is_delete = is_delete;
        ui.log_path = log_path;
        ui.speed_mbps = 0.0;
        ui.eta_secs = 0.0;
        ui.title = if is_delete {
            "Deletion finished".to_string()
        } else {
            "Copy finished".to_string()
        };
    });
    app.busy.store(false, Ordering::SeqCst);
    maybe_auto_close_dashboard(&app);
    app.refresh();
}

fn maybe_auto_close_dashboard(app: &Arc<AppState>) {
    let auto = {
        let s = app.settings.lock().unwrap();
        s.auto_close
    };
    let ok_view = {
        if let Ok(ui) = app.ui.lock() {
            ui.failures.is_empty() && !ui.cancelled && app.overlay_visible.load(Ordering::SeqCst)
        } else {
            false
        }
    };
    if auto && ok_view {
        if let Ok(lock) = app.overlay_hwnd.lock() {
            if let Some(hwnd) = *lock {
                let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    hwnd.0,
                    crate::app::WM_UI_CLOSE_DELAY,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                ) };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Elevated "Run as administrator" retry
// ---------------------------------------------------------------------------

fn elevated_result_path() -> PathBuf {
    std::env::temp_dir().join(ELEVATED_RESULT_FILE)
}

fn elevated_job_path() -> PathBuf {
    std::env::temp_dir().join(ELEVATED_JOB_FILE)
}

fn run_elevated(app: &Arc<AppState>) {
    let spec = match app.last_job.lock() {
        Ok(guard) => match guard.clone() {
            Some(s) => s,
            None => {
                set_ui(app, |ui| {
                    ui.mode = UiMode::Done;
                    ui.failures = vec![(PathBuf::new(), "No previous job to retry.".to_string())];
                    ui.title = "Nothing to retry".to_string();
                });
                return;
            }
        },
        Err(_) => return,
    };
    // Reset the busy flag so the shortcut can run again afterwards.
    app.busy.store(true, Ordering::SeqCst);
    app.reset_job_flags();

    let job_file = elevated_job_path();
    let result_file = elevated_result_path();
    let _ = std::fs::remove_file(&result_file);

    if serde_json::to_string(&spec).is_err() {
        set_ui(app, |ui| {
            ui.mode = UiMode::Done;
            ui.failures = vec![(PathBuf::new(), "Could not serialize the job.".to_string())];
        });
        app.busy.store(false, Ordering::SeqCst);
        return;
    }
    if std::fs::write(&job_file, serde_json::to_string(&spec).unwrap()).is_err() {
        set_ui(app, |ui| {
            ui.mode = UiMode::Done;
            ui.failures = vec![(PathBuf::new(), "Could not write the job file.".to_string())];
        });
        app.busy.store(false, Ordering::SeqCst);
        return;
    }

    let args = format!("--elevated-job \"{}\"", job_file.display());
    let exe_wide = wide(&app.exe_path);
    let args_wide = wide(&args);

    unsafe {
        let hr = ShellExecuteW(
            HWND(std::ptr::null_mut()),
            w!("runas"),
            PCWSTR(exe_wide.as_ptr()),
            PCWSTR(args_wide.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
        .0 as isize;
        if hr <= 32 {
            set_ui(app, |ui| {
                ui.mode = UiMode::Done;
                ui.failures = vec![(
                    PathBuf::new(),
                    format!("Could not start elevated helper (code {}): the UAC prompt may have been declined.", hr),
                )];
            });
            app.busy.store(false, Ordering::SeqCst);
            return;
        }
    }

    set_ui(app, |ui| {
        ui.mode = UiMode::Analyzing;
        ui.title = "Waiting for elevated helper…".to_string();
        ui.detail = "Accept the UAC prompt to run the last job as administrator.".to_string();
        ui.failures.clear();
        ui.cancelled = false;
    });
    show_dashboard(app);

    let poller = app.clone();
    let _ = std::thread::Builder::new()
        .name("elevated-poller".to_string())
        .spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(600);
            while Instant::now() < deadline {
                if result_file.exists() {
                    let text = std::fs::read_to_string(&result_file).unwrap_or_default();
                    let _ = std::fs::remove_file(&result_file);
                    let parsed: Result<ElevatedResult, _> = serde_json::from_str(&text);
                    match parsed {
                        Ok(res) => {
                            let failures: Vec<(PathBuf, String)> = res
                                .failures
                                .into_iter()
                                .map(|(p, m)| (PathBuf::from(p), m))
                                .collect();
                            set_ui(&poller, |ui| {
                                ui.mode = UiMode::Done;
                                ui.files_done = res.files_copied;
                                ui.bytes_done = res.bytes_copied;
                                ui.failures = failures;
                                ui.cancelled = false;
                                ui.is_delete = res.is_delete;
                                ui.title = "Elevated retry finished".to_string();
                            });
                            poller.busy.store(false, Ordering::SeqCst);
                            return;
                        }
                        Err(_) => {
                            set_ui(&poller, |ui| {
                                ui.mode = UiMode::Done;
                                ui.title = "Elevated retry finished".to_string();
                                ui.failures =
                                    vec![(PathBuf::new(), "Unreadable result file.".to_string())];
                            });
                            poller.busy.store(false, Ordering::SeqCst);
                            return;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(400));
            }
            set_ui(&poller, |ui| {
                ui.mode = UiMode::Done;
                ui.failures = vec![(
                    PathBuf::new(),
                    "Elevated helper timed out after 10 minutes.".to_string(),
                )];
                ui.title = "Elevated retry timed out".to_string();
            });
            poller.busy.store(false, Ordering::SeqCst);
        });
}

#[derive(serde::Deserialize)]
struct ElevatedResult {
    ok: bool,
    files_copied: usize,
    bytes_copied: u64,
    is_delete: bool,
    #[serde(default)]
    failures: Vec<(String, String)>,
}

/// Entry path for the elevated child process (no UI, engine only).
fn elevated_main(job_file: &str) -> i32 {
    let text = match std::fs::read_to_string(job_file) {
        Ok(t) => t,
        Err(e) => {
            write_elevated_result(false, 0, 0, false, vec![("", format!("no job file: {}", e))]);
            return 2;
        }
    };
    let spec: Result<JobSpec, _> = serde_json::from_str(&text);
    let spec = match spec {
        Ok(s) => s,
        Err(e) => {
            write_elevated_result(false, 0, 0, false, vec![("", format!("bad job json: {}", e))]);
            return 2;
        }
    };

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pause = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    if spec.dest.as_os_str().is_empty() {
        let sources: Vec<PathBuf> = spec.sources.iter().filter(|p| p.exists()).cloned().collect();
        let summary =
            better_copy_core::engine::run_delete_engine(&sources, 8, cancel, pause, None);
        let failures = summary
            .failures
            .into_iter()
            .map(|(p, m)| (p.display().to_string(), m))
            .collect::<Vec<_>>();
        write_elevated_result(
            failures.is_empty(),
            summary.files_copied,
            summary.bytes_copied,
            true,
            failures,
        );
    } else {
        let sources: Vec<PathBuf> = spec.sources.iter().filter(|p| p.exists()).cloned().collect();
        let summary = better_copy_core::engine::run_engine(
            &sources,
            &spec.dest,
            spec.is_move,
            None,
            cancel,
            pause,
            true,
            None,
        );
        let failures = summary
            .failures
            .into_iter()
            .map(|(p, m)| (p.display().to_string(), m))
            .collect::<Vec<_>>();
        write_elevated_result(
            failures.is_empty(),
            summary.files_copied,
            summary.bytes_copied,
            false,
            failures,
        );
    }
    0
}

fn write_elevated_result(
    ok: bool,
    files_copied: usize,
    bytes_copied: u64,
    is_delete: bool,
    failures: Vec<(String, String)>,
) {
    let result = serde_json::json!({
        "ok": ok,
        "files_copied": files_copied,
        "bytes_copied": bytes_copied,
        "is_delete": is_delete,
        "failures": failures,
    });
    let _ = std::fs::write(elevated_result_path(), serde_json::to_string(&result).unwrap_or_default());
}

// ---------------------------------------------------------------------------
// Tray icon + menu
// ---------------------------------------------------------------------------

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WM_DESTROY: u32 = 0x0002;
    const WM_LBUTTONDBLCLK: u32 = 0x0203;
    const WM_RBUTTONUP: u32 = 0x0205;
    const WM_CONTEXTMENU: u32 = 0x007B;

    if msg == crate::app::WM_TRAY_CALLBACK {
        if wparam.0 as u32 == TRAY_ID {
            let event = lparam.0 as u32;
            match event {
                WM_LBUTTONDBLCLK => {
                    if let Some(app) = APP.get() {
                        toggle_dashboard(app);
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    if let Some(app) = APP.get() {
                        tray_menu(hwnd, app);
                    }
                }
                _ => {}
            }
        }
        LRESULT(0)
    } else if msg == WM_DESTROY {
        PostQuitMessage(0);
        LRESULT(0)
    } else {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

fn tray_menu(hwnd: HWND, app: &Arc<AppState>) {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let menu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(_) => return,
        };

        let settings = app.settings.lock().unwrap().clone();
        let hotkeys = app.hotkeys_enabled.load(Ordering::SeqCst);

        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };

        let _ = AppendMenuW(menu, MF_STRING, TRAY_SHOW, w!("Show Progress"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, TRAY_PAUSE, w!("Pause / Resume Current Job"));
        let _ = AppendMenuW(menu, MF_STRING, TRAY_CANCEL, w!("Cancel Current Job"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(
            menu,
            MF_STRING | check(hotkeys),
            TRAY_HOTKEYS,
            w!("Quick Keys (Ctrl+Shift+V / Ctrl+Shift+Del)"),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING | check(settings.start_on_boot),
            TRAY_AUTOSTART,
            w!("Start with Windows"),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING | check(settings.auto_close),
            TRAY_AUTOCLOSE,
            w!("Auto-close Dashboard on Success"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, TRAY_LOG_DIR, w!("Open Logs Folder"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, TRAY_QUIT, w!("Exit BetterCopy"));

        let cmd = TrackPopupMenu(
            menu,
            windows::Win32::UI::WindowsAndMessaging::TPM_RETURNCMD
                | windows::Win32::UI::WindowsAndMessaging::TPM_RIGHTBUTTON
                | windows::Win32::UI::WindowsAndMessaging::TPM_LEFTALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);

        if cmd == 0 {
            return;
        }

        match cmd {
            TRAY_SHOW => show_dashboard(app),
            TRAY_PAUSE => {
                if app.busy.load(Ordering::SeqCst) {
                    let cur = app.pause.load(Ordering::SeqCst);
                    app.pause.store(!cur, Ordering::SeqCst);
                    app.refresh();
                }
            }
            TRAY_CANCEL => {
                app.cancel.store(true, Ordering::SeqCst);
                app.refresh();
            }
            TRAY_HOTKEYS => {
                let next = !app.hotkeys_enabled.load(Ordering::SeqCst);
                app.hotkeys_enabled.store(next, Ordering::SeqCst);
                if let Ok(mut s) = app.settings.lock() {
                    s.hotkeys_enabled = next;
                    s.save();
                }
            }
            TRAY_AUTOSTART => {
                let current = app.settings.lock().unwrap().start_on_boot;
                let next = !current;
                if let Ok(mut s) = app.settings.lock() {
                    s.start_on_boot = next;
                    s.save();
                }
                let exe = std::env::current_exe()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                app.settings
                    .lock()
                    .unwrap()
                    .apply_autostart(Some(&exe));
            }
            TRAY_AUTOCLOSE => {
                if let Ok(mut s) = app.settings.lock() {
                    s.auto_close = !s.auto_close;
                    s.save();
                }
            }
            TRAY_LOG_DIR => {
                let dir = Settings::logs_dir();
                if let Some(dir) = dir.to_str() {
                    open_folder(dir);
                }
            }
            TRAY_QUIT => {
                app.cancel.store(true, Ordering::SeqCst);
                PostQuitMessage(0);
            }
            _ => {}
        }
    }
}

fn add_tray_icon(app: &Arc<AppState>) -> bool {
    unsafe {
        let tray_hwnd = {
            match app.tray_hwnd.lock() {
                Ok(g) => match *g {
                    Some(h) => h.0,
                    None => return false,
                },
                Err(_) => return false,
            }
        };

        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = tray_hwnd;
        nid.uID = TRAY_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = crate::app::WM_TRAY_CALLBACK;
        nid.hIcon = LoadIconW(HINSTANCE(std::ptr::null_mut()), windows::Win32::UI::WindowsAndMessaging::IDI_APPLICATION)
            .unwrap_or(windows::Win32::UI::WindowsAndMessaging::HICON(std::ptr::null_mut()));
        let tip = wide("BetterCopy — copy & delete helper");
        let copy_len = tip.len().min(nid.szTip.len() - 1);
        nid.szTip[..copy_len].copy_from_slice(&tip[..copy_len]);

        let ok = Shell_NotifyIconW(NIM_ADD, &nid).0 != 0;
        if ok {
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        }
        ok
    }
}

fn remove_tray_icon(app: &Arc<AppState>) {
    unsafe {
        let Some(tray_hwnd) = (match app.tray_hwnd.lock() { Ok(g) => *g, Err(_) => None }) else {
            return;
        };
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = tray_hwnd.0;
        nid.uID = TRAY_ID;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn create_tray_host() -> HWND {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: tray_wnd_proc,
            hInstance: module_instance(),
            lpszClassName: w!("BetterCopyTrayHost"),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            w!("BetterCopyTrayHost"),
            w!(""),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            HMENU(std::ptr::null_mut()),
            module_instance(),
            None,
        )
        .unwrap_or(HWND(std::ptr::null_mut()))
    }
}

// ---------------------------------------------------------------------------
// Hotkey trigger dispatch
// ---------------------------------------------------------------------------

fn on_trigger_event(
    event: better_copy_core::trigger::HotkeyEvent,
) {
    let app = match APP.get() {
        Some(a) => a.clone(),
        None => return,
    };
    if !app.hotkeys_enabled.load(Ordering::SeqCst) {
        return;
    }
    match event {
        better_copy_core::trigger::HotkeyEvent::Paste { clipboard, destination } => {
            let spec = JobSpec {
                sources: clipboard.paths,
                dest: destination,
                is_move: clipboard.is_move,
                verify: true,
            };
            start_copy_job(spec);
        }
        better_copy_core::trigger::HotkeyEvent::Delete { sources } => {
            start_delete_job(sources);
        }
    }
}

// ---------------------------------------------------------------------------
// Action handler for the dashboard buttons
// ---------------------------------------------------------------------------

fn handle_action(action: OverlayAction) {
    let app = match APP.get() {
        Some(a) => a.clone(),
        None => return,
    };
    match action {
        OverlayAction::TogglePause => {
            let cur = app.pause.load(Ordering::SeqCst);
            app.pause.store(!cur, Ordering::SeqCst);
            app.refresh();
        }
        OverlayAction::Cancel => {
            app.cancel.store(true, Ordering::SeqCst);
            app.refresh();
        }
        OverlayAction::Close => hide_dashboard(&app),
        OverlayAction::ViewLog => {
            let log_path = {
                if let Ok(ui) = app.ui.lock() {
                    ui.log_path.clone()
                } else {
                    String::new()
                }
            };
            if log_path.is_empty() {
                let dir = Settings::logs_dir();
                if let Some(dir) = dir.to_str() {
                    open_folder(dir);
                }
            } else {
                open_file(&log_path);
            }
        }
        OverlayAction::RunElevated => run_elevated(&app),
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn acquire_single_instance() -> bool {
    unsafe {
        let name = wide("Local\\BetterCopyNative");
        let _ = CreateMutexW(None, BOOL(0), PCWSTR(name.as_ptr()));
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--elevated-job" {
        let code = elevated_main(&args[2]);
        std::process::exit(code);
    }
    if !acquire_single_instance() {
        std::process::exit(0);
    }

    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut settings = Settings::load();
    settings.apply_autostart(Some(&exe_path));

    let app = Arc::new(AppState::new(exe_path, settings));
    crate::app::set_app(app.clone());
    crate::overlay::set_action_handler(handle_action);

    let overlay = crate::overlay::Overlay::create(module_instance());
    if let Ok(mut lock) = app.overlay_hwnd.lock() {
        *lock = Some(crate::app::SafeHwnd(overlay.hwnd));
    }

    let tray_hwnd = create_tray_host();
    if let Ok(mut lock) = app.tray_hwnd.lock() {
        *lock = Some(crate::app::SafeHwnd(tray_hwnd));
    }
    let _ = add_tray_icon(&app);

    // Start the hotkey trigger; events are filtered by the hotkeys_enabled flag.
    let (tx, rx) = mpsc::channel::<better_copy_core::trigger::HotkeyEvent>();
    let _ = EVENT_TX.set(Mutex::new(tx));
    match better_copy_core::trigger::HotkeyTrigger::start(move |event| {
        if let Some(t) = EVENT_TX.get() {
            if let Ok(guard) = t.lock() {
                let _ = guard.send(event);
            }
        }
    }) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("BetterCopy: hotkey trigger unavailable: {}", e);
        }
    }

    // Message pump
    unsafe {
        loop {
            while let Ok(event) = rx.try_recv() {
                on_trigger_event(event);
            }
            let mut msg = MSG::default();
            let result = GetMessageW(&mut msg, None, 0, 0);
            if result.0 == 0 || result.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&msg);
            let _ = windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
        }
    }

    remove_tray_icon(&app);
}
