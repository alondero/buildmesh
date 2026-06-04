import { useState } from 'react';
import type { Mesh } from '../../stores/meshStore';
import { ProviderDropdown, type ProviderEntry } from './ProviderDropdown';

interface NodeCreationFormProps {
  mesh: Mesh;
  isDropdownOpen: boolean;
  providers: ProviderEntry[];
  onToggleDropdown: (mesh: Mesh) => void;
  onSelectProvider: (mesh: Mesh, providerId: string, useWorktree?: boolean) => void;
  getDefaultProvider: (meshId: number) => Promise<string>;
}

export function NodeCreationForm({
  mesh,
  isDropdownOpen,
  providers,
  onToggleDropdown,
  onSelectProvider,
  getDefaultProvider,
}: NodeCreationFormProps) {
  const [defaultProviderId, setDefaultProviderId] = useState<string | null>(null);

  const refreshDefaultProvider = async () => {
    try {
      setDefaultProviderId(await getDefaultProvider(mesh.id));
    } catch {
      // Tooltip falls back to the generic label below if this fails.
    }
  };

  const defaultProviderLabel =
    providers.find(p => p.id === defaultProviderId)?.label ?? defaultProviderId;
  const addNodeTitle = defaultProviderLabel
    ? `Add agent node (${defaultProviderLabel})`
    : 'Add agent node';

  const handleAddNode = async (altKey: boolean) => {
    const defaultProvider = await getDefaultProvider(mesh.id);
    onSelectProvider(mesh, defaultProvider, !altKey);
  };

  return (
    <div className="relative">
      <div className="flex items-center rounded border border-accent-cyan/30 overflow-hidden">
        <button
          onClick={(e) => { e.stopPropagation(); handleAddNode(e.altKey); }}
          onMouseEnter={refreshDefaultProvider}
          onFocus={refreshDefaultProvider}
          className="flex items-center px-1.5 h-5 text-[12px] font-medium text-accent-cyan hover:bg-accent-cyan/15 transition-colors"
          title={addNodeTitle}
        >
          +
        </button>
        <span className="w-px h-3 bg-accent-cyan/30" />
        <button
          onClick={(e) => { e.stopPropagation(); onToggleDropdown(mesh); }}
          className={`flex items-center px-1 h-5 text-[11px] hover:bg-accent-cyan/15 transition-colors ${isDropdownOpen ? 'text-accent-cyan bg-accent-cyan/10' : 'text-accent-cyan/70'}`}
          title="Choose provider"
        >
          ▾
        </button>
      </div>
      {isDropdownOpen && (
        <ProviderDropdown
          meshId={mesh.id}
          providers={providers}
          onSelect={(providerId, altKey) => onSelectProvider(mesh, providerId, !altKey)}
        />
      )}
    </div>
  );
}
