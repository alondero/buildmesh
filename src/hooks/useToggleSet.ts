/**
 * `useToggleSet<T>()` — issue #463.
 *
 * A small abstraction over `useState<Set<T>>` that bundles the three
 * operations every Probe-row caller needs:
 *
 *   const expanded = useToggleSet<number>();
 *   expanded.isExpanded(n)  // boolean membership check (pure read)
 *   expanded.toggle(n)      // flip membership (add if absent, delete if present)
 *   expanded.clear()        // reset to empty (load-effect reset on mesh / filter change)
 *   expanded.size           // current cardinality
 *
 * Why a hook and not a plain helper
 * ---------------------------------
 * The two callsites (`GitIssuesTab`, `GitPullRequestsTab`) both keep the
 * Set in React state so a re-render reflects the latest expand state,
 * and both reset the Set on every load (mesh change in the issues tab,
 * mesh OR open/closed filter in the PR tab). The reset was always a
 * `setExpanded(new Set())` call duplicated next to the toggle — pulling
 * both into a hook keeps the load-effect reset to a single
 * `expanded.clear()` line and removes the hand-rolled
 * `setExpanded(prev => { const next = new Set(prev); ... })` closure.
 *
 * Why `Set` and not `Record<T, true>`
 * -----------------------------------
 * Membership is the only operation the row needs; a `Set` is the
 * natural shape and keeps `isExpanded` an O(1) hash lookup with no
 * `Object.hasOwn` ceremony. The functional updater is required so
 * `toggle` and `clear` work correctly under React's concurrent
 * batching — a stale closure that read the Set directly would drop
 * intermediate toggles.
 *
 * Generic over `T` (not just `number`)
 * ------------------------------------
 * `T` defaults to `number` because that's what the two callers need
 * today, but the hook accepts any key type. A future caller (branch
 * name, owner/repo pair) can pass `useToggleSet<string>()` without
 * re-implementing — the `tests/unit/use-toggle-set.test.ts` generic
 * test pins that contract.
 *
 * Stable identity for callbacks
 * -----------------------------
 * Each method (`isExpanded`, `toggle`, `clear`) is created once per
 * component instance and never re-created, so they can safely sit in a
 * dependency array or be passed to a memoised child without causing
 * extra renders. The `Set` itself is the only state that changes.
 */

import { useCallback, useMemo, useState } from 'react';

export interface ToggleSet<T> {
  /** True iff `key` is currently in the set. Pure read — does not mutate. */
  isExpanded: (key: T) => boolean;
  /** Flip `key`'s membership. Absent → present, present → absent. */
  toggle: (key: T) => void;
  /** Reset to an empty set. Used by load effects on mesh / filter change. */
  clear: () => void;
  /** Current cardinality — exposed for assertions / future affordances. */
  size: number;
}

export function useToggleSet<T = number>(): ToggleSet<T> {
  const [set, setSet] = useState<Set<T>>(() => new Set<T>());

  // `isExpanded` is a closure over `set`; wrap in `useCallback` so the
  // identity is stable across renders that don't change `set`. React
  // still re-creates it when the Set changes, which is what we want —
  // a memoised child depending on `isExpanded` should re-run when the
  // Set it reads changes.
  const isExpanded = useCallback((key: T) => set.has(key), [set]);

  // Functional updater — critical so two rapid `toggle(key)` calls don't
  // both read the same stale `set` reference and drop one of the flips.
  // The Set is rebuilt per toggle so React's referential equality sees a
  // change (a `Set#add` in place would not trigger a re-render).
  const toggle = useCallback((key: T) => {
    setSet((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  // Reset on mesh / filter change. Functional updater is overkill here
  // (no read), but keeps the pattern consistent with `toggle` and lets
  // a caller schedule `clear()` from a setTimeout / event listener
  // without worrying about closure staleness.
  const clear = useCallback(() => {
    setSet(() => new Set<T>());
  }, []);

  // Bundle into a stable object. `useMemo` keeps the returned ref
  // stable across renders that don't change any of the three callbacks
  // or the Set — only the Set itself causes a new bundle. Children that
  // destructure `{ toggle, clear, isExpanded }` will see new function
  // refs when the Set changes (because `isExpanded` depends on it) but
  // not on unrelated re-renders.
  return useMemo(
    () => ({
      isExpanded,
      toggle,
      clear,
      size: set.size,
    }),
    [isExpanded, toggle, clear, set],
  );
}