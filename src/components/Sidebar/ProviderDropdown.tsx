import { useRef } from 'react';
import { GroupedProviderMenu } from '../Providers/GroupedProviderMenu';
import { SafeLink } from '../shared/SafeLink';
import { hasSpawnableAgent, type SpawnOption } from '../../lib/groups';
import { useViewportClamp } from '../../hooks/useViewportClamp';

// `SpawnOption` is the frontend view of the Spawn Option wire shape
// (issue #583) — produced by `mapBackendProviders` and consumed by the
// harness-grouped render. `GroupedProviderMenu` now consumes `SpawnOption`
// directly (not `ProviderInfo`) so the four desktop spawn surfaces
// share one frontend view and the boundary no longer needs a cast.

/** GitHub README anchor for the install prerequisites (issue #822). */
const PREREQUISITES_URL = 'https://github.com/alondero/buildmesh#prerequisites';

interface ProviderDropdownProps {
  /**
   * Issue #1264 — stable, pre-prefixed key for this cluster. Mirrored
   * onto the menu's `data-dropdown-for` so the shared `useClickOutside`
   * hook can scope to a single cluster when many rows share the same
   * page. The caller MUST build the value via
   * `dropdownId(surface, id)` (e.g. `mesh-5`, `node-10`, `issue-3`)
   * so the per-surface namespace can't collide — raw numeric ids
   * alone would let a `mesh-5` menu satisfy a `node-5` menu's outside
   * check. The prop is renamed from the misleading `meshId` so the
   * contract is obvious at the call site.
   */
  dropdownKey: string;
  providers: SpawnOption[];
  onSelect: (providerId: string, altKey: boolean) => void;
  /**
   * Issue #814 — Escape closes the dropdown. The parent (e.g. the
   * sidebar's `SpawnButtonCluster`) owns the `isOpen` state, so it must
   * provide a callback to flip that back to false. We can't repurpose
   * `onSelect` because Escape isn't a row pick — it's a dismiss.
   * `GroupedProviderMenu`'s keyboard handler also calls this on Escape,
   * so the two paths share the same close action.
   */
  onClose?: () => void;
  /**
   * Issue #814 — stable id used by the parent's trigger button's
   * `aria-controls`. The id is mirrored onto the menu's outer div so the
   * disclosure link is bidirectional (trigger → menu and menu → trigger).
   * Optional so callers that don't use the trigger-button pattern (e.g.
   * `ArchivedNodesTab`'s direct `<GroupedProviderMenu>` render) can omit
   * it without supplying a value.
   */
  menuId?: string;
}

export function ProviderDropdown({ dropdownKey, providers, onSelect, onClose, menuId }: ProviderDropdownProps) {
  // Issue #575 / ADR-0016 — render the harness-grouped, always-expanded
  // Spawn Menu. The single backend-derived list (issue #538 retired the
  // legacy enum-backed rows) is now grouped by `group_key` (== `harness_id`):
  // each harness's native row lands as a clickable header, every Proxied
  // child renders indented below it. Terminal is pinned last by the
  // backend's `order_providers` sort, so the visual order is identical
  // to the stored harness order.
  //
  // Issue #822 — first-run onboarding. On a fresh machine with no agent CLI
  // detected and no keyed provider, the backend emits only the Terminal
  // harness, so the menu is effectively empty with no hint about what to
  // install. Surface an empty-state panel above the (Terminal-only) menu
  // explaining the fix. Terminal stays clickable below — it's still a valid,
  // if limited, spawn — so this augments rather than replaces the menu.
  const noAgent = !hasSpawnableAgent(providers);
  const menuRef = useRef<HTMLDivElement>(null);

  // Issue #837 — viewport clamping is now the shared `useViewportClamp`
  // hook (mirrors `BuildRunDropdown.tsx`). The menu sits `absolute
  // right-0 top-full mt-1` so it can overflow the bottom edge of the
  // sidebar when the trigger lives near the bottom of a long mesh list.
  // The hook reads the rendered height BEFORE the browser paints (via
  // `useLayoutEffect` internally) and applies `translateY(-shift)` to
  // pull the menu up if it would overflow — keeping the existing
  // `top-full mt-1` anchor intact so the close animation doesn't have
  // to re-layout a repositioned popover.
  //
  // `deps: [providers]` — `ProviderDropdown` has no `isOpen` boolean
  // (it's rendered only while its parent dropdown is open, so the
  // parent owns the open lifecycle). The provider list is the right
  // gate: re-measure when the rendered content changes (e.g. an
  // archived-resume filter that drops proxied children).
  useViewportClamp(menuRef, [providers]);
  return (
    <div
      ref={menuRef}
      id={menuId}
      data-dropdown-for={dropdownKey}
      // Issue #814 — `role="menu"` is declared on `GroupedProviderMenu`'s
      // own root (the inner element that holds the menuitems). Nesting
      // `role="menu"` inside another `role="menu"` is invalid ARIA (a
      // menu cannot contain a menu as a direct child — AT will announce
      // the empty-state panel + the menu as two separate menus). The
      // outer shell stays role-less so the inner `GroupedProviderMenu`
      // owns the single menu role regardless of whether it's wrapped by
      // this shell (`Sidebar`/`Issues`/`PRs`/`Archive`) or rendered
      // directly (`ArchivedNodesTab`'s standalone use).
      // `aria-label` stays on this shell so the empty-state panel (when
      // visible) is announced as a child of the surrounding menu
      // surface, with the inner `GroupedProviderMenu` adding its own
      // label for the menu itself.
      aria-label="Select a provider"
      className="absolute right-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded-md shadow-md min-w-[200px] max-h-[400px] overflow-y-auto animate-scale-in origin-top-right"
    >
      {noAgent && (
        <div
          data-testid="spawn-menu-empty-state"
          className="px-3 py-2.5 border-b border-border-subtle"
          // Keep the panel from bubbling to the row's click-outside/toggle
          // handlers (the link has its own stopPropagation, but a click on
          // the surrounding text must not close the dropdown either).
          onClick={(e) => e.stopPropagation()}
        >
          <p className="text-xs font-medium text-text-primary">No agent CLIs found</p>
          <p className="mt-1 text-2xs text-text-muted leading-relaxed">
            Install one of Claude Code, Codex, Antigravity, or OpenCode, or add a
            provider key in Settings&nbsp;&rarr;&nbsp;Providers.
          </p>
          <SafeLink
            url={PREREQUISITES_URL}
            className="mt-1.5 inline-block text-2xs text-text-secondary hover:text-text-primary hover:underline"
            title="Open the setup instructions on GitHub"
          >
            View setup instructions&nbsp;&#8599;
          </SafeLink>
        </div>
      )}
      {/* Issue #814 — forward `onClose` to `GroupedProviderMenu` so its
          keyboard handler (Escape → close) calls the same callback the
          parent's `useClickOutside` and outside-mousedown paths call. */}
      <GroupedProviderMenu providers={providers} onSelect={onSelect} onClose={onClose} />
    </div>
  );
}
