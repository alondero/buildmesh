import type { Mesh } from '../../stores/meshStore';
import { type ProviderEntry } from './ProviderDropdown';
import { SpawnButtonCluster } from './SpawnButtonCluster';

interface NodeCreationFormProps {
  mesh: Mesh;
  isDropdownOpen: boolean;
  providers: ProviderEntry[];
  onToggleDropdown: (mesh: Mesh) => void;
  onSelectProvider: (mesh: Mesh, providerId: string, useWorktree?: boolean) => void;
  getDefaultProvider: (meshId: number) => Promise<string>;
}

/**
 * Sidebar mesh-row wrapper around the canonical `SpawnButtonCluster`
 * (ADR-0016 §2). Keeps the mesh-specific surface here — `Mesh` prop,
 * alt-click-to-spawn-in-mesh-root override, the `getDefaultProvider`
 * mesh-scope — and delegates the visual + dropdown wiring to the cluster
 * so the Issues / PRs probe rows can render the same `+ ▾` pair.
 */
export function NodeCreationForm({
  mesh,
  isDropdownOpen,
  providers,
  onToggleDropdown,
  onSelectProvider,
  getDefaultProvider,
}: NodeCreationFormProps) {
  // Pass `undefined` on a normal click so the backend falls back to
  // mesh.use_worktree (the authoritative DB column on the mesh row).
  // Alt-click is the explicit override to spawn the node in the mesh root,
  // regardless of the mesh default.
  const handleSpawnDefault = async (altKey: boolean) => {
    const defaultProvider = await getDefaultProvider(mesh.id);
    onSelectProvider(mesh, defaultProvider, altKey ? false : undefined);
  };

  return (
    <SpawnButtonCluster
      providers={providers}
      meshId={mesh.id}
      isOpen={isDropdownOpen}
      onToggleDropdown={() => onToggleDropdown(mesh)}
      onSpawnDefault={handleSpawnDefault}
      onSelectProvider={(providerId, altKey) =>
        onSelectProvider(mesh, providerId, altKey ? false : undefined)
      }
      getDefaultProvider={() => getDefaultProvider(mesh.id)}
    />
  );
}
