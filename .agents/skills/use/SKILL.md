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

Each script handles: kill existing **buildmesh-dev** (never the stable hub) → build dev profile → launch raw `buildmesh-dev` binary → verify startup. Check that an existing dev instance belongs to this task before replacing it; another session may be using it. If the script exits non-zero, report the error and stop.

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

## Live diagnostic probes (issue #160)

When the log monitor shows a clean launch but the user reports "spawn succeeded but the terminal is blank" — or any "app looks fine, nothing is happening" symptom — `buildmesh.log` only tells you **spawn worked**. The authoritative probe is the **terminal-output WebSocket**, which replays the snapshot + streams live PTY bytes:

```
ws://localhost:1992/ws/terminal/{node_id}?ticket=<ticket>     # stable hub
ws://localhost:2992/ws/terminal/{node_id}?ticket=<ticket>     # dev profile (buildmesh-dev)
```

The HTTP server's actually-bound port (`current_http_port()`) may be `+1/+2` higher when the preferred slot is held — check `buildmesh.log` for `bound http on :1992` / `:2992`.

**Auth is a single-use ticket**, not a raw `?token=` — the upgrade rejects raw tokens with 401 (issue #500 AC4, regression test at `http/mod.rs:2427`). Mint one:

```powershell
# Root token sources (pick one):
#   1. Open the desktop Remote Access modal (it calls invoke('get_root_token')).
#   2. sqlite3 "$env:APPDATA\com.alond.buildmesh.dev\buildmesh.db" \
#        "SELECT value FROM app_settings WHERE key='remote_access_token';"
#   3. Read the log line "Generated remote access root token" on first launch.
$root   = '<root token>'
$body   = @{ surface='terminal'; node_id=<node_id> } | ConvertTo-Json -Compress
$ticket = (Invoke-RestMethod -Method Post -Uri 'http://localhost:2992/api/ws-ticket' `
           -Headers @{ Authorization = "Bearer $root" } `
           -ContentType 'application/json' -Body $body).ticket

# Connect and read 5s of frames; assert >= 1 byte received on a running node.
$ws = [System.Net.WebSockets.ClientWebSocket]::new()
$cts = [Threading.CancellationTokenSource]::new(5000)
# Pass $cts.Token to ConnectAsync AND ReceiveAsync so a silent socket cannot hang.
$ws.ConnectAsync([Uri]"ws://localhost:2992/ws/terminal/<node_id>?ticket=$ticket",
                 $cts.Token).GetAwaiter().GetResult()
# ... receive frames using $cts.Token; dispose the socket and CTS in finally.
# A timeout means no bytes were observed; it does not prove which layer failed.
```

For "Rust thinks the agent is alive?", use the headless IPC commands registered in `lib.rs` from devtools or a Playwright/CDP harness. Check the current route table before relying on a historical HTTP debug wrapper:

```js
await window.__TAURI_INTERNALS__.invoke('debug_list_agents');     // [] ⇒ PROCESS_REGISTRY empty
await window.__TAURI_INTERNALS__.invoke('debug_crash_snapshot');   // recent panics + counters
```

These four signals (log + WebSocket + the two IPC probes) are the primary authoritative probes for "is the agent actually working?"; tailing `buildmesh.log` alone covers a narrow slice.

## Important

- Monitor keeps running until TaskStop or session end — do NOT stop proactively
- When user is done using the app, stop the monitor with TaskStop
- "Main window found, ready to load content" proves a window was found; assert rendered content separately before claiming frontend success.
- **NEVER launch the .app bundle** — it can be stale. The script uses the raw binary.
