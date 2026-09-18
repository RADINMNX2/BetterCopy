use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::Emitter;

mod arc;

#[derive(Clone, Serialize)]
struct ProgressPayload {
    percent: u32,
    message: String,
}

fn default_install_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        std::env::var("USERPROFILE")
            .unwrap_or_else(|_| "C:".to_string())
    });
    PathBuf::from(base).join("Programs").join("BetterCopy")
}

fn esc_ps(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}

/// Create a Start-Menu shortcut pointing at the installed app.
fn write_shortcut(install_dir: &Path, exe_name: &str) -> bool {
    let target = install_dir.join(exe_name);
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    if appdata.is_empty() {
        return false;
    }
    let link_dir = PathBuf::from(&appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs");
    let _ = std::fs::create_dir_all(&link_dir);
    let link = link_dir.join("BetterCopy.lnk");
    let ps = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.WorkingDirectory='{}';$s.WindowStyle=1;$s.Save();",
        link.display().to_string().replace('\'', "''"),
        target.display().to_string().replace('\'', "''"),
        install_dir.display().to_string().replace('\'', "''")
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Register an Apps & features entry + write uninstall.ps1 into the install dir.
fn register_uninstall(install_dir: &Path) -> bool {
    let key = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\BetterCopy";
    let ps_path = install_dir.join("uninstall.ps1");
    let link = PathBuf::from(
        std::env::var("APPDATA").unwrap_or_default(),
    )
    .join("Microsoft")
    .join("Windows")
    .join("Start Menu")
    .join("Programs")
    .join("BetterCopy.lnk");

    let ps_script = format!(
        "$ErrorActionPreference='SilentlyContinue'\n\
         $dir='{}'\n\
         Remove-Item -Recurse -Force -LiteralPath $dir\n\
         Remove-Item -Force -LiteralPath '{}'\n\
         Remove-Item 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\BetterCopy' -Recurse -Force\n\
         Remove-ItemProperty -Path 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' -Name 'BetterCopy' -Force\n",
        esc_ps(&install_dir.display().to_string()),
        link.display().to_string().replace('\'', "''")
    );
    if std::fs::write(&ps_path, ps_script).is_err() {
        return false;
    }

    let uninstall_cmd = format!(
        "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        ps_path.display()
    );
    let ps = format!(
        "$k='{}';\n",
        key,
    );
    // re-add with values (upgrade-safe overwrite)
    let ps = format!(
        "{ps}New-Item -Path 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall' -Name 'BetterCopy' -Force | Out-Null;\n\
         Set-ItemProperty -Path $k -Name 'DisplayName' -Value 'BetterCopy';\n\
         Set-ItemProperty -Path $k -Name 'DisplayVersion' -Value '0.1.0';\n\
         Set-ItemProperty -Path $k -Name 'Publisher' -Value 'RADIN MNX';\n\
         Set-ItemProperty -Path $k -Name 'InstallLocation' -Value '{}';\n\
         Set-ItemProperty -Path $k -Name 'UninstallString' -Value '{}';\n\
         Set-ItemProperty -Path $k -Name 'NoModify' -Value 1;\n\
         Set-ItemProperty -Path $k -Name 'NoRepair' -Value 1;\n",
        install_dir.display().to_string().replace('\'', "''"),
        uninstall_cmd.replace('\'', "''")
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tauri::command]
fn get_target_dir() -> String {
    default_install_dir().display().to_string()
}

#[tauri::command]
fn install_payload(
    app: tauri::AppHandle,
    target_dir: String,
) -> Result<(), String> {
    let handle = app.clone();
    std::thread::spawn(move || {
        let emit = |pct: u32, msg: &str| {
            let _ = handle.emit(
                "install-progress",
                ProgressPayload {
                    percent: pct,
                    message: msg.to_string(),
                },
            );
        };
        emit(0, "Preparing installer payload...");

        let arc = match arc::Arc::from_current_exe() {
            Ok(a) => a,
            Err(e) => {
                let _ = handle.emit(
                    "install-progress",
                    ProgressPayload {
                        percent: 0,
                        message: e,
                    },
                );
                return;
            }
        };

        let dir = PathBuf::from(&target_dir);
        if arc.extract_to(&dir, |p, m| emit(p, m)).is_err() {
            let _ = handle.emit(
                "install-progress",
                ProgressPayload {
                    percent: 0,
                    message: "Failed to extract payload".into(),
                },
            );
            return;
        }

        emit(88, "Creating Start Menu shortcut...");
        let _ = write_shortcut(&dir, "better_copy_gui.exe");

        emit(95, "Registering with Windows...");
        let _ = register_uninstall(&dir);

        emit(100, "Installation complete");
    });
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_target_dir,
            install_payload
        ])
        .run(tauri::generate_context!())
        .expect("error while running better copy installer");
}