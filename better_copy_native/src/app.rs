use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::settings::Settings;

pub const WM_UI_REFRESH: u32 = 0x8000 + 1;
pub const WM_UI_CLOSE_DELAY: u32 = 0x8000 + 2;
pub const WM_TRAY_CALLBACK: u32 = 0x8000 + 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiMode {
    Hidden,
    Analyzing,
    Running,
    Done,
}

#[derive(Clone, Debug)]
pub struct UiView {
    pub mode: UiMode,
    pub title: String,
    pub detail: String,
    pub files_done: usize,
    pub bytes_done: u64,
    pub total_files: usize,
    pub total_bytes: u64,
    pub speed_mbps: f64,
    pub eta_secs: f64,
    pub failures: Vec<(PathBuf, String)>,
    pub cancelled: bool,
    pub is_delete: bool,
    pub log_path: String,
}

impl Default for UiView {
    fn default() -> Self {
        Self {
            mode: UiMode::Hidden,
            title: String::new(),
            detail: String::new(),
            files_done: 0,
            bytes_done: 0,
            total_files: 0,
            total_bytes: 0,
            speed_mbps: 0.0,
            eta_secs: 0.0,
            failures: Vec::new(),
            cancelled: false,
            is_delete: false,
            log_path: String::new(),
        }
    }
}

impl UiView {
    pub fn percent(&self) -> f64 {
        if self.total_files == 0 {
            return 0.0;
        }
        let ratio = self.files_done as f64 / self.total_files as f64;
        (ratio * 100.0).clamp(0.0, 100.0)
    }
}

/// A serializable description of the last user job (used for the elevated
/// "Run as administrator" retry).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobSpec {
    pub sources: Vec<PathBuf>,
    pub dest: PathBuf,
    pub is_move: bool,
    pub verify: bool,
}

pub struct AppState {
    pub exe_path: String,
    pub ui: Mutex<UiView>,
    pub cancel: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub busy: AtomicBool,
    pub hotkeys_enabled: AtomicBool,
    pub settings: Mutex<Settings>,
    pub overlay_hwnd: Mutex<Option<HWND>>,
    pub tray_hwnd: Mutex<Option<HWND>>,
    pub overlay_visible: AtomicBool,
    pub last_job: Mutex<Option<JobSpec>>,
    last_refresh_ms: AtomicI64,
}

impl AppState {
    pub fn new(exe_path: String, settings: Settings) -> Self {
        Self {
            exe_path,
            ui: Mutex::new(UiView::default()),
            cancel: Arc::new(AtomicBool::new(false)),
            pause: Arc::new(AtomicBool::new(false)),
            busy: AtomicBool::new(false),
            hotkeys_enabled: AtomicBool::new(settings.hotkeys_enabled),
            settings: Mutex::new(settings),
            overlay_hwnd: Mutex::new(None),
            tray_hwnd: Mutex::new(None),
            overlay_visible: AtomicBool::new(false),
            last_job: Mutex::new(None),
            last_refresh_ms: AtomicI64::new(0),
        }
    }

    /// Tears down the previous operation flags so a new job starts clean.
    pub fn reset_job_flags(&self) {
        self.cancel.store(false, Ordering::SeqCst);
        self.pause.store(false, Ordering::SeqCst);
    }

    /// Throttled cross-thread repaint request for the dashboard window.
    pub fn refresh(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if now - self.last_refresh_ms.load(Ordering::Relaxed) < 40 {
            return;
        }
        self.last_refresh_ms.store(now, Ordering::Relaxed);
        if let Ok(lock) = self.overlay_hwnd.lock() {
            if let Some(h) = *lock {
                let _ = unsafe { PostMessageW(h, WM_UI_REFRESH, 0, 0) };
            }
        }
    }

    pub fn set_overlay_visible(&self, visible: bool) {
        self.overlay_visible.store(visible, Ordering::SeqCst);
    }
}

pub static APP: OnceLock<Arc<AppState>> = OnceLock::new();

pub fn set_app(app: Arc<AppState>) {
    let _ = APP.set(app);
}