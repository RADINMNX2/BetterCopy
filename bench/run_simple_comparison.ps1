param (
    [string]$Dest = "bench/dest"
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

# 2. Fixture Checking (only Tiny)
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

# 3. Warm Up Cache
Write-Host "`nWarming up file system cache by reading fixture..." -ForegroundColor Yellow
$WarmUpDest = Join-Path $DestFullPath "warmup_run"
Safe-Clean $WarmUpDest
# Run a fast copy to warm up the cache
Start-Process -FilePath $BcopyPath -ArgumentList $TinyDir, $WarmUpDest -Wait -NoNewWindow
$WarmUpValid = Verify-Copy $TinyDir $WarmUpDest
Safe-Clean $WarmUpDest
Write-Host "Cache warmed up successfully (Warm-up copy verified: $WarmUpValid)" -ForegroundColor Green

# 4. Running the Benchmark
$Configs = @(
    @{ Name = "bcopy (Auto-Tuned)"; Cmd = { Start-Process -FilePath $BcopyPath -ArgumentList $TinyDir, $DestPath -Wait -NoNewWindow } },
    @{ Name = "robocopy /MT:32 /J"; Cmd = { Start-Process -FilePath "robocopy.exe" -ArgumentList $TinyDir, $DestPath, "/E", "/MT:32", "/J", "/NFL", "/NDL", "/NJH", "/NJS", "/NP" -Wait -NoNewWindow } },
    @{ Name = "robocopy /MT:32";    Cmd = { Start-Process -FilePath "robocopy.exe" -ArgumentList $TinyDir, $DestPath, "/E", "/MT:32", "/NFL", "/NDL", "/NJH", "/NJS", "/NP" -Wait -NoNewWindow } }
)

$Results = @()

foreach ($Config in $Configs) {
    $Name = $Config.Name
    $Cmd = $Config.Cmd
    $DestPath = Join-Path $DestFullPath "run_$($Name.Replace(' ', '_').Replace('/', '_').Replace(':', '_'))"
    
    Write-Host "`nBenchmarking $Name..." -ForegroundColor Cyan
    Safe-Clean $DestPath
    
    $Elapsed = Measure-Command {
        & $Cmd
    }
    
    $Valid = Verify-Copy $TinyDir $DestPath
    Write-Host "  Completed in: $([Math]::Round($Elapsed.TotalSeconds, 3)) seconds (Integrity Verified: $Valid)" -ForegroundColor Gray
    
    $Results += [PSCustomObject]@{
        Tool = $Name
        Time = [Math]::Round($Elapsed.TotalSeconds, 3)
        Fps = [Math]::Round(100000 / $Elapsed.TotalSeconds, 1)
        Verified = $Valid
    }
    
    Safe-Clean $DestPath
}

# 5. Output Table
$ResultText = @"
================================================================================
BetterCopy vs Robocopy Simple Benchmark Results (Hot Cache)
Timestamp: $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
Fixture: 100,000 files x 8KB (~800MB)
Defender Real-Time Protection: $(if ($DefenderStatus) { 'Enabled' } else { 'Disabled' })
================================================================================
Tool                           Time (s)       Files/Sec      Valid
--------------------------------------------------------------------------------
$($Results[0].Tool.PadRight(30)) $($Results[0].Time.ToString().PadRight(14)) $($Results[0].Fps.ToString().PadRight(14)) $($Results[0].Verified)
$($Results[1].Tool.PadRight(30)) $($Results[1].Time.ToString().PadRight(14)) $($Results[1].Fps.ToString().PadRight(14)) $($Results[1].Verified)
$($Results[2].Tool.PadRight(30)) $($Results[2].Time.ToString().PadRight(14)) $($Results[2].Fps.ToString().PadRight(14)) $($Results[2].Verified)
================================================================================
"@

Write-Host "`n$ResultText" -ForegroundColor Green

# Save results
$ResultsDir = "bench/results"
if (-not (Test-Path $ResultsDir)) {
    New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
}
$Timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$OutputFile = Join-Path $ResultsDir "simple_comparison_$Timestamp.txt"
$ResultText | Out-File -FilePath $OutputFile

Write-Host "Results saved to $OutputFile" -ForegroundColor Gray
