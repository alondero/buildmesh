import { useMemo, useRef, useState } from 'react';
import type { SpawnOption } from '../../lib/groups';
import { ProviderIcon } from './ProviderIcon';
import { groupByHarness } from '../../lib/groups';
import { SpawnConfigurationMenu } from './SpawnConfigurationMenu';
import { useAriaMenu } from '../../hooks/useAriaMenu';

export interface GroupedProviderMenuProps {
  /** Frontend view of the Spawn Menu (ADR-0016). Already in harness
   *  order; rows with the same `group_key` cluster under their harness
   *  header. `SpawnOption` (issue #583) is the post-`mapBackendProviders`
   *  shape — same fields `ProviderDropdown`/`SpawnButtonCluster` pass in. */
  providers: SpawnOption[];
  /** Called with `(providerId, altKey)` when the user picks a row. */
  onSelect: (providerId: string, altKey: boolean, configurationId?: string) => void;
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
  configurationsEnabled?: boolean;
}

/** Harness-grouped Spawn Menu. Parent rows launch defaults; their disclosure
 * opens capability-driven saved configurations. Pointer and keyboard share
 * the same active parent row. */
export function GroupedProviderMenu({ providers, onSelect, filter, className, onClose, configurationsEnabled = true }: GroupedProviderMenuProps) {
  const [submenu, setSubmenu] = useState<{ option: SpawnOption; anchor: HTMLElement; keyboard: boolean } | null>(null);
  const editing = useRef(false);
  const configurable = (option: SpawnOption) => Boolean(configurationsEnabled && option.capabilities && (
    option.capabilities.supports_model_override || option.capabilities.supports_effort_override || option.capabilities.supports_extra_args
  ));
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

  // Issue #837 — keyboard handler + auto-focus on mount are now
  // subsumed by the shared `useAriaMenu` hook. The hook reads
  // `flatItems.length` as `itemCount`, so a filter change that drops a
  // row (re-render) is picked up live via the hook's ref-mirrored
  // state. `onClose` is the parent's Escape callback — the hook
  // forwards `onClose?.()` directly. `closeOnTab` is left at default
  // (`true`) so the WAI-ARIA menu contract holds across every call
  // site; pre-#837 this menu omitted Tab handling and the test for it
  // (`grouped-provider-menu.test.tsx`) only covered Escape, but the
  // hook's default is the canonical WAI-ARIA `menu` behaviour.
  //
  // Issue #1720 follow-up — single-caret menu. Rows paint ONLY off
  // `activeIndex` (no CSS `hover:` paint), so exactly one row is
  // highlighted at any time. The hook's keyboard walk already moves
  // real focus AND the index together; hover joins that same channel
  // by focusing the entered row, and the row's focus handler syncs the
  // index — pointer and keyboard share ONE caret and can never light
  // two rows.
  useAriaMenu({
    rootRef: menuRef,
    itemCount: flatItems.length,
    itemSelector: '[data-spawn-id]',
    activeIndex,
    setActiveIndex,
    onClose: () => onClose?.(),
  });

  // Sync the roving index to the row that holds focus. Arrow keys
  // already set the index before focus lands, so for them this is an
  // idempotent echo; hover entry (below) relies on it entirely.
  const syncCaretToFocus = (id: string) => {
    const idx = flatIndexById.get(id);
    if (idx !== undefined) setActiveIndex(idx);
  };

  // Build a lookup so each render's `tabIndex` resolves the flat index
  // in O(1). The map is keyed by `SpawnOption.id` (unique per backend
  // row, since the wire contract pairs `id = harness_id[:provider_id]`).
  const flatIndexById = useMemo(() => {
    const map = new Map<string, number>();
    flatItems.forEach((it, i) => map.set(it.id, i));
    return map;
  }, [flatItems]);

  return (
    <div ref={menuRef} className={className} role="menu" aria-label="Select a provider">
      {groups.map(([groupKey, options]) => (
        <div key={groupKey} role="presentation" data-spawn-group={groupKey} className="border-b border-border-subtle last:border-b-0">
          {options.map((option) => (
            <div key={option.id} role="presentation" className="flex"
              onMouseEnter={(e) => {
                if (editing.current) return;
                const anchor = e.currentTarget.querySelector<HTMLElement>('[data-spawn-id]');
                anchor?.focus({ preventScroll: true });
                setSubmenu(anchor && configurable(option) ? { option, anchor, keyboard: false } : null);
              }}
            >
              <button type="button" role="menuitem"
                tabIndex={flatIndexById.get(option.id) === activeIndex ? 0 : -1}
                data-spawn-id={option.id} data-spawn-harness={option.harness_id}
                aria-label={option.label}
                aria-haspopup={configurable(option) ? 'menu' : undefined}
                aria-expanded={configurable(option) ? submenu?.option.id === option.id : undefined}
                onClick={(e) => { e.stopPropagation(); onSelect(option.id, e.altKey); }}
                onMouseEnter={(e) => { if (!editing.current) e.currentTarget.focus({ preventScroll: true }); }}
                onFocus={() => syncCaretToFocus(option.id)}
                onKeyDown={(e) => {
                  if (e.key === 'ArrowRight' && configurable(option)) {
                    e.preventDefault(); e.stopPropagation();
                    setSubmenu({ option, anchor: e.currentTarget, keyboard: true });
                  } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
                    setSubmenu(null);
                  } else if (e.key === 'Escape' && submenu?.option.id === option.id) {
                    e.preventDefault();
                    e.stopPropagation();
                    setSubmenu(null);
                  }
                }}
                className={`min-w-0 flex-1 text-left py-1.5 pr-3 text-xs focus:outline-none flex items-center gap-2 ${option.is_proxied ? 'pl-7' : 'pl-3'} ${
                  flatItems[activeIndex]?.id === option.id ? 'bg-bg-selection text-text-primary' : option.is_proxied ? 'text-text-secondary' : 'text-text-primary'
                }`}
              >
                <ProviderIcon providerId={option.id} className="h-3.5 w-3.5 shrink-0" />
                <span className="flex-1 truncate">{option.label}</span>
                {!option.is_proxied && <span className="text-2xs uppercase tracking-wider text-text-muted">harness</span>}
              </button>
              {configurable(option) && <button type="button" role="menuitem" tabIndex={-1} aria-label={`${option.label} configurations`}
                aria-haspopup="menu" aria-expanded={submenu?.option.id === option.id}
                className="px-2 text-xs text-text-secondary hover:bg-bg-selection"
                onClick={(e) => {
                  e.stopPropagation();
                  const anchor = e.currentTarget.previousElementSibling as HTMLElement;
                  setSubmenu({ option, anchor, keyboard: true });
                }}
              >›</button>}
            </div>
          ))}
        </div>
      ))}
      {submenu && <SpawnConfigurationMenu key={submenu.option.id} {...submenu} onEditingChange={(value) => { editing.current = value; }} onSelect={onSelect} onClose={() => setSubmenu(null)} onDismiss={() => { setSubmenu(null); onClose?.(); }} />}
    </div>
  );
}
