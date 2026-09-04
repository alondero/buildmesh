# 3. Buildmesh Owns Worktree Creation (Not the Agent CLI)

Status: accepted

Buildmesh creates each Agent Node's Git worktree itself — via the `git2` crate in `src-tauri/src/env/mod.rs` (`create_git_worktree` / `add_worktree_impl`) — rather than delegating to the agent CLI's `-w` flag.

## Context

Originally, worktree creation was delegated to the `cwrap`/`claude` CLI: Buildmesh appended `-w <worktree_name>` to the spawn args and the CLI ran `git worktree add` internally. This had two structural problems:

1. **It was cwrap-only.** `-w` was gated on `is_cwrap = matches!(provider, Anthropic | Minimax)`. Non-cwrap providers (OpenCode, Antigravity, Codex) don't implement `git worktree add`, so they got **no worktree at all** — they ran directly in the Mesh root, exposing them to the exact concurrent-agent conflicts worktrees exist to prevent.
2. **It entangled worktree naming with session resume.** The CLI reconstructed its session-data path by mangling the worktree name, so resume needed `-w` to find the session (`e6a8e16`, 2026-05-03) — but re-passing `-w` on resume made the CLI run `git worktree add` again and fail with a fatal "worktree already checked out" (`de8e4a5`, 2026-05-10). The pass-`-w` / omit-`-w` oscillation showed the delegated model was fragile.

Agents also hit `fatal: not a git repository` when operating inside CLI-created worktrees on Windows/WSL, because the worktree's `.git` file used a path format the host APIs couldn't follow.

## Decision

Buildmesh takes ownership of worktree creation:

1. **PR #51** (`1fd5003`, 2026-05-10) removed `-w` delegation and introduced `create_git_worktree`, which creates the worktree before spawning and runs all agents directly inside their worktree directory — for every provider, uniformly. PR #133 (`8bcb57b`, 2026-05-19) then deleted the `is_cwrap` branch entirely, leaving one provider-agnostic spawn path. **Today the spawn-time decision lives in [`src-tauri/src/git/worktree/provision.rs`](../../src-tauri/src/git/worktree/provision.rs) (the "Worktree Provisioner" — `provision_for_spawn(SpawnContext)` returning one of `Reused` / `Adopted` / `Upgraded` / `Created`), with the primitive layer in [`src-tauri/src/git/worktree/mod.rs`](../../src-tauri/src/git/worktree/mod.rs).**
2. **PR #126** (`fa73608`, 2026-05-18) replaced the `git worktree add` shell-out with `git2` library calls (`add_worktree_impl`), primarily to eliminate the Windows console-window flash that every CLI shell-out caused.
3. Buildmesh owns the checkout topology — `branched` (default) vs `detached` worktree modes — and post-creation fix-ups: `.worktreeinclude` copying (file + recursive directory entries as of #248), `.git`-path sanitization for WSL/Windows, and `prune_stale_worktrees` + retry-once self-healing.
4. Resume is a no-op on the worktree: creation only runs when the worktree directory does not already exist, replacing the brittle `-w`-on-resume handling.
5. **Issue #1519** made the parent directory configurable. Resolution follows the per-Mesh override, then the application preference, then `.claude/worktrees`; relative values are Mesh-rooted and absolute values must stay in the Mesh's native/WSL environment. Buildmesh snapshots the exact resolved path onto each new `agent_nodes` row. Configuration changes therefore affect future nodes and idle Pre-spawn Pool inventory, never relocate live worktrees; upgraded rows with no snapshot retain the legacy derivation.

## Considered alternatives

- **Keep delegating to the CLI's `-w`.** Rejected: cwrap-only (no isolation for OpenCode/Antigravity/Codex), and the resume/path coupling was unfixable from outside the CLI.
- **Shell out to the `git` CLI ourselves.** This was the intermediate state (PR #51). Superseded by `git2` (PR #126) to kill the Windows console-window flash and remove a process-spawn dependency.

## Consequences

- **Pros:** Worktree isolation is now provider-agnostic; the resume "already checked out" failure class is gone; Buildmesh controls branched-vs-detached topology (which is what let ADR 0002 safely relax the clean-parent gate); WSL/Windows `.git` paths are sanitized; `.worktreeinclude` lets untracked-but-needed files into the worktree.
- **Buildmesh now owns Git surface area** it previously inherited from the CLI — libgit2 version quirks, lock contention, and repository-corruption handling are now ours.
- **`base_ref` is fully threaded end-to-end.** `SpawnContext.base_ref` reaches `create_git_worktree(.., base_ref)` → `add_worktree_impl(.., base_ref)` → `git worktree add <path> <base_sha>`. Wired in PR #51's consumer-removal aftermath, audited under #230. Pinned by `branched_worktree_bases_off_base_ref_not_head` + `detached_worktree_bases_off_base_ref_not_head` (`git/worktree/mod.rs:1072+`, primitives) and `provision_for_spawn_cold_created_uses_spawn_context_base_ref_not_local_head` (`git/worktree/provision.rs`, orchestrator→primitive seam, issue #248).
- **`.worktreeinclude` directory copying is implemented** as of #248 (`git/worktree/mod.rs`, `apply_worktree_include` now does a recursive copy via `copy_dir_all`). The previous "log-and-skip" branch is gone; directory entries copy recursively, missing sources stay silent. Pinned by `apply_worktree_include_copies_directory_recursively` (happy path) and `apply_worktree_include_directory_missing_source_is_noop` (no-op contract).
- **Node Working Directory is historical state, not a live preference lookup.** New rows persist `worktree_path`; close, resume, watcher, Git, build/run, and UI consumers resolve through that value. This costs one nullable schema column but prevents a later settings edit from redirecting an existing node to a directory it never used.

## Follow-up (issue #409): `raw_path` collapses the worktree-rule copy in `file_watcher`

When PR #383 added the `use_worktree` gate and PR #387 added the trim to
`file_watcher::node_internal_path`, the worktree rule (`use_worktree` +
trimmed + non-empty `worktree_name` → `base_path` or
`base_path/.claude/worktrees/<name>`) was spelled in three places, kept in
lockstep by paired tests:

1. `env::worktree_segment` + `env::node_working_path` — canonical Rust
   authority (`src-tauri/src/env/mod.rs`).
2. `file_watcher::node_internal_path` — paired Rust copy
   (`src-tauri/src/commands/file_watcher.rs`).
3. `getNodeGitPath` in `src/lib/paths.ts` — paired TS copy.

#1↔#3 must stay paired (Tauri IPC async + React sync initial render rule, see
`feedback_cross-language-default-coupling.md`). The two **Rust** copies were
the architectural smell: the rule for "does this node have a worktree, and
what's its dir?" lived in `env`, but `file_watcher` re-spelled it. The
historical reason was that `ResolvedPath` only exposed `host_path` (Windows
UNC) and `spawn_path` (the agent's CWD form), not the **raw** effective path
the `file_watcher` GIT_CHANGED payload needed for the byte-identical-to-frontend
contract.

`env::node_working_path(...).raw_path` closes that gap: it's the POSIX-style
effective path (the input to `to_host_path` / `to_spawn_path`), and it's
exactly what the GIT_CHANGED `internal_path` field has always carried. The
local `file_watcher::node_internal_path` was deleted and its call site now
reads `resolved.raw_path`; the existing paired-test suite was re-pointed at
`env::node_working_path(node).raw_path` to keep the contract pinned.

Net effect: the rule is now defined once in Rust (`env::worktree_segment`).
Any future change to GIT_CHANGED matching (case-folding at emission, WSL↔Windows
canonicalisation) has exactly one place to change on the Rust side, and the
TS paired copy remains the only cross-language coupling.
