---
name: use
description: Launch Buildmesh exe and monitor its debug log output in this session
---

# `/use` — Launch Buildmesh with Debug Monitoring

## When to use

The user says "run /use" — activate this skill.

## What to do

### Step 1: Build and launch

Run the deterministic script:

```bash
./scripts/run.sh
```

This handles: kill existing → build → launch raw binary → verify startup. If it exits non-zero, report the error and stop.

### Step 2: Start log monitor

Once the script confirms success, tail the log:

```bash
tail -f "$HOME/Library/Application Support/com.alond.buildmesh/logs/buildmesh.log"
```

Run this as a background task. Filter for: `ERROR`, `error`, `failed`, `Failed`, `WARN`, `warn`.

### Step 3: Confirm and report

Show PID and confirm log monitor is active.

## Log file location

- **macOS:** `~/Library/Application Support/com.alond.buildmesh/logs/buildmesh.log`
- **Windows:** `%APPDATA%\com.alond.buildmesh\logs\buildmesh.log`

## Important

- Monitor keeps running until TaskStop or session end — do NOT stop proactively
- When user is done using the app, stop the monitor with TaskStop
- If you see "Main window found, ready to load content" in logs, the frontend loaded correctly
- **NEVER launch the .app bundle** — it can be stale. The script uses the raw binary.
