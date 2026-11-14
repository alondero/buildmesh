import { useRef, useState, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import type { Mesh } from '../../types/generated/Mesh';
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { getMeshColor } from '../../lib/meshColors';

export function PromptMeshPicker({ meshes, anchorRef, dropdownKey, onSelect, onClose }: {
  meshes: Mesh[]; anchorRef: RefObject<HTMLElement | null>; dropdownKey: string;
  onSelect: (mesh: Mesh) => void; onClose: () => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  useAnchoredPosition(anchorRef, root, true, { align: 'end', constrainToViewport: true });
  useAriaMenu({ rootRef: root, activeIndex, setActiveIndex, onClose });
  return createPortal(<div ref={root} role="menu" aria-label="Choose a Mesh to spawn in" data-dropdown-for={dropdownKey}
    className="fixed z-[100] min-w-[160px] max-w-[min(320px,calc(100vw-8px))] overflow-y-auto rounded-md border border-border-default bg-bg-overlay p-1 shadow-md">
    {meshes.map((mesh, index) => <button key={mesh.id} type="button" role="menuitem" tabIndex={activeIndex === index ? 0 : -1}
      onClick={() => onSelect(mesh)} className="flex min-h-[24px] w-full items-center gap-2 rounded-sm px-2 py-1 text-left text-xs text-text-primary hover:bg-bg-card-hover focus:bg-bg-card-hover focus:outline-none">
      <span aria-hidden="true" className="h-3 w-[3px] shrink-0 rounded-full" style={{ backgroundColor: getMeshColor(mesh.id, mesh.color).hex }} />
      <span className="min-w-0 break-words">{mesh.name}</span>
    </button>)}
  </div>, document.body);
}
