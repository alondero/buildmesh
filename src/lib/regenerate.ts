import type { SessionStatus } from '../types/generated/SessionStatus';
import type { SpawnOption } from './groups';

/**
 * Issue #1502 — shared Regenerate helpers (in-place kick-start).
 *
 * Previously `NodeItem.tsx` owned both the disabled-status list and the
 * "exclude the current provider" filter inline. #1502 lifts both here so
 * the sidebar context menu AND the new `GridNodeHeader` toolbar/kebab
 * affordances stay in lockstep:
 *
 * - `REGENERATE_DISABLED_STATUSES` — statuses where a fresh
 *   `regenerate_agent_node` IPC would race the in-flight spawn
 *   (`spawning`, `pending`) or the backend would reject it
 *   (`archived`). Greyed-out (not hidden) for discoverability.
 * - `splitRegenerateTargets` — partition the full Spawn Option list into
 *   the node's current provider (for in-place kick-start) + every other
 *   provider. The backend's `decide_resume` already handles
 *   `same_harness == true` cleanly (same harness + `cli_session_id` →
 *   resume, else fresh), so including the current provider is safe.
 */

export const REGENERATE_DISABLED_STATUSES: readonly SessionStatus[] = [
  'spawning',
  'pending',
  'archived',
];

/**
 * Partition the provider list for the Regenerate picker.
 *
 * Returns the current provider (if it is still in the list — it may have
 * been removed since the node was created, or the list may not be loaded
 * yet) separately from every other provider, preserving input order in
 * both slices so `groupByHarness` keeps its stable harness-first grouping.
 *
 * The UI renders `current` in its own "Current (...)" section pinned to
 * the top of the picker (the in-place kick-start affordance), then the
 * `others` grouped by harness below it.
 */
export function splitRegenerateTargets(
  providerList: SpawnOption[] | undefined,
  currentProviderId: string,
): { current: SpawnOption | undefined; others: SpawnOption[] } {
  const list = providerList ?? [];
  const current = list.find((p) => p.id === currentProviderId);
  const others = list.filter((p) => p.id !== currentProviderId);
  return { current, others };
}

/**
 * True when the Regenerate picker has anything to offer — any provider at
 * all, including just the current one (in-place kick-start).
 *
 * Pre-#1502 this was "has alternate providers" (current excluded), so a
 * mesh whose only option was the current provider rendered a disabled
 * trigger. Post-#1502 the current provider alone is enough to enable the
 * action.
 */
export function hasRegenerateTargets(
  providerList: SpawnOption[] | undefined,
): boolean {
  return (providerList ?? []).length > 0;
}

/**
 * Human-readable label for the in-place row: `Current (<provider label>)`.
 */
export function formatCurrentProviderLabel(label: string): string {
  return `Current (${label})`;
}
