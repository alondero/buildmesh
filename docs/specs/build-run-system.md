# Build/Run System — PRD

> **Superseded config-storage section.** The original "Decision: `mesh.toml` at mesh root" below is no longer accurate — `build_command`, `run_command`, `model`, `effort`, `worktree_mode`, `default_provider`, and `use_worktree` all live as columns on the `meshes` table (see `src-tauri/src/db/mod.rs` `SCHEMA_VERSION`, and the IPC surface in `src-tauri/src/commands/mesh_config.rs` and `src-tauri/src/commands/build_run.rs:303`). This PRD is kept for the build/run **flow** (drawer, terminal, worktree context), which is still accurate. Treat the two `mesh.toml` Decision lines as historical.
>
> **Per-context commands (v27, issue #802).** Two nullable columns — `root_build_command` and `root_run_command` — let a mesh run *different* commands at the mesh root than in a worktree. A node running at the mesh root (`env::worktree_segment(node).is_none()`) prefers the `root_*` command and falls back to `build_command` / `run_command` when it's unset; Worktree Nodes always use `build_command` / `run_command`. The resolver is `commands::build_run::resolve_build_run_command`. When neither `root_*` column is set, behaviour is identical to PR #801 (same command in both contexts).

## Overview

The Build/Run system allows users to execute build and run commands directly from an agent node's worktree context, without leaving the agent context. It provides a configurable per-mesh build/run terminal that streams output in a drawer-style overlay.

**Primary use case:** A developer working in an isolated git worktree (via the `-w` flag on cwrap providers) wants to quickly test changes. They click "Build" or "Run", and a terminal appears below the agent terminal showing the output. The terminal stays open after the process exits so the user can review output.

---

## Design Decisions

### Config Storage

**Decision (superseded — see banner above):** `mesh.toml` at mesh root, TOML format, per-mesh (not per-agent-node). The current implementation stores `build.command`, `run.command`, and the agent defaults (`model`, `effort`, `worktree_mode`, `default_provider`, `use_worktree`) as columns on the `meshes` SQLite row, surfaced via `get_mesh_properties` / `update_mesh_field` (`src-tauri/src/commands/mesh_config.rs`). Edited from Mesh Properties in the UI; read at build/run time by `commands::build_run::build_run_inner` via `MeshConfig::from(&mesh)`.

**Historical rationale:** All agent nodes under a mesh share the same build/run commands. Explicit config file is deterministic, version-controlled, and portable.

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

**Decision (superseded — see banner above):** TOML (`mesh.toml`) at mesh root.

**Historical note:** Parsing used hand-rolled regex extraction that did not handle TOML edge cases (trailing comments, multi-line values). MVP-acceptable at the time; the `toml` crate was never adopted. The parser is now gone — the storage is the `meshes` table.

---

## Architecture

### Backend (`src-tauri/src/commands/build_run.rs`)

**Commands:**
- `build_run(node_id, mode)` — spawns PTY, runs command from worktree, emits output events
- `get_mesh_config(mesh_id)` — returns `MeshConfig` derived from the `meshes` row (build/run command + agent defaults)
- `close_build_run(node_id)` — cleans up PTY process

**Process flow:**
1. Load the `meshes` row via `db::get_mesh_by_id(mesh_id)` and derive `MeshConfig::from(&mesh)` (build/run commands + agent defaults)
2. Resolve worktree path via `git worktree list --porcelain`
3. Detect environment (WSL vs Windows) using `env::env_for_path`
4. Spawn shell via `portable-pty` with appropriate working directory
5. Write command to PTY stdin
6. Stream PTY output to frontend via `build-run-output-{node_id}` event
7. Track process in `BuildRunRegistry` (separate from `ProcessRegistry`)

**Error handling:**
- Worktree doesn't exist → returns error suggesting agent hasn't been spawned
- Command not configured (i.e. `build_command`/`run_command` is null on the `meshes` row) → returns error with helpful message

### Frontend (`src/components/BuildRun/`, `src/components/Terminal/BuildRunTerminal.tsx`)

**BuildRunDropdown:**
- Single dropdown button in `GridNodeHeader` (before close button)
- Options: "Build from worktree", "Run from worktree"
- Calls `onBuildRun(nodeId, mode)` callback on Build/Run selection

**BuildRunTerminal:**
- xterm.js terminal with same theme as `AgentTerminal`
- Subscribes to `build-run-output-{sessionId}` events from backend
- Rendered conditionally — only when `openBuildRun` state is non-null for that node
- Fixed height (~35% of agent node card), slides up over agent terminal
- Thin header bar with title and close button

**State management:**
- `openBuildRun: { nodeId: number; mode: 'build' | 'run' | 'terminal' } | null` in `AgentNodeView`
- Lifted to `AgentNodeView` level so the visibility flag survives React remounts
  (the previous `SessionView` owner was deleted in issue #380 — this section
  was updated to reflect the actual current architecture)
- Passed to `NodeCard` as `buildRunOpen` / `setBuildRunOpen` props

**Persistence:**
- The build-run terminal survives React remounts (mesh switches, pane
  reorders, probe-panel expand/collapse) via a singleton
  `BuildRunTerminalRegistry` (`src/components/Terminal/BuildRunTerminalRegistry.ts`)
  that mirrors the agent terminal's `TerminalRegistry` pattern. The xterm
  scrollback, the Rust PTY, and the output listener all survive across
  mount/unmount cycles — the React component is a thin DOM host whose effect
  calls `attach` on mount and `detach` on cleanup, with `dispose` (kills PTY +
  disposes xterm) reserved for the explicit X-button close path.
- A `build-run-exited-{node_id}` event from Rust tells the registry when the
  shell process exits naturally (e.g. user typed `exit` while on another
  mesh), so subsequent `writeToBuildRun` calls cleanly hit the "Build run not
  running" path instead of silently vanishing into a dead PTY.

---

## User Flow

1. User creates a mesh and configures the build/run commands (and any agent defaults) via Mesh Properties — written to the `meshes` row.
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

2. ~~**Duplicate PTY infrastructure**~~ — resolved by deletion: the unused `terminal.rs` PTY command family (`spawn_pty` & co.) was removed; `build_run.rs` is now the only generic-PTY pipeline.

3. **No caching** — `parse_mesh_config()` reads from disk on every call. `resolve_worktree_path()` spawns `git worktree list` on every call. Add module-level caching with invalidation.

4. **BuildRunRegistry vs ProcessRegistry** — build/run processes and agent processes are tracked in separate registries. Could be unified with a discriminated union, but accepted for MVP simplicity.

5. **Path construction in `handleOpenConfig`** — uses `lastIndexOf('/worktrees/')` + `substring` which is fragile on non-Unix paths. Should use proper path utilities.

---

## File Map

| File | Purpose |
|------|---------|
| `src-tauri/src/commands/build_run.rs` | Backend commands (includes the `build-run-exited-{node_id}` sentinel emit) |
| `src-tauri/src/commands/mod.rs` | Module registration |
| `src-tauri/src/lib.rs` | Command handler registration |
| `src/components/BuildRun/BuildRunDropdown.tsx` | Dropdown button component |
| `src/components/BuildRun/index.ts` | Barrel export |
| `src/components/Terminal/BuildRunTerminal.tsx` | Thin React wrapper around the registry (DOM host only) |
| `src/components/Terminal/BuildRunTerminalRegistry.ts` | Singleton that owns xterm + PTY + listener, mirrors `TerminalRegistry`'s attach/detach/dispose contract |
| `src/components/AgentNodeView/AgentNodeView.tsx` | State management + layout (owns `openBuildRun`) |
| `tests/unit/build-run-terminal-persistence.test.tsx` | Regression tests pinning the persistence contract |
| `tests/unit/build-run-terminal-raf-batching.test.tsx` | RAF-batching regression (issue #303) |
| `docs/specs/build-run-system.md` | This document |
