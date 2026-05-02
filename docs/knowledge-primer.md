# Buildmesh — AI Context

## Tech Stack
- **Frontend:** React 19, Zustand 5, xterm.js 6.x, Tailwind 4, TypeScript ~5.8, Vite 7
- **Backend:** Tauri 2, Rust, portable-pty, rusqlite 0.32, git2, tokio
- **Testing:** Vitest (unit/integration) + Playwright (e2e)

## Project Structure
- `src/` — React frontend (Zustand stores, xterm.js TerminalManager)
- `src-tauri/src/` — Rust backend (commands/, db/, env/, models/)
- `tests/unit/` — Vitest unit tests
- `tests/integration/` — Vitest integration tests
- `tests/e2e/` — Playwright e2e (requires app running on port 1991)
- `docs/adr/` — Architecture Decision Records

## Key Conventions

### Terminal Persistence (CRITICAL)
`TerminalManager` is a **singleton**. xterm.js instances survive React remounts via a hidden container stack. Never call `dispose()` on a terminal unless the session is explicitly deleted — see `src/components/Terminal/Terminal.tsx`. Disposing a terminal causes permanent blanking.

### Layout: Grid-Only
Single layout was removed 2026-04-29. Only `grid` layout (split-panes) is valid. The UI auto-scales 1–6 panes via CSS grid.

### WSL Path Mapping
Linux paths from WSL agents must map to Windows UNC paths (`\\wsl$\...`) before backend file operations. Use `env::to_host_path` in `src-tauri/src/env/mod.rs`. Never pass Linux paths to Windows-side APIs.

### Agent Spawning on Windows
Anthropic and Minimax use `cwrap` spawned via `cmd.exe /c` — **not** direct. Gemini and OpenCode are spawned **directly** (no cwrap). See `src-tauri/src/commands/agent.rs`.

### Database Pattern
Use `_inner` helper functions that accept `&Connection` to avoid mutex deadlocks. Public functions lock once and pass the connection through. See `src-tauri/src/db/mod.rs`.

## Anti-Patterns (DO NOT do)
- ❌ Call `dispose()` on an xterm.js Terminal — causes permanent terminal blanking
- ❌ Pass Linux paths (e.g. `/home/user/`) to non-WSL APIs — causes "file not found"
- ❌ Spawn cwrap directly without `cmd.exe /c` on Windows — ConPTY breaks
- ❌ Lock the DB mutex in nested calls — causes deadlocks

## Attention System

### TurnDetector — Provider-Specific Prompt Detection
`src-tauri/src/turn_detector.rs` strips ANSI escape codes, then matches provider-specific prompt regexes:

```
Anthropic/Minimax: r"(?m)^\s*❯\s*$"
Gemini:           r"(?m)^\s*>>>\s*$"
OpenCode:         r"(?m)^\s*[>$❯]\s*$"
```

**`prompts_seen > 1` guard:** The first prompt (on agent spawn) is always skipped — it fires before any user work begins. Only the second and subsequent prompts trigger `attention-needed`. This prevents false attention events on session start.

### LAST_INPUT_TIME Suppression
After user sends input (`\n` or `\r`), `LAST_INPUT_TIME` records the timestamp. For 3 seconds afterward, `attention-needed` events are suppressed to avoid duplicate attention triggers from agent output that immediately follows user typing. See `agent.rs:248-251`.

### Auto-Spawn Behavior
`AgentTerminal` component auto-spawns the agent when mounting a session with `status === 'idle'` and a `provider`. It uses `fitAddon.proposeDimensions()` to get PTY size before calling `spawn_agent`. This couples terminal mount directly to agent spawn — debugging attention issues requires tracing this path.

## Session Management

### Session ID Capture
The PTY reader thread sniffs `session-id:` or `session_id:` from agent output and auto-saves it to the DB for `--resume` support. Don't replicate this — it's backend-only.

### Turn Counting and Session Naming
`session_namer.rs` captures PTY output, increments turn counters, and auto-names sessions (e.g., infers name from working context). It also feeds the TurnDetector.

### Crash Recovery on Startup
`lib.rs:46-54` marks any sessions still showing `Running` status as `Suspended` during app startup, since a crash means no live process exists. These are then auto-resumed via `auto_resume_sessions` on the frontend's first draw.

### auto_resume_sessions
On app restart, the frontend calls `auto_resume_sessions` which iterates all `Suspended` sessions with a `cli_session_id` and calls `spawn_agent_inner` with `SessionIdMode::Resume`. Only Anthropic and Minimax (cwrap providers) are auto-resumed — Gemini and OpenCode skip this path and go directly to `Idle`.

### Early-Exit Detection
The PTY reader thread records `spawned_at`. If the reader exits within 3 seconds, the session is marked `Error` and a `resume-failed` event is emitted. This catches failed `--resume` attempts where the agent CLI exits because the session has expired.

## Agent Process Architecture

### ProcessRegistry — Runtime State
Agent state lives in a **static** `ProcessRegistry`: `HashMap<i64, Arc<AgentProcess>>` using `once_cell::sync::Lazy`. The DB is **not** the source of truth for running agents — it's only used for `cli_session_id` persistence across restarts.

### AgentProcess Fields
Each entry holds:
- `child` — `Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>`
- `writer` — `Arc<Mutex<Box<dyn std::io::Write + Send>>`
- `master` — `Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>`
- `reader_alive` — `Arc<AtomicBool>` — set to `false` on PTY EOF; used to detect if an agent is still alive

All fields are behind `Arc<Mutex<...>>` so the PTY reader thread and Tauri command handlers can both access them safely.

### Worktree Support (`-w`)
cwrap providers (Anthropic, Minimax) get the `-w` flag added to their args, which creates a dedicated worktree per session. This prevents concurrent session conflicts when multiple sessions target the same git repository. See `agent.rs:140-155`.

## Logging and Crash Handling

- Logs written to `buildmesh.log` via `tracing-appender` (not console)
- Panic hook writes to `logs/panic.log` with thread name, thread ID, and full backtrace
- `_guard` from `tracing_appender::non_blocking` is leaked with `Box::leak` to live for app lifetime — dropping it would stop logging

## Environment Detection

- `env_for_path` — heuristics: `/mnt/`, `/home/`, `\\wsl$`, or `/` → WSL; everything else → Windows
- `to_host_path` — converts Linux paths to Windows UNC (`\\wsl$\Ubuntu\home\user`) for Windows-side file operations on WSL sessions