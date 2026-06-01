# 1. Auto-sync Mesh on Agent Node Spawn

When spawning a new **Agent Node**, Buildmesh will automatically fetch and attempt a fast-forward pull on the parent **Mesh** repository if the local repository is clean.

## Context

AI agents spawned in parallel worktrees should begin their work from the latest available remote commits to prevent split-brain conflicts and stale baselines. Previously, users had to manually pull the parent repository, leading to situations where a newly created agent node checked out a baseline that was many commits behind the remote.

## Decision

We will:
1. Run a background `git fetch` and fast-forward pull (`git pull --ff-only`) on the **Mesh**'s base branch before spawning a new **Agent Node**.
2. Perform this synchronization only if the local branch has no uncommitted changes.
3. If the pull fails (e.g., due to network absence or non-fast-forward diverged history), warn the user but proceed with spawning the **Agent Node** from the current local `HEAD` rather than blocking execution.

## Consequences

- **Pros:** Newly spawned agent nodes are automatically synchronized with the remote, eliminating the friction of stale worktree bases.
- **Cons:** Spawning a new agent node now depends on a brief network check. If the network is down or slow, there may be a slight delay before spawning proceeds using the offline fallback.
