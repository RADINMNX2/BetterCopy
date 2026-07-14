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

# Cache Warm-up (First, unmeasured read to ensure hot cache for all runs)
Write-Host "`nWarming up cache by doing a dummy copy..." -ForegroundColor Yellow
$WarmUpDest = Join-Path $DestFullPath "warmup_run"
Safe-Clean $WarmUpDest
Start-Process -FilePath $BcopyPath -ArgumentList $TinyDir, $WarmUpDest -Wait -NoNewWindow
Safe-Clean $WarmUpDest
Write-Host "Cache warmed up." -ForegroundColor Green


# ==============================================================================
# Symmetrical Sandwich Run: 1 -> 2 -> 3 -> 1
# ==============================================================================
Write-Host "`n=== STARTING SANDWICH BENCHMARK ===" -ForegroundColor Magenta

$R1_Bcopy = Run-With-Cooldown "bcopy (Auto-Tuned) - Run 1" $BcopyCmd $BcopyDest
$R2_RoboJ = Run-With-Cooldown "robocopy /MT:32 /J" $RoboJCmd $RoboJDest
$R3_Robo  = Run-With-Cooldown "robocopy /MT:32" $RoboCmd $RoboDest
$R4_Bcopy = Run-With-Cooldown "bcopy (Auto-Tuned) - Run 2" $BcopyCmd $BcopyDest

# Clean up paths after benchmark
Safe-Clean $BcopyDest
Safe-Clean $RoboJDest
Safe-Clean $RoboDest

# ==============================================================================
# Compilation & Report
# ==============================================================================
$BcopyDelta = [Math]::Round($R4_Bcopy.Time - $R1_Bcopy.Time, 3)
$BcopyAvg = [Math]::Round(($R1_Bcopy.Time + $R4_Bcopy.Time)/2, 3)
$BcopyAvgFps = [Math]::Round(100000 / $BcopyAvg, 1)

$ResultText = @"
================================================================================
BetterCopy vs Robocopy Symmetrical Sandwich Benchmark (1,2,3,1)
Timestamp: $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
Fixture: 100,000 files x 8KB (~800MB)
Defender Real-Time Protection: $(if ($DefenderStatus) { 'Enabled' } else { 'Disabled' })
================================================================================
Tool Configuration             Time (s)       Files/Sec      Valid
--------------------------------------------------------------------------------
bcopy (Auto-Tuned) - Run 1     $($R1_Bcopy.Time.ToString().PadRight(14)) $($R1_Bcopy.Fps.ToString().PadRight(14)) $($R1_Bcopy.Verified)
robocopy /MT:32 /J             $($R2_RoboJ.Time.ToString().PadRight(14)) $($R2_RoboJ.Fps.ToString().PadRight(14)) $($R2_RoboJ.Verified)
robocopy /MT:32                $($R3_Robo.Time.ToString().PadRight(14)) $($R3_Robo.Fps.ToString().PadRight(14)) $($R3_Robo.Verified)
bcopy (Auto-Tuned) - Run 2     $($R4_Bcopy.Time.ToString().PadRight(14)) $($R4_Bcopy.Fps.ToString().PadRight(14)) $($R4_Bcopy.Verified)
--------------------------------------------------------------------------------
bcopy Drift (Run 2 - Run 1):   $($BcopyDelta) seconds
bcopy Average:                 $($BcopyAvg) seconds ($BcopyAvgFps Files/Sec)
================================================================================
"@

Write-Host "`n$ResultText" -ForegroundColor Green

# Save results
$ResultsDir = "bench/results"
if (-not (Test-Path $ResultsDir)) {
    New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
}
$Timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$OutputFile = Join-Path $ResultsDir "sandwich_comparison_$Timestamp.txt"
$ResultText | Out-File -FilePath $OutputFile

Write-Host "Results saved to $OutputFile" -ForegroundColor Gray
