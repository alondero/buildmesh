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
    $existing | Stop-Process -Force
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
$BeforeLines = 0
if (Test-Path $LogPath) {
    $BeforeLines = (Get-Content $LogPath).Count
}
# Same delta-capture for the panic-hook outputs (issue #158). Echo the
# counts to stdout so callers can parse them and slice the post-launch file.
$BeforePanicLines = 0
if (Test-Path $PanicLogPath) {
    $BeforePanicLines = (Get-Content $PanicLogPath).Count
}
$BeforePanicEarlyLines = 0
if (Test-Path $PanicEarlyLogPath) {
    $BeforePanicEarlyLines = (Get-Content $PanicEarlyLogPath).Count
}
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
    if (Test-Path $p.Path) {
        $c = (Get-Content $p.Path).Count
        if ($c -gt $p.BeforeCount) {
            Write-Output "ERROR: panic detected in $($p.Path) (was $($p.BeforeCount) lines, now $c). Launch aborted."
            Write-Output "----- panic entry -----"
            Get-Content $p.Path | Select-Object -Skip $p.BeforeCount | ForEach-Object { Write-Output $_ }
            Write-Output "-----------------------"
            exit 1
        }
    }
}

if (Test-Path $LogPath) {
    $AllLines = Get-Content $LogPath
    if ($AllLines.Count -gt $BeforeLines) {
        $NewLines = $AllLines[$BeforeLines..($AllLines.Count - 1)]
        $started = $NewLines | Where-Object { $_ -match "started|ready" }
        if ($started) {
            Write-Output "OK - Buildmesh running"
            exit 0
        }
    }
}

# Fallback: check process is alive
if (-not $proc.HasExited) {
    Write-Output "OK - Process alive (no log confirmation)"
    exit 0
}

Write-Output "ERROR: Buildmesh failed to start"
exit 1
