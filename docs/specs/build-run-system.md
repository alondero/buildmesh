# Build/Run System — PRD

## Overview

The Build/Run system allows users to execute build and run commands directly from an agent node's worktree context, without leaving the agent context. It provides a configurable per-mesh build/run terminal that streams output in a drawer-style overlay.

**Primary use case:** A developer working in an isolated git worktree (via the `-w` flag on cwrap providers) wants to quickly test changes. They click "Build" or "Run", and a terminal appears below the agent terminal showing the output. The terminal stays open after the process exits so the user can review output.

---

## Design Decisions

### Config Storage

**Decision:** `mesh.toml` at mesh root, TOML format, per-mesh (not per-agent-node).

**Rationale:** All agent nodes under a mesh share the same build/run commands. Explicit config file is deterministic, version-controlled, and portable.

```toml
[build]
command = "cargo build --release"

[run]
command = "./target/release/myapp"
```

### Build Context

**Decision:** Default context is the **worktree** (not project root).

**Rationale:** The primary use case is testing isolated worktree changes. The worktree path is resolved via `git rev-parse --show-toplevel` from within the worktree directory.

### Terminal Visibility

**Decision:** Terminal is **hidden by default**, shown only when the user clicks "Build" or "Run". It **stays open** after the process exits (no auto-hide).

**Rationale:** Users may need to review output after a build completes. Fire-and-forget means no frontend state tracking needed beyond the visibility toggle.

### Terminal Rendering

**Decision:** Drawer-style overlay — the Build/Run terminal slides up from the bottom of the agent node card, overlaying the agent terminal below (~35% of card height).

**Rationale:** Keeps agent context always visible. User can switch focus between agent terminal and build/run terminal by clicking into each.

### Shell Platform

**Decision:** Platform default shell via `portable-pty` — PowerShell on Windows, bash on Mac/WSL.

### Config File Format

**Decision:** TOML (`mesh.toml`) at mesh root.

**Current limitation:** Parsing uses hand-rolled regex extraction. Does not handle TOML edge cases (trailing comments, multi-line values). MVP-acceptable; post-MVP: switch to a proper TOML parser.

---

## Architecture

### Backend (`src-tauri/src/commands/build_run.rs`)

**Commands:**
- `build_run(node_id, mode)` — spawns PTY, runs command from worktree, emits output events
- `get_mesh_config(mesh_id)` — parses and returns mesh.toml contents
- `close_build_run(node_id)` — cleans up PTY process

**Process flow:**
1. Load `mesh.toml` from mesh path (via `agent_nodes.mesh_id` → `meshes.path`)
2. Parse config to extract `build.command` or `run.command`
3. Resolve worktree path via `git worktree list --porcelain`
4. Detect environment (WSL vs Windows) using `env::env_for_path`
5. Spawn shell via `portable-pty` with appropriate working directory
6. Write command to PTY stdin
7. Stream PTY output to frontend via `build-run-output-{node_id}` event
8. Track process in `BuildRunRegistry` (separate from `ProcessRegistry`)

**Error handling:**
- `mesh.toml` missing → returns error string
- `mesh.toml` malformed → returns parse error
- Worktree doesn't exist → returns error suggesting agent hasn't been spawned
- Command not configured → returns error with helpful message

### Frontend (`src/components/BuildRun/`, `src/components/Terminal/BuildRunTerminal.tsx`)

**BuildRunDropdown:**
- Single dropdown button in `GridNodeHeader` (before close button)
- Options: "Build from worktree", "Run from worktree", separator, "Open mesh.toml"
- Calls `onBuildRun(nodeId, mode)` callback on Build/Run selection
- "Open mesh.toml" handled directly in frontend via `window.open()`

**BuildRunTerminal:**
- xterm.js terminal with same theme as `AgentTerminal`
- Subscribes to `build-run-output-{sessionId}` events from backend
- Rendered conditionally — only when `openBuildRun` state is non-null for that node
- Fixed height (~35% of agent node card), slides up over agent terminal
- Thin header bar with title and close button

**State management:**
- `openBuildRun: { nodeId: number; mode: 'build' | 'run' } | null` in `SessionView`
- Lifted to `SessionView` level so state survives React remounts
- Passed to `GridLayout` as `buildRunOpen` / `setBuildRunOpen` props

---

## User Flow

1. User creates a mesh and configures `mesh.toml` at the mesh root
2. User spawns an agent in that mesh (creates a git worktree at `worktrees/agent-{node_id}/`)
3. User makes code changes in the worktree
4. User clicks "Build" dropdown → selects "Build from worktree"
5. Build/Run terminal slides up, showing build output
6. User reviews output in terminal
7. User clicks × to close terminal (or leaves it open)
8. User can re-open by clicking Build/Run again

---

## Post-MVP Items

| Issue | Description |
|-------|-------------|
| [#5](https://github.com/alondero/buildmesh/issues/5) | Event-based output streaming — frontend tracks build state via events, not just visibility |
| [#6](https://github.com/alondero/buildmesh/issues/6) | Visual progress indicator — animated indicator when build is running |
| [#7](https://github.com/alondero/buildmesh/issues/7) | Build from project root — allow building from mesh root context, not just worktree |
| [#8](https://github.com/alondero/buildmesh/issues/8) | Frontend state tracking — Zustand store for build/run terminals (disabled states, restart) |
| [#9](https://github.com/alondero/buildmesh/issues/9) | Auto-create worktree — if worktree doesn't exist when Build is clicked, create it on demand |

---

## Technical Debt (Post-MVP)

1. **Hand-rolled TOML parsing** — `extract_toml_value()` uses string search and will break on TOML edge cases. Switch to `toml` crate.

2. **Duplicate PTY infrastructure** — `build_run.rs` re-implements the entire PTY pipeline instead of reusing `terminal.rs`'s `spawn_pty()`. Refactor to call existing command.

3. **No caching** — `parse_mesh_config()` reads from disk on every call. `resolve_worktree_path()` spawns `git worktree list` on every call. Add module-level caching with invalidation.

4. **BuildRunRegistry vs ProcessRegistry** — build/run processes and agent processes are tracked in separate registries. Could be unified with a discriminated union, but accepted for MVP simplicity.

5. **Path construction in `handleOpenConfig`** — uses `lastIndexOf('/worktrees/')` + `substring` which is fragile on non-Unix paths. Should use proper path utilities.

---

## File Map

| File | Purpose |
|------|---------|
| `src-tauri/src/commands/build_run.rs` | Backend commands |
| `src-tauri/src/commands/mod.rs` | Module registration |
| `src-tauri/src/lib.rs` | Command handler registration |
| `src/components/BuildRun/BuildRunDropdown.tsx` | Dropdown button component |
| `src/components/BuildRun/index.ts` | Barrel export |
| `src/components/Terminal/BuildRunTerminal.tsx` | xterm.js terminal component |
| `src/components/SessionView/SessionView.tsx` | State management + layout |
| `docs/specs/build-run-system.md` | This document |
