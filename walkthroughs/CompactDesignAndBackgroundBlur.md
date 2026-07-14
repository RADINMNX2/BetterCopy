# Compact Design and Background Blur Walkthrough

This walkthrough outlines the changes made to introduce native background blur (Acrylic) and a more compact layout for the BetterCopy progress dashboard.

## Changes Made

### 1. Dependency and Rust Integration
We modified [Cargo.toml](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/src-tauri/Cargo.toml) and [lib.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/src-tauri/src/lib.rs):
- Added `window-vibrancy = "0.5"` to the cargo dependencies.
- Added target-gated logic to first attempt `window_vibrancy::apply_mica(&window, None)` (the most performant, zero-lag backdrop sampling effect on Windows 11), falling back to `window_vibrancy::apply_blur(&window, Some((15, 17, 23, 120)))` on Windows 10 or older environments.

### 2. Tauri Configuration and Centering
We modified [tauri.conf.json](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/src-tauri/tauri.conf.json) and [lib.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/src-tauri/src/lib.rs):
- Set window dimensions to `320` width and `115` height to give the layout vertical breathing room.
- Configured `"center": true` in `tauri.conf.json` to center the window on initial launch.
- Added programmatic calls to `window.center()` in `lib.rs` right before showing the window (during copy queue start and single-instance relaunch events), ensuring the window centers dynamically each time it is shown.
- Maintained `"transparent": true` to allow background transparency and DWM blur to show through.

### 3. Drag Target & Graph Config
We modified [main.js](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/ui/main.js) and [index.html](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/ui/index.html):
- Replaced the green progress bar with the canvas speed graph (`.graph-container`), placing it directly under the stats grid.
- Hid the legacy progress bar (`.progress-container`) via inline styles, keeping the DOM structure for JavaScript compatibility.
- Shortened copy status labels in JS callbacks (e.g. using `Done` instead of `Successfully completed!` and `Cancelled` instead of `Cancelled by user.`).
- Programmatically hide the `Cancel` text button on transfer completion to save space.
- Configured the `<canvas>` speed graph width to `288` inside `index.html` (matching the horizontal content width inside the padded container).
- Transformed the speed graph into a **hybrid progress bar** inside `main.js`: speed data points are plotted at horizontal coordinates mapped directly to the copy progress percentage (`canvas.width * (percent / 100)`). The speed line and gradient fill grow dynamically from left to right as the transfer completes, leaving the undrawn future progress space empty.
- Set the graph refresh throttling rate to **250ms** in `main.js` to ensure the progress line extends smoothly and responsively across the canvas in real-time.

### 4. Spacing and Visuals (CSS)
We modified [style.css](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_gui/ui/style.css):
- Configured `.progress-meta` to `font-size: 9.5px` and applied `white-space: nowrap` to all text/span nodes inside the metadata row, preventing any word wrapping.
- Hidden the redundant header and footer containers (`.window-header { display: none; }` and `.window-footer { display: none; }`).
- Hidden the device profiler line (`.profile-block { display: none; }`).
- Hidden the thread dots visualizer (`.thread-visualizer { display: none; }`).
- Stretched the speed graph container to take full width (`.graph-container { width: 100%; }`).
- Set `cursor: move` on the main container background layout to visually indicate the draggable surface.
- Set `cursor: pointer` on active buttons and default cursor on stats values.
- Reduced container background opacity to `0.65` for transparency.
- Adjusted `.window-container` `border-radius` from `16px` to `8px` to match Windows 11 default borderless corner rounding, eliminating outer slivers.
- Hid the bulky list of files (`.jobs-list { display: none; }`).
- Collapsed the stats card grid into a single horizontal row of inline columns (`.stats-grid` / `.stat-card`).
- Increased `.window-content` horizontal padding to `16px`, top padding to `16px`, bottom padding to `12px`, and gap size to `10px` to give the layout room to breathe.
- Added a `.dashboard` flex flow layout with a `10px` gap to separate elements inside the widget.

---

## Validation and Verification

### Compilation Check
- Run `cargo check` inside the workspace root:
  ```bash
  cargo check
  ```
  Result: Completed successfully without errors.

### Process Management & Rebuilding Release
Because the GUI daemon runs in the background (`better_copy_gui.exe`), any styling/config changes require rebuilding the release binary and restarting the daemon:

1. **Kill the running daemon:**
   ```powershell
   Stop-Process -Name better_copy_gui -Force
   ```
2. **Rebuild the release package:**
   ```powershell
   cargo build --release -p better_copy_gui
   ```
3. **Start the new release daemon:**
   ```powershell
   Start-Process C:\Users\kaika\BP\gitprojects\BetterCopy\target\release\better_copy_gui.exe
   ```

### Manual Verification
1. Verify that the window launches with the new compact size (`500x320`) and displays a beautiful blurred Acrylic background when triggered via `Ctrl+Shift+V` in Windows Explorer or Desktop.
