# 24. Assign Session IDs Rather Than Capture Them; Resume Reuses the Existing Worktree

For providers whose session ID we control (Anthropic), Buildmesh
**assigns** a UUID at spawn time and passes it to the CLI via
`--session-id <uuid>`, rather than sniffing the ID back out of PTY output.
PTY regex capture survives only for *self-assigning* providers that print a
labeled UUID (Codex, Antigravity). OpenCode also self-assigns, but its IDs
are `ses_…` (not UUIDs) and are not printed on the TUI — capture is
`opencode session list --format json` filtered by spawn directory, and
resume is `--session <id>` (see `docs/learning/opencode-harness-capabilities.md`).
`CLAUDE_CODE_SESSION_ID` is deliberately **not** used. Resume re-spawns
inside the worktree that already exists on disk rather than re-creating it.

## Context

This ADR records the disposition of issue #36, which proposed two changes:
adopt the `CLAUDE_CODE_SESSION_ID` environment variable as a primary/fallback
session-ID source, and use `EnterWorktree`-style adoption to attach to existing
worktrees on resume. Both proposals were written against an earlier
architecture and are moot today. The relevant history:

1. **`61090a4` (2026-05-01) flipped capture to assignment.** For the
   `anthropic` provider (and other providers whose CLI accepts `--session-id`)
   Buildmesh now mints a `Uuid::new_v4()` in the orchestrator, writes it to
   `agent_nodes.cli_session_id` *before* launch, and passes it as
   `--session-id <uuid>` (`agent/spawn.rs`, `SessionIdMode::Assign`). There
   is nothing to capture: we already know the ID because we chose it.
   OpenCode is **not** in this set — its CLI rejects caller-chosen UUIDs
   (`Invalid session ID`) and unknown `ses_…` IDs (`Session not found`).
2. **PTY regex capture (`session_capture.rs`) now runs only for self-assigning
   providers** — Codex and Antigravity — where the CLI picks its own ID and we
   have no way to know it up front. `ac47f9e` (issue #651) added
   `reader_should_capture_session_id` so the reader thread stays quiet in
   `Assign`/`Resume` mode, guaranteeing exactly one writer of
   `agent_nodes.cli_session_id` per spawn.
3. **`CLAUDE_CODE_SESSION_ID` cannot serve the proposed role.** The documented
   `CLAUDE_CODE_*` environment variables flow *downward*: Claude Code sets them
   for its own subprocesses (Bash/PowerShell tools, hook commands). Buildmesh
   spawns `claude` as the top-level process of a PTY, so the *parent*
   (Buildmesh) cannot read a variable that `claude` sets for its *children*.
   The variable is also Claude-Code-specific, so it does nothing for the only
   providers that still rely on capture (Codex, Antigravity).
4. **The `-w` flag was removed in `de8e4a5` (2026-05-10),** superseding the
   `e6a8e16` resume fix (2026-05-03) that issue #36 references. Resume no
   longer manages a shell-side worktree switch (`grep '"-w"' src-tauri/src`
   returns nothing). See ADR 0003 for why worktree creation moved off the
   agent CLI entirely.

## Decision

1. **Assign, don't capture, wherever we control the ID.** Keep the
   deterministic `--session-id` path as the primary and more reliable
   mechanism. Keep PTY regex capture strictly as the fallback for
   self-assigning providers, behind the `reader_should_capture_session_id`
   single-writer gate.
2. **Do not adopt `CLAUDE_CODE_SESSION_ID`.** It is unreadable by the spawning
   parent and inapplicable to the providers that would need it. No env-var
   injection or read is added.
3. **Resume reuses the existing worktree.** `provision_for_spawn`
   (`git/worktree/provision.rs`) already takes the `Reused` branch when the
   host path exists on disk — "the agent's prior work sits in that directory
   and clobbering it would orphan the user's commits" — so resume simply
   re-spawns inside it. This is the direct equivalent of `EnterWorktree`
   adoption, and needs no CLI flag.

## Consequences

- **Pros:** Session-ID resumption is deterministic and does not depend on the
  timing or format of PTY output for the providers we control. There is one
  authoritative writer per spawn. Resume never re-cuts a worktree, so prior
  commits and uncommitted work are preserved.
- **Cons / limits:** Self-assigning providers that print a labeled UUID
  (Codex, Antigravity) still depend on PTY regex capture; if that capture
  path ever proves flaky, the fix lives in `session_capture.rs`, not in an
  env var. OpenCode captures via `opencode session list` (`services::opencode_session`)
  because its `ses_…` IDs never match the UUID regex.
- **Unchanged / already covered:** Resume worktree reuse is pinned by
  `provision_for_spawn_reused_when_path_already_exists` (existing worktree →
  `Reused`, prior work preserved) and the cold path by
  `provision_for_spawn_created_when_pool_empty_and_no_warm_entry` +
  `provision_for_spawn_cold_created_uses_spawn_context_base_ref_not_local_head`
  (fresh worktree → `Created`). The `-w` removal rationale lives in ADR 0003.
