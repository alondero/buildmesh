---
name: use
description: Launch Buildmesh exe and monitor its debug log output in this session
---

# `/use` — Launch Buildmesh with Debug Monitoring

## When to use

The user says "run /use" — activate this skill. This is the **human-in-the-loop** launch-and-watch path; for autonomous pass/fail verification of a change, use `/verify` instead.

`/use` runs the **dev profile** (`buildmesh-dev`, identity `com.alond.buildmesh.dev`, ports 2991/2992). It builds and launches a separate binary that runs side-by-side with the stable `buildmesh` hub — so launching it **never** interrupts the agents the hub is orchestrating.

## What to do

### Step 1: Build and launch

Run the deterministic dev-profile launch script for the host platform:

- **Windows (default for this project):** `pwsh -File scripts\run-dev.ps1` (or `powershell.exe -File scripts\run-dev.ps1`)
- **macOS / Linux:** `./scripts/run-dev.sh`

Each script handles: kill existing **buildmesh-dev** (never the stable hub) → build dev profile → launch raw `buildmesh-dev` binary → verify startup. If it exits non-zero, report the error and stop.

### Step 2: Start log monitor

Once the script confirms success, tail the **dev-profile** log for the host platform:

- **Windows:** `Get-Content "$env:APPDATA\com.alond.buildmesh.dev\logs\buildmesh.log" -Wait -Tail 0`
- **macOS:** `tail -f "$HOME/Library/Application Support/com.alond.buildmesh.dev/logs/buildmesh.log"`

Run this as a background task. Filter for: `ERROR`, `error`, `failed`, `Failed`, `WARN`, `warn`.

### Step 3: Confirm and report

Show PID and confirm log monitor is active.

## Log file location

Dev profile (what `/use` launches):
- **Windows:** `%APPDATA%\com.alond.buildmesh.dev\logs\buildmesh.log`
- **macOS:** `~/Library/Application Support/com.alond.buildmesh.dev/logs/buildmesh.log`

The stable hub logs to `com.alond.buildmesh` (no `.dev`) — a different directory.

## Important

- Monitor keeps running until TaskStop or session end — do NOT stop proactively
- When user is done using the app, stop the monitor with TaskStop
- If you see "Main window found, ready to load content" in logs, the frontend loaded correctly
- **NEVER launch the .app bundle** — it can be stale. The script uses the raw binary.
