# Conductor Clone — Product Vision & PRD

## 1. Concept & Vision

**What we are building:** A Windows-native AI agent orchestration hub that brings Conductor's multi-agent visibility and workflow management to a mixed Windows Native + WSL development environment.

**Core purpose:** Enable developers to create, monitor, and coordinate multiple AI coding agents across isolated workspaces — with full visibility into what each agent is doing, real-time progress tracking, and a structured path from task → implementation → review → merge.

**Why it matters:** When running agents in parallel (e.g., attack a backlog of refactoring tickets), you lose visibility. Conductor solves this on Mac. Your Windows setup needs the same — but with the added complexity of agents running in fundamentally different environments (Windows native CLIs, WSL Linux toolchains, mixed project types).

**What makes it compelling:**
- At-a-glance status visibility across all agents and workspaces
- Hybrid runtime awareness (Windows vs WSL vs mixed projects)
- One-click workspace creation from issues, branches, or PRs
- First-class diff review and checkpoint rollback
- Script automation for setup/run/archive cycles

---

## 2. Conductor Feature Analysis

| Feature | What Conductor Does | Windows Adaptation |
|---|---|---|
| **Parallel Agents** | Multiple Claude/Codex instances in isolated workspaces | Support both Windows-native agents (Claude Code Windows, Codex) and WSL-hosted agents (Linux CLIs, bash environments) |
| **Isolated Workspaces** | ⌘+N creates new workspace, each with own git branch, chat history, files | Same — but workspaces can target Windows paths, WSL paths (`\\wsl$\Ubuntu\...`), or mixed |
| **Diff Viewer** | Review agent changes inline, see turn-by-turn modifications | Native diff view; WSL diffs may involve files across the mount boundary |
| **Checkpoints** | Auto-snapshot before each agent response, revert to any point | Same — checkpoint storage works across both filesystems |
| **MCP Support** | Connect to external tools via MCP servers | MCP servers may run on Windows or inside WSL; hybrid connection pooling |
| **Slash Commands** | Reusable prompt macros stored as `.md` files | Same — stored in `~/.conductor/commands/` |
| **Scripts (Setup/Run/Archive)** | Automate `npm install`, `npm run dev`, etc. per workspace | Scripts must detect and target correct environment (Windows `.bat`/`.ps1` vs WSL `bash`) |
| **Run Panel / Terminal** | Integrated terminal to run dev servers, tests | Split view: Windows Terminal + WSL terminal tabs; process lifecycle management for both |
| **PR Workflow** | Issue → Workspace → Develop → Review → PR → Merge → Archive | Same structured workflow |
| **Deep Links** | `conductor://` URL scheme for external triggers | Windows URL scheme registration (`conductor://`) |

---

## 3. User Experience & Workflow

### 3.1 Primary User Activities

**Daily Driver Flow:**
1. Open the app → land on the **Workspaces Dashboard**
2. See all active workspaces with status chips (Running, Idle, Error, Archived)
3. Click a workspace → see the **Session View** with agent chat, file tree, terminal output
4. Watch agent work in real-time (streaming logs, token usage)
5. Review changes in **Diff Viewer**
6. Create PR, merge, archive

**Key Interactions:**
- **⌘+N** (Windows: **Ctrl+Shift+N**) → New workspace picker (from branch, issue, or blank)
- **Ctrl+D** → Open diff viewer on selected workspace
- **Ctrl+Shift+P** → Create PR for workspace
- **Right-click workspace** → Archive, duplicate, move to different project
- **Double-click WSL path** in file tree → Opens Ubuntu terminal at that path

### 3.2 Projects Panel (Primary Navigation)

The left sidebar shows a **Projects** tree — the top-level organizational unit:

```
▼ My Projects
  ▼ Web Dashboard
      ▶ feature-user-auth (WSL) — Running ●
      ▶ bug-fix-login-redirect (Win) — Idle ○
      ▶ refactor-api-client (WSL) — Error ✗
  ▼ Backend Services
      ▶ migrate-to-postgres (WSL) — Running ●
      ▶ add-health-checks (Win) — Archived ⊗
  ▼ Scripts & Tools
      ▶ update-deps (WSL) — Completed ✓
```

Each project groups related workspaces. Projects are just top-level folders in the configured root directory.

### 3.3 Session & Agent Visualization

**Session View** is the heart of the app. It shows:

- **Header**: Workspace name, branch, environment badge (WSL/Ubuntu or Windows/Native), checkpoint count
- **Agent Chat Panel**: Scrollable chat with the agent — shows tool calls, file edits, command executions
- **File Tree**: Live view of files changed by the agent, with git status indicators
- **Terminal Panel**: Real-time stdout/stderr from any running processes (dev server, test watcher, etc.)
- **Checkpoint Rail**: Horizontal strip showing snapshot points; hover to preview diff, click to revert

**Status Indicators:**
- **● Running** — Agent is actively working (pulsing indicator)
- **○ Idle** — Waiting for user input
- **✓ Completed** — Task done, awaiting review
- **✗ Error** — Something failed (click to see error log)
- **⊗ Archived** — Stored but not active

### 3.4 Hybrid Environment Awareness

Since your setup is mixed (Windows Native + WSL), the app must track:

| Dimension | Windows Native | WSL (Ubuntu) |
|---|---|---|
| **Agent runtime** | `claude.bat` via cmd/PowerShell | `claude` via bash |
| **File paths** | `C:\Projects\...` | `/home/user/projects/...` or `\\wsl$\Ubuntu\home\...` |
| **Terminal** | Windows Terminal, `cmd.exe`, PowerShell | Ubuntu shell via `wsl.exe` |
| **Git** | Git for Windows | Git inside WSL |
| **Package manager** | npm/pnpm (via npx or native) | apt, npm/pnpm inside WSL |
| **Scripts** | `.bat` / `.ps1` | `.sh` bash scripts |
| **Dev servers** | `localhost:3000` | `localhost:3000` forwarded via `localhost` |

**The app should automatically detect which environment a workspace's project lives in** (by scanning for `package.json` + `node_modules` patterns, or by path location), but allow manual override.

---

## 4. Feature List

### 4.1 Core Features (MVP)

- [ ] **Projects Dashboard** — Sidebar with project tree, workspace list, status chips
- [ ] **Workspace Manager** — Create, open, archive workspaces; branch tracking
- [ ] **Session View** — Chat panel, file tree, terminal panel, checkpoint rail
- [ ] **Parallel Agent Execution** — Launch multiple agents simultaneously
- [ ] **Real-time Streaming** — Live output from agent tool calls and terminal
- [ ] **Diff Viewer** — Side-by-side diff with syntax highlighting, approve/reject
- [ ] **Checkpoint System** — Auto-snapshot before each agent turn; revert UI
- [ ] **Script Automation** — Setup, Run, Archive scripts per workspace
- [ ] **Hybrid Runtime Detection** — Auto-detect WSL vs Windows environment per workspace
- [ ] **Integrated Terminal** — Split Windows Terminal + WSL terminal tabs
- [ ] **PR Workflow** — Create PR from workspace, merge assistance

### 4.2 Secondary Features

- [ ] **MCP Server Management** — Add/remove/configure MCP servers (Windows and WSL variants)
- [ ] **Slash Commands UI** — Create, edit, invoke slash commands via UI
- [ ] **Deep Links** — `conductor://` URL scheme for external integration
- [ ] **Workspaces Page** — Access archived workspaces, full history
- [ ] **Spotlight Testing** — Quick-run specific tests or files
- [ ] **Multi-provider Support** — Connect to OpenRouter, Bedrock, Vertex, Vercel
- [ ] **Codex Support** — Run OpenAI Codex as an alternative agent
- [ ] **Notification Center** — Alerts when agents complete, fail, or need input
- [ ] **Usage / Token Analytics** — Per-workspace and aggregate token usage
- [ ] **Monorepo Support** — Nested source directories, multi-package workspaces

### 4.3 Nice-to-Have

- [ ] **Shared Scripts via conductor.json** — Commit scripts to repo, share with teammates
- [ ] **VS Code / Cursor Integration Tips** — Guidance panel for IDE搭档 usage
- [ ] **Migration from Cursor** — Import MCP servers and rules from Cursor config
- [ ] **OpenAPI Specs** — Programmatically control Conductor via REST API

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
| **UI Framework** | Tauri (Rust backend + web frontend) | Native Windows performance, small binary, Rust backend is well-suited for process management |
| **Frontend** | React + TypeScript + TailwindCSS | Fast iteration, mature component ecosystem |
| **Backend** | Rust (in Tauri) | Process spawning, file watching, SQLite — all native |
| **State Management** | Zustand (frontend) | Lightweight, minimal boilerplate |
| **Database** | SQLite via `rusqlite` | Local checkpoint/workspace metadata, zero setup |
| **Terminal** | xterm.js + windows pts multiplexing | Cross-platform terminal emulator, WSL `tty` passthrough |
| **Diff Engine** | `differ` + `syntect` | Fast targeted diffs without tree-sitter overhead |
| **File Tree** | `notify` (Rust crate) + real-time watch | Windows-native fs watching; WSL side via `inotifywait` |
| **Agent Protocol** | Claude Code via `cc` wrapper (Anthropic + Minimax) | Single agent type; provider switching via `cc` |
| **MCP** | via child `npx` / `cmd` processes | MCP servers are CLI-based; bridge via process IO |

### 5.3 Data Model

```
Project
  └── Workspace
        ├── metadata (name, branch, env, created_at, status)
        ├── chat_history[] (role, content, timestamp, tool_calls)
        ├── checkpoints[] (id, turn_index, git_ref, timestamp)
        ├── scripts { setup, run, archive }
        └── files_changed[] (path, status: modified/created/deleted)

Settings
  ├── default_projects_root: string
  ├── windows_cli_path: string  // path to claude.bat or cmd
  ├── wsl_cli_path: string      // path to claude in WSL
  ├── mcp_servers[]: { name, command, env_vars }
  └── slash_commands[]: { name, description, content }
```

### 5.4 WSL Interop Strategy

Since WSL and Windows file systems differ, the app needs careful path handling:

- **WSL paths stored as**: `/home/user/projects/...` (Unix-style internally)
- **Display layer**: Convert to `\\wsl$\Ubuntu\home\...` for Windows-native tooling
- **Bridge**: Use `wsl.exe` to spawn commands inside WSL; use named pipes for PTY
- **File watching WSL**: Poll `/mnt/...` mounts from Windows side OR use `inotifywait` from WSL side

**Key WSL commands the app will use:**
```bash
wsl.exe --cd <path> --bash -c "claude"           # spawn agent in WSL
wsl.exe --bash -c "cd /path && npm run dev"      # run scripts in WSL
wsl.exe --bash -c "tail -f /proc/.../fd/1"       # stream terminal output
```

---

## 6. UI Mockup (ASCII)

```
┌─────────────────────────────────────────────────────────────────────────┐
│  Conductor Clone                              [Search... Ctrl+K]  [⚙️]   │
├──────────────────────┬──────────────────────────────────────────────────┤
│ ▼ My Projects        │  ┌─ Session: feature-user-auth ──────────────┐   │
│                      │  │ [WSL: Ubuntu] [Branch: user-auth] [⏸ 3]  │   │
│  ▼ Web Dashboard     │  ├───────────────────────────────────────────┤   │
│      ▶ user-auth    │  │                                           │   │
│      ▶ admin-panel  │  │  🤖 Claude: I'll start by examining the   │   │
│  ▼ Backend          │  │  existing auth middleware and...          │   │
│      ▶ api-server   │  │                                           │   │
│      ▶ worker-jobs  │  │  [TOOL CALL] Read: src/auth/middleware.ts │   │
│                      │  │  [TOOL CALL] Edit: src/auth/routes.ts     │   │
│  [+ New Project]    │  │  [TOOL CALL] Bash: npm run test auth     │   │
│                      │  │                                           │   │
│──────────────────────│  ├───────────────┬─────────────────────────────┤   │
│ Workspaces: 5 active│  │ FILE TREE     │ TERMINAL                   │   │
│                      │  │               │                            │   │
│  ● user-auth (WSL)   │  │ ▶ src/        │ $ npm run dev             │   │
│  ○ admin-panel (WSL) │  │   ▶ auth/     │ ✓ Ready on localhost:3000 │   │
│  ○ api-server (Win)  │  │     routes.ts │                            │   │
│  ✗ worker-jobs (WSL)│  │     middleware│                            │   │
│  ✓ archived-task     │  │   ▶ config/  │                            │   │
│                      │  │               │                            │   │
│                      │  └───────────────┴─────────────────────────────┘   │
│                      │  ├─ Checkpoints: [1]--[2]--[3]--[4]--[5] ────┤   │
│                      │  │ Click any checkpoint to preview/revert      │   │
│                      │  └──────────────────────────────────────────────┘   │
│                      ├──────────────────────────────────────────────────┤
│                      │  [Diff Viewer ⌘D]  [Create PR ⌘⇧P]  [Archive]  │
│                      └──────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 7. Decisions (Resolved)

| Decision | Choice | Rationale |
|---|---|---|
| Runtime detection | Auto-detect WSL vs Windows by path, with manual override | Workspaces are definitively one or the other — simple binary choice |
| Checkpoint storage | Git refs via colocated Git | Environment is definitive per workspace — no cross-Git complexity |
| Agent type | Claude Code only via `cc` wrapper | `cc` already handles Anthropic + Minimax provider switching |
| Provider mid-session switch | Close + respawn `cc --<provider> --resume` | Clean, no protocol complexity |
| Session state | Colocalized with environment | WSL workspaces → `~/.claude` inside WSL. Windows workspaces → `~/.claude` on Windows |
| Window model | Single window, sidebar navigation | Focused single-user workflow; multi-window deferred |
| UI framework | Tauri (Rust + web frontend) | Rust backend excels at process/PTY management; small binary |
| Integrated terminal | MVP requirement | Key UX — users need to see dev server output in context |
| Diff engine | `differ` + `syntect` | Fast, sufficient for targeted code edits |
| PR workflow | GitHub CLI (`gh pr create` / `gh pr merge`) | Likely already installed; covers 90% of workflow |
| MCP | Config viewing MVP only; full lifecycle in Phase 2 | MCP servers are CLI-based; add/remove deferred |
| Slash commands | Clickable chips above chat input in MVP | Low cost, high discoverability |
| Chat history | SQLite via `rusqlite` — app owns the data | Not coupled to Claude Code's session file format |
| Projects panel MVP | Create/list/archive workspaces only; rename/delete deferred | MVP scope discipline |
| File tree MVP | Agent-changed files + real-time `notify` watch | Full project tree deferred to Phase 2 |

## 8. Open Questions (Resolved)

1. **Runtime detection** → Auto-detect by path ✓
2. **Multi-window** → Single window only (MVP) ✓
3. **Git integration** → `gh` CLI for PR workflow ✓
4. **Agent compatibility** → Claude Code only via `cc` ✓
5. **Checkpoint format** → Git refs (colocated) ✓
6. **MCP lifecycle** → View-only in MVP ✓
7. **Offline** → Fully local (no cloud dependency) ✓
8. **Pricing** → Open-source / personal tool (no commercial intent stated) ✓

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

## 10. Phased Rollout Plan

### Phase 1: Core Product (MVP)
- Projects sidebar + Workspace manager
- Session view (chat + file tree + terminal)
- Single agent launch (WSL or Windows)
- Basic diff viewer
- Manual checkpoint creation

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

*Document version: 1.1 — 2026-04-13*
*Status: All open questions resolved — ready for implementation*
