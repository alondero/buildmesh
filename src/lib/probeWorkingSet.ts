import type { ProbeTab } from '../stores/uiStore';

/**
 * Probe working-set bookkeeping (ADR-0032).
 *
 * The Probe panel's tool rail shows only the destinations the user has
 * opened this session, in most-recently-used order, capped at four tabs.
 * The full destination list stays behind the rail's "All tools" menu,
 * which reuses the palette's tool-discovery groups (ADR-0031). Session-only
 * by design: the working set starts empty each launch, because "what I was
 * doing when I closed the app" is not a good guess for "what I want tabs
 * for today" — and an unpersisted list needs no migration or staleness
 * handling when destinations merge (issues #1457–#1460).
 */

/** Maximum tabs shown in the rail before the oldest working-set entry is
 *  evicted. Four covers the observed 2–3 destination alternation pattern
 *  with one slot to spare; the ⊞ menu remains one click for everything
 *  else. */
export const PROBE_WORKING_SET_CAP = 4;

/** Move `tab` to the front of the working set, evicting the oldest entry
 *  beyond the cap. Re-activating an existing entry reorders without
 *  growing the set. Pure — the uiStore action owns the `set` call. */
export function pushProbeWorkingSet(
  workingSet: readonly ProbeTab[],
  tab: ProbeTab,
): ProbeTab[] {
  return [tab, ...workingSet.filter((t) => t !== tab)].slice(
    0,
    PROBE_WORKING_SET_CAP,
  );
}
