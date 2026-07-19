import { useAgentNodeStore } from '../stores/agentNodeStore';

/**
 * Cycle through agent nodes with `status === 'awaiting_input'` (issue #64).
 *
 * Wired to `CommandOrControl+.` (Ctrl+. on Windows/Linux, Cmd+. on macOS) so
 * the user can jump to whichever agent is waiting for their input without
 * scanning the grid visually. The binding lives on the Tauri global-shortcut
 * plugin — not a window keydown listener — because xterm.js intercepts every
 * window keydown while an agent terminal has focus, and the user is most
 * likely to press this when their cursor is already in a terminal prompt.
 *
 * Behaviour (matching the arrow-traversal handler in App.tsx for scope):
 *   - scope: only nodes in the same mesh as the currently-active node, in
 *     on-screen order (`position` ascending, same ordering the grid renders).
 *     This means switching meshes via the sidebar doesn't leak Ctrl+. into
 *     the previous mesh's awaiting nodes — the user has explicit context.
 *   - start: the next awaiting node AFTER the current active node's
 *     position. If the active node is itself awaiting, the next press
 *     moves to the *following* awaiting node, not back to itself — same
 *     convention as `git log --skip` / browser tab cycling.
 *   - wrap: when no later awaiting node exists, the search continues from
 *     the start of the list. The first press after wrapping may land on the
 *     active node again if it's the only awaiting one — that's intentional;
 *     "no awaiting nodes" is the only state where this fn returns null.
 *   - no-op cases (return null, no `setActiveNode` call):
 *       • no active node (user hasn't selected anything yet)
 *       • mesh has no nodes with `status === 'awaiting_input'`
 *
 * Pure store mutator (no DOM, no IPC, no platform branching) — the same
 * shape as `toggleGridMaximize` in src/lib/gridShortcuts.ts. Pulling this
 * out of App.tsx keeps the handler's cooldown + focus-guard platform-wiring
 * concerns separate from the testable cycle logic, which is what the
 * `gridShortcuts.test.ts` precedent established.
 */
export function nextAwaitingNodeId(): number | null {
  const state = useAgentNodeStore.getState();
  const activeNode = state.getActiveNode();
  if (!activeNode) return null;

  // Mesh-scope to the active node's mesh, in on-screen order (`position`
  // ascending, same ordering the grid renders). Guaranteed non-empty
  // because the active node itself must be in the filtered list — we
  // wouldn't be here if `getActiveNode()` returned null.
  const meshNodes = state.agentNodes
    .filter(s => s.mesh_id === activeNode.mesh_id)
    .sort((a, b) => a.position - b.position);

  const len = meshNodes.length;
  const currentIndex = meshNodes.findIndex(s => s.id === activeNode.id);

  // Walk forward from `currentIndex + 1` and return the first awaiting node
  // found. Falling off the end of the list means we wrap: scan from index 0
  // up to and including the original position. The `(i + 1) % len` offset
  // and `count <= len` bound combine to a single forward pass with implicit
  // wrap — no second loop, no set lookup. `count <= len` (not `<`) is the
  // subtle bit: the extra iteration re-examines the current index, which
  // returns the active node itself when it's the only awaiting one — i.e.
  // "I want the next awaiting node, but I'm already on it" → no-op fallback
  // to the same id rather than `null` (acceptance criterion: "no-op when
  // no nodes are awaiting input" — pressing Ctrl+. on the only awaiting
  // node is *not* that case, it's a deliberate re-focus).
  for (let count = 1; count <= len; count++) {
    const i = (currentIndex + count) % len;
    const candidate = meshNodes[i];
    if (candidate.status === 'awaiting_input') {
      return candidate.id;
    }
  }
  return null;
}

/**
 * Convenience entry point used by the App.tsx shortcut handler — runs the
 * pure cycle logic and applies the result via `setActiveNode`. Returning
 * the chosen id (or null) keeps the handler's "did anything happen?" branch
 * trivial for future observability hooks (toast, status-line counter, etc.)
 * without coupling this module to a logger.
 */
export function jumpToNextAwaitingNode(): number | null {
  const nextId = nextAwaitingNodeId();
  if (nextId !== null) {
    useAgentNodeStore.getState().setActiveNode(nextId);
  }
  return nextId;
}
