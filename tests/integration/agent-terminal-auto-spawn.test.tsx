/**
 * Regression test for the fresh-spawn 80-col wrap bug (#302).
 *
 * Symptom: a freshly created agent node wrapped its first ~1s of PTY output at
 * FitAddon's 80x24 fallback regardless of how wide the pane actually was,
 * because the auto-spawn effect called `proposeDimensions()` BEFORE the
 * terminal was opened in the DOM. Resizing the window or switching nodes and
 * back would re-fit the terminal, but the agent's PTY stayed at 80 cols
 * (baked in at spawn time) and the first banner lines were permanently
 * wrapped.
 *
 * Fix: the auto-spawn effect must `await terminalManager.attach(sessionId,
 * container)` instead of `getOrCreate`, so `proposeDimensions()` sees the
 * real layout. This test exercises the actual `AgentTerminal` component with
 * a wide container and asserts `spawn_agent` is invoked with cols > 80.
 *
 * Run with: npm test -- --run tests/integration/agent-terminal-auto-spawn.test.tsx
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

// jsdom doesn't ship ResizeObserver; TerminalRegistry.attachToDOM wires
// one up for live fit on container resize. Provide a no-op stub.
if (!('ResizeObserver' in globalThis)) {
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

// Mock heavy Tauri and xterm bits — we're testing the auto-spawn wiring, not
// real PTYs. The FitAddon mock uses container.offsetWidth so we can drive
// the dimensions it reports from the test. `vi.hoisted` runs before the
// SUT import below — necessary because importing `terminalManager` triggers
// the TerminalRegistry constructor, which now eagerly subscribes to Tauri
// events (see issue #332 / agent-spawned reconcile). Without hoisting, the
// mock factory's closure over `mockListeners` would hit TDZ when the
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

vi.mock('@tauri-apps/api/core', () => ({
  // spawnAgent -> fetchAgentNodes -> invoke('list_sessions') is what keeps the
  // store's agentNodes in sync post-spawn. If we return a non-array (the
  // default `{}` from `mockResolvedValue({})`) the store writes that to
  // state.agentNodes and the next render explodes on `find()`. Return [].
  invoke: vi.fn().mockImplementation((cmd: string) => {
    if (cmd === 'list_sessions') return Promise.resolve([]);
    return Promise.resolve({});
  }),
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('@xterm/xterm', () => {
  class MockTerminal {
    write = vi.fn();
    onData = vi.fn();
    onTitleChange = vi.fn();
    onResize = vi.fn();
    onScroll = vi.fn().mockReturnValue({ dispose: vi.fn() });
    // Real xterm's open() inserts a `.xterm` div into the container. Without
    // that, the FitAddon mock (which walks the DOM for `.xterm`) can't tell
    // that the term has been parented.
    open(container: HTMLElement) {
      const el = document.createElement('div');
      el.className = 'xterm';
      container.appendChild(el);
      this.element = el;
    }
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
    buffer = { active: { getWindow: vi.fn() } };
    unicode = {
      register: vi.fn(),
      get activeVersion() { return '11'; },
      set activeVersion(_v: string) { /* noop */ },
    };
    _core = { unicodeService: { _providers: { '11': { version: '11', wcwidth: () => 1, charProperties: () => 0 } }, register: vi.fn() } };
    rows = 24;
    cols = 80;
    options = { fontSize: 10 };
    element: HTMLElement | null = null;
    constructor(_options?: unknown) {}
  }
  return { Terminal: MockTerminal };
});

vi.mock('@xterm/addon-fit', () => {
  // Mirror real FitAddon.proposeDimensions: when no container is supplied,
  // it falls back to the term's parentElement. The bug: proposeDimensions()
  // was called BEFORE term.open() ran, so the term had no parent and
  // FitAddon emitted its 80x24 hard-coded default. The fix (call attach()
  // first) guarantees the term is parented before proposeDimensions runs.
  class MockFitAddon {
    fit = vi.fn();
    dispose = vi.fn();
    proposeDimensions = vi.fn((container?: HTMLElement) => {
      const target = container ?? findXtermParent();
      if (!target) return { cols: 80, rows: 24 };
      const charW = 8;
      const charH = 16;
      return {
        cols: Math.max(2, Math.floor(target.offsetWidth / charW)),
        rows: Math.max(2, Math.floor(target.offsetHeight / charH)),
      };
    });
  }

  function findXtermParent(): HTMLElement | undefined {
    const el = document.querySelector('.xterm') as HTMLElement | null;
    return el?.parentElement ?? undefined;
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

// HandoverProviderCache hits an IPC for the default provider label; stub it
// to a deterministic string so the effect that fetches it doesn't noise up
// the test logs.
vi.mock('../../src/components/Terminal/handoverProviderCache', () => ({
  getHandoverProviderLabel: vi.fn().mockResolvedValue('Anthropic'),
}));

import { AgentTerminal, terminalManager } from '../../src/components/Terminal/Terminal';

const IDLE_NODE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'idle',
  use_worktree: false,
  position: 0,
  created_at: new Date(0).toISOString(),
};

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
};

// jsdom doesn't lay out, so we have to fabricate offsetWidth/offsetHeight.
function setContainerSize(root: HTMLElement, nodeId: number, width: number, height: number) {
  const el = root.querySelector<HTMLElement>(`[data-node-id="${nodeId}"]`);
  if (!el) throw new Error(`no container for node ${nodeId}`);
  Object.defineProperty(el, 'offsetWidth', { configurable: true, value: width });
  Object.defineProperty(el, 'offsetHeight', { configurable: true, value: height });
}

describe('AgentTerminal auto-spawn (issue #302)', () => {
  beforeEach(() => {
    mockListeners.clear();
    vi.clearAllMocks();
    useAgentNodeStore.setState({ agentNodes: [IDLE_NODE], activeNodeId: IDLE_NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ dragTargetNodeId: null });
    terminalManager.dispose(IDLE_NODE.id);
  });

  afterEach(() => {
    terminalManager.dispose(IDLE_NODE.id);
  });

  it('passes cols > 80 to spawn_agent when the pane is wide (fresh spawn, no prior attach)', async () => {
    // 1600px wide pane at 8px/cell = 200 cols, 900px tall at 16px/row = 56 rows.
    const WIDE_WIDTH = 1600;
    const WIDE_HEIGHT = 900;
    const EXPECTED_COLS = Math.floor(WIDE_WIDTH / 8);
    const EXPECTED_ROWS = Math.floor(WIDE_HEIGHT / 16);

    // The auto-spawn effect's bug is timing-dependent: in the real app
    // the agent-output `listen` IPC roundtrip is slow, so the auto-spawn
    // effect's `getOrCreate` continuation runs before the attach effect's
    // `attach` continuation has called `term.open()`. We replicate that
    // ordering here by making `terminalManager.attach` wait several
    // microtasks before resolving. `getOrCreate` stays fast (it doesn't
    // touch the DOM) so its continuation races ahead of the term being
    // parented.
    //
    // Buggy code path: auto-spawn calls getOrCreate (fast) → proposeDimensions
    //   runs against an unparented term → 80x24 fallback.
    // Fixed code path: auto-spawn calls attach (slow) → waits for term to be
    //   parented → proposeDimensions returns real dims.
    const realAttach = terminalManager.attach.bind(terminalManager);
    const attachSpy = vi.spyOn(terminalManager, 'attach').mockImplementation(
      async (nodeId, container) => {
        await new Promise((r) => setTimeout(r, 5));
        return realAttach(nodeId, container);
      },
    );
    // Pre-create the instance so getOrCreate resolves immediately on the
    // fast path the buggy code takes.
    await terminalManager.getOrCreate(IDLE_NODE.id);

    const { container } = render(<AgentTerminal sessionId={IDLE_NODE.id} />);
    setContainerSize(container, IDLE_NODE.id, WIDE_WIDTH, WIDE_HEIGHT);

    await act(async () => {
      // Let the (slow) attach resolve and the auto-spawn effect run.
      await new Promise((r) => setTimeout(r, 30));
    });

    const spawnCalls = vi.mocked(invoke).mock.calls.filter(
      (c) => c[0] === 'spawn_agent',
    );
    expect(spawnCalls.length).toBeGreaterThanOrEqual(1);
    const args = spawnCalls[0]![1] as { rows?: number; cols?: number };
    // The bug: cols=80 (FitAddon fallback) because the term wasn't open
    // when proposeDimensions() was called.
    expect(args.cols).toBe(EXPECTED_COLS);
    expect(args.cols).toBeGreaterThan(80);
    expect(args.rows).toBe(EXPECTED_ROWS);
    expect(args.rows).toBeGreaterThan(24);
    // attach should have been called at least once with the container.
    // With the fix the auto-spawn effect calls it; with the bug only the
    // attach effect does, so this still holds either way — we don't rely
    // on it for the regression, just sanity-check that the spy fired.
    expect(attachSpy).toHaveBeenCalled();
  });
});
