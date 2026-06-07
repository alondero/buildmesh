# 1. Auto-sync Mesh on Agent Node Spawn

When spawning a new **Agent Node** in a worktree, Buildmesh will automatically fetch and attempt a fast-forward pull on the parent **Mesh** repository if the local repository is clean. If the pull fails (network down or history diverged), the spawn proceeds from local `HEAD` and the user sees a non-fatal warning toast.

## Context

AI agents spawned in parallel worktrees should begin their work from the latest available remote commits to prevent split-brain conflicts and stale baselines. Previously, users had to manually pull the parent repository, leading to situations where a newly created agent node checked out a baseline that was many commits behind the remote.

## Decision

We will:
1. Run a background `git fetch origin` followed by a `git pull --ff-only --no-rebase` on the **Mesh**'s current branch before creating a new worktree-backed **Agent Node**. The `--no-rebase` flag is required to defeat a user's global `pull.rebase=true` config: a rebase on a diverged history writes conflict markers to the working tree, which would be a silent-mutation disaster for an auto-sync step.
2. **Skip if the working tree is dirty** (`Ok(SkippedDirty)`). The user is mid-edit; we don't want to fire a network round-trip on a sync we couldn't apply, and we don't want to nag them on every spawn.
3. **Skip if there's no `origin` remote** (`Ok(SkippedNoRemote)`). A purely local Mesh is a valid state.
4. **Fetch → ff-pull on a clean parent with an origin:**
   - `Ok(UpToDate)` if no new commits.
   - `Ok(Synced { new_commits })` on a clean fast-forward.
   - `Ok(FetchedButDiverged { new_commits, reason })` if the fetch succeeded but the local history has diverged from the remote (ff-pull rejected). This is the "partial sync" case — the user has unmerged local work, and the spawn proceeds from local `HEAD` so they can resolve the divergence on their own schedule.
   - `Err(FetchFailed(reason))` on a network or auth failure (fetch subprocess itself failed).
   - `Err(RepoUnusable(reason))` if the path isn't a git repo at all.
5. **Never block spawn.** Any of the above is a hint, not a gate. The worktree is created (if needed) and the agent is spawned regardless. The frontend shows a non-fatal `Sync` toast for the four `FetchedButDiverged` / `FetchFailed` / `RepoUnusable` cases; the three `Synced` / `UpToDate` / `Skipped*` cases are silent.

## Consequences

- **Pros:** Newly spawned agent nodes are automatically synchronized with the remote, eliminating the friction of stale worktree bases. A user who hits network or divergence issues is told what happened and where they stand, rather than discovering it later in an agent's stale context.
- **Cons:** Spawning a new agent node now depends on a brief network check. If the network is down or slow, there may be a slight delay before spawning proceeds using the offline fallback. The toast adds a (small) class of "non-error" notifications to the toast stack, alongside the existing `provider-error` and `Worktree` labels.
- **Scope:** The auto-sync runs only on the worktree-creation path (`spawn.rs` — `if !host_path.exists()`). Resumed nodes do not re-sync; the worktree already exists at whatever commit was last checked out, and the agent will continue from there. A user wanting a manual "sync now" can use the user-facing `git_sync` Tauri command (the same fetch+ff-pull contract) from the Mesh Properties panel.
