import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import {
  _resetShortcutRateGatesForTests,
  dispatchShortcut,
  shortcutBindings,
} from '../../src/lib/shortcutDispatch';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { makeAgentNode } from './helpers/makeAgentNode';
import { seedAgentNodes } from './helpers/seedAgentNodes';

/**
 * The modifier combo bound to the four arrow-traversal actions, fired through
 * the dispatcher (issue #2240).
 *
 * The previous binding, `CommandOrControl+Arrow*`, was captured at the OS layer
 * by Tauri's global-shortcut plugin and stole the readline `backward-word` /
 * `forward-word` gesture (Ctrl+←/→ in bash, zsh, fish, PSReadLine, python/node
 * REPLs, the agent prompt, etc.) — every time the user tried to fix a typo in
 * the middle of a long prompt while a terminal had focus, the keystroke moved
 * the on-screen grid instead.
 *
 * The replacement adds a second modifier: `CommandOrControl+Alt+Arrow*`. No
 * readline or PTY gesture uses two modifiers, so the PTY still receives bare
 * Ctrl+Arrow for word movement. Same gesture as Windows Snap, tmux
 * `prefix+Arrow`, i3/bspwm, VS Code's "Move Editor Group".
 *
 * This file used to read `src/App.tsx` and regex the binding literals. It now
 * asserts the accelerator the table actually registers AND fires each
 * direction against a seeded grid, so a swapped direction (ArrowLeft bound to
 * `arrow-right`) fails on behaviour instead of on a substring.
 */
describe('grid traversal shortcut bindings (issue #2240)', () => {
  // Two nodes = one grid row (`getGridRows(2) === [2]`), so left/right wrap
  // between them and the assertions do not depend on the viewport width.
  const WIRING = [
    ['ArrowLeft', 'arrow-left'],
    ['ArrowRight', 'arrow-right'],
    ['ArrowUp', 'arrow-up'],
    ['ArrowDown', 'arrow-down'],
  ] as const;

  beforeEach(() => {
    _resetShortcutRateGatesForTests();
    seedAgentNodes([
      makeAgentNode({ id: 1, mesh_id: 1, position: 0 }),
      makeAgentNode({ id: 2, mesh_id: 1, position: 1 }),
    ], 1);
    useMeshStore.setState({ selectedMeshId: null });
    // 'all' shows every node regardless of mesh selection, so traversal is
    // about the keypress, not about scope resolution.
    useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
  });

  afterEach(() => {
    document.body.innerHTML = '';
  });

  it.each(WIRING)('binds Ctrl/Cmd+Alt+%s → action %s', (arrow, action) => {
    for (const mac of [true, false]) {
      const binding = shortcutBindings(mac).find(b => b.action === action);
      expect(binding?.key, `${action} on mac=${mac}`).toBe(`CommandOrControl+Alt+${arrow}`);
    }
  });

  it('does NOT bind the bare CommandOrControl+Arrow* form', () => {
    // Regression guard: a future change that drops the `Alt+` would re-steal
    // readline word-movement.
    const bare = /CommandOrControl\+Arrow(Left|Right|Up|Down)$/;
    for (const mac of [true, false]) {
      const keys = shortcutBindings(mac).map(b => b.key);
      expect(keys.filter(k => bare.test(k))).toEqual([]);
    }
  });

  it('moves focus right when arrow-right is fired', () => {
    expect(dispatchShortcut('arrow-right')).toBe('handled');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
  });

  it('moves focus left when arrow-left is fired', () => {
    expect(dispatchShortcut('arrow-left')).toBe('handled');
    // Wraps within the single row: from the first node, left lands on the last.
    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
  });

  it('exits Single view mode on the first press instead of moving focus (wayfinder #982)', () => {
    // In Single the canvas renders the active node, so exiting the mode IS the
    // move; `activeNodeId` must stay put so the user keeps looking at the node
    // they were reading.
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'all' });
    expect(dispatchShortcut('arrow-down')).toBe('handled');
    expect(useUIStore.getState().viewMode).toBe('all');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(1);
  });
});