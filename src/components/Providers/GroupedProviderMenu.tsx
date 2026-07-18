import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { SpawnOption } from '../../lib/groups';
import { ProviderIcon } from './ProviderIcon';
import { groupByHarness } from '../../lib/groups';

export interface GroupedProviderMenuProps {
  /** Frontend view of the Spawn Menu (ADR-0016). Already in harness
   *  order; rows with the same `group_key` cluster under their harness
   *  header. `SpawnOption` (issue #583) is the post-`mapBackendProviders`
   *  shape — same fields `ProviderDropdown`/`SpawnButtonCluster` pass in. */
  providers: SpawnOption[];
  /** Called with `(providerId, altKey)` when the user picks a row. */
  onSelect: (providerId: string, altKey: boolean) => void;
  /** Optional filter (e.g. the archived-resume picker filters to
   *  `resumable: true`). Applied before grouping so the harness header
   *  is hidden when *all* its rows are filtered out. */
  filter?: (provider: SpawnOption) => boolean;
  /** Optional CSS class merged onto the root container. */
  className?: string;
  /**
   * Issue #814 — Escape closes the menu. The parent (e.g. `ProviderDropdown`)
   * owns the "is the menu open" boolean and the click-outside wiring, so it
   * provides a callback to flip that back to false on Escape. We can't
   * repurpose `onSelect` because Escape is a dismiss, not a row pick.
   */
  onClose?: () => void;
}

/**
 * Harness-grouped, always-expanded Spawn Menu (issue #575 / ADR-0016).
 *
 * The backend emits a flat `Vec<ProviderInfo>` already in harness order
 * with `group_key == harness_id`; this component is a pure render — it
 * buckets by `group_key`, keeps the input order within each bucket
 * (so the native row lands first), and renders:
 *
 *   * **Harness header** — the first non-proxied row in each bucket
 *     (`is_proxied === false`) is the clickable native spawn. The whole
 *     row is the button. A `filter` that removes the native row
 *     collapses the bucket to just children (no header) — code-review
 *     finding from issue #575: rendering a Proxied child with the
 *     "harness" badge would mislead the user.
 *   * **Proxied children** — every `is_proxied: true` row in the bucket
 *     is rendered indented as a child button.
 *
 * No hover submenus, no click-to-collapse — the issue calls for an
 * always-expanded flat list so the most common pick (e.g. Claude Code
 * native, or MiniMax via Claude Code) is one click.
 *
 * Issue #814 — WAI-ARIA menu semantics + keyboard nav. The menu now
 * declares `role="menu"` on the root and `role="menuitem"` on every
 * interactive row, with a roving tabindex so only the active row is in
 * the natural Tab order. ArrowDown/ArrowUp cycle focus with wrap-around,
 * Home/End jump to ends, and Escape closes (via `onClose`). The keyboard
 * handler mirrors the WAI-ARIA pattern used by `MeshItem` (issue #735)
 * and `KebabActions` in `GridNodeHeader` so the three menus (context,
 * kebab, spawn) feel identical.
 */
export function GroupedProviderMenu({ providers, onSelect, filter, className, onClose }: GroupedProviderMenuProps) {
  // Group by `group_key`, preserving the backend's harness order and the
  // stable within-bucket order (native row first, then children in their
  // listed order). The filter is applied per-row BEFORE bucketing so a
  // filter-out row is dropped — and if it was the harness header, the
  // bucket collapses to just children (a valid grouped render).
  // The bucketing is shared with `MeshPropertiesTab` and the mobile
  // `ProviderPicker` via `groupByHarness` (issue #583 cleanup).
  const groups = useMemo(
    () => groupByHarness(providers, { filter }),
    [providers, filter],
  );

  // Issue #814 — flat list across every menuitem in render order
  // (native headers + proxied children). The roving tabindex + the
  // keyboard-nav handler walk this list; we precompute it once per
  // render rather than re-querying the DOM on every keydown.
  const flatItems = useMemo(() => {
    const out: SpawnOption[] = [];
    for (const [, options] of groups) out.push(...options);
    return out;
  }, [groups]);

  // Roving tabindex state — only `flatItems[activeIndex]` gets
  // `tabIndex=0`, every other row stays at `-1` so Tab leaves the menu
  // (handled at the parent container).
  const [activeIndex, setActiveIndex] = useState(0);
  const menuRef = useRef<HTMLDivElement>(null);

  // Issue #814 — auto-focus the first menuitem on mount. `useLayoutEffect`
  // (not `useEffect`) fires synchronously after the menu commits, so the
  // very first ArrowDown after open doesn't race a deferred focus call
  // that would otherwise clobber the user's keystroke. Mirrors
  // `MeshItem`'s pattern (issue #735).
  useLayoutEffect(() => {
    setActiveIndex(0);
    menuRef.current
      ?.querySelector<HTMLButtonElement>('[role="menuitem"]')
      ?.focus();
  }, []);

  // Issue #814 — keyboard handler. WAI-ARIA menu contract: keystrokes
  // only apply while focus is inside the menu. We check
  // `document.activeElement` (not `e.target`) because in jsdom tests
  // events are dispatched on `document` while focus is on a menuitem,
  // and a real browser can deliver the keydown to the focused element
  // while the listener is on `document`. Same shape as `MeshItem`'s
  // handler (issue #735) and `KebabActions` (`GridNodeHeader.tsx`).
  //
  // `flatItems` and `activeIndex` are read via refs / re-bound in the
  // deps so the listener always sees the current state — without this,
  // the closure would freeze at the value current when the menu first
  // mounted, and a long-lived menu would not respond to later changes.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const root = menuRef.current;
      if (!root) return;
      const active = document.activeElement;
      if (!(active instanceof Node) || !root.contains(active)) return;
      const total = flatItems.length;
      if (total === 0) return;
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose?.();
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        const next = (activeIndex + 1) % total;
        setActiveIndex(next);
        focusMenuItem(root, next);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        const next = (activeIndex - 1 + total) % total;
        setActiveIndex(next);
        focusMenuItem(root, next);
        return;
      }
      if (e.key === 'Home') {
        e.preventDefault();
        setActiveIndex(0);
        focusMenuItem(root, 0);
        return;
      }
      if (e.key === 'End') {
        e.preventDefault();
        const last = total - 1;
        setActiveIndex(last);
        focusMenuItem(root, last);
        return;
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [activeIndex, flatItems, onClose]);

  // Build a lookup so each render's `tabIndex` resolves the flat index
  // in O(1). The map is keyed by `SpawnOption.id` (unique per backend
  // row, since the wire contract pairs `id = harness_id[:provider_id]`).
  const flatIndexById = useMemo(() => {
    const map = new Map<string, number>();
    flatItems.forEach((it, i) => map.set(it.id, i));
    return map;
  }, [flatItems]);

  return (
    <div
      ref={menuRef}
      className={className}
      // Issue #814 — `role="menu"` + `aria-label` so AT announces
      // "Select a provider, menu" when the dropdown opens. The
      // `aria-label` is shared with the parent `ProviderDropdown`
      // shell so the empty-state panel + menu are announced as one
      // surface.
      role="menu"
      aria-label="Select a provider"
    >
      {groups.map(([groupKey, options]) => {
        // `options[0]` is normally the native harness header (the
        // backend builds it before appending proxied children for
        // the same harness). When a `filter` is provided, the native
        // row may have been filtered out, leaving the first child
        // in its place — that row would then render with a
        // misleading "harness" badge. Find the first non-proxied
        // row explicitly so a filter-out harness header collapses
        // gracefully (all surviving rows render as peers without
        // the harness badge).
        const native = options.find((o) => !o.is_proxied);
        const proxiedChildren = options.filter((o) => o.is_proxied);
        return (
          <div key={groupKey} data-spawn-group={groupKey} className="border-b border-border-subtle last:border-b-0">
            {native && (
              <button
                type="button"
                // Issue #814 — WAI-ARIA menuitem. Roving tabindex: only
                // the active item is `tabIndex=0`, the rest stay at `-1`
                // so Tab leaves the menu (handled by the parent container).
                role="menuitem"
                tabIndex={flatIndexById.get(native.id) === activeIndex ? 0 : -1}
                data-spawn-id={native.id}
                data-spawn-harness={native.harness_id}
                onClick={(e) => { e.stopPropagation(); onSelect(native.id, e.altKey); }}
                className="w-full text-left px-3 py-1.5 text-xs text-text-primary font-medium hover:bg-bg-card focus:bg-bg-card focus:outline-none flex items-center gap-2"
              >
                <ProviderIcon providerId={native.id} className="h-3.5 w-3.5 shrink-0" />
                <span className="flex-1 truncate">{native.label}</span>
                {/* The "native" badge clarifies the row is a harness
                    launch, not a Proxied child — particularly useful
                    for the bare-Claude subscription in a host with
                    no other Claude-compatible accounts. */}
                <span className="text-2xs uppercase tracking-wider text-text-muted">harness</span>
              </button>
            )}
            {proxiedChildren.length > 0 && (
              <div className="pb-1">
                {proxiedChildren.map((child) => (
                  <button
                    type="button"
                    // Issue #814 — see above.
                    role="menuitem"
                    tabIndex={flatIndexById.get(child.id) === activeIndex ? 0 : -1}
                    key={child.id}
                    data-spawn-id={child.id}
                    data-spawn-harness={child.harness_id}
                    onClick={(e) => { e.stopPropagation(); onSelect(child.id, e.altKey); }}
                    className="w-full text-left pl-7 pr-3 py-1 text-xs text-text-secondary hover:bg-bg-card focus:bg-bg-card focus:outline-none flex items-center gap-2"
                  >
                    <ProviderIcon providerId={child.id} className="h-3.5 w-3.5 shrink-0" />
                    <span className="flex-1 truncate">{child.label}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

/**
 * Move focus to the n-th menuitem inside `root`. Mirrors the
 * `submenuItemRefs.current[next]?.focus()` shape used by `NodeItem`
 * (#774) — `querySelectorAll` re-runs each call so a re-rendered
 * button (e.g. after a Proxied filter change) is always reachable,
 * even if a ref array went stale.
 */
function focusMenuItem(root: HTMLElement, index: number) {
  const all = root.querySelectorAll<HTMLButtonElement>('[role="menuitem"]');
  all[index]?.focus();
}
