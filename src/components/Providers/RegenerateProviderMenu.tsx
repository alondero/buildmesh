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
 * Shared by the sidebar `NodeItem` context-menu submenu and the header
 * kebab submenu so the two surfaces never drift (same ordering, same
 * labels, same data attributes).
 *
 * The current row carries `data-is-current="true"` plus a dedicated
 * `${submenuTestId}-current` test id so tests can pin the in-place
 * affordance without parsing labels.
 *
 * Issue #1720 follow-up — single-caret highlight. Exactly ONE row paints
 * the selection surface at any time: the row the caret (focus) sits on.
 * Both hosts drive the rows with real focus — the sidebar's `useSubmenu`
 * `stepSubmenuFocus` walk and the kebab's arrow handling move focus, and
 * hovering a row focuses it too — so this component just tracks focus
 * (`onFocus` sets the index) and paints off it. Hovering moves the
 * highlight instead of lighting a second row; the `current` badge keeps
 * marking the node's provider for identity without a persistent
 * highlight of its own. Rows are left in the natural Tab order (the
 * hosts scope their keyboard walks; the picker adds no roving
 * tabindex of its own).
 */
export function RegenerateProviderMenu({
  providers,
  currentProviderId,
  onPick,
  submenuTestId = 'regenerate-submenu',
}: RegenerateProviderMenuProps) {
  const emptyTestId = `${submenuTestId}-empty`;
  const { current, others } = useMemo(
    () => splitRegenerateTargets(providers, currentProviderId),
    [providers, currentProviderId],
  );
  const otherGroups = useMemo(() => groupByHarness(others), [others]);

  // The caret IS document focus. `caretId` mirrors the focused row so the
  // paint below is a pure function of focus state; pointer entry focuses
  // the row (`e.currentTarget` — no DOM re-query), keyboard walks focus
  // it, and this handler keeps the paint in lockstep either way.
  const [caretId, setCaretId] = useState<string | null>(null);

  if (providers.length === 0) {
    return (
      <div data-testid={emptyTestId} className="px-3 py-1.5 text-xs text-text-muted">
        No providers available
      </div>
    );
  }

  // The single caret surface: paint `bg-bg-selection` only on the row
  // holding focus. No CSS hover/focus paint anywhere — hover and focus
  // both MOVE the caret instead.
  const caretClass = (id: string): string =>
    caretId === id ? 'bg-bg-selection text-text-primary' : '';

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
            data-spawn-id={current.id}
            data-spawn-harness={current.harness_id}
            data-is-current="true"
            data-testid={`${submenuTestId}-current`}
            onClick={() => onPick(current.id, current.label)}
            // Pointer entry joins the keyboard's single caret: focus the
            // row under the cursor (preventScroll so focusing inside the
            // scrollable sidebar list never jumps it); the focus handler
            // syncs the paint.
            onMouseEnter={(e) => e.currentTarget.focus({ preventScroll: true })}
            onFocus={() => setCaretId(current.id)}
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
                  data-spawn-id={native.id}
                  data-spawn-harness={native.harness_id}
                  onClick={() => onPick(native.id, native.label)}
                  onMouseEnter={(e) => e.currentTarget.focus({ preventScroll: true })}
                  onFocus={() => setCaretId(native.id)}
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
                  key={child.id}
                  data-spawn-id={child.id}
                  data-spawn-harness={child.harness_id}
                  onClick={() => onPick(child.id, child.label)}
                  onMouseEnter={(e) => e.currentTarget.focus({ preventScroll: true })}
                  onFocus={() => setCaretId(child.id)}
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
