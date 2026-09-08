use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
    REG_OPEN_CREATE_OPTIONS, REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("BetterCopy");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub hotkeys_enabled: bool,
    pub start_on_boot: bool,
    #[serde(default = "default_true")]
    pub auto_close: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkeys_enabled: true,
            start_on_boot: false,
            auto_close: true,
        }
    }
}

impl Settings {
    pub fn config_dir() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("BetterCopy")
    }

    pub fn logs_dir() -> PathBuf {
        Self::config_dir().join("logs")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        match fs::read_to_string(Self::config_path()) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        if let Ok(raw) = serde_json::to_string_pretty(self) {
            let dir = Self::config_dir();
            if fs::create_dir_all(&dir).is_ok() {
                let _ = fs::write(Self::config_path(), raw);
            }
        }
    }

    /// Keeps the HKCU Run value in sync with the current setting so the app
    /// survives reboots when the user opts in.
    pub fn apply_autostart(&self, exe_path: Option<&str>) {
        unsafe {
            let mut hkey: HKEY = std::mem::zeroed();
            let rc = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                0,
                None,
                REG_OPEN_CREATE_OPTIONS(0),
                KEY_SET_VALUE,
                None,
                &mut hkey,
                None,
            );
            if rc != ERROR_SUCCESS {
                return;
            }
            let mut rc = ERROR_SUCCESS;
            if self.start_on_boot {
                if let Some(exe) = exe_path {
                    let value: Vec<u16> = format!("\"{}\"", exe)
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .collect();
                    let bytes =
                        unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2) };
                    rc = RegSetValueExW(hkey, RUN_VALUE, 0, REG_SZ, Some(bytes));
                }
            } else {
                rc = RegDeleteValueW(hkey, RUN_VALUE);
            }
            let _ = rc;
            let _ = RegCloseKey(hkey);
        }
    }

    /// True when the BetterCopy value is currently present in HKCU Run.
    pub fn autostart_is_active() -> bool {
        unsafe {
            let mut hkey: HKEY = std::mem::zeroed();
            let rc = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                0,
                KEY_QUERY_VALUE,
                &mut hkey,
            );
            if rc != ERROR_SUCCESS {
                return false;
            }
            let mut size: u32 = 0;
            let query = RegQueryValueExW(
                hkey,
                RUN_VALUE,
                None,
                None,
                None,
                Some(&mut size),
            );
            let active = query != ERROR_FILE_NOT_FOUND;
            let _ = RegCloseKey(hkey);
            active
        }
    }
}