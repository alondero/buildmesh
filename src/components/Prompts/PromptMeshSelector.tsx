import type { Mesh } from '../../types/generated/Mesh';
import { getMeshColor } from '../../lib/meshColors';

/** A launch destination, independent of the sidebar's inspection context. */
export function PromptMeshSelector({ meshes, mesh, disabled, onSelect }: {
  meshes: Mesh[]; mesh: Mesh | null; disabled?: boolean; onSelect: (mesh: Mesh) => void;
}) {
  return <span className="inline-flex min-w-0 max-w-full items-center gap-1">
    {mesh && <span aria-hidden="true" className="h-3 w-[3px] shrink-0 rounded-full"
      style={{ backgroundColor: getMeshColor(mesh.id, mesh.color).hex }} />}
    <select aria-label="Spawn in Mesh" title={mesh?.name ?? 'Select Mesh…'} value={mesh?.id ?? ''} disabled={disabled || meshes.length === 0}
      onChange={event => { const selected = meshes.find(item => item.id === Number(event.target.value)); if (selected) onSelect(selected); }}
      className="min-h-[24px] min-w-0 max-w-full rounded-sm bg-bg-input text-xs text-text-primary focus:outline-none focus-visible:ring-1 focus-visible:ring-accent-cyan disabled:opacity-40">
      <option value="" disabled>Select Mesh…</option>
      {meshes.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}
    </select>
  </span>;
}
