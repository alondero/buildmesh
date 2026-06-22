import type { ProviderInfo } from '../../types/generated/ProviderInfo';
import { GroupedProviderMenu } from '../Providers/GroupedProviderMenu';

/**
 * Backwards-compatible subset of the full [`ProviderInfo`] wire shape. Kept
 * exported because the Sidebar, MeshItem, and NodeCreationForm consume it
 * for typed `providerList` plumbing — they need the `id` for the click
 * handler and the `label` / `color` for the row UI. The new grouped render
 * also uses `group_key` / `is_proxied` (passed through by the alias below).
 */
export type ProviderEntry = Pick<
  ProviderInfo,
  'id' | 'label' | 'color' | 'icon' | 'harness_id' | 'provider_id' | 'is_proxied' | 'group_key'
>;

// Tailwind class map for provider badges. Stays in the frontend because Tailwind's
// purge tool needs static class strings — backend can't emit these dynamically.
// Backend ProviderInfo.color (hex) is not used here for the same reason.
// Kept exported for any external consumer that still wants the coloured dot.
export function colorClassForProvider(providerId: string): string {
  const map: Record<string, string> = {
    anthropic: 'bg-blue-500',
    claude: 'bg-blue-500', // Detected Claude Code profile id (#534).
    minimax: 'bg-indigo-500',
    kimi: 'bg-cyan-500',
    agy: 'bg-emerald-500',
    opencode: 'bg-amber-500',
    terminal: 'bg-gray-500',
  };
  return map[providerId] ?? 'bg-gray-500';
}

interface ProviderDropdownProps {
  meshId: number;
  providers: ProviderEntry[];
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
      className="absolute right-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded shadow-lg min-w-[200px] max-h-[400px] overflow-y-auto"
    >
      <GroupedProviderMenu
        providers={providers as ProviderInfo[]}
        onSelect={onSelect}
      />
    </div>
  );
}
