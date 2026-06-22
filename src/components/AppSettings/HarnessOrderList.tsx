import { DndContext, type DragEndEvent } from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
  useSortable,
  arrayMove,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { ProviderIcon } from '../Providers/ProviderIcon';
import type { ProviderInfo } from '../../lib/tauri';

/**
 * Drag-to-reorder list for the spawn-menu harness rows (issue #573 / ADR-0016).
 * Mirrors the mesh-reorder `@dnd-kit` pattern in `Sidebar.tsx` / `MeshItem.tsx`.
 *
 * The rows are the same `ProviderInfo` entries every spawn surface renders, so
 * reordering here reorders the menu everywhere (the backend `order_providers`
 * applies the persisted id order). `Terminal` is excluded — it's pinned last by
 * the backend and isn't user-orderable.
 */

/** Pure: move `activeId` to where `overId` sits, returning the new id order.
 *  Exposed so the reorder math can be unit-tested without simulating a drag
 *  (jsdom can't fire real pointer drags through dnd-kit). */
export function reorderIds(ids: string[], activeId: string, overId: string): string[] {
  const from = ids.indexOf(activeId);
  const to = ids.indexOf(overId);
  if (from === -1 || to === -1 || from === to) return ids;
  return arrayMove(ids, from, to);
}

function HarnessRow({ provider }: { provider: ProviderInfo }) {
  const { setNodeRef, transform, transition, isDragging, attributes, listeners } =
    useSortable({ id: provider.id });
  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };
  return (
    <div
      ref={setNodeRef}
      style={style}
      className="flex items-center gap-3 border border-border-subtle rounded px-3 py-2 bg-bg-card"
    >
      <span
        {...attributes}
        {...listeners}
        className="text-text-muted hover:text-text-secondary cursor-grab active:cursor-grabbing text-[10px] select-none"
        title="Drag to reorder"
        aria-label={`Reorder ${provider.label}`}
      >
        ⋮⋮
      </span>
      <ProviderIcon providerId={provider.id} className="h-5 w-5" />
      <span className="text-base text-text-primary">{provider.label}</span>
    </div>
  );
}

export function HarnessOrderList({
  providers,
  onReorder,
}: {
  providers: ProviderInfo[];
  onReorder: (order: string[]) => void;
}) {
  // Terminal is pinned last by the backend, so it's never part of the orderable
  // rows.
  const rows = providers.filter(p => p.id !== 'terminal');
  // Nothing meaningful to drag with fewer than two rows.
  if (rows.length < 2) return null;

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const next = reorderIds(rows.map(p => p.id), active.id as string, over.id as string);
    onReorder(next);
  };

  return (
    <DndContext onDragEnd={handleDragEnd}>
      <SortableContext items={rows.map(p => p.id)} strategy={verticalListSortingStrategy}>
        <div className="space-y-2">
          {rows.map(p => (
            <HarnessRow key={p.id} provider={p} />
          ))}
        </div>
      </SortableContext>
    </DndContext>
  );
}
