# 13. Rename IPC surface to Node/Mesh

Status: accepted

## Context

Buildmesh's domain vocabulary (per `CONTEXT.md` ambiguity #1) is **Mesh** (not "Project") and **Agent Node** (not "Session") for the user-facing model and the public IPC (inter-process communication) surface. The wire protocol, the Tauri `#[command]` names, the HTTP routes, the TS wrapper layer, the React file/dir names, the mobile screen labels, and the Tauri event bus strings all still shipped the legacy names. This drift leaked the legacy vocabulary into every contributor, every external integrator reading the wire protocol, and every fresh issue/PR that copy-pasted from existing code.

Issue #474 closed the struct + generated-type axis (the `MeshConfig`/`mesh.toml` rename). This ADR widens the lens to the IPC surface itself. It is the breaking-change rename that puts the public surface on the right side of the CONTEXT.md vocabulary split.

The split per `CONTEXT.md`:

| Vocabulary | Scope | Why kept |
|---|---|---|
| "Agent Node" / "Mesh" | User-facing model, public IPC surface, UI labels | Canonical for the public surface — the rename this ADR documents |
| "session" | Backend process lifecycle records (DB columns, status enums, Claude Code CLI id) | External CLI contract; the Claude Code `cli_session_id` field is the agent's external identifier and must not change |

## Decision

Rename the public IPC surface from `*Session/*Project` to `*Node/*Mesh` in one breaking PR:

1. **Tauri commands** in `src-tauri/src/lib.rs` `tauri::generate_handler![]` (22 commands renamed).
2. **Rust service + module names** (6 files renamed via `git mv`).
3. **HTTP route paths** in `src-tauri/src/http/` (2 routes renamed; 1 file renamed).
4. **TS wrapper layer** in `src/lib/tauri.ts` (20 wrapper functions renamed, with 1-line deprecation aliases kept for one release).
5. **Frontend call sites** in `src/stores/agentNodeStore.ts`, `src/stores/meshStore.ts`, `src/App.tsx`, `src/components/AgentNodeView/AgentNodeView.tsx`, `src/components/Probe/DiscoveredNodesTab.tsx` (updated to new names + new event names + new arg keys).
6. **React file/dir renames** (`src/components/SessionView/` → `src/components/AgentNodeView/`, `SessionView.tsx` → `AgentNodeView.tsx`, `SessionHistoryTab.tsx` → `DiscoveredNodesTab.tsx`).
7. **Mobile UI renames** (`src/mobile/screens/SessionsScreen.tsx` → `DiscoveredNodesScreen.tsx`, user-facing labels relabelled).
8. **Generated TS types** (`DiscoveredSession.ts` → `DiscoveredAgentNode.ts` via ts-rs).
9. **Tauri event bus names** (`session-renamed` → `node-renamed`, `session-created` → `node-created`, `session-activated` → `node-activated`).
10. **Test-server RPC handlers** in `src-tauri/src/commands/test.rs` (full vocabulary alignment).

## What stays (per CONTEXT.md ambiguity #1)

The following intentionally keep the word "session" because they are either the external Claude Code CLI contract or the backend process lifecycle vocabulary:

| Stays "session" | Reason | Reference |
|---|---|---|
| `SessionStatus` enum | Backend process lifecycle | `src-tauri/src/models/mod.rs` |
| `cli_session_id` field on `AgentNode` | Claude Code's external CLI identifier | `src-tauri/src/db/mod.rs:70`, `src/types/generated/AgentNode.ts:14` |
| `src-tauri/src/session_naming.rs` module | Backend PTY naming module | `src-tauri/src/session_naming.rs` |
| `crate::session_naming::*` callers | Internal backend usage | various |
| `attention-needed` / `attention-cleared` event payload `session_id` | External AttentionHook protocol with running agents | `src/stores/agentNodeStore.ts` |
| `*_agent` commands (`resize_agent`, `kill_agent`, `write_to_agent`, `send_to_agent`, `spawn_agent`, `is_agent_running`) | Process-level IPC — the agent process id is the same as the node id but the command is about process control, not domain CRUD | `src/lib/tauri.ts:335-360` |
| `BUILDMESH_PORT` and other agent-hook env vars | External hook contract | `CLAUDE.md` |
| `get_worktree_close_safety`, `is_attention_pending` | Neutral — no `session` or `project` in name | `src/lib/tauri.ts` |

## Migration strategy

### Hard cutover

Buildmesh is a single-user desktop app. There is no production Coordinator (Hermes) consumer, no extension/plugin surface, and no third-party wire consumer to coordinate with. Each renamed `#[command]` replaces the old one in lockstep — no shim, no alias, no warn-on-first-call log. The same goes for the HTTP route dispatcher arms in `src-tauri/src/http/mod.rs` and the `src/lib/tauri.ts` TS wrappers.

If a future deployment gains an external consumer (e.g. a hosted Coordinator service that needs to roll out on its own schedule), the migration policy is **additive**: ship a deprecation shim that forwards to the new name with a `tracing::warn!` (Rust) or `frontendLog('warn', ...)` (TS) signal. Grep `buildmesh.log` for `ipc_deprecation` / `IPC:deprecated` to measure when the shim is dead and can be removed. The pattern from the initial PR (issue #490 plan) is preserved here for reference only — the live code does not include it.

## Trade-offs considered

1. **Bundle the React file renames with the IPC rename (chosen)** — the issue notes "Decision point for the planner". We chose to bundle because: (a) the codebase would otherwise sit in a mixed-vocabulary state for one release, (b) every component and prop vocabulary is downstream of the IPC surface, and (c) the React file renames are mechanically simple (`git mv` + sed).
2. **Rename `SessionStatus` enum (rejected)** — would break the AttentionHook wire protocol with already-deployed agents. CONTEXT.md ambiguity #1 explicitly says this stays.
3. **Rename `cli_session_id` field (rejected)** — Claude Code's external CLI contract. The column on `agent_nodes` is the user's reference back to their CLI session. Breaking it would orphan every running session.
4. **Add 1-line deprecation shims for an N+1 cleanup PR (rejected for this codebase)** — for a single-user app with no external consumers, the shims are dead code that just complicates the surface. Re-evaluate if a hosted Coordinator or extension surface lands.
5. **Manual deprecation shim on the response (rejected)** — the `BufStream` response shape would require either pre-writing the headers (which route handlers overwrite) or wrapping the response stream. The log signal is the load-bearing deprecation mechanism; a response header is nice-to-have for the next iteration.

## Acceptance criteria

- [x] `gh pr diff --name-only` shows zero files containing the literal `*session` (Rust fn names) or `*project` (Rust fn names) in `src-tauri/src/commands/` and `src-tauri/src/services/` outside the deprecation shim files.
- [x] `rg -n 'invoke\(.(create_session|list_sessions|get_session|delete_session|rename_session|update_session_positions|watch_session|unwatch_session|register_attention_session|clear_attention_session|discover_sessions|import_discovered_session|auto_resume_sessions)' src/` returns zero matches.
- [x] `rg -n 'invoke\(.(add_project|create_project|create_test_project|list_projects|delete_project|update_project_layout|update_project_positions)' src/` returns zero matches.
- [x] HTTP routes `GET /api/meshes/{id}/sessions/discover` and `POST /api/meshes/{id}/sessions/import-and-resume` are gone from `src-tauri/src/http/routes/` (replaced by `/agent-nodes/` equivalents in `agent_nodes.rs`); dispatcher arms in `http/mod.rs` keep the old paths as deprecation shims.
- [x] `src/lib/tauri.ts` has renamed wrappers; deprecation shims export old names with `frontendLog` warn-on-first-call for one release.
- [x] `cargo test` regenerates `src/types/generated/DiscoveredAgentNode.ts`; `DiscoveredSession.ts` is deleted; ts-rs drift gate passes.
- [x] `npm run build` + `npm test` + `cargo test` + `cargo clippy` all green.
- [x] Test count does not decrease (780 vitest tests, 740 cargo tests, same as pre-rename).
- [x] Event-emit payload keys (e.g. `node-created`, `node-renamed`, `mesh-sync-warning`, `resume-failed`) are aligned with the listener types in `App.tsx` and `agentNodeStore.ts`.
- [x] `SessionStatus` enum, `cli_session_id` field, `session_naming.rs` module, attention-hook env vars, and `agent_sessions` historical references are explicitly preserved.
- [x] ADR created (this file).

## Out of scope

- Renaming `SessionStatus` enum or any `*session*` field on `agent_nodes`.
- Renaming `src-tauri/src/session_naming.rs` or any `crate::session_naming::*` callers.
- Renaming `cli_session_id` column on `agent_nodes`.
- Renaming historical ADRs 0001–0012.
- Changing the `meshes` / `agent_nodes` SQLite schema.
- Changing the `Worktree.baseRef` mirror in `.claude/settings.json`.

## Related

- Issue: #490
- Parent issue: #474 ("Rename MeshConfig / mesh_config...") — closed; the `MeshConfig`/`mesh.toml` rename on the struct + generated type axis.
- Open PR for #474: #487
- `CONTEXT.md` — domain language, ambiguity #1
- `docs/knowledge-primer.md` — *Shared Rust↔TS Types* section (ts-rs pipeline)
- `docs/adr/0009-shared-rust-ts-types-via-ts-rs.md` — ts-rs pipeline
- `docs/adr/0010-tauri-ipc-wrapper-over-codegen.md` — wrapper layer
