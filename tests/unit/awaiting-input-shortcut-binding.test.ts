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
 * The Ctrl/Cmd+. binding for the "jump to next awaiting node" cycle (issue
 * #64), fired through the dispatcher (issue #2240).
 *
 * Tauri's global-shortcut plugin captures at the OS layer (before xterm.js sees
 * the keydown), so this binding fires even when an agent terminal has focus —
 * exactly the state the user is in when they notice a node is waiting for
 * input. The accelerator is `CommandOrControl+Period`: Tauri aliases `Period`
 * to the `.` key (via the underlying `global_hotkey` crate, which also accepts
 * a literal `.`), and `CommandOrControl` resolves to Cmd on macOS and Ctrl on
 * Windows/Linux, so one string covers both platforms.
 *
 * The previous version of this file read `src/App.tsx` and regexed for
 * `key:`-then-`action:` pairs plus a character-budget window between the branch
 * and its call site. That broke whenever the branch grew, and it never proved
 * the binding did anything. Now the action is fired and the store is read.
 */
describe('jump-to-next-awaiting dispatch (issue #64)', () => {
  beforeEach(() => {
    _resetShortcutRateGatesForTests();
    seedAgentNodes([]);
    useMeshStore.setState({ selectedMeshId: null });
    useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
  });

  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('binds CommandOrControl+Period → action jump-to-next-awaiting on both platforms', () => {
    for (const mac of [true, false]) {
      const binding = shortcutBindings(mac).find(b => b.action === 'jump-to-next-awaiting');
      expect(binding?.key, `mac=${mac}`).toBe('CommandOrControl+Period');
    }
  });

  it('uses CommandOrControl (not bare Ctrl/Cmd) so the binding matches both platforms', () => {
    // Regression guard: a future change that hard-codes `Ctrl+Period` would
    // leave macOS users without a way to cycle. The accelerator must use the
    // platform-aware `CommandOrControl` combinator.
    const keys = shortcutBindings(false).map(b => b.key);
    expect(keys).not.toContain('Ctrl+Period');
    expect(keys).not.toContain('Cmd+Period');
  });

  it('moves focus to the next awaiting node when the action is fired', () => {
    seedAgentNodes([
      makeAgentNode({ id: 1, mesh_id: 1, position: 0, status: 'running' }),
      makeAgentNode({ id: 2, mesh_id: 1, position: 1, status: 'awaiting_input' }),
    ], 1);

    expect(dispatchShortcut('jump-to-next-awaiting')).toBe('handled');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
  });

  it('leaves the active node alone when nothing is awaiting input', () => {
    // The "no-op" case must not steal focus from the user's current prompt.
    seedAgentNodes([makeAgentNode({ id: 1, mesh_id: 1, position: 0, status: 'running' })], 1);
    expect(dispatchShortcut('jump-to-next-awaiting')).toBe('handled');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(1);
  });
});
