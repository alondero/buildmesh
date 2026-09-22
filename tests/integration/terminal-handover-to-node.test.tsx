/**
 * "Handover to node" — the agent terminal's right-click menu.
 *
 * The picker offers this Mesh's other agents with the ones sharing this node's
 * tabs first (the implementer/reviewer pairing is the case the action exists
 * for), and picking one hands the terminal selection to that node's PTY through
 * `handover_to_agent`. The paste-and-submit itself is backend work — the
 * target's xterm may not even be mounted — so what this suite pins is the
 * candidate list, its ordering, and the IPC call, not the TUI's reaction to it.
 *
 * Run with: npm test -- --run tests/integration/terminal-handover-to-node.test.tsx
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, fireEvent, waitFor, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

if (!('ResizeObserver' in globalThis)) {
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

// `vi.hoisted` — importing `terminalManager` constructs TerminalRegistry, which
// eagerly subscribes to Tauri events, and the mock factory's closure would hit
// TDZ without this (same shape as agent-terminal-auto-spawn.test.tsx).
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

vi.mock('@tauri-apps/api/core', async () => {
  const { MockChannel } = await import('../setup/tauriChannel');
  return {
    invoke: vi.fn().mockImplementation((cmd: string) => {
      if (cmd === 'list_agent_nodes') return Promise.resolve([]);
      if (cmd === 'list_providers') return Promise.resolve([{ id: 'anthropic', label: 'Claude' }]);
      if (cmd === 'get_default_provider') return Promise.resolve('anthropic');
      return Promise.resolve({});
    }),
    Channel: MockChannel,
  };
});

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
    proposeDimensions = vi.fn(() => ({ cols: 120, rows: 30 }));
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

import { AgentTerminal, terminalManager } from '../../src/components/Terminal/Terminal';
import { seedAgentNodes } from '../unit/helpers/seedAgentNodes';

type MockTerm = {
  hasSelection: ReturnType<typeof vi.fn>;
  getSelection: ReturnType<typeof vi.fn>;
};

const node = (id: number, overrides: Partial<AgentNode> = {}): AgentNode => ({
  id, mesh_id: 1, name: `Agent ${id}`, status: 'ready', provider: 'anthropic',
  path: '/repo', branch: 'main', env: 'windows', use_worktree: false, position: id,
  created_at: '', cli_session_id: null, worktree_name: null, is_pinned: false,
  source_issue: null, source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
  source_pr_pinned_sha: null, signal_health: null, worktree_path: null, ...overrides,
});

const IMPLEMENTER = node(1, { name: 'Implementer', status: 'running' });
const REVIEWER = node(2, { name: 'Reviewer', status: 'ready', position: 2 });
const UNRELATED = node(3, { name: 'Scratch', status: 'idle', position: 3 });
const OTHER_MESH = node(4, { name: 'Elsewhere', mesh_id: 2, position: 1 });

const MESH: Mesh = {
  id: 1, name: 'demo', path: '/repo', layout: 'single', position: 0,
  created_at: new Date(0).toISOString(), scratchpad: '', sandbox: false,
};

// The Reviewer is the review child of the Implementer (circuit ownership
// lineage), which is what puts it in the `sameActivity` half of the picker.
const OWNERSHIPS = {
  2: { node_id: 2, run_id: 7, circuit_id: 2, circuit_name: 'Review', state: 'running', parent_node_id: 1 },
};

function menu(): HTMLElement {
  const el = document.querySelector<HTMLElement>('[data-dropdown-for="terminal-1"]');
  if (!el) throw new Error('terminal context menu is not open');
  return el;
}

/** Mount the source terminal, stub its selection, then open the menu. `attach`
 *  is async — the registry instance only exists once the attach effect has
 *  resolved, so the selection stub has to wait for it. */
async function mountAndOpen(selection?: string) {
  const { container } = render(<AgentTerminal nodeId={IMPLEMENTER.id} focusOnAttach={false} />);
  await waitFor(() => expect(terminalManager.getInstance(IMPLEMENTER.id)).toBeDefined());
  const term = terminalManager.getInstance(IMPLEMENTER.id)!.term as unknown as MockTerm;
  term.getSelection.mockReturnValue(selection ?? '');
  term.hasSelection.mockReturnValue(selection !== undefined);
  fireEvent.contextMenu(container.querySelector(`[data-node-id="${IMPLEMENTER.id}"]`)!);
  return { container, term, open: menu() };
}

/** The picker's rows in render order, keyed by the row hook rather than by
 *  status copy — the menu's other items (Copy, Paste, Clear Terminal, …) carry
 *  no `data-handover-target`. */
function targetRows(open: HTMLElement): string[] {
  return Array.from(open.querySelectorAll<HTMLElement>('[data-handover-target]'))
    .map(button => button.textContent ?? '');
}

describe('terminal handover to an existing node', () => {
  beforeEach(() => {
    mockListeners.clear();
    vi.clearAllMocks();
    localStorage.removeItem('buildmesh.node-groups');
    seedAgentNodes([IMPLEMENTER, REVIEWER, UNRELATED, OTHER_MESH], IMPLEMENTER.id);
    useAgentNodeStore.setState({ circuitOwnerships: OWNERSHIPS });
    useNodeActivityStore.setState({ selections: {}, utilities: {}, groups: [] });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ dragTargetNodeId: null });
    terminalManager.dispose(IMPLEMENTER.id);
  });

  afterEach(() => {
    terminalManager.dispose(IMPLEMENTER.id);
    vi.restoreAllMocks();
  });

  it('lists this Mesh\'s other agents with the shared-activity node first, and hands the selection over', async () => {
    const { open } = await mountAndOpen('review found a race in the parser');
    expect(within(open).getByText('Handover to node')).toBeTruthy();
    // Ordering is the whole "prefer the paired node" affordance: the review
    // child of this node is offered above the unrelated agent, and the other
    // Mesh's agent is not offered at all.
    const rows = targetRows(open);
    expect(rows).toHaveLength(2);
    expect(rows[0]).toContain('Reviewer');
    expect(rows[1]).toContain('Scratch');
    expect(open.textContent).not.toContain('Elsewhere');

    fireEvent.click(within(open).getByText('Reviewer'));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('handover_to_agent', {
        targetNodeId: REVIEWER.id,
        text: 'review found a race in the parser',
      }),
    );
    expect(document.querySelector('[data-dropdown-for="terminal-1"]')).toBeNull();
  });

  it('has nothing to hand over until the terminal has a selection', async () => {
    const { term, open } = await mountAndOpen();
    expect(term.getSelection()).toBe('');
    const reviewer = within(open).getByText('Reviewer').closest('button')!;
    expect(reviewer.disabled).toBe(true);

    fireEvent.click(reviewer);
    expect(invoke).not.toHaveBeenCalledWith('handover_to_agent', expect.anything());
  });

  it('treats a manually grouped node as sharing the activity too', async () => {
    // No ownership lineage here — the manual group is the only thing tying
    // Scratch to the Implementer, so Scratch takes the preferred slot and the
    // by-standing Reviewer drops below it.
    useAgentNodeStore.setState({ circuitOwnerships: {} });
    useNodeActivityStore.setState({ groups: [[UNRELATED.id, IMPLEMENTER.id]] });

    const { open } = await mountAndOpen('hand me over');
    const rows = targetRows(open);
    expect(rows[0]).toContain('Scratch');
    expect(rows[1]).toContain('Reviewer');

    fireEvent.click(within(open).getByText('Scratch'));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('handover_to_agent', {
        targetNodeId: UNRELATED.id,
        text: 'hand me over',
      }),
    );
  });
});
