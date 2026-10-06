# Stable-profile launcher (buildmesh). Exit code contract and the
# early-closing-consumer caveat are documented in scripts/run-dev.ps1 and apply
# here identically: 0 = launched and verified, 1 = failed, and a non-zero that
# still carries an `OK - ` line on stdout is a host-level pipeline teardown, not
# a launch failure (issue #2043).
$ErrorActionPreference = "Stop"

Set-Location "$PSScriptRoot\.."

$Binary = "src-tauri\target\release\buildmesh.exe"
$LogPath = "$env:APPDATA\com.alond.buildmesh\logs\buildmesh.log"
# Panic-hook output files (see src-tauri/src/lib.rs:41-128 + 348-382). Two
# files, two hooks: `panic_early.log` is written by the hook installed in
# `run()` BEFORE Tauri setup; `panic.log` is the main hook's destination
# and carries the full backtrace. Same delta-capture protocol as run-dev.ps1
# (issue #158) so a panic-only crash doesn't masquerade as a successful
# launch.
$PanicLogPath = "$env:APPDATA\com.alond.buildmesh\logs\panic.log"
$PanicEarlyLogPath = "$env:APPDATA\com.alond.buildmesh\logs\panic_early.log"

# 1. Kill existing instances
$existing = Get-Process -Name 'buildmesh' -ErrorAction SilentlyContinue
if ($existing) {
    Write-Output "Stopping existing buildmesh..."
    # -ErrorAction SilentlyContinue: on a re-run the previous instance can exit
    # between Get-Process and Stop-Process. Under $ErrorActionPreference="Stop"
    # that race aborted the script before it ever launched (issue #2043).
    $existing | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 1000
}

# 2. Build (frontend + Rust)
Write-Output "Building..."
npm run tauri build
if ($LASTEXITCODE -ne 0) {
    Write-Output "ERROR: Build failed"
    exit 1
}

# 3. Verify binary exists
if (-not (Test-Path $Binary)) {
    Write-Output "ERROR: Build failed - $Binary not found"
    exit 1
}

# 4. Record log position
# Issue #2043 - the exit code must report the LAUNCH VERDICT and nothing else.
# $ErrorActionPreference = "Stop" is script-wide, so a read that failed for an
# incidental reason (the app still holds buildmesh.log open for writing, a panic
# log momentarily locked, a transient sharing violation) aborted the script and
# left powershell.exe reporting 1 for a launch that was already up and healthy.
# A failed read means "no evidence from this source", so it falls through to the
# next one instead of deciding the verdict. Write-Host keeps the warning off the
# success stream, which would otherwise corrupt this function's return value.
function Get-LogLines {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return @() }
    try {
        return @(Get-Content -LiteralPath $Path)
    } catch {
        Write-Host "WARN: could not read $Path ($($_.Exception.Message))"
        return @()
    }
}

$BeforeLines = (Get-LogLines $LogPath).Count
# Same delta-capture for the panic-hook outputs (issue #158). Echo the
# counts to stdout so callers can parse them and slice the post-launch file.
$BeforePanicLines = (Get-LogLines $PanicLogPath).Count
$BeforePanicEarlyLines = (Get-LogLines $PanicEarlyLogPath).Count
Write-Output "Buildmesh pre-launch line count (buildmesh.log): $BeforeLines"
Write-Output "Buildmesh pre-launch line count (panic.log): $BeforePanicLines"
Write-Output "Buildmesh pre-launch line count (panic_early.log): $BeforePanicEarlyLines"

# 5. Launch raw binary.
# RUST_BACKTRACE=1 enables `std::backtrace::Backtrace::capture()` in the panic
# hook (lib.rs setup()) so %APPDATA%\com.alond.buildmesh\logs\panic.log gets
# real frames instead of the "disabled backtrace" placeholder. Issue #152.
$env:RUST_BACKTRACE = '1'
$proc = Start-Process $Binary -PassThru
Write-Output "Launched PID: $($proc.Id)"

# 6. Verify via log
Start-Sleep -Seconds 3

# Panic-fast-fail (issue #158): a panic-only crash writes to panic.log /
# panic_early.log but never reaches "started|ready" in buildmesh.log. Same
# rationale as run-dev.ps1 — surface it as a clear exit-1 failure from this
# script's exit code alone, and print the new lines verbatim so a human
# running the script directly sees the panic message + backtrace.
foreach ($p in @(@{Path=$PanicLogPath; BeforeCount=$BeforePanicLines},
                 @{Path=$PanicEarlyLogPath; BeforeCount=$BeforePanicEarlyLines})) {
    $c = (Get-LogLines $p.Path).Count
    if ($c -gt $p.BeforeCount) {
        Write-Output "ERROR: panic detected in $($p.Path) (was $($p.BeforeCount) lines, now $c). Launch aborted."
        Write-Output "----- panic entry -----"
        Get-LogLines $p.Path | Select-Object -Skip $p.BeforeCount | ForEach-Object { Write-Output $_ }
        Write-Output "-----------------------"
        exit 1
    }
}

$AllLines = Get-LogLines $LogPath
if ($AllLines.Count -gt $BeforeLines) {
    $NewLines = $AllLines[$BeforeLines..($AllLines.Count - 1)]
    $started = $NewLines | Where-Object { $_ -match "started|ready" }
    if ($started) {
        Write-Output "OK - Buildmesh running"
        exit 0
    }
}

# Fallback: check process is alive. Querying a process that exited and was
# already reaped throws under $ErrorActionPreference="Stop", which used to
# abort instead of reporting the verdict (issue #2043).
$Alive = $false
try {
    $Alive = -not $proc.HasExited
} catch {
    Write-Host "WARN: could not query the launched process ($($_.Exception.Message))"
}
if ($Alive) {
    Write-Output "OK - Process alive (no log confirmation)"
    exit 0
}

Write-Output "ERROR: Buildmesh failed to start"
exit 1
