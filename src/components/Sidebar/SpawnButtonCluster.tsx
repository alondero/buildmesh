import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { ProviderDropdown } from './ProviderDropdown';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { clearDefaultProviderPromises } from '../../lib/providerCache';
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
  /**
   * Issue #1264 — pre-prefixed stable key for this cluster. Passed
   * through to `ProviderDropdown`'s `data-dropdown-for` attribute so
   * the shared `useClickOutside` hook can scope to a single cluster.
   * The caller is responsible for building the value via
   * `dropdownId(surface, id)` (e.g. `mesh-5`, `issue-3`,
   * `session-abc`) so the per-surface namespace can't collide with
   * another surface that happens to share the same numeric id. The
   * prop is renamed from the misleading `meshId` (it was never mesh-
   * specific — issues, PRs, and archived sessions all used it too) so
   * the prefix-contract is obvious at the call site. */
  dropdownKey: string;
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
  onSelectProvider: (providerId: string, altKey: boolean, configurationId?: string) => void;
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
  configurationsEnabled?: boolean;
  /** Optional footer rendered at the bottom of the open menu (e.g. the
   *  worktree-gated "Alt-click spawns in mesh root" hint the sidebar
   *  passes when `mesh.use_worktree` is true). Omitted everywhere else. */
  menuFooter?: ReactNode;
}

export function SpawnButtonCluster({
  providers,
  dropdownKey,
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
  configurationsEnabled = true,
  menuFooter,
}: SpawnButtonClusterProps) {
  // Cache the default provider id for the tooltip + quick-spawn icon so
  // we don't refetch on every render. The parent passes a fresh closure
  // each render, so it is mirrored into a ref and the refresh callback
  // stays stable (a direct effect dep would refetch on every render).
  const [defaultProviderId, setDefaultProviderId] = useState<string | null>(null);
  const getDefaultProviderRef = useRef(getDefaultProvider);
  getDefaultProviderRef.current = getDefaultProvider;

  const refreshDefaultProvider = useCallback(async () => {
    const fn = getDefaultProviderRef.current;
    if (!fn) return;
    try {
      setDefaultProviderId(await fn());
    } catch {
      // Tooltip falls back to the generic label below if this fails — the
      // spawn action's own resolution path (in `onSpawnDefault`) is the
      // authoritative one, so a tooltip-only miss is harmless.
    }
  }, []);

  // Fetch on mount so the icon shows without hovering first; re-fetch when
  // the menu opens so a default changed while it was closed is fresh.
  // Hover/focus still refresh via the handlers below.
  useEffect(() => {
    void refreshDefaultProvider();
  }, [refreshDefaultProvider]);
  useEffect(() => {
    if (isOpen) void refreshDefaultProvider();
  }, [isOpen, refreshDefaultProvider]);

  // Provider mutations anywhere (Settings upsert/remove, per-mesh or
  // app-wide default writes — all funnel through `provider-list-changed`)
  // evict the shared cache and re-resolve, so the icon tracks the live
  // default instead of the mount-time one.
  const handleProviderListChanged = useCallback(() => {
    clearDefaultProviderPromises();
    void refreshDefaultProvider();
  }, [refreshDefaultProvider]);
  useProviderListInvalidation(handleProviderListChanged);

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

  // Variant-B treatment: when the primary action is the default `+` spawn
  // idiom (not an overridden text label like Archive "Resume"), the main
  // half shows the resolved default harness icon and expands to its name
  // on hover/focus of the quick-spawn half only — never on menu open.
  // Before the default resolves it falls back to the bare `+` so the
  // button never shows a wrong icon.
  const iconMode = primaryLabel === '+';
  const defaultOption = defaultProviderId
    ? providers.find(p => p.id === defaultProviderId) ?? null
    : null;
  const defaultOptionLabel = defaultOption?.label ?? defaultProviderId;

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
    // `data-dropdown-for` on the root (not just the menu shell) scopes the
    // shared `useClickOutside` mousedown handler to the whole cluster: a
    // second chevron click must toggle via `onToggleDropdown`, not get
    // pre-closed by mousedown and re-opened by the click that follows it.
    <div className="relative" data-dropdown-for={dropdownKey}>
      <div className={`flex items-center rounded-md border overflow-hidden transition-colors ${isOpen ? 'border-accent-cyan shadow-glow-cyan' : 'border-accent-cyan/30'}`}>
        <button
          data-testid="spawn-default"
          onClick={(e) => { e.stopPropagation(); onSpawnDefault(e.altKey); }}
          onMouseEnter={refreshDefaultProvider}
          onFocus={refreshDefaultProvider}
          disabled={isDisabled}
          aria-label={primaryAriaLabel ?? undefined}
          className="group flex items-center justify-center px-2 min-w-[34px] h-[28px] text-xs font-medium text-accent-cyan hover:bg-accent-cyan/15 active:translate-y-px disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          title={isSpawning ? busyLabel : primaryTitle}
        >
          {isSpawning ? busyLabel : iconMode && defaultOption ? (
            <>
              <ProviderIcon
                providerId={defaultOption.provider_id ?? defaultOption.harness_id}
                className="h-3.5 w-3.5 shrink-0"
              />
              <span className="max-w-0 overflow-hidden whitespace-nowrap opacity-0 transition-all group-hover:max-w-[120px] group-hover:opacity-100 group-hover:ml-1.5 group-focus-visible:max-w-[120px] group-focus-visible:opacity-100 group-focus-visible:ml-1.5">
                {defaultOptionLabel}
              </span>
            </>
          ) : primaryLabel}
        </button>
        <span className="w-px h-5 bg-accent-cyan/30" />
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
          className={`flex items-center justify-center w-[30px] h-[28px] hover:bg-bg-card-hover active:translate-y-px disabled:opacity-50 disabled:cursor-not-allowed transition-colors ${isOpen ? 'text-accent-cyan bg-bg-card' : 'text-text-secondary'}`}
          title="Choose provider"
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true" className={`transition-transform ${isOpen ? 'rotate-180' : ''}`}>
            <path d="M2.5 4.5 6 8l3.5-3.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
      </div>
      {isOpen && !isSpawning && (
        <ProviderDropdown
          dropdownKey={dropdownKey}
          providers={providers}
          configurationsEnabled={configurationsEnabled}
          footer={menuFooter}
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
