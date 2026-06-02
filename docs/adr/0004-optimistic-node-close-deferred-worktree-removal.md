# 4. Optimistic Node Close With Deferred, Durable Worktree Removal

Status: accepted

Closing an Agent Node removes it from the UI immediately. The node's process tree is killed and its database row deleted synchronously (Phase 1), but the slow worktree-**directory** removal is recorded in a durable `pending_worktree_removals` queue and reclaimed in the background — or on the next app launch if interrupted.

## Context

After PRs #238–#240 made worktree removal *correct* on Windows (kill the process tree, atomic-rename staging, retry-with-backoff, tolerate broken/in-use worktrees), close was reliable but **felt frozen**. The frontend (`agentNodeStore.deleteAgentNode`) only dropped the node from the view *after* the entire backend chain returned:

1. `get_worktree_close_safety` — a `git2` status scan with untracked-directory recursion;
2. `kill_agent`;
3. `delete_session` → kill the process tree → **`remove_one_worktree`**, whose dominant cost is `remove_dir_all` over the worktree tree.

On a real project worktree (`node_modules`/`target` is tens of thousands of inodes) that recursive delete is multiple seconds on Windows, plus up to 500 ms of rename backoff — and the node sat on screen the whole time with no feedback. The UI was gated on the single slowest, most deferrable step in the chain.

The two operations conflated in that synchronous call have **opposite characteristics**: a node *lifecycle* change (kill + forget — fast, authoritative, must-not-lose) versus *disk reclamation* (slow, retry-prone, deferrable). The expensive half was also the safest to defer — `remove_one_worktree` is already idempotent (a missing directory counts as removed, stale `.removing` staging is cleared) and all-or-nothing (the atomic-rename gate from #239 means a partial run never corrupts the live tree).

## Decision

Separate *"the node is closed"* (fast, durable, authoritative) from *"its worktree is reclaimed"* (slow, best-effort, resumable).

1. **Durable removal queue.** A `pending_worktree_removals` table (`worktree_path` UNIQUE, `node_name`) records worktrees owed a cleanup. `INSERT OR IGNORE` makes enqueuing idempotent for retries.
2. **Phase 1 — synchronous and authoritative** (`services::agent_node::delete`). Kill the process tree (kept synchronous — we must never part with a node while its agent is live or fighting the eventual removal), then in **one transaction** delete the `agent_nodes` row *and* enqueue the worktree for removal (`db::delete_agent_node_enqueueing_removal`). No directory delete happens here.
3. **Optimistic UI.** `deleteAgentNode` removes the node from the store (and disposes its terminal) *before* awaiting the backend, so close is perceived as instant.
4. **Phase 2 — background drain** (`services::agent_node::process_pending_removals`, spawned by `delete_session`). Runs `remove_one_worktree` per queued entry, dequeuing **only on success**; a removal that still fails stays queued and is surfaced via the `worktree-cleanup-failed` event → a non-blocking toast.
5. **Phase 3 — startup reconcile** (`lib.rs` `setup()`, beside the existing crash-recovery sweep). Drains the queue on launch, so an app quit mid-cleanup *resumes* rather than orphaning the directory.

## Considered alternatives

- **Pure fire-and-forget** (hide the node, run removal in a detached task, no record). Rejected: if the app quits mid-delete the worktree orphans with nothing tracking that it should be gone, and the node is already dropped from state so the user can't retry. The durable queue is the small addition that makes the optimism *honest*.
- **Infer orphans on startup** (delete the row immediately, and on launch remove any `.claude/worktrees/*` directory with no matching node — no new table). Rejected: it *infers* intent rather than recording it, risks deleting a worktree someone deliberately recreated, and leaves nowhere clean to hang the retry/failure toast.
- **Keep close synchronous but add a spinner.** Rejected: honest about the wait but doesn't fix it; the wait is intrinsic to a recursive delete and only grows with repo size.

## Consequences

- **Pros:** Close feels instant regardless of worktree size. The app-quit-mid-cleanup case the design was stress-tested against becomes a *normal, designed-for path* (resumed by startup reconcile) rather than a data-loss edge case. The process kill stays synchronous, so no zombie agents. The drain reuses the idempotent #239 removal machinery unchanged.
- **Close is now eventually-consistent on disk.** "Node gone from the UI" no longer implies "worktree directory gone." Worst case after a mid-cleanup quit: the directory lingers until the next launch reclaims it — invisible to the user, costing only disk. Anything reading `.claude/worktrees/` directly (e.g. the prune panel) may briefly see a directory whose node no longer exists.
- **The test server (`commands/test.rs`) drains synchronously** after `delete`, so Playwright E2E still observes the directory gone when the call returns; only the real app defers.
- **Failure is surfaced, not silent.** A worktree that genuinely can't be removed raises a `worktree-cleanup-failed` toast and remains queued for retry on the next drain or launch.
