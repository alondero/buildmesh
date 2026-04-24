# Conductor Clone — Product Vision & PRD

## 1. Concept & Vision

**What we are building:** A desktop AI agent orchestration hub for running multiple agents across multiple projects concurrently — with auto-resume, git-aware worktrees, and a terminal-native UX.

**Core purpose:** Enable developers to run, monitor, and coordinate multiple AI coding agents (Claude Code, Gemini, Open Code) across any number of projects and sessions simultaneously. The app is the ephemeral UI layer — agents are durable CLI processes that survive app restarts.

**Why it matters:** Running agents in parallel on a complex project means juggling terminals and losing visibility. Conductor Clone gives you a dashboard where every agent session is visible, resumable, and inspectable — with diff review and file change tracking.

**What makes it compelling:**
- Multi-session dashboard: see all agents across all projects at a glance
- Auto-resume: agents keep running when app closes; restored on reopen via `--resume`
- Worktree-aware: sessions on the same repo use git worktrees for true parallelism
- Terminal-native: the agent CLI runs interactively in an xterm — no chat abstraction
- Auto-checkpoints: git-ref snapshots after each prompt, no manual saves needed

---

## 2. Conductor Feature Analysis

| Feature | Conductor Clone MVP | Notes |
|---|---|---|
| **Parallel Agents** | Multiple sessions across projects, auto-resume on restart | Sessions run as durable CLI processes |
| **Projects** | Folders on disk (git or not), auto-named from folder | Open via native folder picker |
| **Sessions** | Auto-named (3-word generator), tied to worktree or folder | `Session` not `Workspace` |
| **Diff Viewer** | syntect-highlighted side-by-side diffs | MVP |
| **File Watcher/Tree** | Directory tree with changed files highlighted | MVP |
| **Checkpoints** | Auto-snapshot (git ref) after each prompt, no UI | No manual save, no revert UI |
| **Terminal** | Single xterm.js showing agent CLI interactively | No chat abstraction |
| **MCP Support** | CLI-native (handled by agent CLIs) | Not in MVP scope |
| **Slash Commands** | Clickable chips in toolbar | Low-cost MVP approach |
| **PR Workflow** | Deferred | Post-MVP |
| **Deep Links** | Deferred | Post-MVP |

---

## 3. User Experience & Workflow

### 3.1 Primary User Activities

**Daily Driver Flow:**
1. Open the app → see all projects and sessions restored (auto-resume)
2. Click a session → see the terminal running the agent CLI
3. Watch agent work in real-time in the xterm
4. Review changes in **Diff Viewer**
5. Auto-checkpoints captured after each prompt (git ref, no UI needed)
6. Close app → agents keep running → reopen → auto-resumed

**Key Interactions:**
- **Open Project** → Native folder picker, folder name becomes project name
- **+ New Session** → Pick project → auto-generate 3-word name → create worktree → launch agent
- **Provider dropdown** → Per-session agent selector (sidebar, next to session)
- **Stop** → Kill agent (session persists, can be resumed)
- **Ctrl+D** → Open diff viewer on selected session

### 3.2 Projects Panel (Primary Navigation)

The left sidebar shows a **Projects** tree — the top-level organizational unit:

```
▼ Projects
  ▼ conductor-clone
      ● fluffy-rainbow-panda (WSL) — Running
      ● sharp-mountain-river (Win) — Running
      ○ gentle-forest-dawn (Win) — Idle
  ▼ my-webapp
      ● swift-ocean-breeze (WSL) — Running
```

Each project is a folder on disk. Sessions are listed under their project. The provider dropdown appears next to each session (default, or pick Claude Code Anthropic/Minimax, Gemini, Open Code).

**First launch:** Shows "Open a project" empty state. **Subsequent launches:** Restores full state with all projects and sessions, auto-resumes running agents.

### 3.3 Session & Agent Visualization

**Session View** is the heart of the app. It shows:

- **Header**: Session name, branch/worktree, environment badge (WSL or Windows), checkpoint count
- **Terminal**: xterm.js showing the agent CLI running interactively — no chat abstraction
- **File Tree**: Project directory tree with changed files highlighted (git status)
- **Checkpoint Rail**: Present but minimal (MVP: auto-snapshot only, no revert UI needed)

**Status Indicators:**
- **● Running** — Agent is actively working
- **○ Idle** — Waiting for user input
- **✗ Error** — Something failed

### 3.4 Hybrid Environment Awareness

Since the setup is mixed (Windows Native + WSL), the app tracks:

| Dimension | Windows Native | WSL (Ubuntu) |
|---|---|---|
| **Agent runtime** | `cwrap --anthropic` / `cwrap --minimax` via cmd | `cwrap` via bash, launched via `wsl.exe --cd <path>` |
| **File paths** | `C:\Projects\...` | `/home/user/projects/...` or `\\wsl$\Ubuntu\home\...` |
| **Terminal** | xterm.js PTY | xterm.js PTY via `wsl.exe` |
| **Git worktrees** | Git for Windows | Git inside WSL |
| **Dev servers** | `localhost:3000` | `localhost:3000` forwarded |

**Auto-detection:** If project path starts with `/home/` or matches a WSL mount pattern → WSL. Otherwise → Windows. Manual override available.

**Key commands:**
```bash
# Windows agent
cwrap --anthropic
cwrap --minimax

# WSL agent (session "swift-ocean-breeze" on branch "swift-ocean-breeze")
wsl.exe --cd /home/user/projects/my-webapp -- cwrap --anthropic
wsl.exe --cd /home/user/projects/my-webapp -- cwrap --minimax

# Other agents
gemini
opencode
```

---

## 4. Feature List

### 4.1 MVP Features

- [x] **Projects** — Open folder via native picker, auto-named from folder
- [x] **Sessions** — Auto-named (3-word generator), worktree/branch auto-created
- [x] **Multi-provider** — Claude Code (Anthropic/Minimax), Gemini, Open Code
- [x] **Provider hierarchy** — Global default > project default > session override
- [x] **Auto-resume** — Agents restored on app reopen via `--resume <session-id>`
- [ ] **Session View** — Single terminal (xterm.js), no chat abstraction
- [ ] **File Tree** — Directory listing with changed files highlighted
- [ ] **File Watcher** — Live file change tracking via `notify`
- [ ] **Diff Viewer** — syntect-highlighted side-by-side diffs
- [ ] **Auto-checkpoints** — Git ref snapshot after each prompt (no revert UI)
- [ ] **Hybrid Runtime** — Auto-detect WSL vs Windows by path
- [ ] **Status Indicators** — Running, Idle, Error per session
- [ ] **Session Kill** — Stop button to terminate agent

### 4.2 Post-MVP

- [ ] **Conductor name refinement** — Auto-rename session after first prompt
- [ ] **Checkpoint revert UI** — Click to revert to git ref snapshot
- [ ] **MCP Server Management** — Add/remove/configure MCP servers
- [ ] **PR Workflow** — Create PR from session
- [ ] **Slash Commands UI** — Create, edit, invoke via UI
- [ ] **Deep Links** — `conductor://` URL scheme
- [ ] **Notification Center** — Alerts when agents complete or fail
- [ ] **Usage / Token Analytics** — Per-session token usage
- [ ] **Script Automation** — Setup/Run/Archive scripts per project

### 4.3 Deferred / Not Planned

- [ ] **Multi-window** — Single window only (MVP)
- [ ] **Workspaces Page** — Archived sessions (deferred)
- [ ] **Monorepo Support** — Nested source dirs (deferred)
- [ ] **OpenAPI** — Programmatic control (deferred)

---

## 5. Technical Architecture

### 5.1 Runtime Model

```
┌─────────────────────────────────────────────────────────┐
│                    Conductor Clone UI                     │
│         (Electron / Tauri — Windows Native)              │
├─────────────────────────────────────────────────────────┤
│  Workspace Manager                                       │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐       │
│  │  Workspace  │  │  Workspace  │  │  Workspace  │  ...  │
│  │  (WSL Agent)│  │  (Win Agent)│  │  (WSL Agent)│       │
│  └─────────────┘  └─────────────┘  └─────────────┘       │
├─────────────────────────────────────────────────────────┤
│  Process Supervisor (per workspace)                     │
│  └─ spawns: claude (WSL bash) OR claude.bat (cmd)         │
│  └─ manages: setup/run/archive scripts                  │
│  └─ streams: stdout/stderr → UI in real-time             │
├─────────────────────────────────────────────────────────┤
│  Checkpoint Store (SQLite locally)                      │
│  ┌────────────────────────────────────────────────────┐ │
│  │ Per-workspace snapshots; git ref backing            │ │
│  └────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────┤
│  File System Watcher (hybrid)                           │
│  ┌────────────────────────────────────────────────────┐ │
│  │ Windows: FileSystemWatcher                          │ │
│  │ WSL: inotify via /dev/... or polling bridge         │ │
│  └────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
```

### 5.2 Technology Choices

| Layer | Choice | Rationale |
|---|---|---|
| **UI Framework** | Tauri 2 (Rust backend + web frontend) | Native Windows performance, small binary, Rust excels at PTY/process management |
| **Frontend** | React 19 + TypeScript + TailwindCSS 4 | Fast iteration, Zustand for state |
| **Backend** | Rust (in Tauri) | Process spawning, PTY management, SQLite, file watching — all native |
| **State Management** | Zustand 5 (frontend) | Lightweight, minimal boilerplate |
| **Database** | SQLite via `rusqlite` | Local session/project metadata, zero setup |
| **Terminal** | xterm.js + `portable-pty` | Cross-platform PTY for agent + shell |
| **Diff Engine** | `syntect` (syntax highlighting) + `difference-rs` | Targeted diffs with proper highlighting |
| **File Watcher** | `notify` (Rust crate) | Windows-native fs watching |
| **Git Operations** | `git2` Rust crate | Worktree creation, checkpoint refs |
| **Agent CLIs** | `cwrap --anthropic/--minimax`, `gemini`, `opencode` | All on system PATH |

### 5.3 Data Model

```
Project
  ├── id: i64
  ├── name: string        // auto-derived from folder name
  ├── path: string        // absolute path to folder on disk
  ├── default_provider: string  // "anthropic" | "minimax" | "gemini" | "opencode"
  └── created_at: datetime

Session
  ├── id: i64
  ├── project_id: i64
  ├── name: string        // auto-generated (3-word hyphenated)
  ├── branch: string     // worktree branch name
  ├── path: string        // absolute path (worktree or project folder)
  ├── env: string        // "windows" | "wsl"
  ├── provider: string    // "anthropic" | "minimax" | "gemini" | "opencode"
  ├── status: string     // "running" | "idle" | "error"
  ├── session_id: string  // CLI's session ID (for --resume)
  └── created_at: datetime

Checkpoint
  ├── id: i64
  ├── session_id: i64
  ├── git_ref: string
  ├── turn_index: i32
  └── created_at: datetime

AppSettings
  ├── default_provider: string  // global default
  ├── default_projects_root: string
  └── ...

### 5.4 WSL Interop Strategy

Since WSL and Windows file systems differ, the app needs careful path handling:

- **WSL paths stored as**: `/home/user/projects/...` (Unix-style internally)
- **Display layer**: Convert to `\\wsl$\Ubuntu\home\...` for Windows-native tooling
- **Bridge**: Use `wsl.exe` to spawn commands inside WSL; use named pipes for PTY
- **File watching WSL**: Poll `/mnt/...` mounts from Windows side OR use `inotifywait` from WSL side

**Key WSL commands the app will use:**
```bash
# Spawn agent in WSL
wsl.exe --cd /home/user/projects/my-webapp -- cwrap --anthropic
wsl.exe --cd /home/user/projects/my-webapp -- cwrap --minimax

# Resume agent session in WSL
wsl.exe --cd /path/to/worktree -- cwrap --anthropic --resume <session-id>
```

---

## 6. UI Mockup (ASCII)

```
┌─────────────────────────────────────────────────────────────────────────┐
│  Conductor Clone                                              [Settings] │
├──────────────────────┬──────────────────────────────────────────────────┤
│ ▼ Projects          │  ┌─ fluffy-rainbow-panda ────────────────────┐   │
│                      │  │ [WSL] [Branch: fluffy-rainbow-panda]    │   │
│  ▼ conductor-clone  │  │ [Anthropic ▼]        [Stop]  [Diff ⌘D] │   │
│    ● fluffy-... (WSL)                                       │   ├───────────────┬─────────────────────────────┤
│    ● sharp-... (Win)                                       │   │ FILE TREE     │ TERMINAL                   │   │
│    ○ gentle-... (Win)                                      │   │               │                            │   │
│  ▼ my-webapp        │                                       │   │ ▶ src/       │ $ cwrap --anthropic        │   │
│    ● swift-o... (WSL)                                      │   │   ▶ auth/ M │ > ready                    │   │
│                      │  └───────────────────────────────────┘   │     routes.ts│                              │   │
│  [+ Open Project]   │                                              │   └───────────┴─────────────────────────────┘   │
│─────────────────────│                                              │  ├─ Checkpoints: [1]--[2]--[3]--[4] ─────┤   │
│ Sessions: 4 active  │                                              └──────────────────────────────────────────────────┘   │
└─────────────────────┴──────────────────────────────────────────────────┘

Legend:
  M = modified (git status)
  [Anthropic ▼] = provider dropdown per session (per-project default or override)
  ● = running  ○ = idle  ✗ = error
  Sessions auto-resume on app restart
  No chat panel — terminal shows agent CLI directly
```

---

## 7. Decisions (Resolved)

| Decision | Choice | Rationale |
|---|---|---|
| Session naming | Auto-generated 3-word hyphenated names | User doesn't need to think about naming; Conductor refines post-MVP |
| Worktree creation | App handles automatically | Session path derived from project path + worktree name |
| Non-git projects | Session runs directly in project folder | No worktree, no isolation — simplest path |
| Agent providers | 4 options: Claude Code (Anthropic/Minimax), Gemini, Open Code | `cwrap` handles Anthropic/Minimax; others are standalone CLIs |
| Provider hierarchy | Global default > project default > session override | Flexible per-session choice without losing sensible defaults |
| Terminal UX | Single xterm showing agent CLI interactively | No chat abstraction — CLI runs as-is |
| Session persistence | CLI owns history; app persists session ID only | App is UI layer only, agents are durable |
| Auto-resume | Resume all running sessions on app restart | All CLIs support `--resume`; agents keep running when app closes |
| Checkpoints | Auto-snapshot after each prompt (git ref), no UI | No manual saves; revert UI deferred to post-MVP |
| File tree | Directory tree with changed files highlighted (git status) | MVP scope — full tree with status indicators |
| MCP | CLI-native (handled by agent CLIs) | Not in MVP scope |
| PR workflow | Deferred | Post-MVP |
| Window model | Single window, sidebar navigation | Focused single-user workflow |
| UI framework | Tauri 2 (Rust + web frontend) | Rust excels at PTY/process management |

---

## 9. Success Metrics

| Metric | Target |
|---|---|
| Time to first workspace created | < 2 minutes from install |
| Agent streaming latency | < 500ms perceived delay |
| Checkpoint creation time | < 1 second per checkpoint |
| Workspaces supported simultaneously | 10+ without performance degradation |
| WSL ↔ Windows file sync correctness | 100% (no phantom changes) |
| Crash-free session hours | > 99% |

---

## 8. Phased Rollout Plan

### Phase 1: MVP
- Projects: open folder, list all opened projects, native folder picker
- Sessions: auto-named, worktree creation, provider dropdown, kill/resume
- Session view: single xterm showing agent CLI (no chat panel)
- Auto-checkpoint after each prompt (git ref, no UI)
- File tree with changed files highlighted
- Diff viewer
- Auto-resume on app restart
- Hybrid runtime detection (WSL vs Windows by path)

### Phase 2: Post-MVP
- Conductor auto-renames session after first prompt
- Checkpoint revert UI
- MCP server management
- PR workflow (via `gh` CLI)
- Slash commands UI
- Notification center
- Token usage analytics

### Phase 3: Future
- Deep links (`conductor://`)
- Multi-window
- Script automation (setup/run/archive)
- OpenAPI for programmatic control

### Phase 2: Parallel Execution
- Multiple simultaneous agents
- Real-time status dashboard
- Auto-checkpoints (before each agent turn)
- Revert to checkpoint UI
- Script automation (setup/run/archive)

### Phase 3: Full Workflow
- PR creation via GitHub CLI
- MCP server management
- Slash commands UI
- Deep links (`conductor://`)
- Hybrid path handling polish

### Phase 4: Polish & Scale
- Multi-provider support (OpenRouter, Bedrock)
- Token usage analytics
- Shared `conductor.json` scripts
- VS Code/Cursor integration tips
- OpenAPI for programmatic control

---

*Document version: 1.2 — 2026-04-23*
*Status: MVP scope defined; implementation plan pending*
