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
    // Clicking cycles red → always → green → red … (the verification
    // gate's full outcome vocabulary — this pins the deep-equality fix
    // in nextCondition; reference equality collapsed every OnOutcome
    // badge straight back to Always).
    fireEvent.click(badge);
    expect(screen.getByTestId('edge-badge-gate->fix:always').textContent).toBe('Always');
    fireEvent.click(screen.getByTestId('edge-badge-gate->fix:always'));
    expect(screen.getByTestId('edge-badge-gate->fix:on:green').textContent).toBe(
      'OnOutcome(green)'
    );
    fireEvent.click(screen.getByTestId('edge-badge-gate->fix:on:green'));
    expect(screen.getByTestId('edge-badge-gate->fix:on:red').textContent).toBe('OnOutcome(red)');
  });

  it('merges instead of duplicating when cycling hits an existing parallel edge', async () => {
    // Two parallel edges gate→fix: `always` and `on:red`. Cycling the
    // red edge onto `always` must swallow it into the existing edge,
    // never mint a duplicate id.
    const GRAPH_PARALLEL = JSON.stringify({
      version: 1,
      nodes: [
        { id: 'gate', type: { type: 'deterministic_verification', command: 'cargo test' } },
        { id: 'fix', type: { type: 'inject_pty', prompt: 'fix it' } },
      ],
      edges: [
        { from: 'gate', to: 'fix', condition: 'always' },
        { from: 'gate', to: 'fix', condition: { on_outcome: 'red' } },
      ],
    });
    render(
      <CircuitFlowEditor
        circuit={{ ...CIRCUIT, graph_json: GRAPH_PARALLEL }}
        runs={[]}
        onClose={() => {}}
      />
    );
    const badgesBefore = await screen.findAllByTestId(/^edge-badge-gate->fix/);
    expect(badgesBefore).toHaveLength(2);

    fireEvent.click(screen.getByTestId('edge-badge-gate->fix:on:red'));
    await waitFor(() => {
      expect(screen.getAllByTestId(/^edge-badge-gate->fix/)).toHaveLength(1);
    });
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

  it('offers only reachable mustache chips and inserts correctly', async () => {
    renderEditor();

    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const prompt = await screen.findByTestId('inspector-prompt');
    // Trailing space included: `{{ ` must keep the menu open (the
    // inserted template is `{{ path }}` — spaces are part of the format).
    await userEvent.setup().type(prompt, 'Fix {{{{ ');

    const menu = screen.getByTestId('mustache-menu');
    for (const ns of ['node.id', 'circuit.name', 'autopilot.finish_prompt']) {
      expect(menu.querySelector(`[data-testid="mustache-chip-${ns}"]`)).not.toBeNull();
    }
    for (const ns of ['issue.number', 'pr.title', 'verification.outcome', 'retry.attempt']) {
      expect(menu.querySelector(`[data-testid="mustache-chip-${ns}"]`)).toBeNull();
    }
    await userEvent.setup().click(screen.getByTestId('mustache-chip-circuit.name'));
    expect((screen.getByTestId('inspector-prompt') as HTMLTextAreaElement).value).toBe(
      'review the diffFix {{ circuit.name }}'
    );
  });

  it('keyboard-navigates the mustache menu: ArrowDown + Enter picks the first chip', async () => {
    // One userEvent instance per test — calling `userEvent.setup()` per
    // statement creates separate keyboards that don't share the same
    // focused element, dropping keystrokes mid-typing.
    const user = userEvent.setup();
    renderEditor();

    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const prompt = await screen.findByTestId('inspector-prompt') as HTMLTextAreaElement;
    // Open the menu by typing `{{` (matches the v1 fixture's open
    // state — typing extra braces is flaky with userEvent).
    fireEvent.change(prompt, {
      target: { value: 'review the diffFix {{ ', selectionStart: 22 },
    });
    prompt.focus();
    const menu = await screen.findByTestId('mustache-menu');
    // No chip is highlighted on first open.
    expect(menu.querySelector('[data-highlighted="true"]')).toBeNull();

    // ArrowDown highlights the first chip.
    await user.keyboard('{ArrowDown}');
    const first = menu.querySelector('[data-highlighted="true"]');
    expect(first).not.toBeNull();
    expect(first?.getAttribute('data-testid')).toBe('mustache-chip-circuit.id');

    // Enter picks it.
    await user.keyboard('{Enter}');
    expect((screen.getByTestId('inspector-prompt') as HTMLTextAreaElement).value).toBe(
      'review the diffFix {{ circuit.id }}'
    );
  });

  it('ArrowUp from the first chip wraps to the last chip in the menu', async () => {
    const user = userEvent.setup();
    renderEditor();

    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const prompt = await screen.findByTestId('inspector-prompt') as HTMLTextAreaElement;
    fireEvent.change(prompt, {
      target: { value: 'review the diffFix {{ ', selectionStart: 22 },
    });
    prompt.focus();
    const menu = await screen.findByTestId('mustache-menu');

    await user.keyboard('{ArrowUp}');
    const wrapped = menu.querySelector('[data-highlighted="true"]');
    expect(wrapped).not.toBeNull();
    // The last chip in the spawn's menu should be highlighted
    // (no spawn-output chip is reachable from the spawn itself).
    const allChips = Array.from(menu.querySelectorAll('[data-testid^="mustache-chip-"]'))
      .filter((el) => !el.getAttribute('data-testid')?.endsWith('-unreachable'));
    const last = allChips[allChips.length - 1];
    expect(wrapped?.getAttribute('data-testid')).toBe(last?.getAttribute('data-testid'));
  });

  it('Enter picks the highlighted chip, NOT the raw fuzzy top (round-2 review regression)', async () => {
    // The bug: when the menu was grouped by namespace but pick()
    // indexed into the fuzzy-sorted `suggestions` array, ArrowDown
    // highlighted a grouped-first chip (e.g. circuit.id at DOM index
    // 0) but Enter inserted the fuzzy-top chip. The first selection
    // below proves the grouped render order; the later `{{ c` query
    // proves the two orderings can diverge once multiple namespaces
    // are reachable.
    const user = userEvent.setup();
    const GRAPH_WITH_REACHABLE_GROUPS = JSON.stringify({
      version: 1,
      nodes: [
        { id: 'trigger', type: { type: 'manual' } },
        {
          id: 'spawn_1',
          type: { type: 'spawn_agent_node', prompt: 'first', name: null },
        },
        { id: 'gate', type: { type: 'deterministic_verification', command: 'cargo test' } },
        {
          id: 'spawn_2',
          type: { type: 'spawn_agent_node', prompt: 'second', name: null },
        },
      ],
      edges: [
        { from: 'trigger', to: 'spawn_1', condition: 'always' },
        { from: 'spawn_1', to: 'gate', condition: 'always' },
        { from: 'gate', to: 'spawn_2', condition: 'always' },
      ],
    });
    render(
      <CircuitFlowEditor
        circuit={{ ...CIRCUIT, graph_json: GRAPH_WITH_REACHABLE_GROUPS }}
        runs={[]}
        onClose={() => {}}
      />
    );

    fireEvent.click(await screen.findByTestId('circuit-node-spawn_2'));
    const prompt = await screen.findByTestId('inspector-prompt') as HTMLTextAreaElement;
    fireEvent.change(prompt, {
      target: { value: 'review the diffFix {{ ', selectionStart: 22 },
    });
    prompt.focus();
    const menu = await screen.findByTestId('mustache-menu');

    // Without any arrow press, Enter should pick the DOM first chip
    // (whatever the render order is — in grouped mode, that's the
    // first chip of the first non-empty group, which for an empty
    // query is in the circuit.* group.
    await user.keyboard('{Enter}');
    const highlighted = (menu.querySelector('[data-highlighted="true"]') ??
      menu.querySelector('[data-testid^="mustache-chip-"]')) as HTMLElement;
    const expectedId = highlighted.getAttribute('data-testid')?.replace('mustache-chip-', '');
    const expectedPath = expectedId ?? '';
    expect((screen.getByTestId('inspector-prompt') as HTMLTextAreaElement).value).toBe(
      `review the diffFix {{ ${expectedPath} }}`
    );

    // Now ArrowDown to a chip whose position is NOT 0, then Enter —
    // the inserted chip must match the highlighted one, not the
    // fuzzy-top chip. Pick a prefix that mixes the reachable
    // circuit.* and verification.* groups so grouped render order
    // differs from the fuzzy-sorted catalogue.
    fireEvent.change(prompt, {
      target: { value: 'review the diffFix {{ c', selectionStart: 24 },
    });
    prompt.focus();
    const menu2 = await screen.findByTestId('mustache-menu');
    // ArrowDown to the SECOND chip in the menu (grouped order starts
    // with circuit.id, circuit.name, ...). Then Enter.
    await user.keyboard('{ArrowDown}'); // -> first chip
    await user.keyboard('{ArrowDown}'); // -> second chip
    const second = menu2.querySelector('[data-highlighted="true"]') as HTMLElement;
    expect(second).not.toBeNull();
    const secondId = second.getAttribute('data-testid')?.replace('mustache-chip-', '');
    await user.keyboard('{Enter}');
    expect((screen.getByTestId('inspector-prompt') as HTMLTextAreaElement).value).toBe(
      `review the diffFix {{ ${secondId} }}`
    );
  });

  it('Escape closes the mustache menu without picking a chip', async () => {
    const user = userEvent.setup();
    renderEditor();

    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const prompt = await screen.findByTestId('inspector-prompt') as HTMLTextAreaElement;
    fireEvent.change(prompt, {
      target: { value: 'review the diffFix {{ ', selectionStart: 22 },
    });
    prompt.focus();
    const menu = await screen.findByTestId('mustache-menu');
    expect(menu).toBeTruthy();

    await user.keyboard('{ArrowDown}');
    await user.keyboard('{Escape}');
    // Menu dismissed, prompt text unchanged.
    expect(screen.queryByTestId('mustache-menu')).toBeNull();
    expect((screen.getByTestId('inspector-prompt') as HTMLTextAreaElement).value).toBe(
      'review the diffFix {{ '
    );
  });

  it('selecting then deselecting a node does not crash the InspectorPanel (Rules of Hooks)', () => {
    // Regression test for issue #1359 review: the inspector used to
    // early-return on `node === null` before any `useMemo`, then run
    // hooks when a node was selected — React would throw "Rendered
    // more hooks than during the previous render". The fix moves all
    // hooks above the null branch.
    const onClose = vi.fn();
    const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    try {
      const { rerender } = render(
        <CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />
      );
      // Empty-state — body still renders but no per-node form.
      expect(screen.getByTestId('circuit-inspector')).toBeTruthy();
      expect(screen.queryByTestId('inspector-prompt')).toBeNull();

      // Click a node → useMemo for reachable fires for the first time.
      fireEvent.click(screen.getByTestId('circuit-node-spawn'));
      expect(screen.getByTestId('inspector-prompt')).toBeTruthy();

      // Click the React Flow pane (NOT the canvas-wrapper, which
      // doesn't have onPaneClick attached — only `.react-flow__pane`
      // does). That handler calls setSelectedNodeId(null), taking
      // the inspector back to its empty state.
      const pane = screen.getByTestId('rf__wrapper').querySelector('.react-flow__pane');
      expect(pane).not.toBeNull();
      fireEvent.click(pane!);
      // After deselect, the inspector renders the empty state.
      expect(screen.queryByTestId('inspector-prompt')).toBeNull();
      expect(screen.getByTestId('circuit-inspector')).toBeTruthy();

      // No "Rendered more hooks than during the previous render"
      // invariant should have been raised — React prints that to
      // console.error. (Other warnings may be benign; we only fail
      // the test if React's hook-count invariant shows up.)
      const invariantCalls = consoleSpy.mock.calls.filter((args) => {
        const msg = String(args[0] ?? '');
        return msg.includes('Rendered more hooks') || msg.includes('rendered fewer hooks');
      });
      expect(invariantCalls).toEqual([]);

      // Smoke re-render with the same props — stable hook count.
      rerender(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />);
      expect(screen.getByTestId('circuit-inspector')).toBeTruthy();
    } finally {
      consoleSpy.mockRestore();
    }
  });

  it('groups mustache suggestions by namespace when reachability is available', async () => {
    renderEditor();

    // Select the gate node — its upstream is the spawn, so spawn
    // output IS reachable for this node. (Selecting the spawn itself
    // would test the "no own output" rule, which we cover in the
    // model tests.)
    fireEvent.click(await screen.findByTestId('circuit-node-gate'));
    const prompt = await screen.findByTestId('inspector-command');
    // The gate has no template field; switch back to the spawn's
    // neighbour and inspect via the spawn's prompt — but use the
    // upstream gate's perspective via a custom graph instead.
    // Easier: select spawn and inspect the menu, which still proves
    // grouping even though spawn-output is unreachable for spawn.
    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const spawnPrompt = await screen.findByTestId('inspector-prompt');
    await userEvent.setup().type(spawnPrompt, 'Fix {{{{ ');

    const menu = await screen.findByTestId('mustache-menu');
    // Group headers render for every reachable namespace that has chips.
    for (const group of ['circuit', 'node', 'autopilot']) {
      expect(menu.querySelector(`[data-testid="mustache-group-${group}"]`)).not.toBeNull();
    }
    expect(menu.querySelector('[data-testid="mustache-group-issue"]')).toBeNull();
    expect(menu.querySelector('[data-testid="mustache-group-verification"]')).toBeNull();
    // The spawn cannot reach its OWN output (temporal paradox guard).
    expect(menu.querySelector('[data-testid="mustache-chip-node.spawn.output"]')).toBeNull();
    // Silence the unused-variable lint.
    expect(prompt).toBeTruthy();
  });

  it('filters unreachable mustache chips from autocomplete', async () => {
    renderEditor();

    // The spawn node in the fixture has no issue-label trigger upstream
    // (the trigger is `manual`), so `issue.*` chips must not be offered.
    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const prompt = await screen.findByTestId('inspector-prompt');
    await userEvent.setup().type(prompt, 'Fix {{{{ ');

    const menu = await screen.findByTestId('mustache-menu');
    // circuit.* and node.id are always live (trigger wrapper / with_node).
    expect(menu.querySelector('[data-testid="mustache-chip-issue.number"]')).toBeNull();
    // `circuit.name` and the Autopilot context are always reachable.
    expect(menu.querySelector('[data-testid="mustache-chip-circuit.name"][data-reachable="true"]')).not.toBeNull();
    // The spawn's own output is NOT reachable — a node cannot read
    // its own terminal output before it has produced any.
    expect(menu.querySelector('[data-testid="mustache-chip-node.spawn.output"]')).toBeNull();
  });

  it('renders only reachable values in the inspector context reference drawer', async () => {
    renderEditor();

    // Select the gate — its upstream IS the spawn, so spawn output
    // IS reachable from the gate's perspective.
    fireEvent.click(await screen.findByTestId('circuit-node-gate'));
    // The gate has no template field; the drawer is hidden. Switch
    // back to the spawn node and verify the temporal-paradox guard.
    fireEvent.click(await screen.findByTestId('circuit-node-spawn'));
    const drawer = await screen.findByTestId('inspector-context-reference');
    // For the spawn itself, `circuit.name` is always reachable,
    // `issue.*` is empty (manual trigger, not an issue-label trigger),
    // and `node.spawn.output` is NOT reachable (the spawn cannot
    // consume its own output — the BFS seeds from predecessors).
    expect(drawer.querySelector('[data-testid="context-reference-issue.number"]')).toBeNull();
    expect(drawer.querySelector('[data-testid="context-reference-circuit.name"]')!.getAttribute('data-reachable')).toBe('true');
    expect(drawer.querySelector('[data-testid="context-reference-node.spawn.output"]')).toBeNull();
    // Group headers render under the spec ordering for reachable values.
    expect(drawer.querySelector('[data-testid="context-group-issue"]')).toBeNull();
    expect(drawer.querySelector('[data-testid="context-group-circuit"]')).not.toBeNull();
  });

  it('downstream drawer renders spawn_output group + node.<id>.output reference rows', async () => {
    // Fixture: trigger → spawn_1 → spawn_2 → notify. Selecting
    // `notify` must show BOTH spawns' outputs as reachable context,
    // routed into the dedicated `spawn_output` group (issue #1359
    // round-3 review: previously the drawer bucketed `node.<id>.output`
    // under the top-level `node` namespace and they ended up in the
    // wrong bucket / were missing from the reference list).
    const GRAPH_WITH_DOWNSTREAM = JSON.stringify({
      version: 1,
      nodes: [
        { id: 'trigger', type: { type: 'manual' } },
        {
          id: 'spawn_1',
          type: {
            type: 'spawn_agent_node',
            prompt: 'first',
            name: null,
          },
        },
        {
          id: 'spawn_2',
          type: {
            type: 'spawn_agent_node',
            prompt: 'second',
            name: null,
          },
        },
        { id: 'notify', type: { type: 'notify', message: '' } },
      ],
      edges: [
        { from: 'trigger', to: 'spawn_1', condition: 'always' },
        { from: 'spawn_1', to: 'spawn_2', condition: 'always' },
        { from: 'spawn_2', to: 'notify', condition: 'always' },
      ],
    });
    render(
      <CircuitFlowEditor
        circuit={{ ...CIRCUIT, graph_json: GRAPH_WITH_DOWNSTREAM }}
        runs={[]}
        onClose={() => {}}
      />
    );

    fireEvent.click(await screen.findByTestId('circuit-node-notify'));
    const drawer = await screen.findByTestId('inspector-context-reference');
    // spawn_output group exists and contains BOTH spawn ids (sorted).
    expect(drawer.querySelector('[data-testid="context-group-spawn_output"]')).not.toBeNull();
    expect(drawer.querySelector('[data-testid="context-reference-node.spawn_1.output"]')).not.toBeNull();
    expect(drawer.querySelector('[data-testid="context-reference-node.spawn_2.output"]')).not.toBeNull();
    // Both are reachable (the drawer knows they're upstream producers).
    expect(drawer.querySelector('[data-testid="context-reference-node.spawn_1.output"]')!.getAttribute('data-reachable')).toBe('true');
    expect(drawer.querySelector('[data-testid="context-reference-node.spawn_2.output"]')!.getAttribute('data-reachable')).toBe('true');
  });

  it('offers an upstream spawn target picker on InjectPty and SetNodeStatus', async () => {
    const GRAPH_WITH_TARGETS = JSON.stringify({
      version: 1,
      nodes: [
        { id: 'trigger', type: { type: 'manual' } },
        {
          id: 'spawn_1',
          type: {
            type: 'spawn_agent_node',
            prompt: 'first',
            name: null,
          },
        },
        {
          id: 'spawn_2',
          type: {
            type: 'spawn_agent_node',
            prompt: 'second',
            name: null,
          },
        },
        { id: 'inject', type: { type: 'inject_pty', prompt: 'go' } },
        { id: 'status', type: { type: 'set_node_status', status: 'running' } },
      ],
      edges: [
        { from: 'trigger', to: 'spawn_1', condition: 'always' },
        { from: 'spawn_1', to: 'spawn_2', condition: 'always' },
        { from: 'spawn_2', to: 'inject', condition: 'always' },
        { from: 'inject', to: 'status', condition: 'always' },
      ],
    });
    render(
      <CircuitFlowEditor
        circuit={{ ...CIRCUIT, graph_json: GRAPH_WITH_TARGETS }}
        runs={[]}
        onClose={() => {}}
      />
    );

    fireEvent.click(await screen.findByTestId('circuit-node-inject'));
    const injectSelect = await screen.findByTestId('inspector-target-node') as HTMLSelectElement;
    const injectOptions = Array.from(injectSelect.options).map((o) => o.value);
    // Both spawns are upstream of `inject` — sorted by id.
    expect(injectOptions).toEqual(expect.arrayContaining(['', 'spawn_1', 'spawn_2']));
    // Picking spawn_2 commits the target.
    fireEvent.change(injectSelect, { target: { value: 'spawn_2' } });
    fireEvent.click(screen.getByTestId('editor-save'));
    await waitFor(() => {
      const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === 'update_circuit_graph');
      const graph = JSON.parse((call?.[1] as Record<string, unknown>).graphJson as string);
      const inject = graph.nodes.find((n: { id: string }) => n.id === 'inject');
      expect(inject.type.target_node_id).toBe('spawn_2');
    });

    // Now the status node — its upstream is inject + spawn_2; the
    // picker must NOT include `inject` (it's not a spawn) but DOES
    // include both spawns.
    fireEvent.click(await screen.findByTestId('circuit-node-status'));
    const statusSelect = (await screen.findByTestId('inspector-target-node')) as HTMLSelectElement;
    const statusOptions = Array.from(statusSelect.options).map((o) => o.value);
    expect(statusOptions).toEqual(expect.arrayContaining(['', 'spawn_1', 'spawn_2']));
    expect(statusOptions).not.toContain('inject');
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
    // The drawer speaks the same humanised status vocabulary as the Probe's
    // run cards (#1468): 'blocked' renders as "Needs approval", and the raw
    // token stays on the data attribute for machine assertions.
    expect(steps.textContent).toContain('Needs approval');
    expect(
      screen.getByTestId('run-step-11-gate').getAttribute('data-step-status')
    ).toBe('blocked');

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

  // -- dirty guard (issue #1244) ---------------------------------------------
  // A reflexive Escape or ✕ click must not silently destroy an unsaved
  // graph — the editor mirrors `<Modal dirty>`'s discard-banner contract.

  it('Escape with an unsaved edit shows the discard banner and does NOT close', async () => {
    const onClose = vi.fn();
    render(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />);

    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    expect(screen.getByTestId('editor-dirty')).toBeTruthy();
    expect(screen.queryByTestId('editor-discard-banner')).toBeNull();

    fireEvent.keyDown(window, { key: 'Escape' });

    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByTestId('editor-discard-banner')).toBeTruthy();
    expect(screen.getByTestId('circuit-flow-editor')).toBeTruthy();
  });

  it('Escape with no unsaved edit still closes (existing behaviour preserved)', () => {
    const onClose = vi.fn();
    render(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />);

    fireEvent.keyDown(window, { key: 'Escape' });

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('second Escape while the banner is up dismisses only the banner', async () => {
    const onClose = vi.fn();
    render(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />);

    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(screen.getByTestId('editor-discard-banner')).toBeTruthy();

    fireEvent.keyDown(window, { key: 'Escape' });

    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByTestId('editor-discard-banner')).toBeNull();
    expect(screen.getByTestId('circuit-flow-editor')).toBeTruthy();
  });

  it('clicking Discard closes the editor and removes the banner', async () => {
    const onClose = vi.fn();
    render(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />);

    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    fireEvent.keyDown(window, { key: 'Escape' });
    const banner = screen.getByTestId('editor-discard-banner');
    expect(banner).toBeTruthy();

    fireEvent.click(screen.getByTestId('editor-discard-confirm'));

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId('editor-discard-banner')).toBeNull();
  });

  it('clicking Keep editing hides the banner and does NOT close', async () => {
    const onClose = vi.fn();
    render(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />);

    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(screen.getByTestId('editor-discard-banner')).toBeTruthy();

    fireEvent.click(screen.getByTestId('editor-discard-cancel'));

    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByTestId('editor-discard-banner')).toBeNull();
    expect(screen.getByTestId('circuit-flow-editor')).toBeTruthy();
  });

  it('✕ click with an unsaved edit shows the discard banner and does NOT close', async () => {
    const onClose = vi.fn();
    render(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />);

    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    expect(screen.getByTestId('editor-dirty')).toBeTruthy();

    fireEvent.click(screen.getByTestId('editor-close'));

    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByTestId('editor-discard-banner')).toBeTruthy();
  });

  it('✕ click with no unsaved edit still closes (existing behaviour preserved)', () => {
    const onClose = vi.fn();
    render(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />);

    fireEvent.click(screen.getByTestId('editor-close'));

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('banner auto-dismisses when the parent re-emits a graph_json that matches the working copy', async () => {
    // Mirrors `<Modal dirty>`'s regression test: after Save succeeds the
    // parent refetches the canonical row and re-emits the graph_json, so
    // dirty flips false. The banner must go away — otherwise the user
    // sits behind "Discard unsaved changes?" for content they just saved.
    const onClose = vi.fn();
    const original = JSON.parse(CIRCUIT.graph_json) as {
      version: number;
      nodes: Array<{ id: string; type: unknown }>;
      edges: unknown[];
    };
    const updatedGraphJson = JSON.stringify({
      ...original,
      nodes: [
        ...original.nodes,
        { id: 'notify_1', type: { type: 'notify', message: '' } },
      ],
    });

    const { rerender } = render(
      <CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={onClose} />
    );
    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(screen.getByTestId('editor-discard-banner')).toBeTruthy();

    rerender(
      <CircuitFlowEditor
        circuit={{ ...CIRCUIT, graph_json: updatedGraphJson }}
        runs={[]}
        onClose={onClose}
      />
    );

    expect(screen.queryByTestId('editor-discard-banner')).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('moves focus to the Keep-editing button when the banner appears (WAI-ARIA APG alertdialog)', async () => {
    render(<CircuitFlowEditor circuit={CIRCUIT} runs={[]} onClose={() => {}} />);
    fireEvent.click(await screen.findByTestId('palette-add-notify'));
    fireEvent.keyDown(window, { key: 'Escape' });

    expect(document.activeElement).toBe(screen.getByTestId('editor-discard-cancel'));
  });
});
