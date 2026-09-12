import { useMemo, useState } from 'react';
import type { SpawnOption } from '../../lib/groups';
import { groupByHarness } from '../../lib/groups';
import { splitRegenerateTargets } from '../../lib/regenerate';
import { ProviderIcon } from './ProviderIcon';

export interface RegenerateProviderMenuProps {
  /** Full Spawn Option list (including the current provider). */
  providers: SpawnOption[];
  /** The node's current provider id — rendered in its own pinned section. */
  currentProviderId: string;
  /** Called with `(providerId, providerLabel)` when the user picks a row. */
  onPick: (providerId: string, providerLabel: string) => void;
  /** Test id for the menu root. Defaults to `regenerate-submenu`. */
  submenuTestId?: string;
  /**
   * Roving tabindex position (flat index across current + alternates in
   * render order). When omitted, every row stays in the natural Tab order
   * (the sidebar `NodeItem` submenu manages focus itself via the shared
   * `useSubmenu` hook and doesn't need roving tabindex). When
   * provided (the `GridRegenerateButton` inline dropdown via `useAriaMenu`),
   * only the active row gets `tabIndex=0` so Tab leaves the menu cleanly.
   */
  activeIndex?: number;
  /**
   * Issue #1720 follow-up — single-caret hover contract. Called INSTEAD of
   * the internal index update for every user-driven caret move (pointer
   * entry, focus landing). The host must keep DOM focus and its roving
   * index in lockstep (see `GridRegenerateButton`'s `moveCaret`). When
   * omitted (the sidebar `NodeItem` submenu), the caret is tracked
   * internally and hover moves REAL focus so the host's
   * `stepSubmenuFocus` walk and the highlight never disagree.
   */
  onActiveIndexChange?: (next: number) => void;
}

/**
 * Issue #1502 — shared Regenerate provider picker.
 *
 * Renders the in-place kick-start row (`Current (<label>)`) pinned to the
 * top, then every other provider grouped by harness (same native-header +
 * proxied-children shape as `GroupedProviderMenu` and the pre-#1502
 * `NodeItem` submenu, including the `data-spawn-group` / `data-spawn-id` /
 * `data-spawn-harness` contract tests rely on).
 *
 * Shared by the sidebar `NodeItem` context-menu submenu, the header kebab
 * submenu, and the `GridRegenerateButton` inline dropdown so the three
 * surfaces never drift (same ordering, same labels, same data attributes).
 *
 * The current row carries `data-is-current="true"` plus a dedicated
 * `${submenuTestId}-current` test id so tests can pin the in-place
 * affordance without parsing labels.
 *
 * Issue #1720 follow-up — single-caret highlight. Exactly ONE row paints
 * the selection surface at any time: the row at the caret index (the
 * `activeIndex` prop when controlled, internal state when not). Hovering
 * a row MOVES the caret to it rather than lighting a second highlight —
 * a native dropdown affordance where pointer and keyboard share one
 * selection. The `current` badge keeps marking the node's provider for
 * identity; it no longer implies a persistent second highlight.
 */
export function RegenerateProviderMenu({
  providers,
  currentProviderId,
  onPick,
  submenuTestId = 'regenerate-submenu',
  activeIndex,
  onActiveIndexChange,
}: RegenerateProviderMenuProps) {
  const emptyTestId = `${submenuTestId}-empty`;
  const { current, others } = useMemo(
    () => splitRegenerateTargets(providers, currentProviderId),
    [providers, currentProviderId],
  );
  const otherGroups = useMemo(() => groupByHarness(others), [others]);

  // Uncontrolled caret (sidebar `NodeItem` submenu): the index lives here
  // and both pointer entry and REAL focus landing update it, so the
  // highlight always sits on the focused row and the host's
  // `stepSubmenuFocus` walk (which moves real focus) drives it correctly.
  const [localIndex, setLocalIndex] = useState(0);
  const controlled = onActiveIndexChange !== undefined;
  const caretIndex = activeIndex ?? localIndex;

  // Flat index lookup for roving tabindex (mirrors `GroupedProviderMenu`):
  // current is 0 when present, then every alternate in render order
  // (native header + proxied children per group, groups in order).
  const flatIndexById = useMemo(() => {
    const map = new Map<string, number>();
    let idx = 0;
    if (current) {
      map.set(current.id, idx++);
    }
    for (const [, options] of otherGroups) {
      const native = options.find((o) => !o.is_proxied);
      const proxied = options.filter((o) => o.is_proxied);
      if (native) map.set(native.id, idx++);
      for (const child of proxied) map.set(child.id, idx++);
    }
    return map;
  }, [current, otherGroups]);

  // Move the ONE caret to `next`. Controlled hosts route through their
  // own lockstep move (focus + index); the uncontrolled sidebar path
  // focuses the row (`focusWithoutScroll` semantics — the sidebar list
  // scrolls under the open menu, and a bare `.focus()` would jump it)
  // so keyboard walking and the highlight share one position.
  const moveCaretFrom = (el: HTMLElement | null, next: number) => {
    if (next === caretIndex) return;
    if (controlled) {
      onActiveIndexChange(next);
      return;
    }
    el?.focus({ preventScroll: true });
    setLocalIndex(next);
  };

  // Focus landing (host-driven keyboard walk, Tab, ...) syncs the
  // uncontrolled caret; in controlled mode the host already owns the
  // index, so the event is a no-op echo of its own focus() call.
  const trackFocus = (id: string) => {
    if (controlled) return;
    const idx = flatIndexById.get(id);
    if (idx !== undefined) setLocalIndex(idx);
  };

  const tabIndexFor = (id: string): number | undefined =>
    activeIndex === undefined ? undefined : flatIndexById.get(id) === activeIndex ? 0 : -1;

  // The single caret surface. Every row paints `bg-bg-selection` only when
  // it IS the caret row; there is no CSS hover/focus paint anywhere (hover
  // and focus both MOVE the caret instead).
  const caretClass = (id: string): string =>
    flatIndexById.get(id) === caretIndex ? 'bg-bg-selection text-text-primary' : '';

  if (providers.length === 0) {
    return (
      <div data-testid={emptyTestId} className="px-3 py-1.5 text-xs text-text-muted">
        No providers available
      </div>
    );
  }

  return (
    <>
      {current && (
        // `role="presentation"` — a `role="menu"` may only own
        // `menuitem`s; the plain grouping div must not appear in the
        // accessibility tree (same rule as the harness-group divs
        // below and the submenu trigger wrappers in `NodeItem` /
        // `KebabActions`).
        <div role="presentation" data-regenerate-section="current" className="border-b border-border-subtle">
          <button
            type="button"
            role="menuitem"
            tabIndex={tabIndexFor(current.id)}
            data-spawn-id={current.id}
            data-spawn-harness={current.harness_id}
            data-is-current="true"
            data-testid={`${submenuTestId}-current`}
            onClick={() => onPick(current.id, current.label)}
            onMouseEnter={(e) => moveCaretFrom(e.currentTarget, flatIndexById.get(current.id) ?? caretIndex)}
            onFocus={() => trackFocus(current.id)}
            title="Regenerate in place on the current provider (kick-start a wonky harness)"
            className={`w-full text-left px-3 py-1.5 text-xs text-text-primary font-medium focus:outline-none flex items-center gap-2 ${caretClass(current.id)}`}
          >
            <ProviderIcon providerId={current.id} className="h-3.5 w-3.5 shrink-0" />
            <span className="flex-1 truncate">{`Current (${current.label})`}</span>
            <span className="text-2xs uppercase tracking-wider text-text-secondary">current</span>
          </button>
        </div>
      )}
      {otherGroups.length === 0 ? (
        current ? null : (
          <div data-testid={emptyTestId} className="px-3 py-1.5 text-xs text-text-muted">
            No providers available
          </div>
        )
      ) : (
        otherGroups.map(([groupKey, options]) => {
          // One native row per harness group by wire contract
          // (`group_key == harness_id`, a single harness profile per
          // group; proxied rows carry the same key with
          // `is_proxied: true`). `find` is intentional, not a drop —
          // see `GroupedProviderMenu`'s identical shape.
          const native = options.find((o) => !o.is_proxied);
          const proxied = options.filter((o) => o.is_proxied);
          return (
            <div
              key={groupKey}
              role="presentation"
              data-spawn-group={groupKey}
              className="border-b border-border-subtle last:border-b-0"
            >
              {native && (
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={tabIndexFor(native.id)}
                  data-spawn-id={native.id}
                  data-spawn-harness={native.harness_id}
                  onClick={() => onPick(native.id, native.label)}
                  onMouseEnter={(e) => moveCaretFrom(e.currentTarget, flatIndexById.get(native.id) ?? caretIndex)}
                  onFocus={() => trackFocus(native.id)}
                  className={`w-full text-left px-3 py-1.5 text-xs text-text-primary font-medium focus:outline-none flex items-center gap-2 ${caretClass(native.id)}`}
                >
                  <ProviderIcon providerId={native.id} className="h-3.5 w-3.5 shrink-0" />
                  <span className="flex-1 truncate">{native.label}</span>
                  <span className="text-2xs uppercase tracking-wider text-text-muted">harness</span>
                </button>
              )}
              {proxied.map((child) => (
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={tabIndexFor(child.id)}
                  key={child.id}
                  data-spawn-id={child.id}
                  data-spawn-harness={child.harness_id}
                  onClick={() => onPick(child.id, child.label)}
                  onMouseEnter={(e) => moveCaretFrom(e.currentTarget, flatIndexById.get(child.id) ?? caretIndex)}
                  onFocus={() => trackFocus(child.id)}
                  className={`w-full text-left pl-7 pr-3 py-1 text-xs text-text-secondary focus:outline-none flex items-center gap-2 ${caretClass(child.id)}`}
                >
                  <ProviderIcon providerId={child.id} className="h-3.5 w-3.5 shrink-0" />
                  <span className="flex-1 truncate">{child.label}</span>
                </button>
              ))}
            </div>
          );
        })
      )}
    </>
  );
}
