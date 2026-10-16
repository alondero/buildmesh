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
# -Detach: return after bounded, log-confirmed readiness, leaving the app alive.
# The readiness timeout starts after the build; compilation remains synchronous.
# -SkipBuild: launch the dev binary already in the target dir without rebuilding
# (what scripts\launch-dev-detached.cmd calls). Everything else still runs: the
# instance identity, the stop of this worktree's previous instance, the
# readiness check and the instance record.
param(
    [int]$CdpPort = 0,
    [switch]$Detach,
    [switch]$TestBridge,
    [switch]$SkipBuild,
    [ValidateRange(1, 300)][int]$ReadinessTimeoutSeconds = 30
)
$ErrorActionPreference = "Stop"

Set-Location "$PSScriptRoot\.."

# Shared launcher helpers (Read-LogFile). Dot-sourced before first use.
. (Join-Path $PSScriptRoot "launcher-common.ps1")

# --- Instance identity -----------------------------------------------------
# Two worktrees can each verify their own build, so "the dev instance" is a
# per-worktree thing, not a per-machine one (issue #2345). A linked worktree
# therefore gets its own app-data profile and its own port offset:
#
#   * Its own profile, because exactly one process may own a profile
#     (src-tauri/src/instance_guard.rs). Sharing the canonical one would make
#     the second launch exit as a duplicate instead of running.
#   * Its own offset, because the ports come from the bundle identifier
#     (`*.dev` = +1000), so two dev builds would both bind 2991-2994.
#
# The main checkout is deliberately unchanged: it keeps the canonical
# `com.alond.buildmesh.dev` profile and the identifier's 1000 offset, so an
# ordinary dev launch behaves exactly as before.
#
# The slug and the profile directory live in launcher-common.ps1, which the log
# helpers share, so a helper reading this worktree's log resolves the same
# directory this launcher will open.

# Ports the app binds at a given offset: the dev test bridge (1991) and the
# HTTP/WebSocket range (1992-1994). These must match src-tauri/src/http/state.rs
# (HTTP_PORT_START/END) and commands/test.rs (TEST_SERVER_PORT).
function Test-PortRangeFree {
    param([Parameter(Mandatory = $true)][int]$Offset)

    foreach ($base in @(1991, 1992, 1993, 1994)) {
        $port = $base + $Offset
        $listener = $null
        try {
            $listener = New-Object System.Net.Sockets.TcpListener(
                [System.Net.IPAddress]::Loopback, $port
            )
            $listener.Start()
        } catch {
            # Taken (or unusable): this range would collide with a live
            # instance, so the caller moves to the next one.
            return $false
        } finally {
            if ($listener) {
                try { $listener.Stop() } catch { }
            }
        }
    }
    return $true
}

# First free range, preferring the one this worktree used last time so its
# ports stay put across restarts. Returns $null when every candidate is busy.
function Select-PortOffset {
    param($Record)

    $candidates = @()
    if ($Record -and $Record.portOffset) { $candidates += [int]$Record.portOffset }
    foreach ($slot in 0..80) { $candidates += 1000 + (100 * $slot) }
    foreach ($candidate in $candidates) {
        if (Test-PortRangeFree $candidate) { return $candidate }
    }
    return $null
}

# The instance this worktree launched last time, or $null. A damaged record is
# treated as no record: it is a cache of a PID, not state anything else reads.
function Read-InstanceRecord {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    try {
        # ReadAllText, not Get-Content: under $ErrorActionPreference="Stop" a
        # bare Get-Content aborts the script on a sharing violation, and this
        # read is best-effort by design.
        return [System.IO.File]::ReadAllText($Path) | ConvertFrom-Json
    } catch {
        Write-Output "Ignoring an unreadable instance record at $Path"
        return $null
    }
}

function Write-InstanceRecord {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][string]$ExpectedBinary,
        $PortOffset,
        [Parameter(Mandatory = $true)][string]$ForProfileDir
    )

    $dir = Split-Path $Path
    if (-not (Test-Path -LiteralPath $dir)) {
        New-Item -ItemType Directory -Path $dir | Out-Null
    }
    $record = [ordered]@{
        pid         = $ProcessId
        binary      = $ExpectedBinary
        profileDir  = $ForProfileDir
        portOffset  = $PortOffset
        launchedAt  = (Get-Date).ToUniversalTime().ToString('o')
    }
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($Path, ($record | ConvertTo-Json), $utf8)
}

# Is this live process the buildmesh-dev *this worktree* launched? Compared by
# executable path, not by name: every worktree builds to
# `<worktree>/src-tauri/target/release-dev/release/buildmesh-dev.exe`, so the
# path is what separates one worktree's instance from another's. A PID alone
# cannot: Windows reuses ids, so a stale record would otherwise stop whatever
# process inherited it.
function Get-OwnedDevProcess {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][string]$ExpectedBinary
    )

    $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if (-not $process) { return $null }
    $path = $null
    try { $path = $process.Path } catch { }
    if (-not $path) { return $null }
    if ([System.IO.Path]::GetFullPath($path) -ne
        [System.IO.Path]::GetFullPath($ExpectedBinary)) { return $null }
    return $process
}

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

$RepoRoot = (Get-Location).Path
$WorktreeSlug = Get-WorktreeSlug -RepoRoot $RepoRoot
# An explicit BUILDMESH_PROFILE_DIR wins: a caller that pinned a profile (or a
# harness fixture) gets exactly that, and this script neither derives nor
# clears it. Otherwise a linked worktree gets a profile of its own.
$ProfileDir = $env:BUILDMESH_PROFILE_DIR
if (-not $ProfileDir) {
    $ProfileDir = Resolve-DevProfileDir -RepoRoot $RepoRoot
}
# The app writes its logs under the profile it actually opened, so the
# readiness and panic checks below have to read that directory — not the
# canonical one — or a worktree's launch would never see its own startup
# lines.
$LogDir = Join-Path $ProfileDir 'logs'
$LogPath = Join-Path $LogDir 'buildmesh.log'
# Panic-hook output files (see src-tauri/src/lib.rs:41-128 + 348-382). Two
# files, two hooks: `panic_early.log` is written by the hook installed in
# `run()` BEFORE Tauri setup, so it captures panics during Tauri-init that
# the main hook can't (it lives in `setup()` and is installed later);
# `panic.log` is the main hook's destination and carries the full backtrace.
# /verify's log-scan tier slices these by pre-launch line count to detect a
# panic-only crash that produces no `ERROR` line in buildmesh.log (issue #158).
$PanicLogPath = Join-Path $LogDir 'panic.log'
$PanicEarlyLogPath = Join-Path $LogDir 'panic_early.log'

$InstanceRecordPath = Join-Path $RepoRoot '.tmp\dev-instance.json'
$InstanceRecord = Read-InstanceRecord $InstanceRecordPath

# 1. Stop the DEV instance this worktree launched, and only that one.
#
#    Process name alone is not an identity: every worktree's dev build is
#    called `buildmesh-dev`, so killing by name took down other sessions'
#    instances and their agents (issue #2345). Two sources of candidates, both
#    confirmed against this worktree's own executable:
#      * the PID recorded by the previous launch, and
#      * any `buildmesh-dev` left behind by a run whose record was lost.
$stopped = @()
$foreign = @()
if ($InstanceRecord -and $InstanceRecord.pid) {
    $recorded = Get-OwnedDevProcess -ProcessId ([int]$InstanceRecord.pid) -ExpectedBinary $Binary
    if ($recorded) {
        $stopped += $recorded
    } elseif (Get-Process -Id ([int]$InstanceRecord.pid) -ErrorAction SilentlyContinue) {
        # Live, but not our binary: a recycled PID. Report it and leave it be.
        $foreign += [int]$InstanceRecord.pid
    }
}
foreach ($candidate in @(Get-Process -Name 'buildmesh-dev' -ErrorAction SilentlyContinue)) {
    if ($stopped.Id -notcontains $candidate.Id) {
        $owned = Get-OwnedDevProcess -ProcessId $candidate.Id -ExpectedBinary $Binary
        if ($owned) { $stopped += $owned }
    }
}
if ($stopped.Count -gt 0) {
    Write-Output "Stopping this worktree's buildmesh-dev (PID $($stopped.Id -join ', '))..."
    # -ErrorAction SilentlyContinue: on a re-run the previous instance can exit
    # between the query and Stop-Process. Under $ErrorActionPreference="Stop"
    # that race aborted the script before it ever launched (issue #2043).
    $stopped | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 1000
}
if ($foreign.Count -gt 0) {
    Write-Output "Left PID $($foreign -join ', ') alone: recorded for this worktree but not running $Binary"
}
# The record is consumed here. Remove it even when nothing matched, so a stale
# PID cannot be mistaken for a live instance on the next run.
Remove-Item -LiteralPath $InstanceRecordPath -Force -ErrorAction SilentlyContinue

# 2. Build the dev profile (frontend + Rust, dev overlay config)
if ($SkipBuild) {
    Write-Output "Skipping the build (-SkipBuild); launching the existing dev binary."
} else {
    Write-Output "Building (dev profile) into $env:CARGO_TARGET_DIR ..."
    npm run tauri:build:dev
    if ($LASTEXITCODE -ne 0) {
        Write-Output "ERROR: Build failed"
        exit 1
    }
}

# 3. Verify binary exists
if (-not (Test-Path $Binary)) {
    Write-Output "ERROR: Build failed - $Binary not found"
    exit 1
}

# 4. Resolve this launch's profile and ports, then create the profile so the
#    log baseline below is real.
#
#    The directory has to exist before the launch: a first run in a worktree
#    has no log file yet, and Read-LogFile reports a missing file as "read
#    with zero lines", which is only true once the app can actually write there.
$null = New-Item -ItemType Directory -Force -Path $LogDir
# $PortOffset is the offset this launch will use, and it is what the instance
# record reports, so a helper can tell which range the instance is on.
# $SetPortOffsetForLaunch is a separate flag: only a value THIS script derives
# may be cleared afterwards. Folding the two together would clear a caller's
# own BUILDMESH_PORT_OFFSET the moment the record needed it.
$PortOffset = $null
$SetPortOffsetForLaunch = $false
if ($env:BUILDMESH_PORT_OFFSET) {
    # An explicit choice by the caller: pass it through untouched and do not
    # probe. Recorded when it is a number so downstream tooling can read the
    # active offset; an unusable value stays null here and the app warns and
    # falls back.
    $pinned = 0
    if ([int]::TryParse($env:BUILDMESH_PORT_OFFSET, [ref]$pinned)) { $PortOffset = $pinned }
    Write-Output "Dev ports: offset $($env:BUILDMESH_PORT_OFFSET) (from BUILDMESH_PORT_OFFSET)"
} elseif ($WorktreeSlug) {
    $PortOffset = Select-PortOffset $InstanceRecord
    if ($null -eq $PortOffset) {
        Write-Output "ERROR: no free dev port range (1991-2994 offsets 1000-9000) for this worktree"
        exit 1
    }
    $SetPortOffsetForLaunch = $true
    Write-Output "Dev ports: offset $PortOffset (test bridge $(1991 + $PortOffset), HTTP $(1992 + $PortOffset)-$(1994 + $PortOffset))"
} else {
    Write-Output "Dev ports: identifier offset 1000 (test bridge 2991, HTTP 2992-2994)"
}
Write-Output "Dev profile: $ProfileDir"

# 5. Record log position
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

# 6. Launch raw binary. The WebView2 loader reads the env var at app start;
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
# Same launch-only scope for the two instance-identity variables: the app reads
# them at startup and nothing else in this shell should. Both are only set when
# this script derived them, so a caller's own values survive the launch (and the
# finally below leaves those alone).
$SetProfileDirForLaunch = -not $env:BUILDMESH_PROFILE_DIR
if ($SetProfileDirForLaunch) { $env:BUILDMESH_PROFILE_DIR = $ProfileDir }
if ($SetPortOffsetForLaunch) { $env:BUILDMESH_PORT_OFFSET = [string]$PortOffset }
# Opt-in development test bridge. The token is generated here and handed to
# the child through the environment. A copy without a trailing newline is
# written for scripts in other processes (ui-shot, the Android live check).
# The value is not printed. Launching without -TestBridge removes a stale file
# so those scripts cannot authenticate against an app that is not listening.
function New-TestBridgeToken {
    $bytes = New-Object byte[] 32
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $rng.GetBytes($bytes)
    } finally {
        $rng.Dispose()
    }
    return ([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant()
}
$TestBridgeTokenPath = Join-Path (Get-Location) '.tmp\test-bridge-token'
if ($TestBridge) {
    $testBridgeToken = New-TestBridgeToken
    $testBridgeDir = Split-Path $TestBridgeTokenPath
    if (-not (Test-Path $testBridgeDir)) {
        New-Item -ItemType Directory -Path $testBridgeDir | Out-Null
    }
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($TestBridgeTokenPath, $testBridgeToken, $utf8)
    $env:BUILDMESH_TEST_BRIDGE = '1'
    $env:BUILDMESH_TEST_BRIDGE_TOKEN = $testBridgeToken
    Write-Output 'Test bridge enabled; credential written to .tmp/test-bridge-token'
} elseif (Test-Path $TestBridgeTokenPath) {
    Remove-Item $TestBridgeTokenPath -Force
}
# Issue #2136: /use, /verify and /verify-ui all launch the dev profile from
# inside an agent's Claude Code session, and that session exports its own
# markers into every process it spawns. Handed on to an agent Buildmesh
# spawns, CLAUDE_CODE_CHILD_SESSION makes Claude Code print "Transcript saving
# is off", write no .jsonl, and park every transcript reader downstream. The
# app scrubs these per spawn (agent::spawn_environment), so this is defence in
# depth for the paths the scrub cannot reach; clearing here makes the dev
# profile behave like a user's own launch. Same launch-only scope as
# RUST_BACKTRACE above. Keep in sync with run-dev.sh.
$ClaudeSessionMarkers = @(
    'CLAUDECODE', 'CLAUDE_CODE_CHILD_SESSION', 'CLAUDE_CODE_SESSION_ID',
    'CLAUDE_CODE_SESSION_ATTENDED', 'CLAUDE_CODE_BRIDGE_SESSION_ID',
    'CLAUDE_CODE_ENTRYPOINT', 'CLAUDE_CODE_EXECPATH',
    'CLAUDE_CODE_MESSAGING_TOKEN', 'CLAUDE_CODE_MESSAGING_SOCKET',
    'CLAUDE_PID', 'CLAUDE_EFFORT'
)
$ClearedMarkers = @($ClaudeSessionMarkers | Where-Object { Test-Path "Env:$_" })
foreach ($name in $ClearedMarkers) {
    Remove-Item "Env:$name" -ErrorAction SilentlyContinue
}
if ($ClearedMarkers.Count -gt 0) {
    Write-Output "Cleared Claude Code session markers for this launch: $($ClearedMarkers -join ', ')"
}
try {
    # ShellExecute starts independently on Windows. RedirectStandard* creates
    # PowerShell stream pumps that can hold a captured shell open until exit.
    $proc = Start-Process $Binary -PassThru
} finally {
    if ($CdpPort -gt 0) {
        Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue
    }
    Remove-Item Env:RUST_BACKTRACE -ErrorAction SilentlyContinue
    Remove-Item Env:BUILDMESH_TEST_BRIDGE -ErrorAction SilentlyContinue
    Remove-Item Env:BUILDMESH_TEST_BRIDGE_TOKEN -ErrorAction SilentlyContinue
    # Only what this script set; a caller's own values belong to the caller.
    if ($SetProfileDirForLaunch) {
        Remove-Item Env:BUILDMESH_PROFILE_DIR -ErrorAction SilentlyContinue
    }
    if ($SetPortOffsetForLaunch) {
        Remove-Item Env:BUILDMESH_PORT_OFFSET -ErrorAction SilentlyContinue
    }
}
Write-Output "Launched PID: $($proc.Id)"
Write-Output "Log: $LogPath"
# Record the instance before waiting on it, so the next run from this worktree
# stops exactly this process. Written early on purpose: a consumer that stops
# reading stdout early can cut the pipe underneath powershell.exe (see the exit
# code note at the top), and the PID must not depend on reaching the end.
Write-InstanceRecord -Path $InstanceRecordPath -ProcessId $proc.Id -ExpectedBinary $Binary `
    -PortOffset $PortOffset -ForProfileDir $ProfileDir

if ($Detach) {
    $ReadinessClock = [System.Diagnostics.Stopwatch]::StartNew()
    while ($ReadinessClock.Elapsed.TotalSeconds -lt $ReadinessTimeoutSeconds) {
        foreach ($p in @(@{Path=$PanicLogPath; Before=$BeforePanic},
                         @{Path=$PanicEarlyLogPath; Before=$BeforePanicEarly})) {
            $growth = Compare-LogGrowth -Path $p.Path -Before $p.Before
            if ($growth.Checked -and $growth.Grew) {
                Write-Output "ERROR: panic detected in $($p.Path). Launch aborted."
                $growth.Lines | Select-Object -Skip $p.Before.Lines.Count | ForEach-Object { Write-Output $_ }
                $proc | Stop-Process -Force -ErrorAction SilentlyContinue
                # The instance this record names no longer exists.
                Remove-Item -LiteralPath $InstanceRecordPath -Force -ErrorAction SilentlyContinue
                exit 1
            }
        }
        if ($proc.HasExited) {
            Write-Output "ERROR: Buildmesh Dev exited before readiness (PID $($proc.Id))"
            Remove-Item -LiteralPath $InstanceRecordPath -Force -ErrorAction SilentlyContinue
            exit 1
        }
        $CurrentLog = Read-LogFile $LogPath
        if ($BeforeLog.Readable -and $CurrentLog.Readable) {
            $SkipLines = $BeforeLog.Lines.Count
            # The rotating writer can replace the log during startup. A shorter
            # log or changed first line belongs to the new generation.
            if ($CurrentLog.Lines.Count -lt $SkipLines -or
                ($SkipLines -gt 0 -and $CurrentLog.Lines.Count -gt 0 -and
                 $CurrentLog.Lines[0] -cne $BeforeLog.Lines[0])) { $SkipLines = 0 }
            $started = $CurrentLog.Lines | Select-Object -Skip $SkipLines | Where-Object { $_ -match 'started|ready' }
            if ($started) {
                Write-Output "OK - Buildmesh Dev running detached (PID $($proc.Id))"
                exit 0
            }
        }
        Start-Sleep -Milliseconds 100
    }
    $proc | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $InstanceRecordPath -Force -ErrorAction SilentlyContinue
    Write-Output "ERROR: Buildmesh Dev readiness timed out after ${ReadinessTimeoutSeconds}s; see $LogPath"
    exit 1
}

# 7. Verify via log
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
Remove-Item -LiteralPath $InstanceRecordPath -Force -ErrorAction SilentlyContinue
exit 1
