/**
 * Tests for the circuit canvas editor (issue #1209).
 *
 * Mounts `CircuitFlowEditor` directly against fixtures so the React Flow
 * canvas, palette, quick-connect, inspector, run overlays, and the save
 * IPC contract can be exercised without the Tauri runtime. Wire shapes
 * come from `src/types/generated/` (never hand-declared), and the graph
 * fixtures use the exact wire shape serde produces
 * (`{"id": "...", "type": { ...kind... }}`).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { CircuitFlowEditor } from '../../src/components/Circuits/CircuitFlowEditor';
import type { AutopilotCircuit } from '../../src/types/generated/AutopilotCircuit';
import type { CircuitRunDetail } from '../../src/types/generated/CircuitRunDetail';

// -- jsdom polyfills React Flow expects -------------------------------------
//
// React Flow measures nodes/handles through ResizeObserver before it
// renders edges (handle bounds drive edge geometry). The naive
// no-op stub leaves every node unmeasured, so edge layers stay empty —
// this stub reports a fixed card size on observe instead.

class ResizeObserverMock {
  callback: ResizeObserverCallback;
  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
  }
  observe(target: Element): void {
    // React Flow derives node dimensions from offsetWidth/Height (both
    // 0 in jsdom); give observed elements a real card size.
    const el = target as HTMLElement & { offsetWidth: number; offsetHeight: number };
    try {
      Object.defineProperty(el, 'offsetWidth', { configurable: true, get: () => 220 });
      Object.defineProperty(el, 'offsetHeight', { configurable: true, get: () => 64 });
    } catch {
      /* non-HTMLElement targets keep their zero dims */
    }
    const entry = {
      target,
      contentRect: { width: 220, height: 64, top: 0, left: 0, right: 220, bottom: 64, x: 0, y: 0 },
      borderBoxSize: [{ inlineSize: 220, blockSize: 64 }],
      contentBoxSize: [{ inlineSize: 220, blockSize: 64 }],
      devicePixelContentBoxSize: [],
    };
    // Defer like a real observer — a synchronous callback lands before
    // React Flow finishes wiring its measurement handlers.
    setTimeout(() => {
      this.callback([entry] as unknown as ResizeObserverEntry[], this as unknown as ResizeObserver);
    }, 0);
  }
  unobserve(): void {}
  disconnect(): void {}
}
(globalThis as Record<string, unknown>).ResizeObserver =
  globalThis.ResizeObserver ?? ResizeObserverMock;

// jsdom has no DOMMatrixReadOnly; React Flow's fit-view math only needs
// the scale component (the documented test polyfill).
class DOMMatrixReadOnlyMock {
  m22: number;
  constructor(transform?: string) {
    const scale = transform?.match(/scale\(([1-9.]+)\)/)?.[1];
    this.m22 = scale !== undefined ? Number(scale) : 1;
  }
}
if (typeof (globalThis as Record<string, unknown>).DOMMatrixReadOnly === 'undefined') {
  (globalThis as Record<string, unknown>).DOMMatrixReadOnly = DOMMatrixReadOnlyMock;
  (window as Record<string, unknown>).DOMMatrixReadOnly = DOMMatrixReadOnlyMock;
}

// -- fixtures -----------------------------------------------------------------

const CIRCUIT: AutopilotCircuit = {
  id: 7,
  mesh_id: 42,
  name: 'nightly-sweep',
  description: '',
  enabled: true,
  concurrency_limit: 1,
  graph_json: JSON.stringify({
    version: 1,
    nodes: [
      { id: 'trigger', type: { type: 'manual' } },
      {
        id: 'spawn',
        type: { type: 'spawn_agent_node', prompt: 'review the diff', name: null },
      },
      { id: 'gate', type: { type: 'deterministic_verification', command: 'cargo test' } },
    ],
    edges: [
      { from: 'trigger', to: 'spawn', condition: 'always' },
      { from: 'spawn', to: 'gate', condition: 'always' },
    ],
  }),
  created_at: '2026-08-22 10:00:00',
  updated_at: '2026-08-22 10:00:00',
};

const RUN_COMPLETED: CircuitRunDetail = {
  run: {
    id: 11,
    circuit_id: 7,
    mesh_id: 42,
    trigger_identity: 'manual:1724000000000',
    state: 'running',
    context_json: '{}',
    created_at: '2026-08-22 10:05:00',
    updated_at: '2026-08-22 10:07:00',
  },
  steps: [
    {
      id: 1,
      run_id: 11,
      node_id: 'trigger',
      agent_node_id: null,
      status: 'completed',
      attempt: 1,
      outcome: 'completed',
      error_message: null,
      started_at: '2026-08-22 10:05:00',
      completed_at: '2026-08-22 10:05:00',
    },
    {
      id: 2,
      run_id: 11,
      node_id: 'gate',
      agent_node_id: null,
      status: 'blocked',
      attempt: 1,
      outcome: null,
      error_message: null,
      started_at: '2026-08-22 10:05:01',
      completed_at: null,
    },
  ],
};

function renderEditor(runs: CircuitRunDetail[] = [RUN_COMPLETED]) {
  return render(<CircuitFlowEditor circuit={CIRCUIT} runs={runs} onClose={() => {}} />);
}

beforeEach(() => {
  vi.mocked(invoke).mockResolvedValue(undefined);
});

describe('CircuitFlowEditor', () => {
  it('renders every node of the stored blueprint as a card', async () => {
    renderEditor();
    expect(await screen.findByTestId('circuit-node-trigger')).toBeTruthy();
    expect(screen.getByTestId('circuit-node-spawn')).toBeTruthy();
    expect(screen.getByTestId('circuit-node-gate')).toBeTruthy();
    // Config summary rides the card.
    expect(screen.getByText(/review the diff/)).toBeTruthy();
  });

  it('shows the palette grouped by category', () => {
    renderEditor();
    expect(screen.getByTestId('circuit-palette')).toBeTruthy();
    expect(screen.getByText('Triggers')).toBeTruthy();
    expect(screen.getByText('Actions')).toBeTruthy();
    expect(screen.getByText('Gates')).toBeTruthy();
    expect(screen.getByText('Joins')).toBeTruthy();
  });

  it('adds a node through the palette and includes it in the saved graph', async () => {
    renderEditor();

    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    expect(screen.getByTestId('circuit-node-notify_1')).toBeTruthy();

    fireEvent.click(screen.getByTestId('editor-save'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'update_circuit_graph',
        expect.objectContaining({ circuitId: 7 })
      );
    });
    const saved = vi.mocked(invoke).mock.calls.find((c) => c[0] === 'update_circuit_graph');
    const graph = JSON.parse((saved?.[1] as Record<string, unknown>).graphJson as string);
    expect(graph.nodes.map((n: { id: string }) => n.id)).toEqual(
      expect.arrayContaining(['trigger', 'spawn', 'gate', 'notify_1'])
    );
    // Palette adds are unwired — existing edges untouched.
    expect(graph.edges).toHaveLength(2);
  });

  it('exposes named outcome handles on gate nodes', async () => {
    renderEditor();
    await screen.findByTestId('circuit-node-gate');
    // DeterministicVerification gates route Green | Red.
    expect(screen.getByTestId('handle-gate-green')).toBeTruthy();
    expect(screen.getByTestId('handle-gate-red')).toBeTruthy();
    // Plain action nodes have no named ports.
    expect(screen.queryByTestId('handle-spawn-completed')).toBeNull();
  });

  it('renders editable condition badges on edges', async () => {
    const GRAPH_WITH_OUTCOME_EDGE = JSON.stringify({
      version: 1,
      nodes: [
        { id: 'gate', type: { type: 'deterministic_verification', command: 'cargo test' } },
        { id: 'fix', type: { type: 'inject_pty', prompt: 'fix it' } },
      ],
      edges: [{ from: 'gate', to: 'fix', condition: { on_outcome: 'red' } }],
    });
    render(
      <CircuitFlowEditor
        circuit={{ ...CIRCUIT, graph_json: GRAPH_WITH_OUTCOME_EDGE }}
        runs={[]}
        onClose={() => {}}
      />
    );
    const badge = await screen.findByTestId('edge-badge-gate->fix:on:red');
    expect(badge.textContent).toBe('OnOutcome(red)');
    // Clicking cycles red → always.
    fireEvent.click(badge);
    expect(screen.getByTestId('edge-badge-gate->fix:always').textContent).toBe('Always');
  });

  it('pulses completed steps and parks an Approve badge on blocked gates', async () => {
    renderEditor();
    expect(await screen.findByTestId('node-completed-trigger')).toBeTruthy();
    const badge = screen.getByTestId('node-blocked-gate');
    expect(badge.textContent).toContain('waiting for approval');

    fireEvent.click(screen.getByTestId('node-approve-gate'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('approve_circuit_step', { runId: 11, nodeId: 'gate' });
    });
  });

  it('opens the inspector with per-kind fields for the selected node', async () => {
    renderEditor();

    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const inspector = await screen.findByTestId('circuit-inspector');
    expect(inspector.textContent).toContain('Spawn Agent Node');
    expect(screen.getByTestId('inspector-prompt')).toBeTruthy();
    expect(screen.getByLabelText('Agent name')).toBeTruthy();

    // Editing writes back into the working copy and marks it dirty.
    await userEvent.setup().type(screen.getByTestId('inspector-agent-name'), 'fix-it');
    expect(screen.getByTestId('editor-dirty')).toBeTruthy();
    fireEvent.click(screen.getByTestId('editor-save'));
    await waitFor(() => {
      const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === 'update_circuit_graph');
      const graph = JSON.parse((call?.[1] as Record<string, unknown>).graphJson as string);
      const spawn = graph.nodes.find((n: { id: string }) => n.id === 'spawn');
      expect(spawn.type.name).toBe('fix-it');
    });
  });

  it('offers {{ mustache chips across all context namespaces and inserts correctly', async () => {
    renderEditor();

    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const prompt = await screen.findByTestId('inspector-prompt');
    await userEvent.setup().type(prompt, 'Fix {{{{');

    const menu = screen.getByTestId('mustache-menu');
    for (const ns of ['issue.number', 'pr.title', 'node.id', 'verification.outcome', 'retry.attempt', 'circuit.name']) {
      expect(menu.querySelector(`[data-testid="mustache-chip-${ns}"]`)).not.toBeNull();
    }
    await userEvent.setup().click(screen.getByTestId('mustache-chip-issue.number'));
    expect((screen.getByTestId('inspector-prompt') as HTMLTextAreaElement).value).toBe(
      'review the diffFix {{ issue.number }}'
    );
  });

  it('lists runs in the history drawer with per-step logs and path highlighting', async () => {
    renderEditor();

    fireEvent.click(screen.getByTestId('toggle-run-history'));
    const drawer = screen.getByTestId('run-history-drawer');
    expect(drawer.textContent).toContain('#11');

    fireEvent.click(screen.getByTestId('history-run-11'));
    const steps = screen.getByTestId('run-steps-11');
    expect(steps.textContent).toContain('trigger');
    expect(steps.textContent).toContain('completed');
    expect(steps.textContent).toContain('blocked');

    // The traversed edge trigger→spawn glows; spawn→gate routes on
    // outcomes the ledger hasn't produced, so it stays dim.
    await waitFor(() => {
      const lit = screen
        .getByTestId('edge-badge-trigger->spawn:always')
        .className.includes('text-accent-cyan');
      expect(lit).toBe(true);
    });
  });

  it('auto-layout keeps every node positioned without crashing', async () => {
    renderEditor();
    await screen.findByTestId('circuit-node-trigger');

    fireEvent.click(screen.getByTestId('autolayout-tb'));
    expect(screen.getByTestId('circuit-node-spawn')).toBeTruthy();
    fireEvent.click(screen.getByTestId('autolayout-lr'));
    expect(screen.getByTestId('circuit-node-gate')).toBeTruthy();
  });

  it('deletes the selected node together with its edges', async () => {
    renderEditor();

    fireEvent.click(await screen.findByTestId('circuit-node-gate'));
    fireEvent.click(screen.getByTestId('inspector-delete-node'));
    expect(screen.queryByTestId('circuit-node-gate')).toBeNull();

    fireEvent.click(screen.getByTestId('editor-save'));
    await waitFor(() => {
      const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === 'update_circuit_graph');
      const graph = JSON.parse((call?.[1] as Record<string, unknown>).graphJson as string);
      expect(graph.nodes.map((n: { id: string }) => n.id)).toEqual(['trigger', 'spawn']);
      expect(graph.edges).toHaveLength(1);
    });
  });

  it('surfaces backend failures inline', async () => {
    vi.mocked(invoke).mockRejectedValue('invalid circuit graph_json: nope');
    renderEditor();
    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    fireEvent.click(screen.getByTestId('editor-save'));
    expect(await screen.findByTestId('editor-error')).toBeTruthy();
  });
});
