---
name: use
description: Launch Buildmesh exe and monitor its debug log output in this session
---

# `/use` — Launch Buildmesh with Debug Monitoring

## When to use

The user says "run /use" — activate this skill.

## What to do

### Step 1: Stop any running instance
```powershell
Stop-Process -Name "buildmesh" -Force -ErrorAction SilentlyContinue
```

### Step 2: Launch app
```powershell
Start-Process "X:/src/buildmesh/src-tauri/target/release/buildmesh.exe" -PassThru
```

### Step 3: Wait for log file to appear
The app writes logs to `%APPDATA%\com.alond.buildmesh\logs\buildmesh.log` via tracing-appender.

Wait 2 seconds then check:
```powershell
Get-ChildItem "$env:APPDATA\com.alond.buildmesh\logs" | Select-Object Name, Length
```

### Step 4: Start Monitor to stream log entries
Use the Monitor tool to tail the log file and stream new lines as conversation messages:

```
command: powershell -Command "Get-Content 'C:/Users/alond/AppData/Roaming/com.alond.buildmesh/logs/buildmesh.log' -Wait -Tail 50"
description: Stream Buildmesh debug log
persistent: true
timeout_ms: 3600000
```

Filter for: `ERROR`, `error`, `failed`, `Failed`, `WARN`, `warn` — plus the startup line.

### Step 5: Confirm and report
Show the running process info and confirm the log monitor is active.

## Log file location

`C:/Users/alond/AppData/Roaming/com.alond.buildmesh/logs/buildmesh.log`

## Important

- The log file uses `tracing-appender` rolling (never rotates, single file)
- Monitor keeps running until TaskStop or session end — do NOT stop proactively
- When user is done using the app, stop the monitor with TaskStop
