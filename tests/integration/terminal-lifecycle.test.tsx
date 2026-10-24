/**
 * Terminal lifetime across an active-node switch (issue #2534).
 *
 * The Session → AgentNode rename left `tests/integration/session-switching.test.tsx`
 * behind. It imported `src/stores/sessionStore` — a module the rename deleted —
 * so the file could not have been running against the current tree, and what it
 * asserted was a mock store compared against itself
 * (`expect(store.getState().activeSessionId).toBe(store.getState().activeSessionId)`).
 * No test that could run against the current tree covered the behaviour it was
 * supposed to check.
 *
 * This file replaces it with that behaviour, asserted against the REAL
 * `TerminalManager` registry (`src/components/Terminal/Terminal.tsx`) rather
 * than a mocked store:
 *
 *   - mounting two Agent Nodes yields two TerminalManager entries;
 *   - switching the active node focuses the newly active terminal, leaves the
 *     other one alone, and disposes no terminal;
 *   - a React remount re-attaches the SAME terminal (the singleton survives);
 *   - deleting a node disposes its own terminal.
 *
 * The rule this pins is the terminal-persistence rule in
 * `docs/development/terminals.md` § "Terminal Persistence (CRITICAL)": a
 * terminal is owned by the registry and is disposed only when its node is
 * deleted. The assertions read the registry, not rendered CSS.
 *
 * Run with: npm test -- --run tests/integration/terminal-lifecycle.test.tsx
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, act, waitFor } from '@testing-library/react';
import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

// jsdom doesn't ship ResizeObserver; TerminalRegistry.attachToDOM wires one up
// for live fit on container resize. Provide a no-op stub.
if (!('ResizeObserver' in globalThis)) {
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

// `vi.hoisted` runs before the imports — necessary because importing
// `terminalManager` triggers the TerminalRegistry constructor, which eagerly
// subscribes to Tauri events (issue #332 / agent-spawned reconcile). Without
// hoisting, the factory's closure over `mockListeners` would hit TDZ when the
// constructor calls listen() during module load.
const mockListeners = vi.hoisted(() => new Map<string, Set<(...args: unknown[]) => void>>());

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation((event: string, callback: (event: { payload: unknown }) => void) => {
    if (!mockListeners.has(event)) mockListeners.set(event, new Set());
    mockListeners.get(event)!.add(callback as (...args: unknown[]) => void);
    return Promise.resolve(() => mockListeners.get(event)?.delete(callback as (...args: unknown[]) => void));
  }),
  emit: vi.fn().mockImplementation((event: string, payload?: unknown) => {
    mockListeners.get(event)?.forEach(cb => cb({ payload }));
    return Promise.resolve();
  }),
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('@xterm/xterm', () => {
  class MockTerminal {
    write = vi.fn((_data?: unknown, callback?: () => void) => {
      callback?.();
    });
    onData = vi.fn();
    onTitleChange = vi.fn();
    onResize = vi.fn();
    onScroll = vi.fn().mockReturnValue({ dispose: vi.fn() });
    // Real xterm's open() inserts a `.xterm` div into the container. The
    // registry re-parents that element on re-attach instead of opening a
    // second terminal, so `open` doubles as the "a new xterm was created"
    // signal this file asserts on.
    open = vi.fn((container: HTMLElement) => {
      const el = document.createElement('div');
      el.className = 'xterm';
      container.appendChild(el);
      this.element = el;
    });
    dispose = vi.fn();
    focus = vi.fn();
    loadAddon = vi.fn();
    attachCustomKeyEventHandler = vi.fn();
    scrollToBottom = vi.fn();
    refresh = vi.fn();
    clear = vi.fn();
    selectAll = vi.fn();
    hasSelection = vi.fn().mockReturnValue(false);
    getSelection = vi.fn().mockReturnValue('');
    paste = vi.fn();
    buffer = { active: { getWindow: vi.fn(), viewportY: 0, baseY: 0 } };
    unicode = {
      register: vi.fn(),
      get activeVersion() { return '11'; },
      set activeVersion(_v: string) { /* noop */ },
    };
    _core = {
      unicodeService: {
        _providers: { '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 } },
        register: vi.fn(),
      },
    };
    rows = 24;
    cols = 80;
    options: Record<string, unknown> = { fontSize: 10 };
    element: HTMLElement | null = null;
    constructor(_options?: unknown) {}
  }
  return { Terminal: MockTerminal };
});

vi.mock('@xterm/addon-fit', () => {
  class MockFitAddon {
    fit = vi.fn();
    dispose = vi.fn();
    proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 });
  }
  return { FitAddon: MockFitAddon };
});

vi.mock('@xterm/addon-serialize', () => ({
  SerializeAddon: class { serialize = vi.fn().mockReturnValue(''); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-search', () => ({
  SearchAddon: class { findNext = vi.fn(); findPrevious = vi.fn(); clearDecorations = vi.fn(); dispose = vi.fn(); },
}));
vi.mock('@xterm/addon-web-links', () => ({
  WebLinksAddon: class { dispose = vi.fn(); constructor(_h?: unknown) {} },
}));
vi.mock('@xterm/addon-unicode11', () => ({
  Unicode11Addon: class { dispose = vi.fn(); },
}));

import { AgentTerminal, terminalManager } from '../../src/components/Terminal/Terminal';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from '../unit/helpers/seedAgentNodes';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const NODE_ONE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  cli_session_id: null,
  worktree_name: null,
  use_worktree: false,
  is_pinned: false,
  source_issue: null,
  source_pr: null,
  head_repo_owner: null,
  head_repo_clone_url: null,
  source_pr_pinned_sha: null,
  signal_health: null,
  position: 0,
  created_at: new Date(0).toISOString(),
  worktree_path: null,
};

const NODE_TWO: AgentNode = {
  ...NODE_ONE,
  id: 2,
  name: 'agent-2',
  position: 1,
};

/**
 * One pane per node, mirroring `NodeCard`'s contract with the terminal
 * (`src/components/AgentNodeView/NodeCard.tsx`): the pane hands the terminal a
 * monotonically changing `focusRequest` when its node becomes active, and
 * `AgentTerminal` is keyed by node id so the terminal is remounted rather than
 * rebuilt when the selection inside a card changes.
 */
function TerminalPane({ nodeId }: { nodeId: number }) {
  const node = useAgentNodeStore(s => s.nodesById[nodeId]);
  const activeNodeId = useAgentNodeStore(s => s.activeNodeId);
  const [focusRequest, setFocusRequest] = useState(0);

  useEffect(() => {
    if (activeNodeId === nodeId) setFocusRequest(request => request + 1);
  }, [activeNodeId, nodeId]);

  if (!node) return null;
  return (
    <AgentTerminal
      key={node.id}
      nodeId={node.id}
      provider={node.provider}
      focusOnAttach
      focusRequest={focusRequest}
    />
  );
}

/**
 * Mount one pane and wait for its registry entry.
 *
 * The panes are mounted one after the other rather than in a single React
 * commit on purpose: `TerminalRegistry.doCreate` lazily imports `@xterm/xterm`,
 * and Vitest's module mock is not applied to a *second concurrent* dynamic
 * import of the same path — the second creation gets the real xterm and dies
 * in `loadUnicode11Widths` with "activate is not a function". Keeping the two
 * creations apart is a test-harness workaround; the behaviour under test does
 * not depend on the two mounts landing in the same tick.
 */
async function mountPane(nodeId: number) {
  const view = render(<TerminalPane nodeId={nodeId} />);
  await waitFor(() => expect(terminalManager.getInstance(nodeId)).toBeDefined());
  return view;
}

/** The registry entry for a node, or a hard failure naming the node. */
function instanceOf(nodeId: number) {
  const instance = terminalManager.getInstance(nodeId);
  if (!instance) throw new Error(`no TerminalManager entry for node ${nodeId}`);
  return instance;
}

describe('terminal lifetime across an active-node switch (#2534)', () => {
  beforeEach(() => {
    mockListeners.clear();
    vi.clearAllMocks();
    // The global test setup mocks `invoke`; this file needs a few specific
    // answers on top of it (provider list, worktree-close safety).
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'list_agent_nodes') return Promise.resolve([]);
      if (cmd === 'list_providers') return Promise.resolve([{ id: 'anthropic', label: 'Claude' }]);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      // The delete flow asks for the worktree-close safety report first. A
      // worktree path with no uncommitted changes and no unpushed commits means
      // "no close risk", so the close does not raise the confirmation prompt.
      if (cmd === 'get_worktree_close_safety') {
        return Promise.resolve({
          worktree_path: '/repo/.claude/worktrees/agent-1',
          has_uncommitted: false,
          has_unpushed: false,
          is_detached: false,
        });
      }
      return Promise.resolve({});
    });
    seedAgentNodes([NODE_ONE, NODE_TWO], NODE_ONE.id);
    terminalManager.dispose(NODE_ONE.id);
    terminalManager.dispose(NODE_TWO.id);
  });

  afterEach(() => {
    terminalManager.dispose(NODE_ONE.id);
    terminalManager.dispose(NODE_TWO.id);
  });

  it('mounting two Agent Nodes yields two TerminalManager entries', async () => {
    await mountPane(NODE_ONE.id);
    await mountPane(NODE_TWO.id);

    // Two distinct registry entries, each with its own xterm.
    const one = instanceOf(NODE_ONE.id);
    const two = instanceOf(NODE_TWO.id);
    expect(one).not.toBe(two);
    expect(one.term).not.toBe(two.term);

    // A node that was not mounted owns no entry.
    expect(terminalManager.getInstance(999)).toBeUndefined();
  });

  it('switching the active node focuses the new terminal and disposes no terminal', async () => {
    await mountPane(NODE_ONE.id);
    await mountPane(NODE_TWO.id);

    const one = instanceOf(NODE_ONE.id);
    const two = instanceOf(NODE_TWO.id);

    // Node 1 is active at mount, so its terminal took the focus; node 2's did
    // not — it is the hidden pane.
    await waitFor(() => expect(one.term.focus).toHaveBeenCalled());
    expect(two.term.focus).not.toHaveBeenCalled();

    const focusCallsOnOneBeforeSwitch = (one.term.focus as ReturnType<typeof vi.fn>).mock.calls.length;

    // The switch: the store's active node moves from 1 to 2.
    await act(async () => {
      useAgentNodeStore.getState().setActiveNode(NODE_TWO.id);
    });

    // The newly active terminal is shown (it takes the focus)…
    await waitFor(() => expect(two.term.focus).toHaveBeenCalled());
    // …and the pane that was hidden keeps the focus it already had: switching
    // away does not pull focus back to the old terminal.
    expect((one.term.focus as ReturnType<typeof vi.fn>).mock.calls.length).toBe(focusCallsOnOneBeforeSwitch);

    // The switch disposes no terminal: both entries survive, untouched.
    expect(terminalManager.getInstance(NODE_ONE.id)).toBe(one);
    expect(terminalManager.getInstance(NODE_TWO.id)).toBe(two);
    expect(one.term.dispose).not.toHaveBeenCalled();
    expect(two.term.dispose).not.toHaveBeenCalled();
  });

  it('a React remount re-attaches the same terminal instead of creating a second', async () => {
    const first = await mountPane(NODE_ONE.id);

    const one = instanceOf(NODE_ONE.id);
    expect(one.term.open).toHaveBeenCalledTimes(1);
    const element = one.term.element;
    if (!element) throw new Error('the terminal did not open an element');

    // Unmounting the pane detaches the terminal from the DOM — it must NOT
    // dispose it. The registry keeps the entry so the next mount re-attaches
    // the same xterm (and its scrollback).
    first.unmount();
    await act(async () => {});
    expect(terminalManager.getInstance(NODE_ONE.id)).toBe(one);
    expect(one.term.dispose).not.toHaveBeenCalled();

    const second = render(<TerminalPane nodeId={NODE_ONE.id} />);
    // The re-attach re-parents the very same xterm element into the new
    // container. A second terminal would be a second element and a second
    // open() call, so this wait is what makes the assertions below mean
    // "the remount happened" rather than "the assertions ran too early".
    await waitFor(() => expect(second.container.contains(element)).toBe(true));

    expect(terminalManager.getInstance(NODE_ONE.id)).toBe(one);
    expect(one.term.open).toHaveBeenCalledTimes(1);
    expect(one.term.dispose).not.toHaveBeenCalled();
  });

  it('deleting a node disposes its own terminal', async () => {
    await mountPane(NODE_ONE.id);
    await mountPane(NODE_TWO.id);

    const one = instanceOf(NODE_ONE.id);
    const two = instanceOf(NODE_TWO.id);

    // Delete node 1 through the real store action, so the dispose happens on
    // the same path production uses (the delete IPC commits, THEN the
    // terminal is disposed — issue #647).
    await act(async () => {
      await useAgentNodeStore.getState().deleteAgentNode(NODE_ONE.id);
    });

    expect(vi.mocked(invoke).mock.calls.some(call => call[0] === 'delete_agent_node')).toBe(true);

    // The deleted node's terminal is gone from the registry and disposed.
    expect(terminalManager.getInstance(NODE_ONE.id)).toBeUndefined();
    expect(one.term.dispose).toHaveBeenCalledTimes(1);

    // The other node's terminal is untouched.
    expect(terminalManager.getInstance(NODE_TWO.id)).toBe(two);
    expect(two.term.dispose).not.toHaveBeenCalled();
  });
});
