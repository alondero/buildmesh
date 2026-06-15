/**
 * Regression test for the "focus stolen mid-typing" bug.
 *
 * Symptom (reported in a 3+ pane grid with a stable node list): while typing
 * into the active node's terminal, keyboard focus is silently dropped — with no
 * click or other obvious action — and the next characters go nowhere. There is
 * no static code path that *moves* focus in that scenario, which means focus is
 * falling out to <body> entirely (a stray DOM reconciliation around xterm's
 * imperatively-appended element, or a WebView2 focus hiccup), rather than being
 * handed to another control.
 *
 * Fix: a focus guardian on each AgentTerminal. When *this* node is the active
 * one and the app window still holds OS focus, but focus has fallen to
 * <body>/null, pull it straight back to the terminal. It must NOT fire when
 * focus legitimately moved to a real control (a button, the search box, a
 * rename input, another pane) — those land on an element, not <body>.
 *
 * Run with: npm test -- --run tests/integration/terminal-focus-guardian.test.tsx
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, act } from '@testing-library/react';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

if (!('ResizeObserver' in globalThis)) {
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

// vi.hoisted runs before the SUT import, which triggers the TerminalRegistry
// constructor's eager listen() (issue #332). Same pattern as the auto-spawn
// test — without it the mock factory's closure over mockListeners hits TDZ.
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

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit = vi.fn();
    dispose = vi.fn();
    proposeDimensions = vi.fn().mockReturnValue({ cols: 80, rows: 24 });
  },
}));
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

vi.mock('../../src/components/Terminal/handoverProviderCache', () => ({
  getHandoverProviderLabel: vi.fn().mockResolvedValue('Anthropic'),
}));

import { AgentTerminal, terminalManager } from '../../src/components/Terminal/Terminal';

// 'running' (not 'idle') so the auto-spawn effect stays out of the way — we
// only care about the focus lifecycle here.
const RUNNING_NODE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
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

async function mountAndSettle() {
  const result = render(<AgentTerminal sessionId={RUNNING_NODE.id} />);
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });
  const inst = terminalManager.getInstance(RUNNING_NODE.id);
  if (!inst) throw new Error('terminal instance was not created');
  // Drop the attach-time focus() call so assertions see only post-mount focus.
  vi.mocked(inst.term.focus).mockClear();
  const host = result.container.querySelector<HTMLElement>(`[data-node-id="${RUNNING_NODE.id}"]`);
  if (!host) throw new Error('terminal host element not found');
  return { ...result, inst, host };
}

async function dispatchFocusOut(host: HTMLElement) {
  await act(async () => {
    host.dispatchEvent(new FocusEvent('focusout', { bubbles: true }));
    // Flush the guardian's deferred (microtask) check.
    await new Promise((r) => setTimeout(r, 0));
  });
}

describe('AgentTerminal focus guardian', () => {
  beforeEach(() => {
    mockListeners.clear();
    vi.clearAllMocks();
    useAgentNodeStore.setState({ agentNodes: [RUNNING_NODE], activeNodeId: RUNNING_NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ dragTargetNodeId: null, fileExplorerContext: null });
    terminalManager.dispose(RUNNING_NODE.id);
    // jsdom's hasFocus() can be unreliable across versions; pin it true so the
    // guardian's "window still focused" guard doesn't gate the test.
    vi.spyOn(document, 'hasFocus').mockReturnValue(true);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  });

  afterEach(() => {
    terminalManager.dispose(RUNNING_NODE.id);
    vi.restoreAllMocks();
  });

  it('restores focus to the active terminal when focus falls out to <body>', async () => {
    const { inst, host } = await mountAndSettle();
    // Nothing is focused → document.activeElement is <body>: the bug signature.
    await dispatchFocusOut(host);
    expect(inst.term.focus).toHaveBeenCalled();
  });

  it('does NOT reclaim focus when it moved to a real control (e.g. a button)', async () => {
    const { inst, host } = await mountAndSettle();
    const button = document.createElement('button');
    document.body.appendChild(button);
    button.focus(); // document.activeElement is now a real element, not <body>
    await dispatchFocusOut(host);
    expect(inst.term.focus).not.toHaveBeenCalled();
    button.remove();
  });

  it('does NOT reclaim focus for a node that is not the active one', async () => {
    const { inst, host } = await mountAndSettle();
    // Another pane is active; this terminal must not yank focus to itself.
    act(() => { useAgentNodeStore.setState({ activeNodeId: 999 }); });
    await dispatchFocusOut(host);
    expect(inst.term.focus).not.toHaveBeenCalled();
  });

  it('does NOT reclaim focus when the app window has lost OS focus', async () => {
    const { inst, host } = await mountAndSettle();
    vi.mocked(document.hasFocus).mockReturnValue(false);
    await dispatchFocusOut(host);
    expect(inst.term.focus).not.toHaveBeenCalled();
  });
});
