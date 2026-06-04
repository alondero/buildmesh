import { ProviderIcon } from '../Providers/ProviderIcon';

export type ProviderEntry = { id: string; label: string; color: string };

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
  };
  return map[providerId] ?? 'bg-gray-500';
}

interface ProviderDropdownProps {
  meshId: number;
  providers: ProviderEntry[];
  onSelect: (providerId: string) => void;
}

export function ProviderDropdown({ meshId, providers, onSelect }: ProviderDropdownProps) {
  return (
    <div
      data-dropdown-for={meshId}
      className="absolute right-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded shadow-lg py-1 min-w-[140px]"
    >
      {providers.map(p => (
        <button
          key={p.id}
          onClick={(e) => { e.stopPropagation(); onSelect(p.id); }}
          className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
        >
          <ProviderIcon providerId={p.id} className="h-3.5 w-3.5" />
          {p.label}
        </button>
      ))}
    </div>
  );
}
