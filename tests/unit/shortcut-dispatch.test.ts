/**
 * The dispatch table's own contract (issue #2240): fire an action id, get back
 * what it did.
 *
 * The old coverage for this wiring read `src/App.tsx` off disk at test time
 * and regexed it — which proved that some text existed near some other text,
 * not that a keystroke does anything. These tests drive
 * `dispatchShortcut(action)` against the real stores and assert the effect,
 * which is the whole point of the table-driven refactor: the binding, its
 * guards and its handler are one row, and a test can reach all three without
 * reading source.
 *
 * Per-action deep coverage lives in the sibling files:
 *   - `awaiting-input-shortcut-binding.test.ts` — the Ctrl/Cmd+. cycle
 *   - `grid-shortcut-binding.test.ts` — the arrow traversal bindings
 *   - `arrow-traversal-throttle.test.ts` — cooldown vs throttle semantics
 *   - `shortcut-catalog-binding.test.ts` — table ↔ catalog parity
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import {
  SHORTCUT_ACTIONS,
  _resetShortcutRateGatesForTests,
  dispatchShortcut,
  shortcutBindings,
} from '../../src/lib/shortcutDispatch';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';
import { makeAgentNode } from './helpers/makeAgentNode';
import { seedAgentNodes } from './helpers/seedAgentNodes';

// Full `Mesh` literal (same shape as `tests/unit/grid-shortcuts.test.ts`) so
// the fixture stays honest if the generated type gains a field.
const MESH: Mesh = {
  id: 1,
  name: 'buildmesh',
  path: '/repo',
  layout: 'grid',
  position: 0,
  created_at: '2026-01-01T00:00:00Z',
  build_command: null,
  run_command: null,
  model: null,
  effort: null,
  use_worktree: true,
  worktree_mode: null,
  default_provider: null,
  base_ref: 'main',
  scratchpad: '',
  sandbox: false,
  pre_spawn_pool_size: 1,
  color: null,
  root_build_command: null,
  root_run_command: null,
};

/** A real focused `<input>`, so `isTextInputFocused()` sees what a user sees. */
function focusATextInput(): void {
  const input = document.createElement('input');
  document.body.appendChild(input);
  input.focus();
}

describe('dispatchShortcut — the table-driven shortcut dispatcher', () => {
  beforeEach(() => {
    _resetShortcutRateGatesForTests();
    seedAgentNodes([]);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({
      viewMode: 'mesh',
      lastNonSingleMode: 'mesh',
      cheatsheetOpen: false,
      omnibarOpen: false,
      omnibarMode: 'files',
      focusGridSearchRequest: 0,
      gridSearchQuery: '',
    });
  });

  afterEach(() => {
    document.body.innerHTML = '';
    vi.restoreAllMocks();
  });

  it('reports unknown-action for an id no row claims', () => {
    expect(dispatchShortcut('no-such-action')).toBe('unknown-action');
  });

  it('reports unknown-action for the context-bound catalog actions (they are not dispatched here)', () => {
    // The cheatsheet advertises terminal/modal/grid-search rows too. Firing
    // them through the app-level dispatcher must say "not mine" rather than
    // silently claiming to handle a gesture that lives on an xterm instance
    // or a component's own key handler.
    for (const action of ['term-copy', 'term-find', 'zoom-in', 'close-modal', 'clear-grid-search']) {
      expect(dispatchShortcut(action), `${action} must not be app-dispatched`).toBe('unknown-action');
    }
  });

  it('opens the omnibar in files mode for open-omnibar and commands mode for open-omnibar-commands', () => {
    // Issue #1409 — the K/P distinction is carried in the action id, so the
    // palette can preselect its mode. Firing the actions proves the mapping
    // the binding table declares.
    expect(dispatchShortcut('open-omnibar')).toBe('handled');
    expect(useUIStore.getState().omnibarOpen).toBe(true);
    expect(useUIStore.getState().omnibarMode).toBe('files');

    // The activation chord closes an open modal (`toggleOmnibar`), so reset
    // before checking the second action's mode.
    useUIStore.setState({ omnibarOpen: false, omnibarMode: 'files' });
    expect(dispatchShortcut('open-omnibar-commands')).toBe('handled');
    expect(useUIStore.getState().omnibarMode).toBe('commands');
  });

  it('toggles the cheatsheet for open-cheatsheet', () => {
    expect(dispatchShortcut('open-cheatsheet')).toBe('handled');
    expect(useUIStore.getState().cheatsheetOpen).toBe(true);
  });

  it('enters Single on the active node for toggle-maximize-grid (issue #668)', () => {
    seedAgentNodes([makeAgentNode({ id: 7, mesh_id: 1, position: 0 })], 7);
    expect(dispatchShortcut('toggle-maximize-grid')).toBe('handled');
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('rotates the grid View Modes for cycle-grid-modes (ticket #987)', () => {
    expect(dispatchShortcut('cycle-grid-modes')).toBe('handled');
    expect(useUIStore.getState().viewMode).toBe('pinned');
  });

  it('lands the canvas in the Filtered view and bumps the focus request for focus-grid-search (issue #998)', () => {
    // #1609: the search controls only exist in Filtered, so the action flips
    // the mode AND requests focus in one store action. Bumping the counter is
    // the whole imperative channel — GridControls' layout effect reads it.
    expect(dispatchShortcut('focus-grid-search')).toBe('handled');
    expect(useUIStore.getState().viewMode).toBe('filtered');
    expect(useUIStore.getState().focusGridSearchRequest).toBe(1);
  });

  it('spawns through the mesh provider path for new-agent (issue #1253)', async () => {
    const selectSpy = vi
      .spyOn(useAgentNodeStore.getState(), 'selectProviderForMesh')
      .mockResolvedValue({ id: 42 } as unknown as AgentNode);
    vi.spyOn(useMeshStore.getState(), 'getDefaultProvider').mockResolvedValue('claude');

    expect(dispatchShortcut('new-agent')).toBe('handled');
    // The handler is async, so the assertion waits for the microtask that
    // resolves the provider before the spawn call is made.
    await vi.waitFor(() => expect(selectSpy).toHaveBeenCalledTimes(1));
    expect(selectSpy.mock.calls[0].slice(0, 3)).toEqual([MESH.id, MESH.name, MESH.path]);
  });

  it('drops a focus-guarded action while a text input holds focus', () => {
    // Alt+G must not collapse the grid behind an inline rename. The outcome
    // is what lets App's `?` listener decide whether to preventDefault without
    // duplicating the guard.
    seedAgentNodes([makeAgentNode({ id: 7, mesh_id: 1, position: 0 })], 7);
    focusATextInput();
    expect(dispatchShortcut('toggle-maximize-grid')).toBe('blocked-focus');
    expect(dispatchShortcut('cycle-grid-modes')).toBe('blocked-focus');
    expect(dispatchShortcut('open-cheatsheet')).toBe('blocked-focus');
    expect(useUIStore.getState().viewMode).toBe('mesh');
    expect(useUIStore.getState().cheatsheetOpen).toBe(false);
  });

  it('does NOT focus-guard the actions that must fire from a terminal prompt', () => {
    // `isTextInputFocused` carves out the xterm helper textarea, and the `?`
    // listener checks `isTerminalFocused` itself — but the awaiting-input
    // cycle has no focus guard at all, because the user is *most* likely at a
    // terminal prompt when they notice a node needs input.
    seedAgentNodes([
      makeAgentNode({ id: 1, mesh_id: 1, position: 0, status: 'awaiting_input' }),
      makeAgentNode({ id: 2, mesh_id: 1, position: 1, status: 'awaiting_input' }),
    ], 1);
    focusATextInput();
    expect(dispatchShortcut('jump-to-next-awaiting')).toBe('handled');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
    expect(dispatchShortcut('open-omnibar')).toBe('handled');
  });
});

describe('shortcutBindings — what App registers with the Tauri plugin', () => {
  it('registers exactly the table rows that have an accelerator', () => {
    const withAccelerator = SHORTCUT_ACTIONS.filter(s => s.accelerator !== null);
    const bindings = shortcutBindings(false);
    expect(bindings).toHaveLength(withAccelerator.length);
    expect(bindings.map(b => b.action).sort()).toEqual(withAccelerator.map(s => s.action).sort());
  });

  it('never registers two actions on the same accelerator', () => {
    // A duplicate accelerator means the second registration loses (or the
    // plugin errors) and one shortcut silently dies — worth a hard gate now
    // that the binding list is derived rather than hand-listed.
    for (const mac of [true, false]) {
      const keys = shortcutBindings(mac).map(b => b.key);
      expect(new Set(keys).size, `duplicate accelerator on mac=${mac}`).toBe(keys.length);
    }
  });

  it('omits the cheatsheet `?` row, which is a window keydown gesture', () => {
    // `?` has no single accelerator shape across layouts, so App dispatches
    // `open-cheatsheet` from a keydown listener instead of registering it.
    expect(shortcutBindings(false).map(b => b.action)).not.toContain('open-cheatsheet');
  });
});