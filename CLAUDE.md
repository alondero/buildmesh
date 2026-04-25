# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Buildmesh is a Tauri desktop application for orchestrating AI agents (Claude Code, Gemini, Open Code) across multiple projects concurrently. Sessions auto-resume on app restart. Supports Windows and WSL environments.

## Build Commands

```bash
# Frontend only (Vite dev server)
npm run dev

# Full Tauri app (dev mode with hot reload)
npm run tauri dev

# Production build
npm run build
```

## Architecture

### High-Level Design

Single-window desktop app with a **sidebar** listing all projects. Each project contains multiple **panes** (sessions). A pane runs an agent harness in a PTY, survives project switching, and continues running in the background. Sidebar shows active pane count per project plus attention indicators when an agent needs input.

```
Project A
  ├─ Pane 1: Claude Code (session_id: abc123)
  └─ Pane 2: Claude Code (session_id: def456)
Project B
  └─ Pane 1: Claude Code (session_id: ghi789)
```

### Project / Pane / Session Relationship

- **Project** = folder on disk (git or not), auto-named from folder name
- **Pane** = UI container that holds one running session
- **Session** = agent harness running in a PTY, identified by an opaque session ID
- Panes persist across project switches — switching views filters to that project's panes but all sessions keep running
- Layout (which panes exist, split sizes, which project owns which pane) is persisted and restored on restart

### MVP Scope

**In MVP:**
- Single provider: Claude Code only (via `cwrap --anthropic`)
- Attention indicator via Stop hook in `.claude/settings.local.json`
- Sessions do NOT auto-resume on app restart — explicit start required

**Deferred (post-MVP):**
- Shell panes (same PTY infrastructure, spawn `cmd.exe`/`bash` instead of agent CLI)
- Additional providers: Gemini, OpenCode, Minimax
- Auto-resume of sessions on restart
- Non-Claude provider attention indicators

### Provider Interface Design

Each provider implements an interface with:
- `spawn(path, session_id?) -> (child_process, session_id)` — start a new session or resume
- `notify_awaiting_input(session_id)` — called by agent to signal attention needed
- `notify_complete(session_id)` — called when agent returns to idle

The app stores only the session ID as an opaque string. Each provider's CLI owns its own resume logic; the app passes `--resume <session_id>` when applicable.

### Agent Attention / Stop Hook Notification Flow

1. User starts a session in a pane — app spawns `cwrap --anthropic` in a PTY
2. Agent runs, user responds, agent eventually reaches a stop point (awaiting input)
3. Agent calls a custom MCP tool configured in `.claude/settings.local.json`:
   ```json
   "stopHook": {
     "command": "node",
     "args": ["notify-attention", "--session-id", "<session_id>"]
   }
   ```
4. The `notify-attention` CLI hits a Tauri event endpoint (`plugin:shell|emit` or a custom endpoint)
5. Frontend receives the event and shows an attention indicator on the pane
6. User clicks pane → continues conversation → indicator clears

### Session Persistence Model

- App stores: project ID, pane layout, agent type, session ID (opaque string)
- Agent stores: its own history, conversation state, resume data
- On restart: pane layout is restored, but sessions show "stopped" state — user manually resumes via the agent's `--resume` flag
- Resume is always initiated by the user or agent startup script, not by the app

### Frontend (`src/`)
- **React 19** + **Vite 7** + **Tailwind 4** for UI
- **Zustand 5** for state management
- **xterm.js** for terminal emulation
- Frontend invokes Tauri commands via `@tauri-apps/api/core` `invoke()`
- Key stores: `sessionStore.ts` (formerly `workspaceStore.ts`), `projectStore.ts`, `settingsStore.ts`

### Rust Backend (`src-tauri/src/`)
- **Tauri 2** with plugins: shell, dialog, opener
- **rusqlite** (bundled SQLite) for persistence
- **portable-pty** for PTY pair creation (agent + shell terminals)
- **git2** for git operations (worktrees, checkpoints)
- **syntect** for syntax highlighting in diffs
- **notify** for file watching

### Module Structure (Rust)
- `lib.rs` / `main.rs` — Tauri app setup, command registration
- `db/mod.rs` — SQLite schema and CRUD operations
- `models/mod.rs` — Rust structs (Project, Session, Checkpoint, etc.)
- `env/mod.rs` — Environment detection (Windows vs WSL)
- `commands/` — Tauri command handlers organized by domain:
  - `agent.rs` — PTY-based agent spawning via `cwrap` (Anthropic/Minimax) and future providers
  - `session.rs` — Session CRUD, pane layout persistence
  - `project.rs` — Project management
  - `checkpoint.rs` — Git ref snapshots
  - `diff.rs` — File diffing with syntect highlighting
  - `terminal.rs` — PTY shell spawning (post-MVP shell panes)
  - `file_watcher.rs` — Directory monitoring
  - `attention.rs` — Stop hook notification endpoint for attention indicators

### Key Data Types
```rust
EnvType: Windows | Wsl
Provider: Anthropic | Minimax | Gemini | OpenCode
SessionStatus: Running | Stopped | AwaitingInput | Error
PaneLayout: Vec<PaneConfig>  // persisted, restored on restart
```

### Provider Trait (Future)
```rust
trait AgentProvider {
    fn spawn(&self, path: &Path, session_id: Option<&str>) -> Result<(Child, SessionId)>;
    fn resume(&self, session_id: &SessionId) -> Result<Child>;
    fn notify_awaiting_input(&self, session_id: &SessionId) -> Result<()>;
}
```

## Database Schema
- **projects** — Folder on disk (git or not), auto-named from folder name
- **sessions** — Running agent tied to a worktree/branch path
- **checkpoints** — Git ref snapshots tied to turn_index (auto-created after each prompt)
- **chat_messages** — Agent conversation history

## Important Cleanup Notes

### `src-ui/` directory — DELETE
The `src-ui/` directory is a divergent, incomplete version of the frontend:
- Lacks entry point files (`main.tsx`, `App.tsx`) — cannot run standalone
- Has different implementations (two-step project selection model)
- Has dead code (`listen` import, unused)
- **Action required:** Delete `src-ui/` and port the provider-dropdown-per-session improvement to `src/`

### Naming transition
- Current: `Workspace` (DB/Store), `workspaceStore.ts`
- Target: `Session`
- `Project` is already correctly named
- The `workspaceStore` will be renamed to `sessionStore` as part of the implementation plan

## Agent Integration

Agents are spawned via PTY using CLI tools on system PATH:
- Windows: `cwrap --anthropic`, `cwrap --minimax`, `gemini`, `opencode`
- WSL: `wsl.exe --cd <path> -- cwrap --anthropic`, etc.
- Output is streamed back via Tauri events (`agent-output`)
- All CLIs support `--resume <session-id>` for re-attach
- CLIs own their own history; app persists only session ID

**Session lifecycle (MVP):** Panes are persisted across project switches. On app restart, pane layout is restored but sessions show a "stopped" state — explicit user action is required to resume via the agent's `--resume` flag.

### Windows Agent Spawning (Critical)
On Windows, `cwrap` is a `.cmd` batch script. `portable-pty`'s `CommandBuilder` uses `CreateProcessW` directly, which **cannot invoke `.cmd` files** — it finds the MSYS2/Git Bash `cmd.exe` first (at `c:\devkitPro\msys2\usr\bin\cmd`) which is not a valid Win32 application (error 193).

**Fix**: For Anthropic/Minimax (cwrap-based providers), spawn via fully-qualified `C:\Windows\System32\cmd.exe`:
```rust
let mut c = CommandBuilder::new("C:\\Windows\\System32\\cmd.exe");
c.arg("/c");
c.arg("cwrap");
c.arg("--anthropic"); // or --minimax
```

This bypasses any PATH-shadowing from MSYS2/Git Bash installations.

### Logging
- Uses `tracing-appender` writing to `%APPDATA%\com.alond.buildmesh\logs\buildmesh.log`
- Enabled in release builds (no stderr/stdout in GUI mode)
- Log level: `info` by default, `debug` for `buildmesh_lib`
