# Builds and launches the DEV profile (buildmesh-dev) so it runs side-by-side
# with the stable hub (buildmesh) without interrupting its agents. This is what
# /use, /verify and /verify-ui call. It only ever touches buildmesh-dev — never
# the hub.
#
# -CdpPort <n>: expose the WebView2 window over the Chrome DevTools Protocol on
# 127.0.0.1:<n> so Playwright can attach to the REAL app window (drive the DOM,
# take screenshots) — see scripts/ui-shot.mjs and the /verify-ui skill.
# Convention: 9223 for agent UI verification. 0 (default) = off.
param([int]$CdpPort = 0)
$ErrorActionPreference = "Stop"

Set-Location "$PSScriptRoot\.."

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

# 1. Kill existing DEV instances only. 'buildmesh-dev' is an exact process-name
#    match, so the stable 'buildmesh' hub is left running.
$existing = Get-Process -Name 'buildmesh-dev' -ErrorAction SilentlyContinue
if ($existing) {
    Write-Output "Stopping existing buildmesh-dev..."
    $existing | Stop-Process -Force
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
$BeforeLines = 0
if (Test-Path $LogPath) {
    $BeforeLines = (Get-Content $LogPath).Count
}

# 5. Launch raw binary. The WebView2 loader reads the env var at app start;
#    it must be set only for the launch (not the build) and cleared after so
#    it never leaks into unrelated WebView2 processes started from this shell.
if ($CdpPort -gt 0) {
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort"
    Write-Output "CDP enabled on 127.0.0.1:$CdpPort"
}
try {
    $proc = Start-Process $Binary -PassThru
} finally {
    if ($CdpPort -gt 0) {
        Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue
    }
}
Write-Output "Launched PID: $($proc.Id)"

# 6. Verify via log
Start-Sleep -Seconds 3
if (Test-Path $LogPath) {
    $AllLines = Get-Content $LogPath
    if ($AllLines.Count -gt $BeforeLines) {
        $NewLines = $AllLines[$BeforeLines..($AllLines.Count - 1)]
        $started = $NewLines | Where-Object { $_ -match "started|ready" }
        if ($started) {
            Write-Output "OK - Buildmesh Dev running"
            exit 0
        }
    }
}

# Fallback: check process is alive
if (-not $proc.HasExited) {
    Write-Output "OK - Process alive (no log confirmation)"
    exit 0
}

Write-Output "ERROR: Buildmesh Dev failed to start"
exit 1
