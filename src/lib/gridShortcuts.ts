import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useUIStore } from '../stores/uiStore';

/**
 * Toggle the on-screen agent-node grid between split-view and solo-view
 * (issue #668). Wired to Alt+G on Windows/Linux and Cmd+G on macOS.
 *
 * Behaviour:
 *   - if a node is currently maximized → restore the grid
 *   - else if there is an active node   → maximize it
 *   - else                              → no-op (nothing to solo)
 *
 * Pulled out of `App.tsx`'s shortcut handler so the three branches can be
 * unit-tested directly against the stores, without standing up the
 * `global-shortcut` plugin or a window event bus. The handler in App.tsx
 * still owns the platform-specific binding (`Alt+G` vs `Cmd+G`), the
 * `createShortcutGuard(300)` anti-spam wrapper, and the input-focus guard —
 * those are platform/wiring concerns that don't belong in a pure store
 * mutator.
 */
export function toggleGridMaximize(): void {
  const maximizedId = useUIStore.getState().maximizedNodeId;
  if (maximizedId !== null) {
    useUIStore.getState().clearMaximizedNode();
    return;
  }
  const activeNode = useAgentNodeStore.getState().getActiveNode();
  if (!activeNode) return;
  // `toggleMaximizedNode` is the right store action even though we're
  // setting rather than toggling: with `maximizedNodeId === null` it
  // becomes a pure "maximize this id" call, and reusing it keeps the
  // "maximize/restore is the same action" invariant in one place.
  useUIStore.getState().toggleMaximizedNode(activeNode.id);
}