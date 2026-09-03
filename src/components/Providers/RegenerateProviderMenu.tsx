import { useMemo } from 'react';
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
   * provided (the `GridNodeHeader` inline dropdown via `useAriaMenu`),
   * only the active row gets `tabIndex=0` so Tab leaves the menu cleanly.
   */
  activeIndex?: number;
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
 * Shared by the sidebar `NodeItem` context-menu submenu AND the new
 * `GridNodeHeader` toolbar dropdown / kebab submenu so the three surfaces
 * never drift (same ordering, same labels, same data attributes).
 *
 * The current row carries `data-is-current="true"` plus a dedicated
 * `${submenuTestId}-current` test id so tests can pin the in-place
 * affordance without parsing labels.
 */
export function RegenerateProviderMenu({
  providers,
  currentProviderId,
  onPick,
  submenuTestId = 'regenerate-submenu',
  activeIndex,
}: RegenerateProviderMenuProps) {
  const emptyTestId = `${submenuTestId}-empty`;
  const { current, others } = useMemo(
    () => splitRegenerateTargets(providers, currentProviderId),
    [providers, currentProviderId],
  );
  const otherGroups = useMemo(() => groupByHarness(others), [others]);

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

  const tabIndexFor = (id: string): number | undefined =>
    activeIndex === undefined ? undefined : flatIndexById.get(id) === activeIndex ? 0 : -1;

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
            title="Regenerate in place on the current provider (kick-start a wonky harness)"
            className="w-full text-left px-3 py-1.5 text-xs text-text-primary font-medium hover:bg-bg-card flex items-center gap-2"
          >
            <ProviderIcon providerId={current.id} className="h-3.5 w-3.5 shrink-0" />
            <span className="flex-1 truncate">{`Current (${current.label})`}</span>
            <span className="text-2xs uppercase tracking-wider text-accent-cyan">current</span>
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
                  className="w-full text-left px-3 py-1.5 text-xs text-text-primary font-medium hover:bg-bg-card flex items-center gap-2"
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
                  className="w-full text-left pl-7 pr-3 py-1 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
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
