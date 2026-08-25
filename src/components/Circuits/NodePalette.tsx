/**
 * NodePalette — the editor's floating authoring palette (issue #1209).
 *
 * Grouped by category (Triggers cyan, Actions green, Gates amber,
 * Joins violet). Items drag onto the canvas (HTML5 dnd — React Flow's
 * `onDrop` reads the payload) and also click-to-add for keyboard users.
 */

import { NODE_SPECS, categoryAccent, type NodeCategory } from './circuitGraphModel';

export const PALETTE_MIME = 'application/x-buildmesh-circuit-node';

const CATEGORY_ORDER: Array<{ id: NodeCategory; title: string }> = [
  { id: 'trigger', title: 'Triggers' },
  { id: 'action', title: 'Actions' },
  { id: 'gate', title: 'Gates' },
  { id: 'join', title: 'Joins' },
];

interface NodePaletteProps {
  onAdd: (discriminator: string) => void;
}

export function NodePalette({ onAdd }: NodePaletteProps) {
  return (
    <div
      data-testid="circuit-palette"
      className="absolute z-20 top-3 left-3 w-44 max-h-[calc(100%-24px)] overflow-y-auto rounded-md border border-border-subtle bg-bg-overlay/95 backdrop-blur p-1.5 shadow-lg"
    >
      {CATEGORY_ORDER.map(({ id, title }) => {
        const accent = categoryAccent(id);
        return (
          <div key={id} className="mb-1">
            <div className={`text-2xs uppercase tracking-wide font-semibold ${accent.text} px-1 py-0.5`}>
              {title}
            </div>
            {NODE_SPECS.filter((s) => s.category === id).map((spec) => (
              <button
                key={spec.discriminator}
                type="button"
                draggable
                onDragStart={(e) => {
                  e.dataTransfer.setData(PALETTE_MIME, spec.discriminator);
                  e.dataTransfer.effectAllowed = 'move';
                }}
                onClick={() => onAdd(spec.discriminator)}
                data-testid={`palette-add-${spec.discriminator}`}
                className={`
                  w-full text-left px-1.5 py-0.5 mb-px rounded-sm text-xs
                  text-text-secondary hover:bg-bg-card-hover hover:text-text-primary cursor-grab
                  border-l-2 ${accent.border}
                `}
              >
                {spec.label}
              </button>
            ))}
          </div>
        );
      })}
    </div>
  );
}
