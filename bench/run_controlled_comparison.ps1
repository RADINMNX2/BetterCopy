param (
    [string]$Dest = "bench/dest",
    [int]$CooldownSeconds = 60
)

$ErrorActionPreference = "Stop"

$BcopyPath = "target\release\bcopy.exe"
if (-not (Test-Path $BcopyPath)) {
    Write-Error "Could not find release binary at $BcopyPath. Please build it first."
}

# 1. Environment & Defender Check
Write-Host "=== Environment Information ===" -ForegroundColor Cyan
$DefenderStatus = Get-MpComputerStatus | Select-Object -ExpandProperty RealTimeProtectionEnabled
Write-Host "Windows Defender Real-Time Protection: $(if ($DefenderStatus) { 'Enabled' } else { 'Disabled' })" -ForegroundColor Yellow

$DestFullPath = [System.IO.Path]::GetFullPath($Dest)
$DestDrive = [System.IO.Path]::GetPathRoot($DestFullPath)
$DriveInfo = [System.IO.DriveInfo]::new($DestDrive)
$FreeSpaceBytes = $DriveInfo.AvailableFreeSpace
Write-Host "Destination path: $DestFullPath (Drive $DestDrive, Free Space: $([Math]::Round($FreeSpaceBytes / 1GB, 2)) GB)" -ForegroundColor Green

# 2. Fixture Checking
$TinyDir = "bench/fixtures/tiny"
if (-not (Test-Path $TinyDir)) {
    Write-Host "Fixture not found. Generating 100,000 tiny-file fixture (800MB)..." -ForegroundColor Cyan
    Start-Process -FilePath $BcopyPath -ArgumentList "--gen-fixture", $TinyDir -Wait -NoNewWindow
} else {
    Write-Host "Tiny Fixture already exists at $TinyDir." -ForegroundColor Green
}

# Ensure destination parent folder exists
New-Item -ItemType Directory -Force -Path $DestFullPath | Out-Null

# Helper to safely delete folders even if files are temporarily locked
function Safe-Clean([string]$Path) {
    if (Test-Path $Path) {
        for ($i = 0; $i -lt 10; $i++) {
            try {
                Remove-Item -Recurse -Force $Path -ErrorAction Stop
                return
            } catch {
                Start-Sleep -Seconds 2
            }
        }
        Remove-Item -Recurse -Force $Path -ErrorAction Ignore
    }
}

# Helper to verify copy integrity
function Verify-Copy([string]$Src, [string]$Dest) {
    try {
        if (-not (Test-Path $Dest)) { return $false }
        $SrcFiles = [System.IO.Directory]::GetFiles($Src, "*", [System.IO.SearchOption]::AllDirectories)
        $DestFiles = [System.IO.Directory]::GetFiles($Dest, "*", [System.IO.SearchOption]::AllDirectories)
        
        if ($SrcFiles.Length -ne $DestFiles.Length) {
            return $false
        }
        
        $SrcSize = [long]0
        foreach ($f in $SrcFiles) {
            $SrcSize += [System.IO.FileInfo]::new($f).Length
        }
        $DestSize = [long]0
        foreach ($f in $DestFiles) {
            $DestSize += [System.IO.FileInfo]::new($f).Length
        }
        
        return $SrcSize -eq $DestSize
    } catch {
        return $false
    }
}

# Helper to run a single timed run with cooldown
function Run-With-Cooldown([string]$Name, [scriptblock]$Cmd, [string]$DestPath) {
    Write-Host "`n[Cleanup] Cleaning destination: $DestPath" -ForegroundColor Gray
    Safe-Clean $DestPath
    
    Write-Host "[Cooldown] Sleeping $CooldownSeconds seconds to let SSD FTL and NTFS MFT settle..." -ForegroundColor Yellow
    Start-Sleep -Seconds $CooldownSeconds
    
    Write-Host "Running $Name..." -ForegroundColor Cyan
    $Elapsed = Measure-Command {
        & $Cmd
    }
    
    $Valid = Verify-Copy $TinyDir $DestPath
    Write-Host "  Completed in: $([Math]::Round($Elapsed.TotalSeconds, 3)) seconds (Integrity Verified: $Valid)" -ForegroundColor Gray
    
    # Return result
    return [PSCustomObject]@{
        Tool = $Name
        Time = [Math]::Round($Elapsed.TotalSeconds, 3)
        Fps = [Math]::Round(100000 / $Elapsed.TotalSeconds, 1)
        Verified = $Valid
    }
}

# Definitions of tools
$BcopyDest = Join-Path $DestFullPath "run_bcopy_auto"
$BcopyCmd = { Start-Process -FilePath $BcopyPath -ArgumentList $TinyDir, $BcopyDest -Wait -NoNewWindow }

$RoboJDest = Join-Path $DestFullPath "run_robo_32_j"
$RoboJCmd = { Start-Process -FilePath "robocopy.exe" -ArgumentList $TinyDir, $RoboJDest, "/E", "/MT:32", "/J", "/NFL", "/NDL", "/NJH", "/NJS", "/NP" -Wait -NoNewWindow }

$RoboDest = Join-Path $DestFullPath "run_robo_32"
$RoboCmd = { Start-Process -FilePath "robocopy.exe" -ArgumentList $TinyDir, $RoboDest, "/E", "/MT:32", "/NFL", "/NDL", "/NJH", "/NJS", "/NP" -Wait -NoNewWindow }

# Cache Warm-up (First, unmeasured read to ensure hot cache for both cycles)
Write-Host "`nWarming up cache by doing a dummy copy..." -ForegroundColor Yellow
$WarmUpDest = Join-Path $DestFullPath "warmup_run"
Safe-Clean $WarmUpDest
Start-Process -FilePath $BcopyPath -ArgumentList $TinyDir, $WarmUpDest -Wait -NoNewWindow
Safe-Clean $WarmUpDest
Write-Host "Cache warmed up." -ForegroundColor Green


# ==============================================================================
# Cycle 1: Forward Order (bcopy -> robocopy /MT:32 /J -> robocopy /MT:32)
# ==============================================================================
Write-Host "`n=== STARTING CYCLE 1 (Forward Order) ===" -ForegroundColor Magenta

$C1_Bcopy = Run-With-Cooldown "bcopy (Auto-Tuned) [C1]" $BcopyCmd $BcopyDest
$C1_RoboJ = Run-With-Cooldown "robocopy /MT:32 /J [C1]" $RoboJCmd $RoboJDest
$C1_Robo  = Run-With-Cooldown "robocopy /MT:32 [C1]" $RoboCmd $RoboDest

# Clean up paths after Cycle 1
Safe-Clean $BcopyDest
Safe-Clean $RoboJDest
Safe-Clean $RoboDest

# ==============================================================================
# Cycle 2: Reverse Order (robocopy /MT:32 -> robocopy /MT:32 /J -> bcopy)
# ==============================================================================
Write-Host "`n=== STARTING CYCLE 2 (Reverse Order) ===" -ForegroundColor Magenta

$C2_Robo  = Run-With-Cooldown "robocopy /MT:32 [C2]" $RoboCmd $RoboDest
$C2_RoboJ = Run-With-Cooldown "robocopy /MT:32 /J [C2]" $RoboJCmd $RoboJDest
$C2_Bcopy = Run-With-Cooldown "bcopy (Auto-Tuned) [C2]" $BcopyCmd $BcopyDest

# Clean up paths after Cycle 2
Safe-Clean $BcopyDest
Safe-Clean $RoboJDest
Safe-Clean $RoboDest

# ==============================================================================
# Compilation & Report
# ==============================================================================
$ResultText = @"
================================================================================
BetterCopy vs Robocopy Controlled Benchmark (Run-Order & FTL Mitigation)
Timestamp: $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
Fixture: 100,000 files x 8KB (~800MB)
Defender Real-Time Protection: $(if ($DefenderStatus) { 'Enabled' } else { 'Disabled' })
================================================================================
Tool Configuration             Cycle 1 (s)    Cycle 2 (s)    Delta (s)    Average (s)
--------------------------------------------------------------------------------
bcopy (Auto-Tuned - 16t)       $($C1_Bcopy.Time.ToString().PadRight(14)) $($C2_Bcopy.Time.ToString().PadRight(14)) $(([Math]::Round($C2_Bcopy.Time - $C1_Bcopy.Time, 3)).ToString().PadRight(12)) $([Math]::Round(($C1_Bcopy.Time + $C2_Bcopy.Time)/2, 3))
robocopy /MT:32 /J             $($C1_RoboJ.Time.ToString().PadRight(14)) $($C2_RoboJ.Time.ToString().PadRight(14)) $(([Math]::Round($C2_RoboJ.Time - $C1_RoboJ.Time, 3)).ToString().PadRight(12)) $([Math]::Round(($C1_RoboJ.Time + $C2_RoboJ.Time)/2, 3))
robocopy /MT:32                $($C1_Robo.Time.ToString().PadRight(14)) $($C2_Robo.Time.ToString().PadRight(14)) $(([Math]::Round($C2_Robo.Time - $C1_Robo.Time, 3)).ToString().PadRight(12)) $([Math]::Round(($C1_Robo.Time + $C2_Robo.Time)/2, 3))
================================================================================
"@

Write-Host "`n$ResultText" -ForegroundColor Green

# Save results
$ResultsDir = "bench/results"
if (-not (Test-Path $ResultsDir)) {
    New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
}
$Timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$OutputFile = Join-Path $ResultsDir "controlled_comparison_$Timestamp.txt"
$ResultText | Out-File -FilePath $OutputFile

Write-Host "Results saved to $OutputFile" -ForegroundColor Gray
