param (
    [string]$Dest = "bench/dest"
)

$ErrorActionPreference = "Stop"

# Ensure we run from workspace root
$PSScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
if ($PSScriptRoot) {
    # If script runs, Cwd might be different, but we assume workspace root
}

$BcopyPath = "target\release\bcopy.exe"
if (-not (Test-Path $BcopyPath)) {
    Write-Error "Could not find release binary at $BcopyPath. Please build it first."
}

# Check and generate fixture
$FixtureDir = "bench/fixtures/tiny"
if (-not (Test-Path $FixtureDir)) {
    Write-Host "Fixture not found. Generating 100,000 tiny-file fixture (800MB)..." -ForegroundColor Cyan
    Start-Process -FilePath $BcopyPath -ArgumentList "--gen-fixture", $FixtureDir -Wait -NoNewWindow
} else {
    Write-Host "Fixture already exists at $FixtureDir." -ForegroundColor Green
}

# Preflight check: Verify destination free space
$DestFullPath = [System.IO.Path]::GetFullPath($Dest)
$DestDrive = [System.IO.Path]::GetPathRoot($DestFullPath)
$DriveInfo = [System.IO.DriveInfo]::new($DestDrive)
$FreeSpaceBytes = $DriveInfo.AvailableFreeSpace
$RequiredSpace = 5GB # We'll need a bit more space since we run multiple tests, though we clean up after each.

if ($FreeSpaceBytes -lt $RequiredSpace) {
    Write-Error "Insufficient disk space on $DestDrive. Required: 5 GB, Available: ($($FreeSpaceBytes / 1GB) GB)"
}
Write-Host "Destination path: $DestFullPath (Drive $DestDrive, Free Space: $([Math]::Round($FreeSpaceBytes / 1GB, 2)) GB)" -ForegroundColor Green

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
                Write-Host "  Retrying cleanup of $Path in 2 seconds... ($($i+1)/10)" -ForegroundColor Yellow
                Start-Sleep -Seconds 2
            }
        }
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

    return $Times[0]
}

# 1. Run bcopy (Auto-Tuned threads)
$BcopyAutoDest = Join-Path $DestFullPath "bcopy_auto"
$BcopyAutoTime = Run-Bench "bcopy (Auto-Tuned)" {
    Start-Process -FilePath $BcopyPath -ArgumentList $FixtureDir, $BcopyAutoDest -Wait -NoNewWindow
} $BcopyAutoDest

# 2. Run bcopy (12 threads)
$Bcopy12Dest = Join-Path $DestFullPath "bcopy_12"
$Bcopy12Time = Run-Bench "bcopy (12 threads)" {
    Start-Process -FilePath $BcopyPath -ArgumentList $FixtureDir, $Bcopy12Dest, "-t", "12" -Wait -NoNewWindow
} $Bcopy12Dest

# 3. Run bcopy (32 threads)
$Bcopy32Dest = Join-Path $DestFullPath "bcopy_32"
$Bcopy32Time = Run-Bench "bcopy (32 threads)" {
    Start-Process -FilePath $BcopyPath -ArgumentList $FixtureDir, $Bcopy32Dest, "-t", "32" -Wait -NoNewWindow
} $Bcopy32Dest

# 4. Run robocopy /MT:32 /J
$Robo32JDest = Join-Path $DestFullPath "robo_32_j"
$Robo32JTime = Run-Bench "robocopy /MT:32 /J" {
    Start-Process -FilePath "robocopy.exe" -ArgumentList $FixtureDir, $Robo32JDest, "/E", "/MT:32", "/J", "/NFL", "/NDL", "/NJH", "/NJS" -Wait -NoNewWindow
} $Robo32JDest

# 5. Run robocopy /MT:32 (no /J)
$Robo32Dest = Join-Path $DestFullPath "robo_32"
$Robo32Time = Run-Bench "robocopy /MT:32" {
    Start-Process -FilePath "robocopy.exe" -ArgumentList $FixtureDir, $Robo32Dest, "/E", "/MT:32", "/NFL", "/NDL", "/NJH", "/NJS" -Wait -NoNewWindow
} $Robo32Dest

# 6. Run robocopy /MT:8 (default parallel)
$Robo8Dest = Join-Path $DestFullPath "robo_8"
$Robo8Time = Run-Bench "robocopy /MT:8" {
    Start-Process -FilePath "robocopy.exe" -ArgumentList $FixtureDir, $Robo8Dest, "/E", "/MT:8", "/NFL", "/NDL", "/NJH", "/NJS" -Wait -NoNewWindow
} $Robo8Dest


# Print table and write results
$TotalFiles = 100000

$BcopyAutoFps = [Math]::Round($TotalFiles / $BcopyAutoTime, 1)
$Bcopy12Fps   = [Math]::Round($TotalFiles / $Bcopy12Time, 1)
$Bcopy32Fps   = [Math]::Round($TotalFiles / $Bcopy32Time, 1)
$Robo32JFps   = [Math]::Round($TotalFiles / $Robo32JTime, 1)
$Robo32Fps    = [Math]::Round($TotalFiles / $Robo32Time, 1)
$Robo8Fps     = [Math]::Round($TotalFiles / $Robo8Time, 1)

$ResultText = @"
======================================================================
BetterCopy Performance Benchmark Results vs Robocopy Configurations
Timestamp: $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
Fixture: 100,000 files x 8KB (~800MB)
Destination: $DestFullPath
======================================================================
Tool                           Time (s)           Files/Sec
----------------------------------------------------------------------
bcopy (Auto-Tuned)             $([Math]::Round($BcopyAutoTime, 3).ToString().PadRight(18)) $BcopyAutoFps
bcopy (12 threads)             $([Math]::Round($Bcopy12Time, 3).ToString().PadRight(18)) $Bcopy12Fps
bcopy (32 threads)             $([Math]::Round($Bcopy32Time, 3).ToString().PadRight(18)) $Bcopy32Fps
robocopy /MT:32 /J             $([Math]::Round($Robo32JTime, 3).ToString().PadRight(18)) $Robo32JFps
robocopy /MT:32                $([Math]::Round($Robo32Time, 3).ToString().PadRight(18)) $Robo32Fps
robocopy /MT:8                 $([Math]::Round($Robo8Time, 3).ToString().PadRight(18)) $Robo8Fps
======================================================================
"@

Write-Host "`n$ResultText" -ForegroundColor Green

# Save results
$ResultsDir = "bench/results"
if (-not (Test-Path $ResultsDir)) {
    New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
}
$Timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$OutputFile = Join-Path $ResultsDir "comparison_$Timestamp.txt"
$ResultText | Out-File -FilePath $OutputFile

Write-Host "Results saved to $OutputFile" -ForegroundColor Gray
