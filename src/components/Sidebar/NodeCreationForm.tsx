import type { Mesh } from '../../stores/meshStore';
import { type SpawnOption } from '../../lib/groups';
import { dropdownId } from '../../lib/dropdownId';
import { SpawnButtonCluster } from './SpawnButtonCluster';

interface NodeCreationFormProps {
  mesh: Mesh;
  isDropdownOpen: boolean;
  /** A spawn for this mesh is in flight — forwarded to the cluster so the
   *  `+` shows "Spawning…" and both buttons disable (guards double-spawn). */
  isSpawning?: boolean;
  providers: SpawnOption[];
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
  isSpawning,
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
      // Issue #1264 — surface prefix keeps this menu's
      // `data-dropdown-for` from colliding with a node- or
      // terminal-keyed menu on the same numeric id.
      dropdownKey={dropdownId('mesh', mesh.id)}
      isOpen={isDropdownOpen}
      isSpawning={isSpawning}
      onToggleDropdown={() => onToggleDropdown(mesh)}
      onSpawnDefault={handleSpawnDefault}
      onSelectProvider={(providerId, altKey) =>
        onSelectProvider(mesh, providerId, altKey ? false : undefined)
      }
      getDefaultProvider={() => getDefaultProvider(mesh.id)}
    />
  );
}
