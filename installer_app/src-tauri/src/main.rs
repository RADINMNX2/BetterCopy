#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--uninstall") {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let script = dir.join("uninstall.ps1");
                if script.exists() {
                    let _ = std::process::Command::new("powershell")
                        .args([
                            "-NoProfile",
                            "-ExecutionPolicy",
                            "Bypass",
                            "-File",
                            script.to_str().unwrap_or_default(),
                        ])
                        .status();
                }
            }
        }
        std::process::exit(0);
    }

    better_copy_installer_lib::run();
}