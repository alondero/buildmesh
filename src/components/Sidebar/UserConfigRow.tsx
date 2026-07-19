/**
 * UserConfigRow — issue #60. Sidebar entry that opens the User Config File
 * Explorer at the resolved ~/.claude directory.
 *
 * Why a row (not just a button)
 * -----------------------------
 * Visual parity with the meshes section: the User Config row should read
 * as "another thing the sidebar can browse," not "a one-off shortcut."
 * The row is purely presentational — it owns no state and never renders
 * children. The click handler reads visibility from `useUIStore` (the
 * single source of truth shared with the UserConfigPanel) and toggles it,
 * so the same action that opens can also re-open a previously-closed
 * panel without an extra "force open" branch at the call site.
 *
 * Why no DnD
 * ----------
 * User Config is not a mesh — there is no row to reorder, no children
 * to drag, no right-click context menu. Mirroring the meshes section's
 * drag-handle / reorder affordance would be decorative; the user config
 * row is a static entry. Skipping `useSortable` keeps the row a few
 * lines and avoids dragging the dnd-kit DndContext/SortableContext
 * down here for one element.
 */

import { FolderOpenIcon } from '../shared/FolderOpenIcon';
import { useUIStore } from '../../stores/uiStore';

export function UserConfigRow() {
  const toggleUserConfig = useUIStore((s) => s.toggleUserConfig);

  return (
    <div className="px-2 py-1 border-t border-border-subtle">
      <div className="flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-bg-card/50 transition-colors">
        <span
          aria-hidden="true"
          className="text-text-muted"
        >
          <FolderOpenIcon className="w-4 h-4" />
        </span>
        <span className="flex-1 text-sm font-sans text-text-secondary truncate">
          User Config
        </span>
        <button
          type="button"
          onClick={toggleUserConfig}
          aria-label="Open user config"
          title="Open user config"
          className="p-1 rounded-md text-text-muted hover:text-accent-cyan hover:bg-bg-card transition-colors"
        >
          <FolderOpenIcon className="w-4 h-4" />
        </button>
      </div>
    </div>
  );
}
