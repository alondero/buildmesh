import { useRef, useState } from 'react';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { MeshColorPicker } from './MeshColorPicker';
import { useMeshStore } from '../../stores/meshStore';

interface MeshRecolorModalProps {
  meshId: number;
  meshName: string;
  /** The mesh's current display hex (resolved colour, palette or custom). */
  currentColor: string;
  onClose: () => void;
}

/**
 * The sidebar swatch's "recolour" popover — the same colour picker as the
 * New-mesh modal, reused so creation and later editing stay visually
 * identical. Persists the chosen hex via `updateMeshColor` on Save.
 */
export function MeshRecolorModal({ meshId, meshName, currentColor, onClose }: MeshRecolorModalProps) {
  const updateMeshColor = useMeshStore((s) => s.updateMeshColor);
  const [color, setColor] = useState(currentColor);
  const [saving, setSaving] = useState(false);
  const saveButtonRef = useRef<HTMLButtonElement>(null);

  const handleSave = async () => {
    setSaving(true);
    await updateMeshColor(meshId, color);
    setSaving(false);
    onClose();
  };

  return (
    <Modal
      onClose={onClose}
      labelledBy="mesh-recolor-title"
      maxWidth="max-w-xs"
      defaultFocusRef={saveButtonRef}
    >
      <div className="flex items-start justify-between mb-4">
        <h2 id="mesh-recolor-title" className="text-sm font-semibold text-text-primary truncate">
          Colour for {meshName}
        </h2>
        <ModalCloseButton onClose={onClose} />
      </div>

      <div className="mb-5">
        <MeshColorPicker value={color} onChange={setColor} />
      </div>

      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onClose}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Cancel
        </button>
        <button
          ref={saveButtonRef}
          type="button"
          onClick={handleSave}
          disabled={saving}
          className="px-3 py-1.5 text-xs font-medium text-accent-cyan bg-accent-cyan/10 hover:bg-accent-cyan/20 border border-accent-cyan/20 rounded-md transition-colors disabled:opacity-50"
        >
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    </Modal>
  );
}
