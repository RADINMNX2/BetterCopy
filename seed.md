# Design: BetterCopy — a parallel copy engine bound to Ctrl+Shift+V

Generated on 2026-07-13
Status: DRAFT
Mode: Builder

## Problem Statement

Windows 11's built-in copy processes files one at a time, which makes it
slow on exactly the thing people copy most: directories full of small
files. Every third-party alternative is either abandonware, ad-supported,
license-encumbered for commercial use, or achieves its Explorer integration
by injecting a DLL into `explorer.exe` and hooking `SHFileOperation` — which
is precisely why they trip antivirus and feel untrustworthy.

Build a copy engine you can read end-to-end, triggered by a hotkey Windows
doesn't already use.

## What Makes This Cool

**Auto-tuning is the "whoa," not raw speed.** The tool is designed for the
hardware people actually have — NVMe, SSDs, USB-C enclosures — and it
interrogates source and destination (device class, UASP vs BOT, removal
policy) to pick the concurrency itself, then tells you what it decided and
why. You paste; it does the right thing. Spinning disks get a safe
sequential fallback, not cleverness.

**And the trigger is boring on purpose.** `RegisterHotKey` is a documented,
first-party Win32 API. No `WH_KEYBOARD_LL` hook, no DLL injection, no shell
extension, no driver, no admin rights. Ctrl+V keeps doing the safe native
thing. Ctrl+Shift+V does the fast thing. The "toggle" is your fingers.

## Premises

1. The value is auto-tuned concurrency, not a faster memcpy.
2. This is a copy engine with a paste trigger — not a file manager. Explorer
   stays.
3. Avoiding Explorer interception entirely is a feature, not a compromise.
   It's the reason the engine stays small enough to read in one sitting and
   the whole tool stays trustworthy.

## Approaches Considered

### Approach A: Tray app + low-level keyboard hook
Swallow Ctrl+V when an Explorer window is foreground. Gives the true hijack
with an on/off toggle. Effort M, Risk Low — but a `WH_KEYBOARD_LL` hook is
structurally a keylogger and some AV heuristics treat it as one.

### Approach B: Hook + shell extension + MSIX + code signing
Covers every paste path (ribbon button, right-click, drag-drop). Effort L,
Risk Med. Shell extension DLLs get locked by `explorer.exe`, making the
edit-build-test loop miserable. Signing cert costs real money.

### Approach C: Bind Ctrl+Shift+V — **SELECTED**
Same engine, no interception of any kind. Effort S, Risk Very Low.

## Recommended Approach

**Approach C.** The engine is the interesting part and it's fully separable
from the trigger. If the engine doesn't beat Explorer on your actual
hardware, the trigger doesn't matter. C gets you to that answer in a
weekend. A remains available later as a ~150-line addition if you decide you
want it — the engine doesn't change.

## MVP Scope

**In:** local SSD/NVMe and UASP USB copies · focus-gated Ctrl+Shift+V with
rename-box guard · destination grace period · preflight (free space,
copy-into-self, writability) · small/large queue split with
worker pool · two-phase moves + same-volume rename · reparse points never
traversed · errors never block the queue · cancel · minimal progress window
with the concurrency-decision line · tray icon · HDD guard rail (1 thread).

**Out (deferred, context preserved below):** verify pass · pause (cancel only) ·
ReFS block cloning · relative-handle creates and directory sharding (add
only if MVP benchmarks fall short on tiny files) · USB micro-probe (MVP
treats unknown devices conservatively at 2–4 threads) · elevation offer
(MVP just reports access-denied in preflight).

The MVP bar: on your NVMe → UGREEN enclosure with a tiny-file-heavy mix, it
visibly beats Explorer, and Ctrl+Shift+V never misfires in another app.

---

## Architecture

```
┌─────────────────────────────────────────┐
│ Tray app (message-only window)          │
│  RegisterHotKey(MOD_CONTROL|MOD_SHIFT,  │
│                 'V') → WM_HOTKEY         │
└──────────────┬──────────────────────────┘
               │
       ┌───────▼────────┐      ┌──────────────────┐
       │ Source:        │      │ Destination:     │
       │ clipboard      │      │ foreground       │
       │ CF_HDROP       │      │ Explorer window  │
       │ + PREFERRED    │      │ → IShellWindows  │
       │   DROPEFFECT   │      │ → IFolderView2   │
       │   (copy/cut)   │      │ → current path   │
       └───────┬────────┘      └────────┬─────────┘
               └────────────┬───────────┘
                            ▼
                 ┌──────────────────────┐
                 │ Device profiler      │
                 │ → concurrency policy │
                 └──────────┬───────────┘
                            ▼
                 ┌──────────────────────┐
                 │ Copy engine          │
                 │ + progress window    │
                 └──────────────────────┘
```

### 1. Trigger
`RegisterHotKey(hwnd, 1, MOD_CONTROL | MOD_SHIFT, 0x56)` on a message-only
window. Handle `WM_HOTKEY`.

**But not globally.** `RegisterHotKey` steals the keystroke system-wide, and
Ctrl+Shift+V is paste-as-plain-text in Chrome, Word, Slack, Discord, and
VS Code. A permanently registered hotkey breaks every one of them —
violating the core promise that nothing else on the machine changes.

Fix, without introducing a keyboard hook:
`SetWinEventHook(EVENT_SYSTEM_FOREGROUND, ...)` — a lightweight, documented
foreground-change notification — and register the hotkey **only while an
Explorer window (`CabinetWClass`) or the desktop is foreground**, unregister
the instant focus moves anywhere else. The hotkey now cannot interfere with
other applications by construction, and the interception story is still
zero hooks, zero injection.

**Rename-box guard:** renaming a file in Explorer places focus in an `Edit`
control while the top-level window is still `CabinetWClass`. Check the
focused control class via `GetGUIThreadInfo` before acting; if it's an edit
control, pass the keystroke through (unregister + `SendInput` replay, or
simply no-op) so the user's paste lands in the rename box.

Single-instance mutex. A second hotkey press during a transfer appends a new
job to the queue — each job carries its own source list and destination.

### 2. Resolve source
Open the clipboard, read `CF_HDROP`, enumerate with `DragQueryFileW`. Also
read the `CFSTR_PREFERREDDROPEFFECT` format — `DROPEFFECT_MOVE` (2) means
the user pressed Ctrl+X, `DROPEFFECT_COPY` (1) means Ctrl+C. Honor it.

If the clipboard has no `CF_HDROP` when the hotkey fires, show a tray
balloon and stop. A silent no-op on a stale or empty clipboard is the
confusing case.

### 3. Resolve destination
Enumerate shell windows via `IShellWindows` (SHDocVw), match against
`GetForegroundWindow()`, then `IServiceProvider` → `IShellBrowser` →
`IFolderView2` → `GetFolder(IID_IPersistFolder2)` → `GetCurFolder()` → path.

Fallbacks: if the foreground window is the desktop (`Progman`/`WorkerW`), use
the Desktop folder. If it's not Explorer at all, show a small "paste to
where?" prompt rather than doing nothing silently.

**Wrong-destination guard:** foreground focus can change between Ctrl+C and
the hotkey — alt-tab away, come back, fat-finger Ctrl+Shift+V while a
*different* Explorer window is focused, and 40 GB lands in the wrong folder.
Resolving from the foreground window is the intended behavior; the mitigation
is to show the resolved destination in the progress window with a 1–2 second
grace period before I/O starts. Costs nothing on correct pastes, saves the
disaster case.

### 4. Device profiler (the interesting part)
For both source and destination:
- `CreateFileW("\\\\.\\X:")` → `IOCTL_STORAGE_GET_DEVICE_NUMBER` → physical
  device number. Same number = same physical disk.
- `IOCTL_STORAGE_QUERY_PROPERTY` with `StorageDeviceSeekPenaltyProperty` →
  `IncursSeekPenalty == FALSE` means SSD/NVMe.
- Check for UNC path / remote drive (`GetDriveTypeW` → `DRIVE_REMOTE`) —
  used only to reject out-of-scope paths in preflight.

**Design target (MVP): local SSD/NVMe and UASP USB enclosures.** HDDs are not
an optimization target — no pipelining rows, no buffer tuning for spinning
media. One guard rail survives (~10 lines): the seek-penalty check stays,
and a detected HDD gets **1 thread, plain sequential copy** — Explorer-
equivalent, never worse. External HDDs remain the cheapest bulk-backup
medium; someone will paste onto one eventually, and the tool must not
seek-thrash their backups. It just doesn't try to be clever there.

Policy — concurrency is set by the slower device, i.e.
`min(source policy, dest policy)`:

| Device class | Per-device policy | Notes |
|---|---|---|
| SSD / NVMe (incl. UASP USB) | 4–8 | Scale with queue depth. |
| Unknown (bridge doesn't report) | 2–4 | Conservative; micro-probe deferred. |
| BOT USB / thumb drives | 1–2 | No command queuing; workers just queue at the protocol layer. |
| HDD (seek penalty detected) | **1** | Guard rail only. Sequential, done. |

Same physical device: same-volume moves are renames (see engine); same-
volume copies on SSD share read/write bandwidth — expect compressed gains,
not losses.

Show the decision in the progress window: *"NVMe → UASP USB SSD · 6 threads."*
This is the feature. Don't hide it behind a settings page.

**USB / removable media — a distinct device class:**

- **UASP vs BOT is what matters, not the disk inside.** Modern NVMe USB-C
  enclosures use UASP (command queuing → parallelism works; the 10–20 Gbps
  link saturates first) — full SSD treatment. Older/cheap enclosures and
  most flash thumb drives are BOT: one command in flight, parallel workers
  just queue at the protocol layer — conservative row.
- **Bridges lie.** Many USB-SATA/NVMe bridges don't pass through
  `StorageDeviceSeekPenaltyProperty`, so IOCTL-based detection returns
  unknown. Fallback: a ~2-second micro-probe (random 4K reads) on first
  contact, result cached per volume serial. For USB, measuring the device
  beats trusting its self-description; the IOCTL matrix remains the fast
  path for internal drives.
- **"Quick removal" policy.** Windows defaults removable drives to
  synchronous writes (no write cache), which throttles tiny-file metadata
  throughput regardless of enclosure quality. It's a Windows policy, not a
  hardware limit. Detect (`IOCTL_STORAGE_GET_HOTPLUG_INFO` / device
  properties) and *report* it in the UI — never change it for the user.
- **Yank-safety is a design requirement here, not an edge case.** On a USB
  disconnect mid-transfer, distinguish device-gone errors from per-file
  errors: pause the whole queue and poll for reconnect rather than burning
  through thousands of per-file failures. The two-phase move guarantee
  matters most exactly here — a move to a USB drive yanked at 90% must
  leave all sources intact.

**Small-file ceiling regardless of matrix:** for thousands of tiny files the
bottleneck is NTFS metadata operations (create, close, timestamps, MFT
updates), which serialize on the destination volume no matter how many
threads you throw at it. Cap the small-file pool at ~4 locally; more buys
nothing and any time spent "tuning" past that is spent against a wall.

### 5. Preflight (before any I/O)
- **Scope check:** source or destination is `DRIVE_REMOTE` or a UNC path →
  "This destination isn't supported — use Ctrl+V," stop. BetterCopy is a
  local-media tool; this check is what enforces that. Applies to sources
  too, not just destinations.
- **Free space:** sum the work list vs `GetDiskFreeSpaceExW`. Fail fast,
  not at 94%.
- **Copy-into-self:** canonicalize both paths and reject if the destination
  is inside a source (`C:\A` → `C:\A\B` recurses until the disk fills).
- **Writability probe:** create-and-delete a temp file in the destination.
  `ERROR_ACCESS_DENIED` → offer elevation *now*, not at minute 20.

### 6. Copy engine
- Walk the tree first, build a work list, split into **small** (<1 MB) and
  **large** (≥1 MB) queues. Small files are syscall-bound; large files are
  bandwidth-bound. They want different handling.
- **Never traverse reparse points.** Directory junctions and symlinks can
  point at ancestors — every Windows install ships junction loops
  (`AppData\Local\Application Data`), so a naive recursive walk never
  terminates. Copy reparse points as reparse points where privilege allows,
  otherwise skip with a log entry. Do not follow them, period.
- Large files: `CopyFileExW` with `COPY_FILE_NO_BUFFERING` above ~256 MB so
  you don't evict the entire page cache. Use the progress callback.
  **Unbuffered I/O interacts with thread count:** 8 concurrent unbuffered
  copies means 8 × 8–16 MB aligned buffers and no read-ahead; fewer workers
  with bigger buffers usually wins. Cap the large-file queue at 2–3 workers
  even on NVMe and let the small-file pool carry the concurrency.
- Small files: bounded worker pool, plain `CopyFileExW`.
- **Moves are two-phase.** For `DROPEFFECT_MOVE`, never delete-as-you-go:
  copy the whole batch, confirm it completed, *then* delete sources. A
  cancelled or crashed move must never leave a file existing in neither
  place. Exception: same-volume move is a `MoveFileExW` rename — instant,
  no data touched. The profiler already knows the volumes match; use it.
- **Cloud placeholders** (`FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`, e.g.
  OneDrive): naive reads trigger hydration — a 200 KB placeholder becomes a
  4 GB download mid-transfer, wrecking the ETA and possibly the disk.
  Policy: copy the placeholder as-is via `CopyFileExW` default behavior and
  note it in the log; never force hydration silently.
- Long paths: prefix everything with `\\?\` and stop worrying about
  MAX_PATH.
- Collisions: skip / overwrite / rename / newer-wins, with "apply to all."
- **Errors never block the queue.** Log, continue, retry the failures at the
  end, show a summary. This alone beats Explorer, which freezes the whole
  transfer on one locked file.
- Optional verify pass: BLAKE3 both sides. Off by default.

**Fidelity semantics — decided now, not discovered later.** `CopyFileExW`
preserves attributes and alternate data streams, preserves modified time,
sets creation time to now, and does not copy ACLs (destination inherits).
That exactly matches Explorer: document as parity, move on. EFS-encrypted
file → non-NTFS destination fails rather than silently decrypting — surface
in the error log. Sparse files lose sparseness — acceptable, log it.

**Cancellation & pause.** Cancel mid-file deletes the partial destination
file (the `CopyFileExW` default — don't set the flag that keeps partials).
Cancel during a move is inherently safe: two-phase means no source has been
deleted yet. Pause between files, not mid-file — `COPY_FILE_RESTARTABLE` is
notoriously slow. Hold `SetThreadExecutionState(ES_SYSTEM_REQUIRED)` while
running so the machine doesn't sleep at 60% of an overnight transfer.

**Device-gone resilience.** Distinguish per-file errors from device-gone
errors (`ERROR_DEVICE_NOT_CONNECTED`, `ERROR_NOT_READY`,
`ERROR_FILE_NOT_FOUND` storms after a disconnect). Per-file → retry list.
Device-gone → pause the entire queue and poll for reconnect, instead of
burning through 10,000 files' worth of failures.

**After a successful move:** clear the clipboard. Windows convention treats
a cut as single-use; otherwise a second Ctrl+Shift+V "moves" sources that no
longer exist and produces a wall of errors.

### 7. UI — Tauri window lifecycle

One progress window (custom HTML/CSS): aggregate MB/s, files/sec, ETA, the
concurrency-decision line, per-job rows, cancel. Tray icon for
"running / idle" and Quit.

**Pre-created, never destroyed.** The window is created hidden at app
startup and toggled with `show()`/`hide()`. A cold WebView2 spin-up per
paste would add 200–500 ms of visible lag; keeping it warm makes appearance
Explorer-instant.

**Auto-open:** after preflight passes, `show()` **without stealing focus**
— the user is usually still working in Explorer, and a focus-stealing
progress dialog is infuriating.

**Auto-close:**
- Success, zero errors → hide after ~500 ms (so fast copies don't flicker).
- Any errors → stay open with the error summary and retry results. Never
  auto-close over an error list; that's how files get silently lost.
- User interacting (hover, expanded details) → defer hiding until
  mouse-leave or explicit close.

**Taskbar presence, Explorer-style:** while a transfer runs, show a taskbar
button with Tauri's `set_progress_bar` green progress fill — half of what
makes a transfer feel native. The window itself stays in front only if
summoned or if an error needs attention.

**Multiple jobs consolidate** into the one window as stacked rows (falls
out of the queue design) — never one window per paste.

Progress events flow engine → UI over Tauri IPC, throttled to ~30
updates/sec.

### Language & stack
**Tauri: Rust engine + HTML/CSS UI over WebView2** (preinstalled on
Windows 11, so the binary stays ~5–10 MB). The copy engine, profiler,
hotkey, and shell resolution live in the Rust process; the UI is a
custom-designed page — live throughput graph, the concurrency-decision line
rendered properly — which is more impressive than a stock Fluent dialog and
the cheapest path to a high-quality look. Forcibly decouples the headless
Milestone 1 engine from the GUI, which the milestones wanted anyway.
Known costs: two-language project, COM shell chain in Rust is verbose
(budget extra time for Milestone 2), Fluent look must be rebuilt in CSS if
native appearance is desired.

## Beyond the Policy Matrix — collected techniques

Unordered context, not milestones — all deferred behind MVP benchmark
evidence. Relative-handle creates and ReFS cloning are the breakthroughs;
the rest are compounding percents.

### Attacking the metadata ceiling

**Relative-handle creates.** Every `CreateFileW` with a full path forces
complete path resolution — every component parsed and ACL-checked, per file.
`NtCreateFile` accepts a `RootDirectory` handle: open each destination
directory once, create children relative to it, keep an LRU cache of open
directory handles. Deletes ~80% of path-resolution work on a 100k-file tree.
Robocopy doesn't do this; Explorer doesn't. The single biggest legit
user-mode metadata win.

**Shard workers by directory, not by file.** NTFS serializes concurrent
creates within one directory (directory index lock, MFT contention) — four
threads hammering one folder mostly queue on each other; four threads each
owning a different subdirectory scale nearly linearly. The ~4-thread local
ceiling isn't hardware, it's a contention pattern, and it's schedulable
around: make the small-file scheduler work-steal *directories*.

**One handle, all operations.** Create with write+attribute access, write
data, set timestamps via `FILE_BASIC_INFO` on the same handle, close.
Naive implementations open each file up to three times (create, write,
`SetFileTime`) — 3 Defender-visible handle events and 3× object-manager
overhead per tiny file. One write-handle close per file is the floor.

**Pipeline the walk.** Rather than strict walk-then-copy, start copying at
~1000 discovered files while enumeration continues (big-buffer
`NtQueryDirectoryFile`). On huge trees the walk is minutes of metadata reads
otherwise serialized in front of the transfer. (Preflight totals become
running estimates until the walk completes — acceptable trade.)

### Not copying at all

**Block cloning on ReFS.** `FSCTL_DUPLICATE_EXTENTS_TO_FILE` makes
same-volume copies copy-on-write: a 60 GB directory "copies" in under a
second because no data moves. First-party, documented, safe. Explorer uses
it only recently and inconsistently. Profiler addition: same ReFS volume →
clone, report "0 bytes copied." Same-volume only, and ReFS is uncommon on
consumer machines (Dev Drive is its main consumer surface, and Microsoft
scopes Dev Drives to trusted developer files — not general storage, not
removable drives). Keep it as a free win when detected, not a headline.

### Defender — the constrained wall

There is no legitimate way to make Defender scan less per new file, and
this tool must not try: auto-adding exclusions or evading scan triggers is
malware behavior and the reputational opposite of the project's premise.
What is legit:

- **Dev Drive performance mode** — first-party async Defender scanning that
  doesn't block file close. Niche (see block-cloning note on Dev Drive
  scope), but detect it and benefit where present.
- **Minimize scan-triggering handle events** — the one-handle rule above;
  hit the floor of one close per file.
- **Honest attribution** — when small-file throughput is scan-bound, the
  progress window says "throughput limited by antivirus scanning." No
  advice, no automation, no exclusion prompts. Users configure their own AV;
  the tool just declines to be blamed for a wall it didn't build.

### Considered and rejected

`SetFileValidData` (admin-only, can expose stale disk contents — a security
hole for single-digit gains) · TxF (deprecated) · ODX offload (SAN hardware
only) · IORING (no create/write coverage yet).

## Open Questions

- Verify pass: worth the read-back cost, or is that just cargo-culted from
  TeraCopy?
- What happens when the user hits Ctrl+Shift+V twice — queue the second
  batch, or run a second transfer concurrently? (Queue. Two concurrent
  transfers to the same disk defeats the entire auto-tuning premise.)

## Benchmark (self-contained, in-repo)

One script, one fixture, hard 1 GB cap, everything generated and gitignored:

```
bench/
  run.ps1          # the whole benchmark — committed
  fixtures/        # generated — gitignored
  results/         # timing output — gitignored
```

`run.ps1 -Dest E:\` does three things:

1. **Generate the fixture** into `bench/fixtures/tiny/` if absent
   (idempotent): 100k files × 8 KB across 500 dirs ≈ **800 MB**. Total
   disk use — fixture plus one destination copy at a time — stays under
   1 GB per drive by construction; the script asserts this against free
   space before writing anything.
2. **Run it** through `bcopy` and through `robocopy /MT:8` (the stand-in
   baseline; Explorer can't be scripted), 3 runs each, deleting the
   destination copy between runs.
3. **Print a table** (median wall time and files/sec per tool) and write
   it to `bench/results/<timestamp>.txt`.

`.gitignore`: `bench/fixtures/` and `bench/results/`.

That's the whole harness. Correctness edge cases (junction cycle, locked
file, >260-char paths, OneDrive placeholder — Win11 syncs Desktop/Documents
by default, so tiny-file sources will contain them — and mid-transfer USB
yank) are **manual test procedures**, not part of the benchmark script —
they test behavior, not speed.

## Success Criteria

1. On **your** drives with **your** typical file mix, it measurably beats
   Explorer — and on any device it doesn't optimize for (HDD, BOT USB), it
   is not *slower*.
2. A locked or permission-denied file doesn't stall the batch.
3. Ctrl+V still behaves exactly as it always did.
4. You can read the whole source in one sitting.

## Next Steps

**Milestone 1 — Engine, headless.** CLI: `bcopy <src...> <dst>`. Tree walk
(no reparse traversal), work list, seek-penalty + UASP detection, policy
table, small/large worker pools with the one-handle rule, two-phase move +
same-volume rename, error log with end-of-run retry pass. Build
`bench/run.ps1` alongside it and benchmark on the NVMe → UGREEN pair.

**Milestone 2 — Trigger.** Message-only window, `SetWinEventHook`
foreground gating, `RegisterHotKey` register/unregister, rename-box guard,
clipboard `CF_HDROP` + drop effect, Explorer destination resolution,
grace-period confirmation. Usable at this point.

**Milestone 3 — Progress UI + preflight.** Progress window (throughput,
ETA, the concurrency-decision line, cancel), tray icon, single-instance,
preflight checks, clipboard-clear after move.

**Ship the MVP there.** Everything in the deferred list waits for benchmark
evidence or real usage demand.
