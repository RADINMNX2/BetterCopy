param (
    [string]$Dest = "bench/dest",
    [int]$CooldownSeconds = 30
)

$ErrorActionPreference = "Stop"

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
$TinyDirFullPath = [System.IO.Path]::GetFullPath($TinyDir)
if (-not (Test-Path $TinyDir)) {
    Write-Error "Fixture not found at $TinyDir."
} else {
    Write-Host "Tiny Fixture exists at $TinyDir." -ForegroundColor Green
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

# Core function to execute and measure Windows Explorer Copy
function Measure-Explorer-Copy([string]$SrcDir, [string]$DestDir) {
    $AbsoluteSrc = [System.IO.Path]::GetFullPath($SrcDir)
    $AbsoluteDest = [System.IO.Path]::GetFullPath($DestDir)
    
    $Shell = New-Object -ComObject Shell.Application
    
    if (-not (Test-Path $AbsoluteDest)) {
        New-Item -ItemType Directory -Force -Path $AbsoluteDest | Out-Null
    }
    $DestFolder = $Shell.NameSpace($AbsoluteDest)
    
    $MaxFiles = [System.IO.Directory]::GetFiles($AbsoluteSrc, "*", [System.IO.SearchOption]::AllDirectories).Length
    
    Add-Type -AssemblyName UIAutomationClient
    $Root = [System.Windows.Automation.AutomationElement]::RootElement
    $ClassCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ClassNameProperty,
        "#32770"
    )
    
    $ExplorerPids = (Get-Process explorer -ErrorAction SilentlyContinue).Id
    $BaselineHwnds = @()
    if ($ExplorerPids) {
        $Dialogs = $Root.FindAll([System.Windows.Automation.TreeScope]::Children, $ClassCondition)
        foreach ($Dlg in $Dialogs) {
            if ($Dlg.Current.ProcessId -in $ExplorerPids) {
                $BaselineHwnds += $Dlg.Current.NativeWindowHandle
            }
        }
    }
    
    Write-Host "Triggering Windows Explorer Copy via Shell COM (passing folder string)..." -ForegroundColor Gray
    $Stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    
    # Pass the folder path directly to prevent COM enumeration bottleneck
    $DestFolder.CopyHere($AbsoluteSrc, 16 + 512)
    
    $WindowDetected = $false
    $CopiedFolderFullPath = Join-Path $AbsoluteDest "tiny"
    
    $LastCountTime = [System.Diagnostics.Stopwatch]::StartNew()
    $CurrentCount = 0
    
    while ($Stopwatch.ElapsedMilliseconds -lt 600000) {
        $CurrentDialogs = $Root.FindAll([System.Windows.Automation.TreeScope]::Children, $ClassCondition)
        $NewDialogActive = $false
        foreach ($Dlg in $CurrentDialogs) {
            if ($Dlg.Current.ProcessId -in $ExplorerPids) {
                $Hwnd = $Dlg.Current.NativeWindowHandle
                if ($Hwnd -notIn $BaselineHwnds) {
                    $NewDialogActive = $true
                    $WindowDetected = $true
                }
            }
        }
        
        # Check files copied so far, but only once every 5 seconds to avoid thrashed file IO
        if ($LastCountTime.ElapsedMilliseconds -ge 5000 -or -not $NewDialogActive) {
            if (Test-Path $CopiedFolderFullPath) {
                try {
                    $CurrentCount = [System.IO.Directory]::GetFiles($CopiedFolderFullPath, "*", [System.IO.SearchOption]::AllDirectories).Length
                } catch {
                    $CurrentCount = 0
                }
            }
            $LastCountTime.Restart()
        }
        
        if ($WindowDetected -and -not $NewDialogActive) {
            if ($CurrentCount -ge $MaxFiles) {
                break
            }
        }
        
        if (-not $WindowDetected -and $CurrentCount -ge $MaxFiles -and $CurrentCount -gt 0) {
            break
        }
        
        Start-Sleep -Milliseconds 250
    }
    $Stopwatch.Stop()
    
    return $Stopwatch.Elapsed.TotalSeconds
}

# Helper to run a single timed run with cooldown
function Run-With-Cooldown([string]$Name, [string]$DestPath) {
    Write-Host "`n[Cleanup] Cleaning destination: $DestPath" -ForegroundColor Gray
    Safe-Clean $DestPath
    
    Write-Host "[Cooldown] Sleeping $CooldownSeconds seconds to let SSD FTL and NTFS MFT settle..." -ForegroundColor Yellow
    Start-Sleep -Seconds $CooldownSeconds
    
    Write-Host "Running $Name..." -ForegroundColor Cyan
    $ElapsedSeconds = Measure-Explorer-Copy $TinyDirFullPath $DestPath
    
    $CopiedPath = Join-Path $DestPath "tiny"
    $Valid = Verify-Copy $TinyDirFullPath $CopiedPath
    
    Write-Host "  Completed in: $([Math]::Round($ElapsedSeconds, 3)) seconds (Integrity Verified: $Valid)" -ForegroundColor Gray
    
    return [PSCustomObject]@{
        Tool = $Name
        Time = [Math]::Round($ElapsedSeconds, 3)
        Fps = [Math]::Round(100000 / $ElapsedSeconds, 1)
        Verified = $Valid
    }
}

$ExplorerDest = Join-Path $DestFullPath "run_explorer"

# Cache Warm-up
Write-Host "`nWarming up cache by doing a dummy copy..." -ForegroundColor Yellow
$WarmUpDest = Join-Path $DestFullPath "warmup_run"
Safe-Clean $WarmUpDest
$Dummy = Measure-Explorer-Copy $TinyDirFullPath $WarmUpDest
Safe-Clean $WarmUpDest
Write-Host "Cache warmed up." -ForegroundColor Green


# ==============================================================================
# Execution
# ==============================================================================
Write-Host "`n=== STARTING WINDOWS EXPLORER BENCHMARK ===" -ForegroundColor Magenta

$R1 = Run-With-Cooldown "Windows Explorer - Run 1" $ExplorerDest
$R2 = Run-With-Cooldown "Windows Explorer - Run 2" $ExplorerDest
$R3 = Run-With-Cooldown "Windows Explorer - Run 3" $ExplorerDest

# Clean up paths after benchmark
Safe-Clean $ExplorerDest

# ==============================================================================
# Compilation & Report
# ==============================================================================
$AvgTime = [Math]::Round(($R1.Time + $R2.Time + $R3.Time)/3, 3)
$AvgFps = [Math]::Round(100000 / $AvgTime, 1)

$ResultText = @"
================================================================================
Windows Explorer Benchmark (3 runs)
Timestamp: $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
Fixture: 100,000 files x 8KB (~800MB)
Defender Real-Time Protection: $(if ($DefenderStatus) { 'Enabled' } else { 'Disabled' })
================================================================================
Run                            Time (s)       Files/Sec      Valid
--------------------------------------------------------------------------------
Windows Explorer - Run 1       $($R1.Time.ToString().PadRight(14)) $($R1.Fps.ToString().PadRight(14)) $($R1.Verified)
Windows Explorer - Run 2       $($R2.Time.ToString().PadRight(14)) $($R2.Fps.ToString().PadRight(14)) $($R2.Verified)
Windows Explorer - Run 3       $($R3.Time.ToString().PadRight(14)) $($R3.Fps.ToString().PadRight(14)) $($R3.Verified)
--------------------------------------------------------------------------------
Average Time:                  $($AvgTime) seconds ($AvgFps Files/Sec)
================================================================================
"@

Write-Host "`n$ResultText" -ForegroundColor Green

# Save results
$ResultsDir = "bench/results"
if (-not (Test-Path $ResultsDir)) {
    New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
}
$Timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$OutputFile = Join-Path $ResultsDir "explorer_benchmark_$Timestamp.txt"
$ResultText | Out-File -FilePath $OutputFile

Write-Host "Results saved to $OutputFile" -ForegroundColor Gray
