import { GroupedProviderMenu } from '../Providers/GroupedProviderMenu';
import type { SpawnOption } from '../../lib/groups';

// `SpawnOption` is the frontend view of the Spawn Option wire shape
// (issue #583) — produced by `mapBackendProviders` and consumed by the
// harness-grouped render. `GroupedProviderMenu` now consumes `SpawnOption`
// directly (not `ProviderInfo`) so the four desktop spawn surfaces
// share one frontend view and the boundary no longer needs a cast.

interface ProviderDropdownProps {
  meshId: number;
  providers: SpawnOption[];
  onSelect: (providerId: string, altKey: boolean) => void;
}

export function ProviderDropdown({ meshId, providers, onSelect }: ProviderDropdownProps) {
  // Issue #575 / ADR-0016 — render the harness-grouped, always-expanded
  // Spawn Menu. The single backend-derived list (issue #538 retired the
  // legacy enum-backed rows) is now grouped by `group_key` (== `harness_id`):
  // each harness's native row lands as a clickable header, every Proxied
  // child renders indented below it. Terminal is pinned last by the
  // backend's `order_providers` sort, so the visual order is identical
  // to the stored harness order.
  return (
    <div
      data-dropdown-for={meshId}
      className="absolute right-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded-md shadow-lg min-w-[200px] max-h-[400px] overflow-y-auto"
    >
      <GroupedProviderMenu providers={providers} onSelect={onSelect} />
    </div>
  );
}
