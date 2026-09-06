import type { ProbeTab } from './probeContext';

/**
 * Probe working-set bookkeeping (ADR-0032).
 *
 * The Probe panel's tool rail shows only the destinations the user has
 * opened this session, capped at four. Two orderings, deliberately separate:
 *
 * - **`tabs` — display order.** Insertion-ordered and spatially stable:
 *   a new destination appends at the end, survivors never move, and
 *   activation does NOT reorder anything. WAI-ARIA arrow-key navigation
 *   walks these stable positions, so every destination in the set is
 *   reachable in both directions — an earlier draft reordered the strip in
 *   MRU order on every activation, which trapped every tab past index 1
 *   into an ArrowRight ping-pong (and made ArrowLeft dead code).
 * - **`mru` — recency order.** Most recent first; drives *eviction only*.
 *   When a fifth destination is activated, the least recently visited one
 *   drops out of both lists.
 *
 * Session-only by design: the working set starts empty each launch, because
 * "what I was doing when I closed the app" is not a good guess for "what I
 * want tabs for today" — and an unpersisted list needs no migration or
 * staleness handling when destinations merge (issues #1457–#1460).
 */

/** Maximum tabs shown in the rail before the least recently visited entry
 *  is evicted. Four covers the observed 2–3 destination alternation pattern
 *  with one slot to spare; the ⊞ menu remains one click for everything
 *  else. */
export const PROBE_WORKING_SET_CAP = 4;

export interface ProbeWorkingSet {
  /** Display order for the rail — insertion-ordered, spatially stable. */
  readonly tabs: readonly ProbeTab[];
  /** Recency order, most recent first. Same membership as `tabs`. */
  readonly mru: readonly ProbeTab[];
}

export const EMPTY_PROBE_WORKING_SET: ProbeWorkingSet = { tabs: [], mru: [] };

/** Record a visit to `tab`: move it to the front of the recency list and
 *  append it to the display list if new, then evict whatever fell off the
 *  recency list. Pure — the uiStore actions own the `set` calls. */
export function pushProbeWorkingSet(
  set: ProbeWorkingSet,
  tab: ProbeTab,
): ProbeWorkingSet {
  const mru = [tab, ...set.mru.filter((t) => t !== tab)].slice(
    0,
    PROBE_WORKING_SET_CAP,
  );
  // Survivors keep their display positions; only evicted entries drop out.
  const kept = set.tabs.filter((t) => mru.includes(t));
  const tabs = set.tabs.includes(tab) ? kept : [...kept, tab];
  return { tabs, mru };
}
