import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useUIStore } from '../stores/uiStore';

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