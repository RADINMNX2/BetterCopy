# Implementation Plan: Explorer Hotkey Trigger & Tauri Progress GUI

Develop the Win32 global hotkey trigger with Explorer-focus gating, COM-based destination path resolution (Milestone 2), and build the Tauri-based progress dashboard with taskbar progress state integration (Milestone 3).

## User Review Required

> [!IMPORTANT]
> - **Tauri Initialization:** We will initialize a new crate `better_copy_gui` inside our Cargo workspace. This crate will contain the Tauri configuration, system tray menu, and WebView2 front-end files.
> - **COM Shell Dependencies:** The Explorer resolution logic requires Win32 COM APIs (`IShellWindows`, `IServiceProvider`, `IShellBrowser`, `IFolderView2`). We will enable the necessary `windows` crate features (`Win32_System_Com`, `Win32_System_Ole`) to call these interfaces safely.
> - **Hotkey Replay:** Replaying keys in the Rename-box guard requires calling `SendInput`. This is a low-level Win32 API that simulates keyboard events to pass the keystroke through to the edit box.

We will proceed with the implementation of the Win32 hotkey trigger and COM resolution modules once approved.

## Open Questions

> [!NOTE]
> None. The design guidelines in [seed.md](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/seed.md) clearly specify the interface names and hotkey gating conditions.

---

## Proposed Changes

### Cargo Workspace Configuration

Introduce the new GUI crate.

#### [MODIFY] [Cargo.toml](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/Cargo.toml)
- Add `better_copy_gui` to the workspace members.

---

### Win32 Gating & COM Resolution (Milestone 2)

Implement trigger logic in a dedicated module inside the core library.

#### [NEW] [better_copy_core/src/trigger.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_core/src/trigger.rs)
- **Message Window & Hotkey**: Implement message-only Win32 window using `CreateWindowExW` and `RegisterHotKey`.
- **WinEvent Hook Focus Gating**: Hook `EVENT_SYSTEM_FOREGROUND` via `SetWinEventHook` to register/unregister the hotkey dynamically when focus enters/leaves Explorer or the Desktop.
- **Rename Guard**: Call `GetGUIThreadInfo` to check class name of focused controls; bypass trigger if focus is inside an `Edit` box.
- **COM Explorer Path Resolver**: Instantiate `IShellWindows`, locate the foreground `CabinetWClass` window, query its `IServiceProvider` -> `IShellBrowser` -> `IFolderView2` -> `IPersistFolder2`, and extract the current directory path.

---

### Tauri GUI App Crate (Milestone 3)

#### [NEW] [better_copy_gui](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui)
- Tauri-based desktop app hosting the progress WebView2 window.
- **tauri.conf.json**: Configure app window settings (hidden on startup, taskbar visibility, custom size, green progress bar support).
- **Frontend Dashboard (HTML/CSS/JS)**: A premium dark-mode glassmorphic page rendering aggregate throughput speeds, time remaining (ETA), and per-job progress rows.
- **Main Loop**: Listen to trigger events, run preflight checks (space check, UNC path check, self-copy block), spawn copy engine threads, and pipe progress states to the frontend window via Tauri IPC.

---

## Verification Plan

### Automated Tests
- Run `cargo check` and `cargo test` on the workspace to verify compilation and COM binding resolution.

### Manual Verification
- **Hotkey Focus Gating**: Verify that `Ctrl+Shift+V` triggers `bcopy` only when an Explorer window or the desktop is focused, and does not interfere with plain-text paste in other apps (VS Code, Chrome, etc.).
- **Rename Box Check**: Enter file rename mode (F2) in Explorer and press `Ctrl+Shift+V`; verify that text is pasted inside the edit box rather than launching a copy transfer.
- **Progress Visuals**: Verify that the Tauri progress window matches the premium dark-mode UI designs, taskbar shows green progress fills, and closed/hidden behaviors align with success/error outcomes.
