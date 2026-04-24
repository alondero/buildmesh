# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Conductor Clone is a Tauri desktop application for orchestrating AI agents (Claude Code, Gemini, Open Code) across multiple projects concurrently. Sessions auto-resume on app restart. Supports Windows and WSL environments.

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

### Frontend (`src/`)
- **React 19** + **Vite 7** + **Tailwind 4** for UI
- **Zustand 5** for state management
- **xterm.js** for terminal emulation
- Frontend invokes Tauri commands via `@tauri-apps/api/core` `invoke()`
- Key stores: `workspaceStore.ts`, `projectStore.ts`, `settingsStore.ts`

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
  - `agent.rs` — PTY-based agent spawning via `cwrap`/`gemini`/`opencode`
  - `workspace.rs` — Session CRUD (note: still named "workspace" in DB, transitioning to "session")
  - `project.rs` — Project management
  - `checkpoint.rs` — Git ref snapshots
  - `diff.rs` — File diffing with syntect highlighting
  - `terminal.rs` — PTY shell spawning
  - `file_watcher.rs` — Directory monitoring

### Key Data Types
```rust
EnvType: Windows | Wsl
Provider: Anthropic | Minimax | Gemini | OpenCode  // cwrap handles Anthropic/Minimax
SessionStatus: Running | Idle | Error | Archived
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
