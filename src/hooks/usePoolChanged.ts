import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';

/**
 * Event name emitted by the Rust pool service whenever a mesh's
 * pre-spawn Worktree Pool *ready* count changes. The five emit sites
 * (see `services::warm_pool::emit_pool_changed`) are:
 *
 *   * `try_claim`                — `available → claimed`, count -1.
 *   * `prewarm_one`               — `filling → available`, count +1.
 *   * `drain_excess_warm_entries` — drops N `available` rows, count -N.
 *   * `update_mesh_pool_size`     — target changes; also triggers a
 *                                  synchronous single-mesh drain.
 *   * `reconcile_on_startup`      — end-of-pass settle signal.
 *
 * Centralised here so the Rust and TS halves stay in sync (one
 * grep-able symbol beats a stringly-typed listener). Symmetric
 * constant in `src-tauri/src/services/warm_pool.rs` as
 * `POOL_COUNT_CHANGED_EVENT`. Drift is caught by the
 * `pool_count_changed_event_name_matches_frontend_constant` Rust test
 * and the `exposes event name as a single source of truth` TS test.
 */
export const POOL_COUNT_CHANGED_EVENT = 'pool-count-changed';

/**
 * Subscribes to the `pool-count-changed` Tauri event and calls
 * `refresh` whenever any mesh's pre-spawn pool mutates. The Worktrees
 * Probe's per-mesh badge (`WorktreeManagerTab`) uses this to re-fetch
 * its `get_mesh_pool_count` value without polling.
 *
 * **Payload is `{ mesh_id: number }`** — currently ignored. A future
 * per-mesh listener can short-circuit without a DB hit by comparing
 * the payload to its active `meshId`. Today every event triggers one
 * O(1) SQLite COUNT query, which is fine for the badge's scale (one
 * probe tab per mesh, idle ~2s tick cadence).
 *
 * Pass a stable reference (wrap with `useCallback` if your refresh
 * function would otherwise be a new closure on every render) so the
 * listener isn't torn down and re-attached on every parent re-render.
 * Mirrors `useProviderListInvalidation`'s contract.
 */
export function usePoolChanged(refresh: () => void): void {
  useEffect(() => {
    const unlisten = listen(POOL_COUNT_CHANGED_EVENT, () => {
      refresh();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh]);
}
