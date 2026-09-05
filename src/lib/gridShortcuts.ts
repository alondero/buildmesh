import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useMeshStore } from '../stores/meshStore';
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

// ---- Issue #998 — focus grid search ----

/**
 * Tauri global-shortcut binding for the `focus-grid-search` action. Lifted
 * out of App.tsx so the platform-branch logic (and, more importantly, the
 * macOS `⌘+⌥+F` two-modifier collision carve-out) can be unit-tested
 * directly without regexing App.tsx source.
 *
 * Why the macOS branch carries the extra `Alt+` modifier: bare `⌘+F` is
 * already taken by the terminal's find action (xterm's
 * `attachCustomKeyEventHandler` matches `Cmd+F` to the terminal's
 * `'find'` KeyAction). The Tauri global-shortcut plugin registers at the
 * OS level, so claiming `⌘+F` would beat the focus-level handler and every
 * `⌘+F` in an agent terminal would jump to the grid search instead of
 * opening the terminal's find bar. The two-modifier `⌘+⌥+F` follows the
 * readline-free two-modifier principle shared by the
 * `Ctrl/Cmd+Alt+Arrow*` grid-traversal bindings — no readline, terminal,
 * or other app shortcut uses two meta+alt modifiers together, so the
 * chord stays free.
 *
 * On Windows/Linux, bare `Ctrl+F` is free (no readline gesture uses it)
 * so no carve-out is needed.
 *
 * `as const` on the action string is required so the App.tsx handler's
 * `if (action === 'focus-grid-search')` type-narrows correctly; the
 * `key` field stays a plain string so the Tauri plugin accepts both
 * branches uniformly.
 */
export function buildFocusGridSearchBinding(isMac: boolean): {
  key: string;
  action: 'focus-grid-search';
} {
  return isMac
    ? { key: 'CommandOrControl+Alt+F', action: 'focus-grid-search' }
    : { key: 'CommandOrControl+F', action: 'focus-grid-search' };
}

// ---- Issue #1253 — Ctrl+T new-agent shortcut ----

/**
 * Ctrl+T (Cmd+T on macOS) spawns a fresh agent node on the active mesh, falling
 * back to the selected mesh when no node is active. Bound at the Tauri global
 * level (App.tsx wires the platform chord) so the gesture fires while an xterm
 * terminal has focus.
 *
 * Provider resolution (issue #1253):
 *   - active node exists → reuse its provider (preserves the pre-#1253 path
 *     so switching between nodes doesn't randomly switch the spawn provider)
 *   - no active node     → defer to `useMeshStore.getDefaultProvider`, which
 *     mirrors `resolve_default_provider`'s post-#538 `'claude'` fallback and
 *     applies per-mesh + app-wide overrides from the same precedence chain
 *     the sidebar `+` button uses.
 *
 * Routing through `selectProviderForMesh` (instead of calling
 * `createAgentNode` directly) is what preserves the issue #283 invariant —
 * the create→activate→select-mesh sequence lives in exactly one place.
 *
 * Pulled out of App.tsx's `handleShortcut` for unit-test parity with
 * `toggleGridMaximize` / `cycleGridMode`: the test file can drive the
 * resolution path directly without standing up the global-shortcut plugin or
 * the `shortcut-triggered` event bus. App.tsx still owns the platform
 * binding (`Ctrl+T` / `Cmd+T`), the `createShortcutGuard(300)` anti-burst
 * wrapper, and the input-focus guard — those are wiring concerns that don't
 * belong in a pure store mutator.
 */
export async function triggerNewAgentShortcut(): Promise<void> {
  const nodeStore = useAgentNodeStore.getState();
  const meshStore = useMeshStore.getState();
  const activeNode = nodeStore.getActiveNode();
  const meshId = activeNode?.mesh_id ?? meshStore.selectedMeshId;
  if (!meshId) return;
  const mesh = meshStore.meshesById.get(meshId);
  if (!mesh) return;
  const provider =
    activeNode?.provider ?? (await meshStore.getDefaultProvider(mesh.id));
  await nodeStore.selectProviderForMesh(mesh.id, mesh.name, mesh.path, provider, undefined);
}