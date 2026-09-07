import { formatError } from '../../lib/errorUtils';
import { useRef, useState } from 'react';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { MeshColorPicker } from './MeshColorPicker';
import { useMeshStore } from '../../stores/meshStore';
import { pickMeshFolder } from '../../lib/tauri';
import { defaultMeshColor } from '../../lib/meshColors';

interface MeshCreateModalProps {
  onClose: () => void;
  /**
   * Palette hex the colour picker starts on. Optional — when omitted
   * the modal derives its own default from the current mesh count
   * (`defaultMeshColor(meshes.length)`). The owning UI should pass an
   * explicit value only when the colour picker must mirror a
   * non-default palette seed (e.g. a programmatic open that pre-seeds
   * a specific colour for the next mesh).
   */
  defaultColor?: string;
}

/**
 * The "New mesh" modal. Replaces the old one-shot native-dialog-then-create
 * flow (`add_mesh`) with a two-step: pick a location, pick a colour, then
 * create. Scope is deliberately just these two properties (issue: mesh colour
 * picker) — more mesh-creation options may follow.
 */
export function MeshCreateModal({ onClose, defaultColor }: MeshCreateModalProps) {
  const createMesh = useMeshStore((s) => s.createMesh);
  const selectMesh = useMeshStore((s) => s.selectMesh);
  // The modal owns its own default-colour derivation (rather than the
  // parent reaching into `useMeshStore.getState().meshes.length`
  // inside the JSX render) so the abstraction boundary is correct —
  // App.tsx has no business calculating palette offsets. Senior-
  // review round 4: reading store state in JSX render was a known
  // anti-pattern (concurrent-render tear safety).
  const meshCount = useMeshStore((s) => s.meshes.length);
  const initialColor = defaultColor ?? defaultMeshColor(meshCount);

  const [folder, setFolder] = useState<{ path: string; name: string } | null>(null);
  const [color, setColor] = useState(initialColor);
  const [picking, setPicking] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const createButtonRef = useRef<HTMLButtonElement>(null);

  const handleChooseFolder = async () => {
    setPicking(true);
    setError(null);
    try {
      const picked = await pickMeshFolder();
      // null means the user cancelled the OS dialog — leave the prior
      // selection (if any) untouched rather than clearing it.
      if (picked) setFolder(picked);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setPicking(false);
    }
  };

  const handleCreate = async () => {
    if (!folder || creating) return;
    setCreating(true);
    setError(null);
    const mesh = await createMesh(folder.name, folder.path, color);
    setCreating(false);
    if (mesh) {
      selectMesh(mesh.id);
      onClose();
    } else {
      setError('Failed to create mesh. See logs for details.');
    }
  };

  return (
    <Modal
      onClose={onClose}
      labelledBy="mesh-create-title"
      maxWidth="max-w-md"
      closeOnBackdrop={false}
      defaultFocusRef={createButtonRef}
    >
      <div className="flex items-start justify-between mb-4">
        <h2 id="mesh-create-title" className="text-sm font-semibold text-text-primary">
          New mesh
        </h2>
        <ModalCloseButton onClose={onClose} />
      </div>

      {/* Location */}
      <div className="mb-4">
        <div className="text-2xs uppercase tracking-wide text-text-muted mb-1.5">Location</div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={handleChooseFolder}
            disabled={picking}
            className="px-3 py-1.5 text-xs font-medium text-accent-cyan bg-accent-cyan/10 hover:bg-accent-cyan/20 border border-accent-cyan/20 rounded-md transition-colors disabled:opacity-50"
          >
            {picking ? 'Choosing…' : folder ? 'Change folder…' : 'Choose folder…'}
          </button>
          {folder && <span className="text-xs font-medium text-text-primary truncate">{folder.name}</span>}
        </div>
        {folder && (
          <p className="mt-1.5 text-xs font-mono text-text-secondary bg-bg-card border border-border-subtle rounded-md px-2 py-1 break-all">
            {folder.path}
          </p>
        )}
      </div>

      {/* Colour */}
      <div className="mb-5">
        <div id="mesh-create-color-label" className="text-2xs uppercase tracking-wide text-text-muted mb-1.5">
          Colour
        </div>
        <MeshColorPicker value={color} onChange={setColor} labelId="mesh-create-color-label" />
      </div>

      {error && <p className="mb-3 text-xs text-status-error">{error}</p>}

      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onClose}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Cancel
        </button>
        <button
          ref={createButtonRef}
          type="button"
          onClick={handleCreate}
          disabled={!folder || creating}
          className="px-3 py-1.5 text-xs font-medium text-accent-cyan bg-accent-cyan/10 hover:bg-accent-cyan/20 border border-accent-cyan/20 rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {creating ? 'Creating…' : 'Create mesh'}
        </button>
      </div>
    </Modal>
  );
}
