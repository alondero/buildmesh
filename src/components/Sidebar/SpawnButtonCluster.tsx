import { useEffect, useId, useRef, useState } from 'react';
import { ProviderDropdown } from './ProviderDropdown';
import type { SpawnOption } from '../../lib/groups';

/**
 * Canonical `+ ▾` Spawn Menu cluster (ADR-0016 §2 — "Sidebar, Issues probe,
 * PRs probe, archived-resume, and mobile all render the same ordered, grouped
 * menu; none re-orders or re-derives it"). The cluster is the shared visual
 * surface for spawning a new agent node from any desktop app entry point:
 *
 *   - Sidebar mesh row (via NodeCreationForm) → `create_agent_node`
 *   - Issues probe row → `create_issue_node` + `start_node_background`
 *   - PRs probe row   → `create_pr_node`   + `start_node_background`
 *
 * Each parent owns the spawn *action* (different Tauri commands, different
 * default-resolution chains); the cluster owns the *visual* and the dropdown
 * wiring, so the three call sites compose the same `ProviderDropdown` →
 * `GroupedProviderMenu` ladder without duplicating the button pair.
 */
interface SpawnButtonClusterProps {
  /** Provider list — already filtered/sorted by the parent (per ADR-0016 §2
   *  the parent must NOT re-derive the order/grouping). */
  providers: SpawnOption[];
  /** Stable key for this cluster — passed through to `ProviderDropdown`'s
   *  `data-dropdown-for` attribute so click-outside handlers can scope to
   *  a single cluster when many rows share the same page. For meshes /
   *  issues / PRs this is `number`; for archived-session resumption
   *  (issue #813) the natural key is the string `session_id`. The value
   *  is string-coerced when assigned to the DOM attribute, so either
   *  shape works at runtime. */
  meshId: number | string;
  /** Visible label on the primary (+ default) action button. Defaults
   *  to "+" — the canonical spawn idiom across the sidebar / issues /
   *  PRs probe rows. Override for surfaces where the spawn is a
   *  non-spawn action: the Archive Resume flow (issue #813) uses
   *  "Resume" because the action imports + adopts an existing CLI
   *  session rather than spawning fresh. Width-wise the cluster is
   *  designed around a single character so a label like "Resume"
   *  still fits inside the 360px dock without breaking the
   *  row-resize-with-the-other-columns behaviour. */
  primaryLabel?: string;
  /** Visible label shown while `isSpawning` is true. Defaults to
   *  "Spawning..."; Resume uses "Resuming..." instead so the dock
   *  reads consistently with the action it actually performs. */
  busyLabel?: string;
  /** Accessible label on the primary action. Defaults to the cluster
   *  tooltip ("Add agent node (<default>)"); the Archive flow
   *  overrides with "Resume session" so AT users get a specific
   *  affordance instead of "add agent node". */
  primaryAriaLabel?: string;
  /** Whether this cluster's dropdown is currently open. */
  isOpen: boolean;
  /** Toggle the dropdown open/closed. */
  onToggleDropdown: () => void;
  /** Called when the bare `+` is clicked. The parent resolves the default
   *  provider (per-mesh > app-wide > fallback chain, server-side) and runs
   *  the appropriate spawn action. `altKey` is forwarded for surfaces that
   *  have a special alt-click semantics (the sidebar uses it to spawn the
   *  new node in the mesh root, bypassing the per-mesh `use_worktree`); other
   *  surfaces can ignore it. */
  onSpawnDefault: (altKey: boolean) => void;
  /** Called when a provider row is picked from the open dropdown. `altKey`
   *  forwarded for the same reason as `onSpawnDefault`. */
  onSelectProvider: (providerId: string, altKey: boolean) => void;
  /** Optional — returns the default provider id for the cluster's tooltip.
   *  If omitted, the tooltip falls back to the generic "Add agent node".
   *  Hover/focus triggers a fetch; a rejection is swallowed because the
   *  `+` click already triggers the same fetch via `onSpawnDefault`. */
  getDefaultProvider?: () => Promise<string>;
  /** Disables both buttons (e.g. any spawn is in flight across the surface).
   *  Distinct from `isSpawning`, which marks THIS cluster's spawn in flight
   *  and also rewrites the `+` label to "Spawning...". */
  disabled?: boolean;
  /** THIS cluster's spawn is in flight — `+` becomes "Spawning..." and both
   *  buttons disable. Dropdowns should be closed by the parent in this case
   *  (the cluster does not auto-close). */
  isSpawning?: boolean;
}

export function SpawnButtonCluster({
  providers,
  meshId,
  isOpen,
  primaryLabel = '+',
  busyLabel = 'Spawning...',
  primaryAriaLabel,
  onToggleDropdown,
  onSpawnDefault,
  onSelectProvider,
  getDefaultProvider,
  disabled,
  isSpawning,
}: SpawnButtonClusterProps) {
  // Cache the default provider id for the tooltip so we don't refetch on
  // every render. The hover/focus handler triggers the fetch; click is
  // covered by `onSpawnDefault` which the parent resolves independently.
  const [defaultProviderId, setDefaultProviderId] = useState<string | null>(null);

  const refreshDefaultProvider = async () => {
    if (!getDefaultProvider) return;
    try {
      setDefaultProviderId(await getDefaultProvider());
    } catch {
      // Tooltip falls back to the generic label below if this fails — the
      // spawn action's own resolution path (in `onSpawnDefault`) is the
      // authoritative one, so a tooltip-only miss is harmless.
    }
  };

  // Tooltip + accessible label on the primary action. Defaults to the
  // canonical "+ spawn" wording; surfaces that aren't a spawn (Archive
  // Resume) override via `primaryAriaLabel` so the hover hint and
  // aria-label describe what the click actually does.
  const defaultProviderLabel =
    providers.find(p => p.id === defaultProviderId)?.label ?? defaultProviderId;
  const baseLabel = primaryAriaLabel ?? 'Add agent node';
  const primaryTitle = defaultProviderLabel
    ? `${baseLabel} (${defaultProviderLabel})`
    : baseLabel;

  const isDisabled = disabled || isSpawning;

  // Issue #814 — trigger ref + stable menu id for the WAI-ARIA menu-button
  // disclosure pattern (`aria-haspopup` / `aria-expanded` / `aria-controls`).
  // The id is per-instance via React's `useId` so two clusters on the same
  // page (e.g. several sidebar rows) get distinct ids and screen readers
  // announce the right menu.
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuId = useId();

  // Issue #814 — focus return on Escape. `GroupedProviderMenu`'s keyboard
  // handler also listens for Escape and calls `onClose`, so two listeners
  // fire on Escape (both call close — idempotent). This listener's job is
  // focus return: by focusing the trigger *synchronously* before React
  // processes the queued state update, the trigger keeps focus across the
  // re-render that unmounts the menu. Without this, Escape would leave
  // focus on `body` (the menuitem that had focus just unmounted), which
  // is the WAI-ARIA anti-pattern the MeshItem fix (#735) specifically
  // avoids.
  useEffect(() => {
    if (!isOpen) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      // Focus the trigger FIRST — the menu's own Escape handler also
      // closes the menu, so by the time React re-renders to unmount it,
      // focus is already on the surviving toggle button. Using a
      // `requestAnimationFrame` (matching MeshItem / KebabActions) defers
      // the focus call past the unmount so the trigger ref is still
      // attached and the browser doesn't drop the focus call.
      const trigger = triggerRef.current;
      requestAnimationFrame(() => trigger?.focus());
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [isOpen]);

  return (
    <div className="relative">
      <div className="flex items-center rounded-md border border-accent-cyan/30 overflow-hidden">
        <button
          data-testid="spawn-default"
          onClick={(e) => { e.stopPropagation(); onSpawnDefault(e.altKey); }}
          onMouseEnter={refreshDefaultProvider}
          onFocus={refreshDefaultProvider}
          disabled={isDisabled}
          aria-label={primaryAriaLabel ?? undefined}
          className="flex items-center px-1.5 h-5 text-xs font-medium text-accent-cyan hover:bg-accent-cyan/15 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          title={isSpawning ? busyLabel : primaryTitle}
        >
          {isSpawning ? busyLabel : primaryLabel}
        </button>
        <span className="w-px h-3 bg-accent-cyan/30" />
        <button
          ref={triggerRef}
          data-testid="spawn-dropdown-toggle"
          onClick={(e) => { e.stopPropagation(); onToggleDropdown(); }}
          disabled={isDisabled}
          // Issue #814 — WAI-ARIA menu-button disclosure pattern. The
          // toggle advertises the menu it controls via `aria-controls`
          // (the menu's stable id), the open state via `aria-expanded`,
          // and the popup type via `aria-haspopup="menu"`. Screen readers
          // announce "Choose provider, menu button, collapsed/expanded".
          // Issue #813 — Resume surfaces describe the picker as "Choose
          // provider to resume with" so AT users get the action context
          // instead of a generic "Choose provider".
          aria-haspopup="menu"
          aria-expanded={isOpen}
          aria-controls={isOpen ? menuId : undefined}
          aria-label={primaryAriaLabel
            ? `Choose provider to ${primaryAriaLabel.toLowerCase()} with`
            : 'Choose provider'}
          className={`flex items-center px-1 h-5 text-xs hover:bg-accent-cyan/15 disabled:opacity-50 disabled:cursor-not-allowed transition-colors ${isOpen ? 'text-accent-cyan bg-accent-cyan/10' : 'text-accent-cyan/70'}`}
          title="Choose provider"
        >
          ▾
        </button>
      </div>
      {isOpen && !isSpawning && (
        <ProviderDropdown
          meshId={meshId}
          providers={providers}
          onSelect={onSelectProvider}
          // Issue #814 — Escape closes the dropdown. The cluster re-uses
          // `onToggleDropdown` because toggling an open cluster is
          // semantically equivalent to closing it (the toggle target is
          // the cluster state, not any particular spawn action). The
          // `GroupedProviderMenu`'s own Escape handler also calls this;
          // both paths converge on the same close action, so a double-fire
          // is idempotent.
          onClose={onToggleDropdown}
          // Stable id used by `aria-controls` on the trigger above. Tests
          // can read this attribute off the menu root to verify the
          // disclosure wiring (ProviderDropdown doesn't currently mirror
          // the id onto its outer div — the menu's accessible name comes
          // from its `aria-label`, which is the user-facing contract).
          menuId={menuId}
        />
      )}
    </div>
  );
}
