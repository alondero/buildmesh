import { invalidateGitSummaryForPath } from './useGitSummary';
import { invalidateOpenPrForNode } from './useOpenPr';

/**
 * Drop the `useGitSummary` + `useOpenPr` caches for one agent node and make
 * every mounted subscriber re-fetch immediately (issue #1004).
 *
 * Why this exists — the freshness-window interaction
 * --------------------------------------------------
 * Both hooks sit on `pathInvalidatedCache` clients with a
 * `minRefetchIntervalMs` window (60s for the PR, 2s for the summary): a
 * `GIT_CHANGED` that arrives while the cached value is still "fresh" is
 * suppressed, and only a trailing refetch at the end of the window lands.
 * That is the right rate guard for an agent streaming edits, but it makes
 * a freshly-spawned node's header sit empty:
 *
 *   1. Stage-1 of the spawn commits the node row with `worktree_name`
 *      already set, so `GridNodeHeader` mounts both hooks against a
 *      worktree directory that stage-2 has not created yet. The fetch
 *      returns `null` and the cache stamps it as fresh.
 *   2. `watch_agent_node` only emits its leading-edge `git-changed` when
 *      the watched path exists (`src-tauri/src/commands/file_watcher.rs`),
 *      so nothing fires for the node until an agent writes a file.
 *   3. When an event finally does arrive, the freshness window suppresses
 *      it — the chips stay stale for up to the full window.
 *
 * The same shape bites after an autopilot wrap-up opens a PR: the cached
 * `null` is fresh, so the `PR #N` chip lags. This closes the "wiring that
 * up is a v1.1" TODO in `useOpenPr.ts`.
 *
 * Call this at the moments the cached answer becomes structurally wrong —
 * `node-spawn-completed` (the worktree now exists) and
 * `autopilot-pr-created` (a PR now exists) — not on every git event; the
 * window is doing its job everywhere else.
 *
 * Lives in `src/hooks/` next to the two cache clients (both are
 * module-private to their hook files) so the store can reach it without
 * importing a component. It pulls in no store or component module, so
 * there is no cycle back to `agentNodeStore` / `App`.
 *
 * @param nodeId  the `useOpenPr` cache key
 * @param gitPath the node's resolved `getNodeGitPath(node)` — the
 *                `useGitSummary` cache key AND the `GIT_CHANGED`
 *                subscription path both hooks are registered under
 */
export function invalidateNodeCaches(nodeId: number, gitPath: string): void {
  invalidateGitSummaryForPath(gitPath);
  invalidateOpenPrForNode(nodeId, gitPath);
}
