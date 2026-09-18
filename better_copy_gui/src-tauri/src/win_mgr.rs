use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindow};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppOpts {
    pub startup: bool,
    pub close_to_tray: bool,
    pub start_as_admin: bool,
}

impl Default for AppOpts {
    fn default() -> Self {
        Self {
            startup: false,
            close_to_tray: true,
            start_as_admin: false,
        }
    }
}

pub struct TrayState {
    pub opts: Arc<Mutex<AppOpts>>,
}

impl TrayState {
    pub fn new(app: &AppHandle) -> Self {
        let opts = app
            .path()
            .app_config_dir()
            .ok()
            .and_then(|dir| std::fs::read_to_string(dir.join("settings.json")).ok())
            .and_then(|raw| serde_json::from_str::<AppOpts>(&raw).ok())
            .unwrap_or_default();
        Self {
            opts: Arc::new(Mutex::new(opts)),
        }
    }

    pub fn save(&self, app: &AppHandle) {
        if let Ok(dir) = app.path().app_config_dir() {
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(opts) = self.opts.lock() {
                if let Ok(raw) = serde_json::to_string_pretty(&*opts) {
                    let _ = std::fs::write(dir.join("settings.json"), raw);
                }
            }
        }
    }

    fn set<F: FnOnce(&mut AppOpts)>(&self, app: &AppHandle, f: F) -> AppOpts {
        {
            let mut opts = self.opts.lock().unwrap();
            f(&mut opts);
        }
        self.save(app);
        let _ = app.emit("tray-state-changed", self.opts.lock().unwrap().clone());
        self.opts.lock().unwrap().clone()
    }

    pub fn opts(&self) -> AppOpts {
        self.opts.lock().unwrap().clone()
    }
}

fn is_elevated_windows() -> bool {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("net")
            .arg("session")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

fn set_autostart(enabled: bool) -> bool {
    #[cfg(target_os = "windows")]
    {
        let key = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";
        let result = if enabled {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            std::process::Command::new("reg")
                .args([
                    "add",
                    key,
                    "/v",
                    "BetterCopy",
                    "/t",
                    "REG_SZ",
                    "/d",
                    &format!("\"{}\"", exe),
                    "/f",
                ])
                .status()
        } else {
            std::process::Command::new("reg")
                .args(["delete", key, "/v", "BetterCopy", "/f"])
                .status()
        };
        result.map(|s| s.success()).unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = enabled;
        false
    }
}

fn relaunch_plain_and_exit(app: &AppHandle) {
    #[cfg(target_os = "windows")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let exe_str = exe.display().to_string();
            let _ = std::process::Command::new(exe_str).spawn();
        }
    }
    app.exit(0);
}

fn relaunch_elevated_and_exit(app: &AppHandle) {
    #[cfg(target_os = "windows")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let exe_str = exe.display().to_string().replace('\'', "''");
            let script = format!(
                "Start-Process -FilePath '{}' -Verb RunAs",
                exe_str
            );
            let _ = std::process::Command::new("powershell")
                .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
                .spawn();
        }
    }
    app.exit(0);
}

pub fn ensure_admin_per_state(app: &AppHandle) {
    let opts = app.state::<TrayState>();
    if opts.opts().start_as_admin && !is_elevated_windows() {
        relaunch_elevated_and_exit(app);
    }
}

fn position_tray_menu(app: &AppHandle, win: &WebviewWindow) -> Result<(), String> {
    let monitor = app
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .ok_or("no primary monitor")?;
    let scale = monitor.scale_factor();
    let mpos = monitor.position();
    let msize = monitor.size();
    let wsize = win.outer_size().map_err(|e| e.to_string())?;
    let margin = (8.0 * scale) as i32;
    let x = mpos.x + msize.width as i32 - wsize.width as i32 - margin;
    let y = mpos.y + msize.height as i32 - wsize.height as i32 - margin;
    win.set_position(PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())
}

pub fn toggle_tray_menu(app: &AppHandle) -> Result<(), String> {
    let win = app.get_webview_window("tray-menu").ok_or("no tray-menu window")?;
    position_tray_menu(app, &win)?;
    if win.is_visible().map_err(|e| e.to_string())? {
        win.hide().map_err(|e| e.to_string())
    } else {
        win.show().map_err(|e| e.to_string())?;
        win.set_focus().map_err(|e| e.to_string())
    }
}

fn show_main(app: &AppHandle) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("no main window")?;
    win.show().map_err(|e| e.to_string())?;
    win.set_focus().map_err(|e| e.to_string())
}
#[tauri::command]
pub fn open_main_window(app: AppHandle) -> Result<(), String> {
    show_main(&app)
}

#[tauri::command]
pub fn open_settings_window(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("settings")
        .ok_or("no settings window")?;
    win.center().map_err(|e| e.to_string())?;
    win.show().map_err(|e| e.to_string())?;
    win.set_focus().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_tray_state(state: State<'_, TrayState>) -> AppOpts {
    state.opts()
}

#[tauri::command]
pub fn tray_action(
    app: AppHandle,
    state: State<'_, TrayState>,
    action: String,
    enabled: Option<bool>,
) -> Result<AppOpts, String> {
    match action.as_str() {
        "open" => {
            show_main(&app)?;
        }
        "settings" => {
            open_settings_window(app)?;
        }
        "startup" => {
            let on = enabled.unwrap_or(true);
            if set_autostart(on) {
                state.set(&app, |o| o.startup = on);
            }
        }
        "admin" => {
            let on = enabled.unwrap_or(true);
            if on {
                if !is_elevated_windows() {
                    state.set(&app, |o| o.start_as_admin = true);
                    relaunch_elevated_and_exit(&app);
                    return Ok(state.opts());
                }
            } else if is_elevated_windows() {
                // Dropping admin while elevated: relaunch plain to shed the token
                state.set(&app, |o| o.start_as_admin = false);
                relaunch_plain_and_exit(&app);
                return Ok(state.opts());
            }
            state.set(&app, |o| o.start_as_admin = on);
        }
        "close_to_tray" => {
            let on = enabled.unwrap_or(true);
            state.set(&app, |o| o.close_to_tray = on);
        }
        "close" => {
            let close_to_tray = { state.opts.lock().unwrap().close_to_tray };
            if close_to_tray {
                if let Some(win) = app.get_webview_window("main") {
                    win.hide().map_err(|e| e.to_string())?;
                }
            } else {
                app.exit(0);
            }
        }
        "quit" => {
            app.exit(0);
        }
        other => return Err(format!("unknown action: {}", other)),
    }
    Ok(state.opts())
}