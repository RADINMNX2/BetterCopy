# BetterCopy ⚡

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform: Windows](https://img.shields.io/badge/Platform-Windows-0078d7.svg)](https://microsoft.com)
[![Language: Rust](https://img.shields.io/badge/Language-Rust-ea4a35.svg)](https://www.rust-lang.org/)

**BetterCopy** is an auto-tuning, parallel copy engine for Windows. It provides a blistering-fast replacement for native file copy operations, bound directly to a global hotkey (**`Ctrl+Shift+V`**) that activates only when you focus Windows Explorer or the Desktop.

---

## ✦ The Philosophy

BetterCopy is built around a simple, uncompromising brand promise:
* **No Admin:** Runs entirely in user space. No elevation prompts, no UAC bypasses.
* **No Injection:** Zero DLL injection into `explorer.exe` or hooking of `SHFileOperation`.
* **No Residue:** No background services, tray clutter, or leftover registry junk.
* **Low Footprint:** CPU usage is near-zero when idle (~0.01% from the background daemon polling 100x/sec for focus/hotkeys). Memory usage sits at ~30–50MB of RAM (required to keep the Tauri/WebView2 dashboard engine pre-warmed for instant paste response).

---

## ✦ Product Positioning

BetterCopy occupies a unique sweet spot in the file-transfer ecosystem, bridging the gap between raw command-line performance and seamless native consumer usability:

| Dimension | Standard Windows Copy | CLI Tools (Robocopy / Rsync) | Shell Replacements (TeraCopy / FastCopy) | BetterCopy ⚡ |
| :--- | :--- | :--- | :--- | :--- |
| **Speed (Tiny Files)** | 🐌 Slow (Sequential I/O) | ⚡ Fast (Parallelized) | ⚡ Fast (Parallelized) | ⚡ **Fast (Parallelized)** |
| **Interface** | Native & Seamless | CLI Only (Shell/Scripts) | Heavy custom GUI / Overlay | **Transient Dashboard (Auto-dismisses)** |
| **Trigger** | `Ctrl+V` | Manual command invocation | Overrides standard copy handlers | **`Ctrl+Shift+V` (Focus-gated)** |
| **System Residue** | None (Built-in) | None | System services & registry hooks | **None (No admin required, zero residue)** |
| **Security Footprint** | None | None | DLL Injection / Shell Hooks (Trips AV) | **None (Safe native Win32 & COM APIs)** |
| **Configuration** | Zero Config | Complex flags (`/MT`, `/J`, `/E`) | Manual buffer & cache size tuning | **Zero Config (IOCTL-based Auto-Tuning)** |

---

## ✦ How It Works

Windows Explorer copies files sequentially, one-by-one. Third-party tools either require you to use their own clunky file manager or inject dangerous hooks into the Windows shell. BetterCopy offers a third way:

1. **Focus-Gated Trigger:** A lightweight message-only Win32 window registers a global hotkey (`Ctrl+Shift+V`). 
2. **Context Gating:** It listens to native Windows focus events (`SetWinEventHook`) to ensure the hotkey is only active when Windows Explorer (`CabinetWClass`) or the Desktop is in the foreground. If you are renaming a file or focused in another application, the keystroke passes through naturally.
3. **COM Destination Resolution:** When triggered, it queries the active Explorer window via COM APIs (`IShellWindows` ➔ `IFolderView2`) to find exactly where your cursor is focused, reads the source files from your clipboard (`CF_HDROP`), and starts copying.
4. **Auto-Tuning Engine:** It queries physical storage geometries using Win32 storage IOCTLs:
   * **NVMe SSDs:** Automatically ramps up thread concurrency (16 threads by default) to maximize queue depth.
   * **HDDs:** Safely falls back to single-threaded sequential copying to prevent thrashing.
   * **USB Enclosures:** Detects modern UASP vs. legacy BOT protocols to dynamically optimize I/O.
5. **Dual-Queue Architecture:** Separate worker pools process small (<1MB) and large (>=1MB) files concurrently, preventing massive sequential files from starving tiny files.

---

## ✦ Performance (Sandwich Benchmarks)

We benchmarked BetterCopy against Microsoft's industry-standard `robocopy` using a symmetrical sandwich test structure (exposing SSD write-amplification noise and clearing file-caching drift) with a **100,000 tiny-file fixture (800MB total)**.

| Copy Engine | Commands / Options | Average Time (s) | Speedup vs. Best Robocopy |
| :--- | :--- | :--- | :--- |
| **BetterCopy (`bcopy`)** | Default (Auto-Tuned) | **20.19s** | **1.65x faster** (Baseline) |
| **Robocopy** | `/MT:32` (Threads) | 33.29s | 1.00x |
| **Robocopy** | `/MT:32 /J` (Unbuffered) | 35.80s | 0.93x |
| **Windows Explorer** | Defender Disabled (Manual) | 240s - 300s (4m - 5m) | 0.11x - 0.14x (8x - 9x slower) |

*BetterCopy achieves a **64.9% throughput improvement** over the fastest possible Robocopy configuration, and is **12x to 15x faster** than native Windows Explorer (with Defender disabled) when copying tiny files on NVMe drives.*

---

## ✦ Architecture

```
                 [ Ctrl+Shift+V Global Hotkey ]
                              │
                    ( Gated Focus Check )
                   /                     \
        Active in Explorer?           Focused elsewhere?
               /                             \
    [ COM Path Resolution ]            [ Pass-through Key ]
              │
    [ Auto-Tune Storage Profile ]
    (Query IOCTL Seek Penalty/UASP)
         /                      \
    Spinning Disk?            Internal NVMe / SSD?
       /                          \
[ 1 Thread (Sequential) ]    [ Dual Queues (16 Threads) ]
                              ├── Small Files (<1MB)
                              └── Large Files (>=1MB) (Unbuffered I/O)
```

---

## ✦ Usage

### Global Hotkey (GUI)
1. Launch the BetterCopy daemon (`better_copy_gui.exe`).
2. Go to Windows Explorer or your Desktop.
3. Copy one or more folders (`Ctrl+C`).
4. Navigate to your target directory and press **`Ctrl+Shift+V`**.
5. A beautiful, transient Tauri progress dashboard will show the execution plan, speed, and real-time concurrency status.

### Command Line Interface (`bcopy`)
For scripting, automation, or CLI-first workflows:
```bash
# Copy source to destination using the auto-tuned parallel engine
bcopy.exe copy "C:\source\path" "D:\dest\path"

# Move source to destination safely (copy, verify, delete)
bcopy.exe move "C:\source\path" "D:\dest\path"
```

---

## ✦ Building from Source

### Prerequisites
* [Rust](https://www.rust-lang.org/) (Stable channel)
* [Node.js](https://nodejs.org/) & `npm` (for Tauri GUI)
* Windows SDK (for Win32 COM and storage APIs)

### Step-by-Step Build

1. Clone the repository:
   ```bash
   git clone https://github.com/articulite/BetterCopy.git
   cd BetterCopy
   ```

2. Build the CLI engine:
   ```bash
   cargo build --release --bin bcopy
   ```

3. Build and package the GUI app:
   ```bash
   cd better_copy_gui
   npm install
   npm run tauri build
   ```

---

## ✦ FAQ

#### Q: Why does the dashboard show a spinner/loading state before the copy starts?
**A:** Before transferring a single byte, BetterCopy performs a complete **pre-flight directory traversal (tree walk)** and **safety checks** (calculating total sizes, verifying write permissions, checking for copy-into-self loops, and ensuring there is enough free disk space). 

Standard Windows Explorer starts copying immediately and guesses the total size/time on the fly (which is why the progress bar fluctuates wildly or fails halfway through due to out-of-space errors). BetterCopy spends a brief moment upfront building an inventory so it can categorize files into small/large queues and allocate threads optimally. The actual transfer is so fast that the total end-to-end time is still significantly shorter!

#### Q: Why does `Ctrl+Shift+V` sometimes not do anything?
**A:** BetterCopy uses **focus gating** to protect your workflow. The global hotkey is only active when standard Windows Explorer (`CabinetWClass`) or the Desktop is focused. Additionally, if you are actively renaming a file (your cursor is in an Explorer rename text box), the hotkey is automatically suspended to allow you to type or paste naturally without triggering a file copy.

#### Q: Does it support network drives / UNC paths?
**A:** No, remote/UNC network paths are currently rejected during preflight. BetterCopy is optimized for local bus protocols (SATA, NVMe, USB) using low-level device IOCTL queries to tune concurrency. Network share copying behaves differently under latency constraints and is planned for a future release.

---

## ✦ License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

---

## ✦ Roadmap & TODO

### Core Copy Engine
- [x] **Focus-Gated Global Hotkey (`Ctrl+Shift+V`)** — Activates only when standard Windows Explorer or Desktop is in the foreground.
- [x] **Rename Guard** — Passes keystroke through when editing file names.
- [x] **Context Path Resolution** — Dynamically queries active Explorer via COM APIs and clipboard `CF_HDROP`.
- [x] **Storage Profiling & Auto-Tuning** — Detects HDD vs SSD (seek penalty) and UASP vs BOT USB via Win32 storage IOCTLs to auto-select optimal thread concurrency.
- [x] **Path & Loop Protection** — Bypasses `MAX_PATH` (using `\\?\` prefix) and skips reparse points (symlinks/junctions).
- [x] **Dual-Queue Architecture** — Separate worker pools for small (<1MB) and large (>=1MB) files to prevent starvation.
- [x] **Direct I/O & Low-Overhead Copy** — One-handle-rule implementation with selective pre-allocation and cached metadata writes.
- [x] **Unbuffered I/O** — Bypasses system file caching for large files.
- [x] **Two-Phase Moves** — Safe transaction-like moves (copy-verify-delete) and same-volume rename optimization.
- [x] **Preflight Safeguards** — Performs space checks, UNC rejection, write probing, and self-copy detection.
- [x] **Sleep Prevention** — Inhibits system standby during copy using Win32 execution state management.
- [x] **Job Cancellation** — Safe abort triggers with rollback of partial target files.
- [ ] **Verify-on-Write** — Integrity hash verification (e.g., xxHash/SHA-256) and checksum manifest generation.
- [ ] **In-Flight Pause & Resume** — Support pausing and resuming active copy processes.
- [ ] **Device-Gone Resiliency** — Automatically pause and poll/retry when external drives disconnect.
- [ ] **ReFS Block Cloning** — Support native fast cloning on ReFS drives.
- [ ] **UAC Elevation Prompts** — Offer privilege elevation on write permission failures.

### GUI & User Experience
- [x] **Tauri Integration** — Self-contained background application with system tray and single-instance locks.
- [x] **Transient Progress Dashboard** — Borderless, centered window displaying queue details, speed (MB/s), ETA, and taskbar progress states.
- [x] **Windows Aesthetics** — Modern interface styling utilizing Windows Mica/Acrylic transparency blur.
- [x] **Automatic Hide/Dismiss** — Auto-dismisses window upon completion of all jobs.
- [ ] **Copy/Move Pause Buttons** — Frontend buttons to pause/resume copy actions in the GUI.
- [ ] **Detailed Error log viewer** — Detailed report of failures within the UI.

### Packaging & Benchmarking
- [x] **Symmetrical Benchmarking Harness** — PowerShell fixture generator and test harness.
- [ ] **MSI Installer Package** — Professional installer creation.
- [ ] **Cross-Platform Ports** — Adapt core parallel copy functionality for macOS and Linux.
