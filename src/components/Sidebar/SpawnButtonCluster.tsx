import { useState } from 'react';
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
   *  a single cluster when many rows share the same page. For meshes this
   *  is `mesh.id`; for probe rows it is the issue/PR number. */
  meshId: number;
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
   *  tooltip is non-critical and the `+` click already triggers the same
   *  fetch via `onSpawnDefault`. */
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

  const defaultProviderLabel =
    providers.find(p => p.id === defaultProviderId)?.label ?? defaultProviderId;
  const addNodeTitle = defaultProviderLabel
    ? `Add agent node (${defaultProviderLabel})`
    : 'Add agent node';

  const isDisabled = disabled || isSpawning;

  return (
    <div className="relative">
      <div className="flex items-center rounded-md border border-accent-cyan/30 overflow-hidden">
        <button
          data-testid="spawn-default"
          onClick={(e) => { e.stopPropagation(); onSpawnDefault(e.altKey); }}
          onMouseEnter={refreshDefaultProvider}
          onFocus={refreshDefaultProvider}
          disabled={isDisabled}
          className="flex items-center px-1.5 h-5 text-[12px] font-medium text-accent-cyan hover:bg-accent-cyan/15 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          title={isSpawning ? 'Spawning...' : addNodeTitle}
        >
          {isSpawning ? 'Spawning...' : '+'}
        </button>
        <span className="w-px h-3 bg-accent-cyan/30" />
        <button
          data-testid="spawn-dropdown-toggle"
          onClick={(e) => { e.stopPropagation(); onToggleDropdown(); }}
          disabled={isDisabled}
          className={`flex items-center px-1 h-5 text-[11px] hover:bg-accent-cyan/15 disabled:opacity-50 disabled:cursor-not-allowed transition-colors ${isOpen ? 'text-accent-cyan bg-accent-cyan/10' : 'text-accent-cyan/70'}`}
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
        />
      )}
    </div>
  );
}
