import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useUIStore } from '../stores/uiStore';
import type { NonSingleViewMode } from '../stores/uiStore';

/**
 * Toggle the on-screen agent-node grid between grid view and Single mode
 * (issue #668; migrated to View Modes in wayfinder #982 — Single subsumes
 * the old maximizedNodeId). Wired to Alt+G on Windows/Linux and Cmd+G on
 * macOS.
 *
 * Behaviour:
 *   - in Single mode               → restore the grid mode Single was
 *                                    entered from (`exitSingleMode`)
 *   - else if there is an active node → enter Single (it renders the
 *                                    active node, so the mode switch alone
 *                                    solos it)
 *   - else                         → no-op (nothing to solo)
 *
 * Pulled out of `App.tsx`'s shortcut handler so the three branches can be
 * unit-tested directly against the stores, without standing up the
 * `global-shortcut` plugin or a window event bus. The handler in App.tsx
 * still owns the platform-specific binding (`Alt+G` vs `Cmd+G`), the
 * `createShortcutGuard(300)` anti-spam wrapper, and the input-focus guard —
 * those are platform/wiring concerns that don't belong in a pure store
 * mutator. Deeper View-Mode shortcut redesign (cycling, traversal across
 * modes) is ticket #987's scope, not this migration's.
 */
export function toggleGridMaximize(): void {
  const ui = useUIStore.getState();
  if (ui.viewMode === 'single') {
    ui.exitSingleMode();
    return;
  }
  const activeNode = useAgentNodeStore.getState().getActiveNode();
  if (!activeNode) return;
  ui.setViewMode('single');
}

// The grid View Modes `cycleGridMode` rotates through, in ViewModeSwitcher
// order (ticket #987). 'single' is deliberately excluded — it's Alt+G's solo
// toggle (`toggleGridMaximize`), a separate gesture — so this stays a pure
// grid-mode rotation.
const GRID_MODE_CYCLE: readonly NonSingleViewMode[] = ['mesh', 'pinned', 'all'];

/**
 * Rotate the canvas through the grid View Modes: Mesh → Pinned → All → Mesh
 * (ticket #987). Bound to Ctrl+Alt+G / Cmd+Alt+G in App.tsx — a keyboard peer
 * to the mouse-only `ViewModeSwitcher`, kept distinct from Alt+G's Single solo
 * toggle (`toggleGridMaximize`).
 *
 * From 'single' (Alt+G's territory), re-enter the cycle at the mode Single was
 * opened from (`lastNonSingleMode`) rather than skipping past it — so the first
 * press out of a solo view lands you back where you were, and the next
 * advances. Pure store-mutator (like `toggleGridMaximize`), so App.tsx owns the
 * platform binding, focus guard, and cooldown.
 */
export function cycleGridMode(): void {
  const ui = useUIStore.getState();
  if (ui.viewMode === 'single') {
    ui.setViewMode(ui.lastNonSingleMode);
    return;
  }
  const idx = GRID_MODE_CYCLE.indexOf(ui.viewMode);
  ui.setViewMode(GRID_MODE_CYCLE[(idx + 1) % GRID_MODE_CYCLE.length]);
}