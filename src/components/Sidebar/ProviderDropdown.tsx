import { ProviderIcon } from '../Providers/ProviderIcon';

export type ProviderEntry = { id: string; label: string; color: string; legacy?: boolean };

// Tailwind class map for provider badges. Stays in the frontend because Tailwind's
// purge tool needs static class strings — backend can't emit these dynamically.
// Backend ProviderInfo.color (hex) is not used here for the same reason.
// Kept exported for any external consumer that still wants the coloured dot.
export function colorClassForProvider(providerId: string): string {
  const map: Record<string, string> = {
    anthropic: 'bg-blue-500',
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
  // Dynamic harness profiles surface first; the hardcoded legacy enum providers
  // sit below a "Legacy" header so the migration (PRD #534 / issue #536) shows
  // both side-by-side without regressing the old options. An entry with no
  // `legacy` flag is treated as dynamic.
  const dynamic = providers.filter(p => !p.legacy);
  const legacy = providers.filter(p => p.legacy);

  const renderButton = (p: ProviderEntry) => (
    <button
      // Dynamic and legacy lists can carry the same id (e.g. "terminal"), so the
      // React key must namespace by group to stay unique.
      key={`${p.legacy ? 'legacy' : 'dyn'}-${p.id}`}
      onClick={(e) => { e.stopPropagation(); onSelect(p.id, e.altKey); }}
      className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
    >
      <ProviderIcon providerId={p.id} className="h-3.5 w-3.5" />
      {p.label}
    </button>
  );

  return (
    <div
      data-dropdown-for={meshId}
      className="absolute right-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded shadow-lg py-1 min-w-[140px]"
    >
      {dynamic.map(renderButton)}
      {legacy.length > 0 && (
        <div className="px-3 pt-1.5 pb-0.5 text-[10px] font-semibold uppercase tracking-wide text-text-muted border-t border-border-subtle mt-1">
          Legacy
        </div>
      )}
      {legacy.map(renderButton)}
    </div>
  );
}
