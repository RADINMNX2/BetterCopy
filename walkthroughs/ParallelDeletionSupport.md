# Walkthrough: Parallel Deletion via Ctrl+Shift+Delete

Implement high-performance parallel deletes in BetterCopy, triggered by a focus-gated `Ctrl+Shift+Delete` shortcut inside Windows Explorer or the Desktop.

## Changes Made

### 1. File Traversal for Deletion
* Added `DeleteList` and `build_delete_list` in [walker.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_core/src/walker.rs).
* Traverses selected directories recursively, building a list of files and subdirectories.
* Reparse points (junctions/symlinks) are marked for deletion but never recursively traversed, preventing cyclic loops or accidental deletion of linked target contents.

### 2. Parallel Deletion Engine
* Implemented `run_delete_engine` in [engine.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_core/src/engine.rs).
* Dynamically profiles the source drive using `profile_device` to tune thread concurrency (16 threads for NVMe/SSD, 1 thread for HDD, 2 for BOT USB).
* Employs a multi-threaded worker pool to execute concurrent file deletions.
* Performs post-deletion cleanup of empty folders in reverse depth-first order (deepest folders first) to satisfy file system hierarchies.
* Suppresses system sleep during execution.

### 3. Dual Focus-Gated Hotkeys
* Refactored hotkey listener in [trigger.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_core/src/trigger.rs) to register `Ctrl+Shift+V` (`0x56`) and `Ctrl+Shift+Delete` (`0x2E`) independently.
* Query active selection in Explorer/Desktop using COM interfaces (`IFolderView2::GetSelection(true)`) to obtain an `IShellItemArray`, converting items to filesystem `PathBuf` paths.
* Defined `HotkeyEvent` enum representing `Paste` and `Delete` events.
* Updated `replay_hotkey` to replay either paste or delete if a rename or edit text box is currently focused.
* Updated manual trigger test binary in [trigger_test.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_core/src/trigger_test.rs).

### 4. Tauri Integration & UI/UX Polishing
* Refactored background channel in [lib.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/src-tauri/src/lib.rs) to process sequential copy, move, and delete jobs.
* Implemented real-time deletion rate (files/second) and ETA calculations in the background thread.
* Polished the frontend in [index.html](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/ui/index.html) and [main.js](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/ui/main.js):
  * **Dynamic Labels:** Automatically changes `"Preparing copy..."` to `"Preparing delete..."` and the stats label `"Speed"` to `"Delete Rate"`.
  * **Formatted Values:** Formats files/sec rates (e.g. `2,450 files/s`) and uses `.toLocaleString()` for file counts (e.g., `51,386 / 100,000 files`).
  * **Canvas wave graph:** Plots files/second over time, generating a dynamic active wave graph representing deletion speed instead of a flat 0.0 MB/s line.
  * **Clean transfer items:** Hides the destination arrow for deletes, representing them cleanly with a trash can icon (`🗑️ Delete: [Name]`).

---

## Validation Results

### 1. Compilation Verification
Verified compile success with clean build state (0 warnings, 0 errors):
```powershell
cargo check --workspace
```
Output:
```text
    Checking better_copy_core v0.1.0 (C:\Users\kaika\BP\gitprojects\BetterCopy\better_copy_core)
    Checking better_copy_gui v0.1.0 (C:\Users\kaika\BP\gitprojects\BetterCopy\better_copy_gui\src-tauri)
    Checking bcopy v0.1.0 (C:\Users\kaika\BP\gitprojects\BetterCopy\bcopy)
    Finished dev profile [unoptimized + debuginfo] target(s) in 1.51s
```

### 2. Manual Verification Guide
1. Launch the BetterCopy application.
2. Select any directory containing a high number of tiny files inside Windows Explorer or on the Desktop.
3. Press `Ctrl+Shift+Delete`.
4. Observe that the BetterCopy progress dialog pops up showing:
   - Description: `Deleting selected files...`
   - Progress bar filling up as files are concurrently deleted.
   - Dialog hiding automatically once completed.
5. Focus a browser window and press `Ctrl+Shift+Delete`. Confirm that the browser's native "Clear browsing data" dialog opens, proving that our focus gating successfully releases the hotkey hook.
