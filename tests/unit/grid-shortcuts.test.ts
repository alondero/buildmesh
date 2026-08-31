import { describe, it, expect, beforeEach } from 'vitest';
import { toggleGridMaximize, cycleGridMode, buildFocusGridSearchBinding } from '../../src/lib/gridShortcuts';
import { useUIStore } from '../../src/stores/uiStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import { seedAgentNodes } from './helpers/seedAgentNodes';

// Minimal valid AgentNode — we never invoke any backend; `getActiveNode` only
// scans the in-memory array. Mirrors the shape used by sibling shortcut tests
// (`tests/unit/grid-node-header.test.tsx`) so the layout stays familiar.
// `env` must be a valid `EnvType` (`'windows' | 'wsl'`); vitest transpiles
// tests with esbuild and doesn't strict-typecheck them, so a bogus value
// would slip past CI today — but the test stays correct if/when the
// vitest config adds typecheck.
const NODE: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: 'agent-7',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'claude',
  status: 'running',
  use_worktree: false,
  position: 0,
  created_at: '2026-06-27T00:00:00Z',
  scratchpad: '',
  sandbox: false,
  cli_session_id: null,
  worktree_name: null,
  source_issue: null,
  archived: false,
  is_pinned: false,
};

describe('toggleGridMaximize (#668 Alt+G / Cmd+G; View Modes wayfinder #982)', () => {
  beforeEach(() => {
    // Reset both stores to a known grid-mode baseline; tests opt-in to state.
    useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
    seedAgentNodes([]);
  });

  it('no-ops when there is no active node', () => {
    // Acceptance criterion #1: pressing Alt+G with no active node is a no-op,
    // not an error and not a phantom entry into Single.
    seedAgentNodes([NODE], null);
    toggleGridMaximize();
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  it('no-ops when there are no nodes at all', () => {
    toggleGridMaximize();
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  it('enters Single on the active node from a grid mode', () => {
    // Acceptance criterion #2: grid mode + active node â†’ Single solo view.
    // Single renders the active node, so the mode switch alone solos it —
    // there is no per-node id to assert anymore.
    seedAgentNodes([NODE], NODE.id);
    toggleGridMaximize();
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('restores the grid mode Single was entered from', () => {
    // Acceptance criterion #3: second press exits Single, regardless of
    // which node is active at the time — the restore path is keyed on the
    // mode, not the active node, so the user always gets out of whatever
    // solo view they're in.
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'pinned' });
    // Note: we deliberately leave `activeNodeId` pointing at a different
    // node to prove the restore doesn't depend on the active node matching.
    seedAgentNodes([NODE, { ...NODE, id: 9, name: 'agent-9', position: 1 }], 9);
    toggleGridMaximize();
    expect(useUIStore.getState().viewMode).toBe('pinned');
  });

  it('toggles back and forth: mesh â†’ single â†’ mesh â†’ single', () => {
    // Sequence covers all three acceptance criteria in one call chain.
    seedAgentNodes([NODE], NODE.id);

    toggleGridMaximize();
    expect(useUIStore.getState().viewMode).toBe('single');

    toggleGridMaximize();
    expect(useUIStore.getState().viewMode).toBe('mesh');

    toggleGridMaximize();
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('leaves the active node alone when restoring (passive Esc-style exit)', () => {
    // The Ctrl+Arrow "exit-and-move" path in App.tsx is the navigation
    // gesture. Alt+G is the *toggle* — it must not mutate `activeNodeId`,
    // because the user is in solo-view of the node they're already on.
    seedAgentNodes([NODE], NODE.id);
    toggleGridMaximize();
    toggleGridMaximize();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(NODE.id);
  });
});

describe('cycleGridMode (#987 Ctrl+Alt+G / Cmd+Alt+G view-mode cycle)', () => {
  beforeEach(() => {
    // The cycle is a pure store rotation — it never reads the node arrays, so
    // agentNodes stays empty. Only the UI store's mode state matters.
    useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
    seedAgentNodes([]);
  });

  it('advances Mesh â†’ Pinned â†’ All â†’ Mesh in switcher order', () => {
    cycleGridMode();
    expect(useUIStore.getState().viewMode).toBe('pinned');
    cycleGridMode();
    expect(useUIStore.getState().viewMode).toBe('all');
    cycleGridMode();
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  it('re-enters the cycle at lastNonSingleMode when currently in Single', () => {
    // From a solo view, the first Ctrl+Alt+G lands you back where you were
    // (the mode Single was entered from), not skipping past it — so it does
    // not double up with the *next* mode on the very first press.
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'pinned' });
    cycleGridMode();
    expect(useUIStore.getState().viewMode).toBe('pinned');
    // The subsequent press advances from there.
    cycleGridMode();
    expect(useUIStore.getState().viewMode).toBe('all');
  });

  it('records each grid mode it lands on as the Single restore target', () => {
    // Every non-single mode set updates lastNonSingleMode (uiStore.setViewMode),
    // so a later Alt+G solo/exit returns to wherever the cycle left the user.
    cycleGridMode(); // mesh â†’ pinned
    expect(useUIStore.getState().lastNonSingleMode).toBe('pinned');
    cycleGridMode(); // pinned â†’ all
    expect(useUIStore.getState().lastNonSingleMode).toBe('all');
  });

  it('never lands on Single — the solo toggle owns that gesture (Alt+G)', () => {
    for (let i = 0; i < 6; i++) {
      cycleGridMode();
      expect(useUIStore.getState().viewMode).not.toBe('single');
    }
  });
});

describe('buildFocusGridSearchBinding (issue #998 Ctrl+F / ⌘+⌥+F)', () => {
  // The binding is the one piece of cross-platform state in this PR that
  // is *not* a pure store mutator: it's a literal Tauri global-shortcut
  // shape that App.tsx hands to `useGlobalShortcuts`. We test it as a
  // runtime contract (input isMac → output binding) rather than regexing
  // App.tsx source, so a refactor that inlines the ternary, extracts a
  // config map, or rewrites the binding shape in any other way that
  // preserves behaviour will keep the tests green.
  //
  // The macOS branch is the only one with a non-trivial safety contract
  // (the `Alt+` carve-out avoids re-colliding with `term-find`'s bare
  // `⌘+F`), so that's the assertion the regression guard exists to make.
  // The Win/Linux branch is asserted too, so a future refactor that
  // accidentally drops the chord entirely (or inverts the platform
  // branches) is caught.

  it('binds bare Ctrl+F on Windows / Linux (no readline collision)', () => {
    const binding = buildFocusGridSearchBinding(false);
    expect(binding.action).toBe('focus-grid-search');
    expect(binding.key).toBe('CommandOrControl+F');
  });

  it('binds Cmd+Alt+F on macOS — the term-find collision carve-out', () => {
    // The carve-out: bare ⌘+F is claimed by xterm's terminal find action
    // (matched by Terminal.tsx's `attachCustomKeyEventHandler` to the
    // `'find'` KeyAction). A Tauri global-shortcut registration at the
    // OS level would beat the focus-level handler, so every ⌘+F in an
    // agent terminal would jump to the grid search instead of opening
    // the terminal's find bar. The two-modifier ⌘+⌥+F follows the
    // readline-free two-modifier principle shared by the
    // `Ctrl/Cmd+Alt+Arrow*` grid-traversal bindings — no readline,
    // terminal, or other app shortcut uses two meta+alt modifiers
    // together, so the chord stays free.
    const binding = buildFocusGridSearchBinding(true);
    expect(binding.action).toBe('focus-grid-search');
    expect(binding.key).toBe('CommandOrControl+Alt+F');
    // Pin the *exact* form of the carve-out so a future edit that
    // changes `Alt+` to `Shift+` (which IS a readline gesture — it
    // captures Ctrl+Shift+F in many shells) or to `Meta+` (which would
    // require a different `CommandOrControl+Meta+...` form Tauri may
    // not accept) is caught here rather than at runtime in a focused
    // agent terminal.
    expect(binding.key).not.toMatch(/\+Shift\+/);
  });

  it('tags the action as a string literal type so App.tsx dispatch narrows correctly', () => {
    // App.tsx's `if (action === 'focus-grid-search')` relies on the
    // action being a string-literal type, not a generic `string`. If
    // the helper widened the action to `string`, TypeScript would
    // raise `This comparison appears unintentional` (ts(2820)) in the
    // dispatch handler. A regression that widens the type would break
    // the compile, not the runtime — but it's still worth pinning
    // here so the next maintainer doesn't "fix" the `as const` away.
    const binding = buildFocusGridSearchBinding(false);
    // The exact literal is what we care about; assignability to a
    // narrower literal type proves the type-tag survived.
    const narrowed: 'focus-grid-search' = binding.action;
    expect(narrowed).toBe('focus-grid-search');
  });
});