# Builds and launches the DEV profile (buildmesh-dev) so it runs side-by-side
# with the stable hub (buildmesh) without interrupting its agents. This is what
# /use, /verify and /verify-ui call. It only ever touches buildmesh-dev — never
# the hub.
#
# EXIT CODE (issue #2043)
#   0 = launched, verified. 1 = build failed, panic detected, or the app never
#   came up. The exit code reports the launch verdict and nothing else: the log
#   reads below treat a read failure as "no evidence" and fall through to the
#   next source, so an incidental error while the app is already up and healthy
#   can no longer turn a successful launch into exit 1.
#
#   Read the exit code together with stdout, and prefer the `OK - ` line as the
#   success signal. If the CONSUMER stops reading stdout early (`... 2>&1 |
#   Select-Object -First N`, `| head`, a truncated capture buffer) the pipe
#   closes underneath powershell.exe and it returns a non-zero status even when
#   this script reached `exit 0` and printed its OK line. That failure happens
#   in the host at pipeline teardown — it is identical for Write-Output,
#   Write-Host and [Console]::Out, is unaffected by $ErrorActionPreference, and
#   cannot be prevented from in here. Treat a non-zero with an `OK - ` line as
#   success; treat a non-zero WITHOUT one as a real failure.
#
# -CdpPort <n>: expose the WebView2 window over the Chrome DevTools Protocol on
# 127.0.0.1:<n> so Playwright can attach to the REAL app window (drive the DOM,
# take screenshots) — see scripts/ui-shot.mjs and the /verify-ui skill.
# Convention: 9223 for agent UI verification. 0 (default) = off.
param([int]$CdpPort = 0)
$ErrorActionPreference = "Stop"

Set-Location "$PSScriptRoot\.."

# Shared launcher helpers (Read-LogFile). Dot-sourced before first use.
. (Join-Path $PSScriptRoot "launcher-common.ps1")

# The dev profile must NOT share src-tauri\target\release\ with the stable
# hub: cargo's binary output filename is fixed by the crate's [[bin]] name
# ("buildmesh"), so both profiles would write to buildmesh.exe. With the
# stable hub holding that file open, the dev build fails with "Access is
# denied" on every incremental link. Pointing CARGO_TARGET_DIR at a
# separate release-dev/ subdir gives the dev build its own buildmesh-dev.exe
# (via Tauri's mainBinaryName overlay) and keeps the lock contention away
# from the hub.
$env:CARGO_TARGET_DIR = Join-Path (Resolve-Path "src-tauri") "target\release-dev"
# When CARGO_TARGET_DIR is set, cargo nests the profile subdir
# (`<target>/<profile>/<binary>`), so the release build drops the exe at
# release-dev\release\buildmesh-dev.exe — not directly under release-dev.
$Binary = Join-Path $env:CARGO_TARGET_DIR "release\buildmesh-dev.exe"
$LogPath = "$env:APPDATA\com.alond.buildmesh.dev\logs\buildmesh.log"
# Panic-hook output files (see src-tauri/src/lib.rs:41-128 + 348-382). Two
# files, two hooks: `panic_early.log` is written by the hook installed in
# `run()` BEFORE Tauri setup, so it captures panics during Tauri-init that
# the main hook can't (it lives in `setup()` and is installed later);
# `panic.log` is the main hook's destination and carries the full backtrace.
# /verify's log-scan tier slices these by pre-launch line count to detect a
# panic-only crash that produces no `ERROR` line in buildmesh.log (issue #158).
$PanicLogPath = "$env:APPDATA\com.alond.buildmesh.dev\logs\panic.log"
$PanicEarlyLogPath = "$env:APPDATA\com.alond.buildmesh.dev\logs\panic_early.log"

# 1. Kill existing DEV instances only. 'buildmesh-dev' is an exact process-name
#    match, so the stable 'buildmesh' hub is left running.
$existing = Get-Process -Name 'buildmesh-dev' -ErrorAction SilentlyContinue
if ($existing) {
    Write-Output "Stopping existing buildmesh-dev..."
    # -ErrorAction SilentlyContinue: on a re-run the previous instance can exit
    # between Get-Process and Stop-Process. Under $ErrorActionPreference="Stop"
    # that race aborted the script before it ever launched (issue #2043).
    $existing | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 1000
}

# 2. Build the dev profile (frontend + Rust, dev overlay config)
Write-Output "Building (dev profile) into $env:CARGO_TARGET_DIR ..."
npm run tauri:build:dev
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
# log momentarily locked) aborted the script and left powershell.exe reporting 1
# for a launch that was already up and healthy. Read-LogFile treats a failed
# read as "no evidence" and the verification below falls through to the next
# source instead of deciding the verdict.
$BeforeLog = Read-LogFile $LogPath
# Same delta-capture for the panic-hook outputs (issue #158). Echo the counts
# to stdout so /verify can parse them and slice the post-launch file without
# re-deriving from disk state. An unreadable baseline prints "unreadable"
# rather than a misleading 0, which would make every post-launch line look new.
$BeforePanic = Read-LogFile $PanicLogPath
$BeforePanicEarly = Read-LogFile $PanicEarlyLogPath
Write-Output "Buildmesh Dev pre-launch line count (buildmesh.log): $(Format-Count $BeforeLog)"
Write-Output "Buildmesh Dev pre-launch line count (panic.log): $(Format-Count $BeforePanic)"
Write-Output "Buildmesh Dev pre-launch line count (panic_early.log): $(Format-Count $BeforePanicEarly)"

# 5. Launch raw binary. The WebView2 loader reads the env var at app start;
#    it must be set only for the launch (not the build) and cleared after so
#    it never leaks into unrelated WebView2 processes started from this shell.
#    RUST_BACKTRACE=1 has the same scope concern (issue #152): enables
#    std::backtrace::Backtrace::capture() in the panic hook so the dev
#    profile's panic.log gets real frames, not the "disabled backtrace"
#    placeholder. Always on — even when CDP is off, since dev is where you
#    most often see a panic while iterating.
if ($CdpPort -gt 0) {
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort"
    Write-Output "CDP enabled on 127.0.0.1:$CdpPort"
}
$env:RUST_BACKTRACE = '1'
try {
    $proc = Start-Process $Binary -PassThru
} finally {
    if ($CdpPort -gt 0) {
        Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue
    }
    Remove-Item Env:RUST_BACKTRACE -ErrorAction SilentlyContinue
}
Write-Output "Launched PID: $($proc.Id)"

# 6. Verify via log
Start-Sleep -Seconds 3

# Panic-fast-fail (issue #158): a panic-only crash writes to panic.log /
# panic_early.log but never reaches "started|ready" in buildmesh.log, so
# surface that condition before the normal "started|ready" check. Print the
# new lines verbatim so a human running the script directly sees the panic
# message + backtrace, matching the failure-summary shape /verify step 8
# produces (skill.md `### panic.log + panic_early.log slices`).
foreach ($p in @(@{Path=$PanicLogPath; Before=$BeforePanic},
                 @{Path=$PanicEarlyLogPath; Before=$BeforePanicEarly})) {
    $growth = Compare-LogGrowth -Path $p.Path -Before $p.Before
    # Unchecked means a read failed (already warned). Skipping is deliberate:
    # it is neither a panic nor a clean bill of health, and the launch falls
    # through to the startup and process checks below.
    if (-not $growth.Checked) { continue }
    if ($growth.Grew) {
        Write-Output "ERROR: panic detected in $($p.Path) (was $($p.Before.Lines.Count) lines, now $($growth.Lines.Count)). Launch aborted."
        Write-Output "----- panic entry -----"
        $growth.Lines | Select-Object -Skip $p.Before.Lines.Count | ForEach-Object { Write-Output $_ }
        Write-Output "-----------------------"
        exit 1
    }
}

# Startup confirmation rides the same guard: an unreadable baseline or a failed
# current read cannot prove startup, so the launch falls through to the process
# check instead of guessing.
$startup = Compare-LogGrowth -Path $LogPath -Before $BeforeLog
if ($startup.Checked -and $startup.Grew) {
    $NewLines = $startup.Lines[$BeforeLog.Lines.Count..($startup.Lines.Count - 1)]
    $started = $NewLines | Where-Object { $_ -match "started|ready" }
    if ($started) {
        Write-Output "OK - Buildmesh Dev running"
        exit 0
    }
}

# Fallback: check process is alive. A query failure is reported as "not alive"
# so this ends in the explicit failed-to-start verdict below rather than an
# unhandled error whose exit code and message would be misleading.
$Alive = $false
try {
    $Alive = -not $proc.HasExited
} catch {
    Write-Warning "could not query the launched process: $($_.Exception.Message)"
}
if ($Alive) {
    Write-Output "OK - Process alive (no log confirmation)"
    exit 0
}

Write-Output "ERROR: Buildmesh Dev failed to start"
exit 1
