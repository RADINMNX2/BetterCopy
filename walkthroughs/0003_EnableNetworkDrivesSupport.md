# Walkthrough: Enable Network/UNC Drives Support

This walkthrough summarizes the changes made to enable support for copying to and from network drives and UNC paths.

## Changes Made

### 1. Preflight Bypass in [lib.rs (Tauri GUI)](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/src-tauri/src/lib.rs#L143-L158)
* **The Constraint:** The Tauri frontend wrapper previously checked `dest_profile.is_remote` and `src_profile.is_remote` in the preflight phase, blocking any copy/move job that involved remote drives or UNC paths with a `"Remote destinations/sources are not supported."` error.
* **The Change:** Removed this preflight constraint. Remote drives and UNC paths are now allowed to pass to the core copy engine.

### 2. Auto-Tuning Concurrency for Remote Drives
* **Tuning Policy:** Network drives (detected via `GetDriveTypeW == DRIVE_REMOTE`) remain constrained to a safe **2 threads** to prevent network flooding and congestion, while local SSDs/NVMes continue to run at 16 threads.
* **Path Translation:** Path prefixes starting with `\\` are correctly translated to `\\?\UNC\` to bypass Win32 path length limits.

### 3. Documentation Updates in [README.md](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/README.md)
* Updated the FAQ section to explain that network/UNC drives are fully supported with auto-tuned concurrency (2-thread cap) to prevent network congestion, while noting that cloud-sync folders (e.g. OneDrive, Google Drive) are not officially supported.

---

## Validation Results

### 1. Compilation
* Ran `cargo check` on the workspace to verify there are no syntax or type errors.
* **Result:** Compilation succeeded.

### 2. Manual Verification
* Mapped drives and UNC paths now pass the preflight check and invoke `run_engine` using the 2-thread auto-tuned remote profile, keeping copying fast yet safe on local networks.
