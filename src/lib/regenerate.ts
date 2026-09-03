import type { SessionStatus } from '../types/generated/SessionStatus';
import type { SpawnOption } from './groups';

/**
 * Issue #1502 — shared Regenerate helpers (in-place kick-start).
 *
 * The disabled-status list and the current/alternates partition live
 * here so the sidebar context menu AND the `GridNodeHeader`
 * toolbar/kebab affordances stay in lockstep (-consumed via the
 * `useRegenerateAction` hook, which is the single owner of the
 * confirm state machine and IPC dispatch).
 *
 * Deliberately NOT here: one-line call-site expressions. `Current
 * (${label})` and `(providerList ?? []).length > 0` read better inline
 * than behind an exported helper with a five-line doc block.
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
