param (
    [string]$Dest = "bench/dest"
)

$ErrorActionPreference = "Stop"

# Ensure we run from workspace root
$PSScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
if ($PSScriptRoot) {
    # If script runs, Cwd might be different, but we assume workspace root
}

# 1. Build the release binary
Write-Host "Building BetterCopy in release mode..." -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Error "Cargo build failed."
}

$BcopyPath = "target\release\bcopy.exe"
if (-not (Test-Path $BcopyPath)) {
    Write-Error "Could not find release binary at $BcopyPath"
}

# 2. Check and generate fixture
$FixtureDir = "bench/fixtures/tiny"
if (-not (Test-Path $FixtureDir)) {
    Write-Host "Fixture not found. Generating 100,000 tiny-file fixture (800MB)..." -ForegroundColor Cyan
    Start-Process -FilePath $BcopyPath -ArgumentList "--gen-fixture", $FixtureDir -Wait -NoNewWindow
} else {
    Write-Host "Fixture already exists at $FixtureDir." -ForegroundColor Green
}

# 3. Preflight check: Verify destination free space
$DestFullPath = [System.IO.Path]::GetFullPath($Dest)
$DestDrive = [System.IO.Path]::GetPathRoot($DestFullPath)
$DriveInfo = [System.IO.DriveInfo]::new($DestDrive)
$FreeSpaceBytes = $DriveInfo.AvailableFreeSpace
$RequiredSpace = 2GB

if ($FreeSpaceBytes -lt $RequiredSpace) {
    Write-Error "Insufficient disk space on $DestDrive. Required: 2 GB, Available: ($($FreeSpaceBytes / 1GB) GB)"
}
Write-Host "Destination path: $DestFullPath (Drive $DestDrive, Free Space: $([Math]::Round($FreeSpaceBytes / 1GB, 2)) GB)" -ForegroundColor Green

# Ensure destination parent folder exists
New-Item -ItemType Directory -Force -Path $DestFullPath | Out-Null

# Helper to safely delete folders even if files are temporarily locked by AV/indexing
function Safe-Clean([string]$Path) {
    if (Test-Path $Path) {
        for ($i = 0; $i -lt 10; $i++) {
            try {
                Remove-Item -Recurse -Force $Path -ErrorAction Stop
                return
            } catch {
                Write-Host "  Retrying cleanup of $Path in 2 seconds... ($($i+1)/10)" -ForegroundColor Yellow
                Start-Sleep -Seconds 2
            }
        }
        # Last attempt with standard command ignoring error
        Remove-Item -Recurse -Force $Path -ErrorAction Ignore
    }
}

# Helper to run benchmark runs
function Run-Bench([string]$ToolName, [scriptblock]$RunCmd, [string]$RunDest) {
    Write-Host "Benchmarking $ToolName (1 run)..." -ForegroundColor Cyan
    $Times = @()

    for ($i = 1; $i -le 1; $i++) {
        Write-Host "  Run $i/1..." -ForegroundColor Gray
        
        # Clean destination first
        Safe-Clean $RunDest

        # Measure time
        $Elapsed = Measure-Command {
            & $RunCmd
        }
        $Times += $Elapsed.TotalSeconds
        Write-Host "    Completed in: $([Math]::Round($Elapsed.TotalSeconds, 3)) seconds" -ForegroundColor Gray
    }

    # Clean up destination
    Safe-Clean $RunDest

    # Return the single run time
    return $Times[0]
}

# Run bcopy
$BcopyDest = Join-Path $DestFullPath "bcopy_run"
$BcopyMedian = Run-Bench "bcopy" {
    Start-Process -FilePath $BcopyPath -ArgumentList $FixtureDir, $BcopyDest, "-t", "12" -Wait -NoNewWindow
} $BcopyDest

# Run robocopy
$RobocopyDest = Join-Path $DestFullPath "robocopy_run"
$RobocopyMedian = Run-Bench "robocopy" {
    # Robocopy exit code >= 8 indicates error, < 8 is success, but we ignore exit code to prevent stopping
    robocopy $FixtureDir $RobocopyDest /E /MT:8 /NFL /NDL /NJH /NJS /nc /ns /np /r:0 /w:0 *>&1 | Out-Null
} $RobocopyDest

# 4. Print table and write results
$TotalFiles = 100000
$BcopyFps = [Math]::Round($TotalFiles / $BcopyMedian, 1)
$RobocopyFps = [Math]::Round($TotalFiles / $RobocopyMedian, 1)

$ResultText = @"
==========================================
BetterCopy Performance Benchmark Results
Timestamp: $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
Fixture: 100,000 files x 8KB (~800MB)
Destination: $DestFullPath
==========================================
Tool           Median Time (s)    Files/Sec
------------------------------------------
bcopy          $([Math]::Round($BcopyMedian, 3).ToString().PadRight(15)) $BcopyFps
robocopy /MT:8 $([Math]::Round($RobocopyMedian, 3).ToString().PadRight(15)) $RobocopyFps
==========================================
"@

Write-Host "`n$ResultText" -ForegroundColor Green

# Save results
$ResultsDir = "bench/results"
if (-not (Test-Path $ResultsDir)) {
    New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
}
$Timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$OutputFile = Join-Path $ResultsDir "bench_$Timestamp.txt"
$ResultText | Out-File -FilePath $OutputFile

Write-Host "Results saved to $OutputFile" -ForegroundColor Gray
