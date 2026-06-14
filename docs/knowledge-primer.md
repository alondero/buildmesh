# Buildmesh — AI Context

## Tech Stack
- **Frontend:** React 19, Zustand 5, xterm.js 6.x, Tailwind 4, TypeScript ~5.8, Vite 7
- **Backend:** Tauri 2, Rust, portable-pty, rusqlite 0.32, git2, tokio
- **Testing:** Vitest (unit/integration) + Playwright (e2e)

## Project Structure
- `src/` — React frontend (Zustand stores, xterm.js TerminalManager)
- `src-tauri/src/` — Rust backend (commands/, db/, env/, git/, models/). All direct `git2` access lives in `git/` — `primitives` (dirty/ahead-behind/short-sha/head-branch), `worktree` (Worktree Node create/inspect/remove), `sync` (auto-sync), `health` (mesh drift/hostage/recovery); `commands/git.rs` & `prune.rs` are thin `#[command]` adapters over it (ADR 0007). `env/` is path + Windows/WSL conversion only.
- `tests/unit/` — Vitest unit tests
- `tests/integration/` — Vitest integration tests
- `tests/e2e/` — Playwright e2e (requires app running on port 1991)
- `docs/adr/` — Architecture Decision Records

## Key Conventions

### Terminal Persistence (CRITICAL)
`TerminalManager` is a **singleton**. xterm.js instances survive React remounts via a hidden container stack. Never call `dispose()` on a terminal unless the agent node is explicitly deleted — see `src/components/Terminal/Terminal.tsx`. Disposing a terminal causes permanent blanking.

### Layout: Grid-Only
Single layout was removed 2026-04-29. Only `grid` layout (split-panes) is valid. The UI auto-scales 1–6 panes via CSS grid.

### WSL Path Mapping
Linux paths from WSL agents must map to Windows UNC paths (`\\wsl$\...`) before backend file operations. Use `env::to_host_path` in `src-tauri/src/env/mod.rs`. Never pass Linux paths to Windows-side APIs.

### Agent Spawning on Windows
Anthropic and Minimax use `cwrap` spawned via `cmd.exe /c` — **not** direct. Antigravity and OpenCode are spawned **directly** (no cwrap). See `src-tauri/src/commands/agent.rs`.

### Database Pattern
Use `_inner` helper functions that accept `&Connection` to avoid mutex deadlocks. Public functions lock once and pass the connection through. See `src-tauri/src/db/mod.rs`.

### Shared Rust↔TS Types (wire-shape source of truth)
Wire types that cross the Tauri `invoke` boundary **or** the mobile HTTP server are generated from Rust with [`ts-rs`](https://github.com/Aleph-Alpha/ts-rs), not hand-declared in TS. The Rust struct is the single source of truth (issue #359).

- **Producing a type:** add `TS` to the derive list and `#[ts(export, export_to = "Name.ts")]` to the struct/enum (e.g. `models::Mesh`, `models::AgentNode`, the `EnvType`/`Provider`/`SessionStatus` enums, `commands::pr::GitHubIssue`, `commands::git::GitStatus`, `services::session_discovery::DiscoveredSession`).
- **Generation:** `cargo test` (run in `src-tauri/`) runs ts-rs's auto-generated `export_bindings_*` tests, which write `.ts` files to `src/types/generated/`. The dir is set by `TS_RS_EXPORT_DIR` in `src-tauri/.cargo/config.toml`. **Generated files are committed** and must never be hand-edited (they carry a "Do not edit" banner).
- **Consuming a type:** import from `src/types/generated/`. Stores and `src/lib/tauri.ts` / `src/mobile/api.ts` re-export the generated type under the name call sites already use.
- **`i64`/`u64`/`usize` → `#[ts(as = "i32")]`** (and `Option<i64>` → `#[ts(as = "Option<i32>")]`). ts-rs defaults 64-bit ints to `bigint`, but serde_json sends them as JS numbers; the annotation makes the generated type say `number`. Forgetting it produces `bigint`, which fails the TS build — drift caught, not shipped.
- **serde attributes are honoured** (ts-rs `serde-compat`, on by default): `#[serde(rename_all = "snake_case")]` on `SessionStatus` makes the union `"awaiting_input"`, matching the DB and frontend. (A `rename_all = "lowercase"` here silently emitted `"awaitinginput"` — the exact drift class #359 closes.)
- **CI gate:** `.github/workflows/build.yml` runs `cargo test` then `git diff --exit-code src/types/generated`. A Rust struct change that isn't reflected in committed bindings fails the build.
- **Still hand-maintained (migrate later):** `src/lib/status.ts`'s `SessionStatus` (a UI-config copy), and the `Diff*`/`FileNode`/`OpenPr`/`GitBranchStatus` types in `tauri.ts`/`api.ts`. These are not yet generated.

## Anti-Patterns (DO NOT do)
- ❌ Call `dispose()` on an xterm.js Terminal — causes permanent terminal blanking
- ❌ Pass Linux paths (e.g. `/home/user/`) to non-WSL APIs — causes "file not found"
- ❌ Spawn cwrap directly without `cmd.exe /c` on Windows — ConPTY breaks
- ❌ Lock the DB mutex in nested calls — causes deadlocks
- ❌ Hand-declare a TS interface for a Rust wire type, or hand-edit a file in `src/types/generated/` — derive `TS` on the Rust struct and import the generated type instead (issue #359)

## Attention System

### How It Works
Agents signal they need user input via a Claude Code stop hook configured in `.claude/settings.local.json` (written by `inject_attention_hook` in `agent.rs`). The hook fires on Claude Code's built-in `idle_prompt` matcher and calls a curl command that hits the backend's HTTP server:

```
Notification: [{ matcher: "idle_prompt", hooks: [{ type: "command", command: "curl -sf -X POST http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID" }] }]
```

The hook reads `$BUILDMESH_PORT` (set per-agent in `spawn_environment`) at run time rather than baking a literal port, so it routes correctly across the 1992→1994 fallback and to the dev profile's 2992 when an agent is spawned by `buildmesh-dev`.

When the HTTP server receives `POST /api/attention/{session_id}`, it immediately:
1. Inserts the session ID into `ATTENTION_PENDING` (in-memory `HashSet`)
2. Updates the DB status to `AwaitingInput`
3. Emits `attention-needed` Tauri event to the frontend
4. Calls `session_naming::on_turn()` (triggers async LLM rename)

**No timer, polling, or debounce** — the event fires synchronously and immediately.

### `prompts_seen > 1` Guard
The `idle_prompt` matcher is a Claude Code internal. The first `idle_prompt` (on agent spawn) is skipped — only the second and subsequent prompts trigger the hook. This prevents false attention events on startup.

### Auto-Spawn Behavior
`AgentTerminal` component auto-spawns the agent when mounting an agent node with `status === 'idle'` and a `provider`. It uses `fitAddon.proposeDimensions()` to get PTY size before calling `spawn_agent`. This couples terminal mount directly to agent spawn — debugging attention issues requires tracing this path.

## Agent Node Management

### Agent Node ID Capture
The PTY reader thread sniffs `session-id:` or `session_id:` from agent output and auto-saves it to the DB for `--resume` support. Don't replicate this — it's backend-only.

### Turn Counting and Node Naming
`session_naming.rs` captures PTY output and auto-names agent nodes via LLM summarisation (slug-based, e.g. `fix-auth-flow`). Buffering is gated: `on_output` only starts collecting after the first `on_turn` (first idle-prompt webhook) fires, so the Claude Code startup chrome — banner, "Bypass Permissions" warning, plugin/skill listing — is discarded before it can reach the LLM. The rename runs async one turn later, against clean post-startup content.

### Crash Recovery on Startup
`lib.rs:46-54` marks any agent nodes still showing `Running` status as `Suspended` during app startup, since a crash means no live process exists. These are then auto-resumed via `auto_resume_nodes` on the frontend's first draw.

### auto_resume_nodes
On app restart, the frontend calls `auto_resume_nodes` which iterates all `Suspended` agent nodes with a `cli_session_id` and calls `spawn_agent_inner` with `SessionIdMode::Resume`. Only Anthropic and Minimax (cwrap providers) are auto-resumed — Antigravity and OpenCode skip this path and go directly to `Idle`.

### Early-Exit Detection
The PTY reader thread records `spawned_at`. If the reader exits within 3 seconds, the agent node is marked `Error` and a `resume-failed` event is emitted. This catches failed `--resume` attempts where the agent CLI exits because the session has expired.

## Agent Process Architecture

### ProcessRegistry — Runtime State
Agent state lives in a **static** `ProcessRegistry`: `HashMap<i64, Arc<AgentProcess>>` using `once_cell::sync::Lazy`. The DB is **not** the source of truth for running agents — it's only used for `cli_session_id` persistence across restarts.

### AgentProcess Fields
Each entry holds:
- `child` — `Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>`
- `writer` — `Arc<Mutex<Box<dyn std::io::Write + Send>>`
- `master` — `Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>`
- `reader_alive` — `Arc<AtomicBool>` — set to `false` on PTY EOF; used to detect if an agent is still alive
- `job` — `Option<process_util::JobHandle>` — a Windows Job Object containing the agent's whole process tree (see *Killing the process tree* below); `None` on non-Windows or if assignment failed

The PTY handles are behind `Arc<Mutex<...>>` so the PTY reader thread and Tauri command handlers can both access them safely.

### Killing the process tree (Windows)
`kill_session` must kill **everything** the agent spawned, or a survivor pins the worktree's directory (as its CWD or via an open handle) and blocks removal on close. `taskkill /T` alone is insufficient: it walks *live* parent→child links, so it misses any descendant whose parent already exited — e.g. a dev server the agent backgrounded then orphaned. The fix is a **Job Object** (`process_util::JobHandle`): at spawn we assign the PTY shell to a kill-on-close job, so every process it later spawns is *contained* however it detaches. `kill_session` calls `TerminateJobObject` first (reaches detached/orphaned descendants), then keeps `taskkill /T` + `child.kill()` as fallbacks for the rare case job assignment failed. Assign happens immediately after spawn, before the shell launches the agent CLI, so the whole tree is covered. FFI to `kernel32` is declared inline via `extern "system"` (same no-new-deps pattern as `services::usage.rs`).

### Worktree Support (git2-based)
Buildmesh creates a dedicated worktree per agent node **itself**, via `git2` in `git/worktree.rs` (`create_git_worktree` → `add_worktree_impl`) — for **all** providers, not just cwrap. This prevents concurrent agent node conflicts when multiple agent nodes target the same git repository. Two modes: `branched` (default, a real branch per worktree) and `detached` (a throwaway detached HEAD); both are cut from the configured Base Ref (default `origin/main`), resolved via `resolve_base_commit` with a fall-back to local `HEAD` when the ref is unresolvable (#230). See `docs/adr/0003-buildmesh-owns-worktree-creation.md` for why this moved off the agent CLI's old `-w` flag, and `docs/adr/0007-extract-git-module.md` for why the worktree lifecycle now lives in the `git` module.

**Resume:** worktrees are created only when the directory does not already exist (the `if !host_path.exists()` guard in `spawn.rs`), so resume simply re-spawns inside the existing worktree — no re-creation, and none of the old `-w` "already checked out" failures.

**Auto-sync on spawn (issue #213):** before creating a *new* worktree (resume doesn't re-sync), `git::sync::fetch_origin` runs `git fetch <remote>` + `git pull --ff-only --no-rebase` on the parent mesh. The `--no-rebase` is required to defeat a global `pull.rebase=true` config — a rebase on a diverged history would write conflict markers to the working tree, silently mutating the user's local branch on what's supposed to be a read-only step. The sync is best-effort: dirty parents, no-origin repos, and already-up-to-date branches are silent; a fetch failure, diverged history, or unreadable repo surfaces a `mesh-sync-warning` toast (frontend label: `Sync`) and spawn proceeds from local HEAD. See `docs/adr/0001-auto-sync-mesh-on-node-spawn.md` and [[buildmesh-pull-rebase-default]].

**Close/removal (optimistic + deferred):** closing a node is split in two. Phase 1 (`services::agent_node::delete`) kills the process tree (via the Job Object — see *Killing the process tree* above, so a dev server the agent spawned can't keep the directory pinned) and, in one transaction, deletes the `agent_nodes` row *and* enqueues the worktree into `pending_worktree_removals` — fast and authoritative, so the UI drops the node at once. The slow recursive directory delete (`remove_one_worktree`) runs as a background drain (`process_pending_removals`) that dequeues only on success; an app quit mid-cleanup is resumed by the startup reconcile in `lib.rs` `setup()`. Net: "node gone from UI" no longer implies "directory gone" — close is eventually-consistent on disk, and a stuck removal raises a `worktree-cleanup-failed` toast. See `docs/adr/0004-optimistic-node-close-deferred-worktree-removal.md`.

## Logging and Crash Handling

- Logs written to `buildmesh.log` via `tracing-appender` (not console)
- Panic hook writes to `logs/panic.log` with thread name, thread ID, and full backtrace
- `_guard` from `tracing_appender::non_blocking` is leaked with `Box::leak` to live for app lifetime — dropping it would stop logging

## Environment Detection

- `env_for_path` — heuristics: `/mnt/`, `/home/`, `\\wsl$`, or `/` → WSL; everything else → Windows
- `to_host_path` — converts Linux paths to Windows UNC (`\\wsl$\Ubuntu\home\user`) for Windows-side file operations on WSL sessions