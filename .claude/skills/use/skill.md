---
name: use
description: Launch Buildmesh exe and monitor its debug log output in this session
---

# `/use` — Launch Buildmesh with Debug Monitoring

## When to use

The user says "run /use" — activate this skill.

## Prerequisites

**IMPORTANT:** Before launching, you MUST ensure the frontend is bundled into the exe:

1. Check if frontend files exist: `ls X:/src/buildmesh/dist/`
2. If dist is missing or stale, run `cd X:/src/buildmesh && npm run tauri build`
   - This runs BOTH the frontend build (`npm run build`) AND the Rust compile
   - `cargo build --release` alone is NOT sufficient — it doesn't bundle the frontend
   - If only Rust changed (no frontend changes), `npm run tauri build` still needs to run to bundle

## What to do

### Step 1: Check for existing instance
```powershell
$existing = Get-Process -Name 'buildmesh' -ErrorAction SilentlyContinue
if ($existing) {
    Write-Output "Found existing buildmesh process(es):"
    $existing | Format-Table Id, Path, StartTime -AutoSize
    Stop-Process -Id $existing.Id -Force
    Start-Sleep -Milliseconds 500
}
```
Capture the PID(s) of any pre-existing instance. **IMPORTANT:** Check the `Path` column — if it shows `target\debug\buildmesh.exe`, that debug build expects Vite dev server on port 1420 and will show "can't reach this page" if launched standalone. Only `target\release\buildmesh.exe` has bundled frontend and works without dev server.

Stop ALL buildmesh instances before proceeding.

### Step 2: Ensure frontend is bundled
```powershell
# Verify we're about to launch the RELEASE build, not debug
$releaseExe = "X:\src\buildmesh\src-tauri\target\release\buildmesh.exe"
$debugExe = "X:\src\buildmesh\src-tauri\target\debug\buildmesh.exe"

# Stop any debug builds that might be running
Get-Process -Name 'buildmesh' -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "*debug*" } | Stop-Process -Force

# Verify release exe exists and is newer than debug
if (-not (Test-Path $releaseExe)) {
    Write-Output "Release exe not found, running npm run tauri build..."
    cd X:/src/buildmesh && npm run tauri build
} else {
    $releaseInfo = Get-Item $releaseExe
    $debugExists = Test-Path $debugExe
    if ($debugExists) {
        $debugInfo = Get-Item $debugExe
        if ($debugInfo.LastWriteTime -gt $releaseInfo.LastWriteTime) {
            Write-Output "DEBUG build is newer than RELEASE — running npm run tauri build to update release"
            cd X:/src/buildmesh && npm run tauri build
        }
    }
}
```
This bundles the frontend into the release exe. Wait for completion.

### Step 3: Record log state before launch
Before launching, capture the last write time and last line of the existing log:
```powershell
$logPath = "$env:APPDATA\com.alond.buildmesh\logs\buildmesh.log"
$beforeLines = if (Test-Path $logPath) { (Get-Content $logPath -Raw).TrimEnd() -split "`n" } else { @() }
$beforeLastLine = if ($beforeLines.Count -gt 0) { $beforeLines[-1] } else { $null }
$beforeLastTime = if ($beforeLines.Count -gt 0) { [regex]::Match($beforeLastLine, '^(\d{4}-\d{2}-\d{2}T[\d:]+)').Value } else { $null }
Write-Output "Log before: last entry = $beforeLastTime"
```
Store `$beforeLastTime` — you will use this to confirm the NEW instance wrote itself.

### Step 4: Launch app and capture the new PID
```powershell
$proc = Start-Process "X:/src/buildmesh/src-tauri/target/release/buildmesh.exe" -PassThru
Write-Output "Launched PID: $($proc.Id)"
```
**CRITICAL:** The `-PassThru` return value is the ONLY reliable confirmation of launch success.
If `$proc` is `$null` or the Id is 0, the launch failed.

### Step 5: Wait and confirm via log
Wait 2 seconds, then read the log and find the first new entry AFTER `$beforeLastTime`:
```powershell
Start-Sleep -Seconds 2
$logPath = "$env:APPDATA\com.alond.buildmesh\logs\buildmesh.log"
$allLines = if (Test-Path $logPath) { Get-Content $logPath } else { @() }
$newLines = $allLines | Where-Object {
    $line = $_
    $match = [regex]::Match($line, '^(\d{4}-\d{2}-\d{2}T[\d:]+)')
    $match.Success -and ($null -eq $beforeLastTime -or $match.Value -gt $beforeLastTime)
}
if ($newLines) {
    Write-Output "New log entries:"
    $newLines | ForEach-Object { Write-Output $_ }
} else {
    Write-Output "WARNING: No new log entries found after launch"
}
```
The new instance is confirmed when you see "Buildmesh started" in the new entries.

### Step 6: Start log monitor
Use the Monitor tool to tail the log file and stream new lines as conversation messages:

```
command: powershell -Command "Get-Content 'C:/Users/alond/AppData/Roaming/com.alond.buildmesh/logs/buildmesh.log' -Wait -Tail 50"
description: Stream Buildmesh debug log
persistent: true
timeout_ms: 3600000
```

Filter for: `ERROR`, `error`, `failed`, `Failed`, `WARN`, `warn` — plus the startup line.

### Step 7: Confirm and report
Show the confirmed running process info (PID, StartTime) from the captured `$proc` object and confirm the log monitor is active.

## Log file location

`C:/Users/alond/AppData/Roaming/com.alond.buildmesh/logs/buildmesh.log`

## Important

- The log file uses `tracing-appender` rolling (never rotates, single file)
- Monitor keeps running until TaskStop or session end — do NOT stop proactively
- When user is done using the app, stop the monitor with TaskStop
- If you see "Main window found, ready to load content" in logs, the frontend loaded correctly

## Launch Verification Rules

** ALWAYS verify via the log, never via the tool return value.** `Start-Process -PassThru` can return `$null` on success in some contexts. The only reliable confirmation is: (a) the process appears in `Get-Process`, AND (b) a new "Buildmesh started" entry appears in the log after `$beforeLastTime`.

** ALWAYS detect existing instances before launching.** Use `Get-Process` to find any running `buildmesh` processes, stop them cleanly, then launch fresh. This prevents the dual-instance confusion.
