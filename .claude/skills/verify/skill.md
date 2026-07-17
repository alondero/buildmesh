---
name: verify
description: Autonomously verify a Buildmesh code change works — tiered build/lint/test/launch/log-scan loop with hill-climb on failure
---

# `/verify` — Autonomous verification for Buildmesh

## When to use

The user invoked `/verify`, `/verify quick`, `/verify standard`, `/verify full`, or `/verify --escalate`. Also activate when the user says "verify this works", "confirm the change is good", or asks you to run the full check before declaring a task done.

Distinct from `/use`: `/use` is a human-in-the-loop launch-and-watch. `/verify` is autonomous — it runs checks, fixes what fails, and returns a pass/fail report.

Sibling skill: if the change is **visible in the UI**, follow up with `/verify-ui` — it drives the real dev-profile window via Playwright-over-CDP and captures before/after screenshots for the PR.

## Tiers

Default tier is **standard**. Parse the first argument:

| Argument | Tier |
|---|---|
| (none) or `standard` | `standard` |
| `quick` | `quick` |
| `full` | `full` |
| `--escalate` | run `quick` → `standard` → `full`, stop at first failure |

### `quick` (~30–90 s)

Run in order; on any failure, enter hill-climb (see below). All steps must pass to call the tier green.

1. `npm run build` — runs `tsc` then `vite build`. tsc errors out on type issues; vite catches missing imports.
2. `cargo build --manifest-path src-tauri/Cargo.toml` — Rust compile.
3. `npm run test:unit` — Vitest unit suite (118+ tests, mocked Tauri).

### `standard` (~3–4 min) — default

1–3 above, plus:

4. `cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings` — Rust lint with warnings-as-errors.
5. `scripts\check.ps1 rust` — Rust unit tests via the `check.ps1` wrapper (bakes in the worktree workarounds: builds `dist/mobile/`, clears `BUILDMESH_PREFILL`, pins Git-for-Windows first in PATH). Gates regressions like the BOM test `encode_for_powershell_produces_no_bom_utf16le` in `src-tauri/src/agent/spawn_environment.rs`. Fail-fast on non-zero exit.
6. `npm run test:integration` — Vitest integration suite (uses mocked `invoke()`; **does not** require a running Tauri instance, so safe in this tier).

### `full` (~5–8 min)

1–6 above, plus:

6. `npm run tauri:build:dev` — produces the **dev profile** bundle at `src-tauri\target\release-dev\release\buildmesh-dev.exe` (identity `com.alond.buildmesh.dev`, ports 2991/2992). The dev build sets `CARGO_TARGET_DIR=src-tauri\target\release-dev` (via `scripts\run-dev.ps1` on Windows / `scripts\run-dev.sh` elsewhere) so it lands in a separate target dir from the stable hub and never file-locks the hub's `buildmesh.exe`. Cargo nests the profile subdir, hence the `release-dev\release\` two-level path.
7. Launch the app via `scripts\run-dev.ps1`. The script kills any existing `buildmesh-dev` process (**never** the stable hub), verifies the binary, records the pre-launch log line count, starts the binary, and confirms startup.
8. **Strict log scan** of `%APPDATA%\com.alond.buildmesh.dev\logs\buildmesh.log` against the line count captured before launch — see rules below.
9. `npm run test:e2e` — Playwright e2e. Playwright's `webServer` config (`playwright.config.ts`) boots its own `npm run tauri dev`, which uses the **base** identity and ports **1991/1992** (not the dev profile). If a stable hub is running it holds 1991, so pause the hub for this step or run e2e separately. The dev exe launched in steps 6–8 uses 2991/2992 and does not collide with Playwright's dev server.

## Hill-climb protocol

On any failing step:

1. **Capture the failure shape.** First error line; file path and line number if present; error code (`E0382`, `TS2345`, `LNK1107`, etc.); panic message; first failing test name.
2. **Check Known failure modes below first.** If the failure matches a recipe, apply the recipe directly and re-run the same tier from step 1. Don't re-derive what the recipe already encodes.
3. **Otherwise diagnose normally.** Read the failing file, find the cause, edit, re-run the same tier from step 1.
4. **Iteration cap: 5 per tier.** If still failing after 5 fix attempts, **stop** and report to the user with: tier, which step failed, what was tried each iteration, the still-failing output.
5. **On success at the requested tier, stop.** Do not auto-escalate unless `--escalate` was passed.
6. **After any novel failure** (no matching recipe) that you successfully fix, **append a new recipe** to the Known failure modes section below, before declaring done. Format: `**Trigger pattern** → root cause. Fix: <one line>. Memory: <link if applicable>.`

## Diagnostic probes

Beyond `buildmesh.log`, two live probes give you a stronger signal than log lines when the user reports a "the app looks right but nothing is happening" symptom:

### WebSocket terminal-output probe (issue #160)

`buildmesh.log` tells you **spawn worked**. The WebSocket terminal endpoint tells you **xterm.js would have content**. For a regression class like "spawn succeeded but terminal blank," the WebSocket is the only probe that distinguishes "PTY died" from "frontend never received bytes."

**Endpoint:**

```
ws://localhost:1992/ws/terminal/{node_id}?ticket=<ticket>
```

- **Port**: `1992` for the stable hub; **`2992`** for the dev profile (buildmesh-dev, identity `com.alond.buildmesh.dev`). `current_http_port()` returns the actually-bound port (falls back to `1993/1994` / `2993/2994` if the preferred port is held).
- **`node_id`**: the `agent_nodes.id` of the node in question — find it via `invoke('list_agent_nodes')` (or scrape `SELECT id, name FROM agent_nodes` from `%APPDATA%\com.alond.buildmesh.dev\buildmesh.db` via `sqlite3`).
- **Auth (issue #500 AC4 + #551):** a *single-use ticket*, NOT a raw `?token=`. The upgrade handler rejects raw `?token=` with 401 (`http::mod.rs:2427` regression test). The ticket is bound to `{ surface: "terminal", node_id: <id> }` so a terminal ticket can't open `/ws/events` and vice-versa.

**How to mint a ticket (Windows + PowerShell):**

```powershell
$root = '<root token — see How to find the root token below>'
# Or from the DB:  sqlite3 "$env:APPDATA\com.alond.buildmesh.dev\buildmesh.db" \
#   "SELECT value FROM app_settings WHERE key='remote_access_token';"
# Or by reading the title bar / log line: "Generated remote access root token"

$body = @{ surface='terminal'; node_id=123 } | ConvertTo-Json -Compress
$resp = Invoke-RestMethod -Method Post -Uri 'http://localhost:2992/api/ws-ticket' `
         -Headers @{ Authorization = "Bearer $root" } `
         -ContentType 'application/json' -Body $body
$ticket = $resp.ticket

$ws = New-Object System.Net.WebSockets.ClientWebSocket
$cts = New-Object System.Threading.CancellationTokenSource
$ws.ConnectAsync(([Uri]"ws://localhost:2992/ws/terminal/123?ticket=$ticket"), $cts.Token).GetAwaiter().GetResult()

# Read N seconds of frames, count bytes:
$buf = [byte[]]::new(64*1024); $total = 0L
while ($total -lt 1 -and $ws.State -eq 'Open') {
  $seg = New-Object System.ArraySegment[byte] ($buf, 0, $buf.Length)
  $r = $ws.ReceiveAsync($seg, $cts.Token).GetAwaiter().GetResult()
  if ($r.Count -gt 0) { $total += $r.Count; $frame = [System.Text.Encoding]::UTF8.GetString($buf,0,$r.Count); Write-Host $frame -NoNewline }
}
```

**Assertion:** within 5 s of connecting to a *running* node's terminal, expect **≥ 1 byte** received (snapshot + at least one fresh PTY frame). 0 bytes after 5 s ⇒ "PTY never produced output OR PTY channel history is empty."

### Tauri IPC process-state probe

The Rust side has two headless debug commands registered in `lib.rs:447, 451` that answer "does Rust think the agent is alive?" without opening DevTools:

| Tauri command (`invoke('x')`) | Returns | Use to |
|---|---|---|
| `debug_list_agents` | `Vec<AgentDebugState>` — every running node's pid/cmd/start time | confirm `PROCESS_REGISTRY` still has the node |
| `debug_crash_snapshot` | `CrashSnapshot` — recent crash counters + last panic info | triage a "service quit unexpectedly" symptom |

Originally exposed over HTTP as `GET /api/debug/state?token=...` (PR #163); the modern HTTP route table at `http::mod.rs:1641-1646` matches only `/api/{nodes,providers,meshes}` exactly, so use the Tauri IPC commands from a Playwright/CDP harness (or `await window.__TAURI_INTERNALS__.invoke('debug_list_agents')` from the devtools console) until someone re-exposes the HTTP wrapper.

**Acceptance:** when an agent is reported as alive in the UI but the terminal is blank, `debug_list_agents` should still list it. If it doesn't, the spawn process is genuinely gone (look for `Failed to spawn` / exit events in `buildmesh.log`).

## Strict log scan (full tier, step 8)

Three log files come out of `scripts\run-dev.ps1` step 7, all under `$env:APPDATA\com.alond.buildmesh.dev\logs\`:

- `buildmesh.log` — `tracing-appender` output (every `info!`/`warn!`/`error!` and HTTP request line)
- `panic.log` — main panic hook (`src-tauri/src/lib.rs:348-382`): timestamp + thread name + thread id + backtrace
- `panic_early.log` — pre-setup panic hook (`src-tauri/src/lib.rs:41-128`): captures panics during Tauri-init that the main hook can't, because the main hook isn't installed until `setup()` runs

`run-dev.ps1` emits `Buildmesh Dev pre-launch line count (<file>): N` lines on stdout before launching. Capture those three numbers; the delta is the slice to scan.

### `buildmesh.log` slice

Scan new lines for the patterns below.

| Pattern in new lines | Action |
|---|---|
| ` ERROR ` or `panic` or ` panicked at ` | **FAIL** |
| `Illegal invocation` | **FAIL** — Web API receiver binding regression. Memory: `buildmesh-webapi-receiver-binding` |
| `command .* not found` or `command not registered` | **FAIL** — Tauri command not in `lib.rs` invoke handler. Memory: `tauri-command-registration` |
| `Failed to spawn` or `error 193` or `0xc0000142` | **FAIL** — Agent spawn regression. Memory: `buildmesh-agent-spawn-regressions` |
| ` WARN ` (not in benign list below) | **REPORT** in the summary, do not fail |
| Nothing matched, process still alive | **PASS** the log-scan step |

Benign WARN list (seed; extend as you confirm a WARN is non-actionable):
- Tauri startup `IPC scope not set` boilerplate

### `panic.log` + `panic_early.log` slices (issue #158)

A clean panic-only crash writes to one of the panic-hook files but does NOT necessarily produce an `ERROR` line in `buildmesh.log` (the main hook writes to `panic.log` and `eprintln!`s; it never pushes to the tracing pipeline). So a Rust panic at startup currently passes the `buildmesh.log` scan above.

**Rule:** any new line in `panic.log` or `panic_early.log` is an **unconditional FAIL** of the log-scan step.

**Failure summary must include** (so the user can read the panic in the PR):

- The panic entry's first line (timestamp + message + thread name + location for `panic.log`; timestamp + msg + loc for `panic_early.log`).
- The full `Backtrace:` block for `panic.log` entries (everything from `Backtrace:` to the next blank line or EOF — that's the actually-useful part of the dump).
- The originating file (so the user can tell which hook fired).

**Where to read on Windows + PowerShell:**

```powershell
$prePanic = <captured from run-dev.ps1 stdout, e.g. 0>
$preEarly = <captured from run-dev.ps1 stdout>
$panic = "$env:APPDATA\com.alond.buildmesh.dev\logs\panic.log"
$early = "$env:APPDATA\com.alond.buildmesh.dev\logs\panic_early.log"
foreach ($f in @($panic, $early)) {
  if (Test-Path $f) {
    $all = Get-Content $f
    $pre = if ($f -eq $panic) { $prePanic } else { $preEarly }
    if ($all.Count -gt $pre) {
      $newLines = $all[$pre..($all.Count - 1)]
      # Surface the panic entry verbatim
      $newLines | ForEach-Object { Write-Output $_ }
      # then mark this step FAILED
    }
  }
}
```

On macOS/Linux the same delta is `tail -n +$((pre + 1)) "$f"` against the equivalent `XDG_DATA_HOME`/`$HOME/Library/Application Support` path — `scripts\run-dev.sh` prints the same `pre-launch line count` lines.

## Known failure modes

Seed entries — pattern-match the failure against these first before diagnosing. Append new entries here when you fix a novel failure.

- **`error: linking with link.exe failed` / `LNK1107` during `cargo build` or `tauri build`** → A previous instance is still running and holds the output file. Fix: `Stop-Process -Name buildmesh-dev -Force` (dev-profile build) then retry once. A plain `cargo build` writes `buildmesh.exe`, so if the stable hub is running, build the dev profile instead (`npm run tauri:build:dev`) — it writes `buildmesh-dev.exe` and won't conflict.
- **PowerShell agent spawn fails with parse error referencing the prefill text** → BOM in base64 UTF-16LE payload to `-EncodedCommand`. Fix: strip BOM after encoding in `src-tauri/src/main.rs`. Memory: `buildmesh-powershell-encoding-fix`.
- **`Illegal invocation` thrown in WebView2 or silently swallowed by a Tauri listener** → Web API stored as a method (`this.x = requestAnimationFrame`) rebinds receiver. Fix: use arrow wrapper or `.bind(window)`. Memory: `buildmesh-webapi-receiver-binding`.
- **Tauri command works in code but `invoke('cmd_name', ...)` rejects at runtime with "command not found"** → `#[tauri::command]` macro alone isn't enough; the command must be added to the `invoke_handler` `generate_handler!` list in `src-tauri/src/lib.rs`. Memory: `tauri-command-registration`.
- **Terminal panes stack vertically when switching meshes / projects** → `detach()` is not removing `term.element` from its parent before reuse. Fix: explicit `element.parentNode?.removeChild(element)` in detach. Memory: `buildmesh-terminal-container-reuse-bug`.
- **Agent spawn fails with `--model` argument missing** → empty-string `model` (a blank `meshes.model` column, originally surfaced when the value came from a `mesh.toml` `[agent] model = ""` line) is being passed as `--model ""`. Fix: `.is_empty()` guard before adding the arg in the spawn code. Memory: `buildmesh-model-empty-string-bug`.
- **`gh pr create` fails with "No commits between X and X"** → current branch equals base ref, and `--head` was passed. Fix: skip `--head` when `current_branch == base_ref`. Memory: `buildmesh-pr-same-branch-fix`.
- **~20 `git::worktree` / `agent::spawn` / `commands::pr` tests fail with `failed to resolve path '/home/<user>/AppData/...'`** → a non-Git-for-Windows `git.exe` (e.g. devkitPro's MSYS2 git) is first in PATH and writes POSIX-style worktree gitdir paths Windows libgit2 can't resolve. Fix: run via `scripts\check.ps1` (pins `C:\Program Files\Git\cmd` first since 2026-07-09) or prepend it to PATH manually.
- **Playwright e2e times out waiting for `http://localhost:1420`** → a stale `tauri dev` process from a previous run is bound but unhealthy. Fix: `Stop-Process -Name node -Force` (kills Vite) and `Stop-Process -Name buildmesh -Force` (kills the dev exe), then re-run.
- **`vitest run tests/integration` fails with "ECONNREFUSED 127.0.0.1:1991"** → an integration test is hitting the HTTP test bridge instead of the mocked `invoke()`. Likely a test using `invokeViaHttp` from `tests/e2e/utils/tauri-http.ts`. Fix: that test belongs in `tests/e2e/`, not `tests/integration/`; move it or run it inside Playwright.
- **Symptom: user reports "terminal blank after spawn" but `buildmesh.log` shows `Process spawned` / no `Failed to spawn`** → log says spawn worked; only the terminal-output WebSocket can prove the PTY actually piped bytes. Fix: connect to `ws://localhost:{1992|2992}/ws/terminal/{node_id}?ticket=<single-use ticket>` (mint ticket via `POST /api/ws-ticket` with bearer root token + body `{"surface":"terminal","node_id":<id>}`) and assert ≥1 byte received within 5 s — see *Diagnostic probes* below. 0 bytes ⇒ PTY dead; cross-check `await window.__TAURI_INTERNALS__.invoke('debug_list_agents')` to confirm `PROCESS_REGISTRY` still holds the node. Memory: `buildmesh-terminal-blank-probe`.
- **`/verify full` log-scan fails on `panic.log` but `buildmesh.log` looks clean** → the main panic hook (`src-tauri/src/lib.rs:348-382`) writes only to `panic.log` + stderr; it never logs to the tracing pipeline. So a Rust panic during normal operation is invisible to the `buildmesh.log` pattern scan. Fix: surface the full panic entry (timestamp + thread + message + location + `Backtrace:` block) from `panic.log` in the failure summary, then read the stack frames to find the offending Rust file:line — the backtrace points at the *first* frame inside Buildmesh code, not the panic message origin (which is often inside `tracing`/`tauri`/etc.). If the entry is in `panic_early.log` instead, the panic fired BEFORE `setup()` installed the main hook — suspect Tauri config, app-data-dir resolution, or `db::init` failure. Memory: `buildmesh-panic-log-scan`.

## Reporting

When you finish (pass or fail, including iteration-cap stop), produce a structured summary the user can scan in 5 seconds:

```
/verify <tier> → <PASS | FAIL @ step N>

Quick:    ✓ build  ✓ cargo  ✓ unit
Standard: ✓ clippy ✓ cargo test ✓ integration
Full:     ✓ tauri build  ✓ launch  ✗ log scan
          └─ "ERROR rusqlite: UNIQUE constraint failed" at line 1247

Fixes applied this run:
- <one line per fix>

WARN (informational):
- <each warn line, or "none">
```

If the log-scan step failed because of a panic-file entry, append the panic entry under the failure line:

```
Full:     ✓ tauri build  ✓ launch  ✗ log scan (panic.log)
          └─ "[2026-07-17T12:34:56Z] PANIC in thread 'main' (ThreadId(2)):
              called `Result::unwrap()` on an `Err` value at
              src-tauri/src/services/foo.rs:123:45
              Backtrace:
                0: rust_panic
                1: services::foo::bar
                ..."
```

The user reads this to triage without opening `%APPDATA%\com.alond.buildmesh.dev\logs\panic.log` themselves.

## Notes

- Always use `npm run tauri build` for the full bundle — never `cargo build --release` directly. The Tauri CLI handles frontend → backend wiring; bypassing it produces an exe with stale frontend assets. Memory: `feedback_frontend-bundling`.
- This skill takes precedence over the global `/verify` skill because it lives in `.claude/skills/`. The global skill's generic "figure out how to run the app" is replaced here by the Buildmesh-specific pipeline.
- The skill file is meant to grow. When you append a new Known failure mode, keep entries one-line and pattern-first so future pattern-matching stays cheap.
- A panic-only crash is invisible to the `buildmesh.log` pattern scan (issue #158). The log-scan step also tails `panic.log` + `panic_early.log` and treats any new line as a fail. Memory: `buildmesh-panic-log-scan`.
