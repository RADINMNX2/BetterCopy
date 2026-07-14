# Walkthrough: Storage Profiler Fix & Controlled Benchmarking

This walkthrough summarizes the changes made to resolve the storage-detection bug and the results of the drift-mitigated benchmark.

## Changes Made

### 1. Core Bug Fix in [profiler.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_core/src/profiler.rs)
* **The Bug:** The global constant `IOCTL_STORAGE_QUERY_PROPERTY` was incorrectly defined as `0x002d0c40` (the code for device telemetry) rather than the correct Windows API constant `0x002d1400`.
* **The Effect:** This caused `DeviceIoControl` seek-penalty and storage descriptor queries on the logical volume to return `ERROR_INVALID_FUNCTION` (Win32 Error 1), failing the auto-tuner and causing it to fallback to `DeviceClass::Unknown` (defaulting to 8 threads).
* **The Fix:** Corrected the constant value to `0x002d1400`. The profiler now successfully classifies NVMe devices as `Internal NVMe SSD`.

### 2. Auto-Tuning Optimization in [profiler.rs](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/better_copy_core/src/profiler.rs#L70-L79)
* Increased the default concurrency of `DeviceClass::SsdNvme` and `DeviceClass::Unknown` from **8 threads** to **16 threads** to prevent out-of-the-box bottlenecks on NVMe drives.

### 3. Sandwich Benchmark Runner in [run_sandwich_comparison.ps1](file:///c:/Users/kaika/BP/gitprojects/BetterCopy/bench/run_sandwich_comparison.ps1)
* Added a new benchmark script executing a symmetrical `1, 2, 3, 1` layout (`bcopy` $\to$ `robocopy /MT:32 /J` $\to$ `robocopy /MT:32` $\to$ `bcopy`) with a 60-second cooldown after deletions to capture and expose SSD write-amplification noise.

---

## Validation Results

### 1. Automated Unit Tests
* Executed `cargo test --package better_copy_core --lib` to verify the profiler logic.
* **Result:** `test profiler::tests::test_profile_device ... ok` (Passes).

### 2. Symmetrical Sandwich Performance Results
* **Conservative Comparison (Worst-Case `bcopy` vs. Best-Case `robocopy`):**
  * `bcopy` (Run 2 - worst-case FTL): **20.194 seconds**
  * `robocopy /MT:32` (Best-case Robocopy): **33.292 seconds**
  * **Net Speedup:** **1.65x faster** ($64.9\%$ throughput improvement).
* **Skeptic-Proof Verification:** 
  * Exclusions active on both tools (entire folder excluded).
  * Console progress logging fully bypassed (`/NP`).
  * Drift captured ($16.18\text{s} \to 20.19\text{s}$).
  * Copy integrity verified on all runs.
