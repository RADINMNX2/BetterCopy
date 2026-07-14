param (
    [string]$Dest = "bench/dest",
    [int]$TimedRuns = 3
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

# 1. Environment & Defender Check
Write-Host "=== Environment Information ===" -ForegroundColor Cyan
$DefenderStatus = Get-MpComputerStatus | Select-Object -ExpandProperty RealTimeProtectionEnabled
Write-Host "Windows Defender Real-Time Protection: $(if ($DefenderStatus) { 'Enabled' } else { 'Disabled' })" -ForegroundColor Yellow

$DestFullPath = [System.IO.Path]::GetFullPath($Dest)
$DestDrive = [System.IO.Path]::GetPathRoot($DestFullPath)
$DriveInfo = [System.IO.DriveInfo]::new($DestDrive)
$FreeSpaceBytes = $DriveInfo.AvailableFreeSpace
$RequiredSpace = 40GB

Write-Host "Destination path: $DestFullPath (Drive $DestDrive, Free Space: $([Math]::Round($FreeSpaceBytes / 1GB, 2)) GB)" -ForegroundColor Green
if ($FreeSpaceBytes -lt $RequiredSpace) {
    Write-Error "Insufficient disk space on $DestDrive. Required: 40 GB, Available: ($([Math]::Round($FreeSpaceBytes / 1GB, 2)) GB)"
}

# 2. Fixture Checking and Generation
$TinyDir = "bench/fixtures/tiny"
$MediumDir = "bench/fixtures/medium"
$LargeDir = "bench/fixtures/large"

Write-Host "`n=== Fixture Initialization ===" -ForegroundColor Cyan

# Tiny Fixture (100k x 8KB = 800MB)
if (-not (Test-Path $TinyDir)) {
    Write-Host "Generating Tiny Fixture (100,000 files x 8KB)..." -ForegroundColor Yellow
    Start-Process -FilePath $BcopyPath -ArgumentList "--gen-fixture", $TinyDir -Wait -NoNewWindow
} else {
    Write-Host "Tiny Fixture already exists." -ForegroundColor Green
}

# Medium Fixture (10k x 2MB = 20GB)
if (-not (Test-Path $MediumDir)) {
    Write-Host "Generating Medium Fixture (10,000 files x 2MB = 20GB)..." -ForegroundColor Yellow
    New-Item -ItemType Directory -Force -Path $MediumDir | Out-Null
    $Buffer = [byte[]]::new(2 * 1024 * 1024) # 2MB of zeroes
    for ($d = 0; $d -lt 10; $d++) {
        $Subdir = Join-Path $MediumDir "dir_$d"
        New-Item -ItemType Directory -Force -Path $Subdir | Out-Null
        Write-Host "  Generating dir_$d (1000 files)..." -ForegroundColor Gray
        for ($f = 0; $f -lt 1000; $f++) {
            $FilePath = Join-Path $Subdir "file_$f.bin"
            [System.IO.File]::WriteAllBytes($FilePath, $Buffer)
        }
    }
} else {
    Write-Host "Medium Fixture already exists." -ForegroundColor Green
}

# Large Fixture (1 x 10GB = 10GB)
$LargeFile = Join-Path $LargeDir "large.bin"
if (-not (Test-Path $LargeFile)) {
    Write-Host "Generating Large Fixture (1 file x 10GB)..." -ForegroundColor Yellow
    New-Item -ItemType Directory -Force -Path $LargeDir | Out-Null
    fsutil file createnew $LargeFile 10737418240 | Out-Null
} else {
    Write-Host "Large Fixture already exists." -ForegroundColor Green
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

# Helper to verify copy integrity using fast .NET APIs
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

# Helper to compute statistics
function Get-Stats([double[]]$Values) {
    $Sum = 0
    foreach ($v in $Values) { $Sum += $v }
    $Mean = $Sum / $Values.Length
    
    $SumOfSquares = 0
    foreach ($v in $Values) {
        $SumOfSquares += [Math]::Pow($v - $Mean, 2)
    }
    $Variance = $SumOfSquares / $Values.Length
    $StdDev = [Math]::Sqrt($Variance)
    
    $Sorted = $Values | Sort-Object
    $Mid = [Math]::Floor($Values.Length / 2)
    if ($Values.Length % 2 -eq 0) {
        $Median = ($Sorted[$Mid - 1] + $Sorted[$Mid]) / 2
    } else {
        $Median = $Sorted[$Mid]
    }
    
    return [PSCustomObject]@{
        Mean = $Mean
        Median = $Median
        StdDev = $StdDev
    }
}

# Benchmark execution engine
function Run-Fixture-Benchmark([string]$FixtureName, [string]$SrcPath, [int]$TotalFiles) {
    Write-Host "`n==================================================" -ForegroundColor Cyan
    Write-Host "Running Benchmark for: $FixtureName" -ForegroundColor Cyan
    Write-Host "==================================================" -ForegroundColor Cyan

    $Configs = @(
        @{ Name = "bcopy (Auto-Tuned)"; Cmd = { Start-Process -FilePath $BcopyPath -ArgumentList $SrcPath, $DestPath -Wait -NoNewWindow } },
        @{ Name = "bcopy (12 threads)"; Cmd = { Start-Process -FilePath $BcopyPath -ArgumentList $SrcPath, $DestPath, "-t", "12" -Wait -NoNewWindow } },
        @{ Name = "bcopy (32 threads)"; Cmd = { Start-Process -FilePath $BcopyPath -ArgumentList $SrcPath, $DestPath, "-t", "32" -Wait -NoNewWindow } },
        @{ Name = "robocopy /MT:32 /J"; Cmd = { Start-Process -FilePath "robocopy.exe" -ArgumentList $SrcPath, $DestPath, "/E", "/MT:32", "/J", "/NFL", "/NDL", "/NJH", "/NJS", "/NP" -Wait -NoNewWindow } },
        @{ Name = "robocopy /MT:32";    Cmd = { Start-Process -FilePath "robocopy.exe" -ArgumentList $SrcPath, $DestPath, "/E", "/MT:32", "/NFL", "/NDL", "/NJH", "/NJS", "/NP" -Wait -NoNewWindow } },
        @{ Name = "robocopy /MT:8";     Cmd = { Start-Process -FilePath "robocopy.exe" -ArgumentList $SrcPath, $DestPath, "/E", "/MT:8", "/NFL", "/NDL", "/NJH", "/NJS", "/NP" -Wait -NoNewWindow } }
    )

    $Results = @()

    foreach ($Config in $Configs) {
        $Name = $Config.Name
        $Cmd = $Config.Cmd
        $DestPath = Join-Path $DestFullPath "$($FixtureName)_$($Name.Replace(' ', '_').Replace('/', '_').Replace(':', '_'))"

        Write-Host "Benchmarking $Name..." -ForegroundColor Magenta
        
        # 1. Warm-up Run (Cached State Setup, time discarded)
        Write-Host "  Warm-up run..." -ForegroundColor Gray
        Safe-Clean $DestPath
        & $Cmd
        
        # Verify integrity on the warm-up run
        $Verified = Verify-Copy $SrcPath $DestPath
        if (-not $Verified) {
            Write-Host "  [WARNING] Verification failed for $Name. Copy was incomplete or corrupted!" -ForegroundColor Red
        } else {
            Write-Host "  Copy verified successfully." -ForegroundColor Green
        }
        Safe-Clean $DestPath

        # 2. Timed Runs
        $Times = @()
        for ($i = 1; $i -le $TimedRuns; $i++) {
            Write-Host "  Run $i/$TimedRuns..." -ForegroundColor Gray
            Safe-Clean $DestPath
            
            $Elapsed = Measure-Command {
                & $Cmd
            }
            $Times += $Elapsed.TotalSeconds
            Write-Host "    $([Math]::Round($Elapsed.TotalSeconds, 3)) seconds" -ForegroundColor Gray
        }
        Safe-Clean $DestPath

        # Compute statistics
        $Stats = Get-Stats $Times
        $Fps = [Math]::Round($TotalFiles / $Stats.Median, 1)

        $Results += [PSCustomObject]@{
            Tool = $Name
            Median = [Math]::Round($Stats.Median, 3)
            Mean = [Math]::Round($Stats.Mean, 3)
            StdDev = [Math]::Round($Stats.StdDev, 3)
            Fps = $Fps
            Verified = $Verified
        }
    }

    return $Results
}

# Run all benchmarks
$TinyResults = Run-Fixture-Benchmark "Tiny_100k_8KB" $TinyDir 100000
$MediumResults = Run-Fixture-Benchmark "Medium_10k_2MB" $MediumDir 10000
$LargeResults = Run-Fixture-Benchmark "Large_1_10GB" $LargeDir 1

# Format and write results
$ResultHeader = @"
================================================================================
BetterCopy Performance Benchmark Results vs Robocopy Configurations
Timestamp: $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
Defender Real-Time Protection: $(if ($DefenderStatus) { 'Enabled' } else { 'Disabled' })
Destination: $DestFullPath
================================================================================
"@

function Format-Table-Text($Title, $Results) {
    $Text = "`nFixture: $Title`n"
    $Text += "--------------------------------------------------------------------------------`n"
    $Text += "Tool                           Median (s) Mean (s)   StdDev (s) Files/Sec  Valid`n"
    $Text += "--------------------------------------------------------------------------------`n"
    foreach ($r in $Results) {
        $Tool = $r.Tool.PadRight(30)
        $Med = $r.Median.ToString().PadRight(10)
        $Mean = $r.Mean.ToString().PadRight(10)
        $SD = $r.StdDev.ToString().PadRight(10)
        $Fps = $r.Fps.ToString().PadRight(10)
        $Val = $r.Verified.ToString()
        $Text += "$Tool $Med $Mean $SD $Fps $Val`n"
    }
    $Text += "--------------------------------------------------------------------------------`n"
    return $Text
}

$ResultText = $ResultHeader
$ResultText += Format-Table-Text "Tiny Fixture (100,000 files x 8KB)" $TinyResults
$ResultText += Format-Table-Text "Medium Fixture (10,000 files x 2MB)" $MediumResults
$ResultText += Format-Table-Text "Large Fixture (1 file x 10GB)" $LargeResults

Write-Host "`n$ResultText" -ForegroundColor Green

# Save results
$ResultsDir = "bench/results"
if (-not (Test-Path $ResultsDir)) {
    New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
}
$Timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$OutputFile = Join-Path $ResultsDir "comprehensive_$Timestamp.txt"
$ResultText | Out-File -FilePath $OutputFile

Write-Host "Results saved to $OutputFile" -ForegroundColor Gray
