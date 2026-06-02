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

1. **PR #51** (`1fd5003`, 2026-05-10) removed `-w` delegation and introduced `create_git_worktree`, which creates the worktree before spawning and runs all agents directly inside their worktree directory — for every provider, uniformly. PR #133 (`8bcb57b`, 2026-05-19) then deleted the `is_cwrap` branch entirely, leaving one provider-agnostic spawn path.
2. **PR #126** (`fa73608`, 2026-05-18) replaced the `git worktree add` shell-out with `git2` library calls (`add_worktree_impl`), primarily to eliminate the Windows console-window flash that every CLI shell-out caused.
3. Buildmesh owns the checkout topology — `detached` (default) vs `branched` worktree modes — and post-creation fix-ups: `.worktreeinclude` file copying, `.git`-path sanitization for WSL/Windows, and `prune_stale_worktrees` + retry-once self-healing.
4. Resume is a no-op on the worktree: creation only runs when the worktree directory does not already exist, replacing the brittle `-w`-on-resume handling.

## Considered alternatives

- **Keep delegating to the CLI's `-w`.** Rejected: cwrap-only (no isolation for OpenCode/Antigravity/Codex), and the resume/path coupling was unfixable from outside the CLI.
- **Shell out to the `git` CLI ourselves.** This was the intermediate state (PR #51). Superseded by `git2` (PR #126) to kill the Windows console-window flash and remove a process-spawn dependency.

## Consequences

- **Pros:** Worktree isolation is now provider-agnostic; the resume "already checked out" failure class is gone; Buildmesh controls branched-vs-detached topology (which is what let ADR 0002 safely relax the clean-parent gate); WSL/Windows `.git` paths are sanitized; `.worktreeinclude` lets untracked-but-needed files into the worktree.
- **Buildmesh now owns Git surface area** it previously inherited from the CLI — libgit2 version quirks, lock contention, and repository-corruption handling are now ours.
- **Regression — `base_ref` was orphaned.** `base_ref` (default `origin/main`) was introduced (`431e858`, 2026-05-08) as a value written to `{mesh}/.claude/settings.json` **for the CLI to consume**. When PR #51 removed the CLI's worktree logic, that consumer disappeared, but `base_ref` is still persisted and editable in the UI. `add_worktree_impl` never reads it — it bases every worktree off the Mesh root's live `repo.head()`. The only link to the remote is ADR 0001's best-effort fast-forward, which is silently skipped when the parent is dirty (now allowed, ADR 0002) or offline. Net effect: worktrees can be cut from a stale local HEAD with the configured base ref ignored. Tracked for repair in [#230](https://github.com/alondero/buildmesh/issues/230).
- **`.worktreeinclude` directory copying is unimplemented** (`env/mod.rs`) — only individual files are copied; directory entries are logged and skipped.
- **Doc debt:** `docs/knowledge-primer.md` (the "Worktree Support (`-w`)" section and the resume-fix note) still describes the delegated `-w` model as current and must be updated to reflect this decision.
