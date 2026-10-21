/**
 * Tests for the Autopilot Circuits Probe tab (spec #1205 / walking
 * skeleton #1206).
 *
 * Strategy mirrors `autopilot-probe-tab.test.tsx`: mount the full
 * `ProbePanel` with the Circuits destination opened via `openProbeTab`
 * (the post-#1375 on-demand entry point), then assert on rendered
 * structure and the exact IPC contract the tab fires. Wire shapes come
 * from `src/types/generated/` so drift between the Rust structs and
 * these fixtures is caught at compile time.
 */

import { describe, it, expect, vi, beforeAll, beforeEach } from 'vitest';
import { preloadProbeTabs } from './helpers/preloadProbeTabs';
import { render, screen, waitFor, fireEvent, act, configure } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';

// Every async query in this file polls up to RTL's 1s default. Under the
// full `vitest run tests/unit tests/integration` load (263 files, one
// threads pool — the exact command CI runs) the first render of this tab can
// miss that window and fail a test that passes in isolation, which is how
// issue #1520's restored CI found it. The timeout is only an upper bound:
// a satisfied query resolves immediately, so raising it cannot turn a real
// failure into a pass, it only stops the suite from reporting load as a
// product regression. Assertions are unchanged.
configure({ asyncUtilTimeout: 10_000 });
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { AutopilotCircuit } from '../../src/types/generated/AutopilotCircuit';
import type { CircuitRunDetail } from '../../src/types/generated/CircuitRunDetail';
import type { CircuitQueueEntry } from '../../src/types/generated/CircuitQueueEntry';
import type { CircuitHistoryEntry } from '../../src/types/generated/CircuitHistoryEntry';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { formatWallClock } from '../../src/components/Circuits/runIdentity';
import { openProbeDestination } from './helpers/openProbeDestination';
// Direct wrapper access for the IPC-contract block below.
import {
  listCircuitsWithRuns,
  listCircuitQueue,
  listCircuitProbe,
  createCircuit,
  setCircuitEnabled,
  deleteCircuit,
  triggerCircuitNow,
  listCircuitRuns,
  pauseCircuitRun,
  resumeCircuitRun,
  cancelCircuitRun,
  cancelCircuitRuns,
  moveCircuitRun,
  reorderCircuitQueue,
  approveCircuitStep,
  getCircuit,
  updateCircuitGraph,
} from '../../src/lib/tauri';

const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
  // Circuits Probe reads this from the mesh row (#1475 — wire path
  // already exists post-#1470). The default-2 is what the mesh would
  // have after a fresh upgrade; pin it so the test fixtures match the
  // contract the Probe reads.
  circuit_run_capacity: 2,
};

const CIRCUIT: AutopilotCircuit = {
  id: 7,
  mesh_id: 42,
  name: 'nightly-sweep',
  description: '',
  enabled: true,
  graph_json: '{"version":1,"nodes":[],"edges":[]}',
  created_at: '2026-08-22 10:00:00',
  updated_at: '2026-08-22 10:00:00',
  is_preset: false,
};

const RUN_DONE: CircuitRunDetail = {
  run: {
    id: 11,
    circuit_id: 7,
    mesh_id: 42,
    trigger_identity: 'manual:1724000000000',
    state: 'completed',
    context_json: '{}',
    source_agent_node_id: null,
    created_at: '2026-08-22 10:05:00',
    updated_at: '2026-08-22 10:07:00',
    attention_dismissed_at: null,
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
      node_id: 'spawn',
      agent_node_id: 900,
      status: 'completed',
      attempt: 1,
      outcome: 'completed',
      error_message: null,
      started_at: '2026-08-22 10:05:00',
      completed_at: '2026-08-22 10:07:00',
    },
  ],
};

const RUN_RUNNING: CircuitRunDetail = {
  run: { ...RUN_DONE.run, id: 12, state: 'running' },
  steps: [],
};

const QUEUE: CircuitQueueEntry[] = [
  {
    run: { ...RUN_DONE.run, id: 21, state: 'pending', trigger_identity: 'issue:21:run' },
    circuit_name: 'nightly-sweep',
    queue_rank: 1,
  },
  {
    run: { ...RUN_DONE.run, id: 22, state: 'pending', trigger_identity: 'issue:22:run' },
    circuit_name: 'nightly-sweep',
    queue_rank: 2,
  },
];

/** One entry per wait/capacity/configuration/recovery kind (issue #1909). */
const HISTORY_ENTRIES: CircuitHistoryEntry[] = [
  { id: 1, node_id: null, attempt: null, kind: 'queue_wait',
    detail: '{"reason":"mesh_capacity","capacity":2}', source: 'circuit_worker.admission',
    disposition: 'waiting', observed_at: '2026-08-22 10:05:00' },
  { id: 2, node_id: 'spawn', attempt: 1, kind: 'step_capacity_wait',
    detail: '{"before":null,"after":"{\\"agent_limit\\":true}"}',
    source: 'circuit_worker.capacity', disposition: 'waiting', observed_at: '2026-08-22 10:05:10' },
  { id: 3, node_id: null, attempt: null, kind: 'review_continuation',
    detail: '{"from_run_id":88}', source: 'operator',
    disposition: 'applied', observed_at: '2026-08-22 10:06:00' },
];

function mockBackend(overrides: {
  circuits?: AutopilotCircuit[];
  runs?: CircuitRunDetail[];
  queue?: CircuitQueueEntry[];
queueWaits?: Record<string, string>;
  /** Backend-reported outstanding attention, independent of the ledger window. */
  outstandingAttention?: number;
  /** Make the unbounded count fail, as a transient backend failure would. */
  countAttentionFails?: boolean;
} = {}) {
  const circuits = overrides.circuits ?? [CIRCUIT];
  const runs = overrides.runs ?? [RUN_DONE, RUN_RUNNING];
  const queue = overrides.queue ?? [];
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'list_circuits') return Promise.resolve(circuits);
    if (cmd === 'list_circuit_runs') return Promise.resolve(runs);
    if (cmd === 'list_circuit_queue') return Promise.resolve(queue);
    if (cmd === 'circuit_run_history') {
      return Promise.resolve({ entries: HISTORY_ENTRIES, checkpoints: [], coverage: [] });
    }
    if (cmd === 'list_circuit_probe') {
      return Promise.resolve({
        circuits: circuits.map((circuit) => ({
          circuit,
          runs: runs.filter((r) => r.run.circuit_id === circuit.id),
        })),
        queue,
        queue_waits: overrides.queueWaits,
      });
    }
    if (cmd === 'list_circuits_with_runs') {
      return Promise.resolve(
        circuits.map((circuit) => ({
          circuit,
          runs: runs.filter((r) => r.run.circuit_id === circuit.id),
        })),
      );
    }
    if (
      cmd === 'create_circuit' ||
      cmd === 'set_circuit_enabled' ||
      cmd === 'delete_circuit' ||
      cmd === 'pause_circuit_run' ||
      cmd === 'resume_circuit_run' ||
      cmd === 'cancel_circuit_run' ||
      cmd === 'cancel_circuit_runs' ||
      cmd === 'move_circuit_run' ||
      cmd === 'reorder_circuit_queue' ||
      cmd === 'approve_circuit_step'
    ) {
      return Promise.resolve(args && cmd === 'create_circuit' ? CIRCUIT : undefined);
    }
    // The bulk acknowledgement returns the number of runs it changed.
    if (cmd === 'dismiss_all_circuit_run_attention') return Promise.resolve(2);
    // The outstanding count is backend-computed and deliberately independent of
    // the windowed ledger payload (which caps attention at 50/circuit).
    if (cmd === 'count_circuit_run_attention') {
      if (overrides.countAttentionFails) return Promise.reject(new Error('count unavailable'));
      return Promise.resolve(overrides.outstandingAttention ?? runs.filter((r) => r.run.state === 'failed').length);
    }
    if (cmd === 'trigger_circuit_now') return Promise.resolve(13);
    // Default-fall-through so failures surface the unexpected command.
    return Promise.resolve({ cmd });
  });
}

/** Hold the initial snapshot so status assertions exercise the loaded state. */
function deferCircuitProbe() {
  const fallback = vi.mocked(invoke).getMockImplementation();
  let release!: () => void;
  const pending = new Promise<void>((resolve) => { release = resolve; });
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'list_circuit_probe') {
      return pending.then(() => fallback?.(cmd, args) ?? Promise.resolve({ cmd }));
    }
    return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
  });
  return release;
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  seedAgentNodes([]);
  useUIStore.setState({
    probeOpen: false,
    probeTab: 'files',
    activeDiffFile: null,
    activeCircuitEditorId: null,
  });
});

// Load the lazy Probe tab chunks up front so rendering a tab does not wait on disk.
beforeAll(preloadProbeTabs, 60_000);

describe('CircuitsProbeTab', () => {
  it('uses the active run allowance when the editable blueprint no longer contains review', async () => {
    mockBackend({ circuits: [CIRCUIT], runs: [{ ...RUN_RUNNING, review_round_limit: 3 }] });
    openProbeDestination('circuits');
    const select = await screen.findByRole('combobox', { name: 'Review rounds' }) as HTMLSelectElement;
    expect(select.value).toBe('3');
  });
  it('edits review rounds directly on a collapsed run card and reflects the refreshed allowance', async () => {
    const graph = { version: 3, nodes: [
      { id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
      { id: 'retry', type: { type: 'retry_limit', max_retries: 3 } },
    ], edges: [{ from: 'retry', to: 'verdict', condition: 'always' }, { from: 'verdict', to: 'retry', condition: 'always' }] };
    const circuits = [{ ...CIRCUIT, graph_json: JSON.stringify(graph) }];
    const run = { ...RUN_RUNNING, review_round_limit: 3, run: { ...RUN_RUNNING.run, context_json: '{"retry.attempt":"3","retry.max_retries":"3"}' } };
    mockBackend({ circuits, runs: [run] });
    const fallback = vi.mocked(invoke).getMockImplementation();
    let finish!: () => void;
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => cmd === 'set_circuit_review_round_limit'
      ? new Promise<void>((resolve) => { finish = resolve; }) : fallback!(cmd, args));
    openProbeDestination('circuits');
    const select = await screen.findByRole('combobox', { name: 'Review rounds' }) as HTMLSelectElement;
    expect(screen.getByTestId('run-toggle-12').getAttribute('aria-expanded')).toBe('false');
    fireEvent.change(select, { target: { value: '0' } });
    expect(invoke).toHaveBeenCalledWith('set_circuit_review_round_limit', { runId: 12, maxRounds: 0 });
    expect(select.disabled).toBe(true);
    mockBackend({ circuits, runs: [{ ...run, review_round_limit: 0, run: { ...run.run, context_json: '{"retry.attempt":"3","retry.max_retries":"unlimited","review.round_limit":"0"}' } }] });
    await act(async () => finish());
    await waitFor(() => expect((screen.getByRole('combobox', { name: 'Review rounds' }) as HTMLSelectElement).value).toBe('0'));
    expect(screen.getByTestId('run-review-rounds-12').textContent).toBe('Round 3 of unlimited');
  });
  it('exposes the built-in Review Blueprint before it has any runs', async () => {
    mockBackend({ circuits: [{ ...CIRCUIT, is_preset: true, enabled: false }], runs: [] });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-manage'));
    const inspect = await screen.findByRole(
      'button',
      { name: 'Inspect Review Blueprint' },
      { timeout: 10_000 },
    );
    fireEvent.click(inspect);
    expect(useUIStore.getState().activeCircuitEditorId).toBe(CIRCUIT.id);
    expect(screen.queryByLabelText(`Enable ${CIRCUIT.name}`)).toBeNull();
  });

  it('adds one round to a failed review and focuses the same run', async () => {
    const graph = { version: 3, nodes: [{ id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } }], edges: [] };
    const failed = { run: { ...RUN_DONE.run, state: 'failed', source_agent_node_id: 42, context_json: '{"source.review_preset":"1"}' },
      steps: [{ ...RUN_DONE.steps[0], node_id: 'verdict', status: 'completed', outcome: 'working' }] };
    const circuits = [{ ...CIRCUIT, graph_json: JSON.stringify(graph) }];
    mockBackend({ circuits, runs: [failed] });
    const fallback = vi.mocked(invoke).getMockImplementation();
    let finish!: (id: number) => void;
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'continue_circuit_review') return new Promise<number>((resolve) => { finish = resolve; });
      return fallback?.(cmd, args) ?? Promise.resolve(null);
    });
    openProbeDestination('circuits');
    await screen.findByTestId('circuits-view-history');
    fireEvent.click(screen.getByTestId('circuits-view-history'));
    fireEvent.click(await screen.findByRole('button', { name: 'Review again · +1 round' }));
    expect(invoke).toHaveBeenCalledWith('continue_circuit_review', { runId: 11, additionalRounds: 1 });
    expect(screen.getByTestId('run-continue-review-11').hasAttribute('disabled')).toBe(true);
    mockBackend({ circuits, runs: [{ run: { ...failed.run, state: 'running', context_json: '{"source.review_preset":"1","review.extended":"1","retry.attempt":"4","retry.max_retries":"4"}' }, steps: [] }] });
    await act(async () => { finish(11); });
    await waitFor(() => expect(screen.getByTestId('circuits-view-activity').getAttribute('aria-selected')).toBe('true'));
    expect(screen.getByTestId('run-card-11').getAttribute('data-run-focused')).toBe('true');
    expect(screen.getByTestId('run-review-rounds-11').textContent).toBe('Round 4 of 4');
  });

  it('keeps failed review evidence and shows a recovery error when the saved agent is unavailable', async () => {
    const graph = { version: 3, nodes: [{ id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } }], edges: [] };
    mockBackend({ circuits: [{ ...CIRCUIT, graph_json: JSON.stringify(graph) }], runs: [{
      run: { ...RUN_DONE.run, state: 'failed', source_agent_node_id: 42, context_json: '{"source.review_preset":"1"}' },
      steps: [{ ...RUN_DONE.steps[0], node_id: 'verdict', outcome: 'working', error_message: 'Review requires changes' }],
    }] });
    const fallback = vi.mocked(invoke).getMockImplementation();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => cmd === 'continue_circuit_review'
      ? Promise.reject('The saved session is unavailable. Recover from the PR branch.') : fallback!(cmd, args));
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    fireEvent.click(await screen.findByRole('button', { name: 'Review again · +1 round' }));
    expect(await screen.findByText('The saved session is unavailable. Recover from the PR branch.')).toBeTruthy();
    expect(screen.getByTestId('run-card-11')).toBeTruthy();
    expect(screen.getByTestId('run-continue-review-11').hasAttribute('disabled')).toBe(false);
  });

  it('ignores an older snapshot when a refresh completes out of order', async () => {
    mockBackend();
    let resolveFirst!: (snapshot: unknown) => void;
    let resolveSecond!: (snapshot: unknown) => void;
    const firstSnapshot = new Promise<unknown>((resolve) => { resolveFirst = resolve; });
    const secondSnapshot = new Promise<unknown>((resolve) => { resolveSecond = resolve; });
    let probeCall = 0;
    const fallback = vi.mocked(invoke).getMockImplementation();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'list_circuit_probe') {
        return probeCall++ === 0 ? firstSnapshot : secondSnapshot;
      }
      return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
    });

    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-manage'));
    await waitFor(() => expect(vi.mocked(listen)).toHaveBeenCalledWith(
      'circuit-run-updated',
      expect.any(Function),
    ));

    // A run-update starts a newer request before the initial request returns.
    await emit('circuit-run-updated', {});
    await act(async () => {
      resolveSecond({
        circuits: [{ circuit: { ...CIRCUIT, name: 'newer snapshot' }, runs: [] }],
        queue: [],
      });
      await secondSnapshot;
    });
    expect(await screen.findByText('newer snapshot')).toBeTruthy();

    await act(async () => {
      resolveFirst({
        circuits: [{ circuit: CIRCUIT, runs: [] }],
        queue: [],
      });
      await firstSnapshot;
    });
    expect(screen.queryByText('nightly-sweep')).toBeNull();
    expect(screen.getByText('newer snapshot')).toBeTruthy();
  });

  it('does not reload or set state when an action resolves after unmount', async () => {
    mockBackend();
    let resolveTrigger!: () => void;
    const triggerPromise = new Promise<void>((resolve) => { resolveTrigger = resolve; });
    const fallback = vi.mocked(invoke).getMockImplementation();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'trigger_circuit_now') return triggerPromise;
      return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
    });

    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-manage'));
    const user = userEvent.setup();
    await user.click(await screen.findByTestId('circuit-trigger-7'));
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('trigger_circuit_now', { circuitId: 7 });

    act(() => useUIStore.setState({ probeOpen: false }));
    expect(screen.queryByTestId('circuits-probe-tab')).toBeNull();
    await act(async () => {
      resolveTrigger();
      await triggerPromise;
    });

    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_circuit_probe')).toHaveLength(1);
  });

  it('does not reload the previous mesh when an action resolves after a mesh switch', async () => {
    mockBackend();
    let resolveTrigger!: () => void;
    const triggerPromise = new Promise<void>((resolve) => { resolveTrigger = resolve; });
    const fallback = vi.mocked(invoke).getMockImplementation();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'trigger_circuit_now') return triggerPromise;
      return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
    });

    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-manage'));
    const user = userEvent.setup();
    await user.click(await screen.findByTestId('circuit-trigger-7'));

    const otherMesh = { ...MESH, id: 43, name: 'other mesh' };
    act(() => useMeshStore.setState({
      meshes: [MESH, otherMesh],
      meshesById: new Map([[MESH.id, MESH], [otherMesh.id, otherMesh]]),
      selectedMeshId: otherMesh.id,
    }));
    await waitFor(() => {
      expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_circuit_probe')).toHaveLength(2);
    });

    await act(async () => {
      resolveTrigger();
      await triggerPromise;
    });
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_circuit_probe')).toHaveLength(2);
  });

  it('keeps failures in History, not Activity', async () => {
    mockBackend({ runs: [
      { ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed' } },
      RUN_DONE, RUN_RUNNING,
    ] });
    openProbeDestination('circuits');
    await screen.findByTestId('run-card-12');
    // Activity is strictly live — the failed run must not present here.
    expect(screen.queryByTestId('run-card-9')).toBeNull();
    fireEvent.click(screen.getByTestId('circuits-view-history'));
    expect(screen.getByTestId('run-card-9')).toBeTruthy();
  });

  it('shows issue-led queued work and the recorded global limit beside spare run slots', async () => {
    const queue = [{ ...QUEUE[0], run: { ...QUEUE[0].run, context_json: JSON.stringify({ 'issue.number': '2185', 'issue.title': 'Reduce agent launch time' }) } }];
    mockBackend({ runs: [RUN_RUNNING], queue, queueWaits: { [queue[0].run.id]: JSON.stringify({ reason: 'agent_capacity', required: 2, available: 1 }) } });
    openProbeDestination('circuits');
    expect(await screen.findByText('1 / 2 slots occupied')).toBeTruthy();
    expect(screen.getByText(/Global agent limit/).textContent).toContain('2 agent slot(s); 1 free');
    expect(screen.getByTestId('circuit-activity-queue-summary').textContent).toContain('#2185 Reduce agent launch time');
    fireEvent.click(screen.getByTestId('circuits-view-queue'));
    expect(screen.getByTestId(`queue-run-${queue[0].run.id}`).textContent).toContain('#2185 Reduce agent launch time');
    expect(screen.getByTestId(`queue-pending-reason-${queue[0].run.id}`).textContent).toContain('Global agent limit');
  });

  it('does not present reordered activity cards as stable numbered slots', async () => {
    const running = { ...RUN_RUNNING, run: { ...RUN_RUNNING.run, updated_at: '2026-08-22 10:01:00' } };
    const paused = {
      ...RUN_RUNNING,
      run: { ...RUN_RUNNING.run, id: 13, circuit_id: 8, state: 'paused', updated_at: '2026-08-22 10:02:00' },
    };
    mockBackend({ circuits: [CIRCUIT, { ...CIRCUIT, id: 8, name: 'review' }], runs: [running, paused] });
    openProbeDestination('circuits');

    expect(await screen.findByTestId('run-card-12')).toBeTruthy();
    expect(screen.getByTestId('run-card-13')).toBeTruthy();
    expect(screen.getByText('2 / 2 slots occupied')).toBeTruthy();
    expect(screen.queryByText(/^Slot \d+$/)).toBeNull();
  });

  it('prioritizes attention in History and separates activity, queue, and configuration', async () => {
    const failed = { ...RUN_DONE, run: { ...RUN_DONE.run, id: 13, circuit_id: 8, state: 'failed' } };
    mockBackend({
      circuits: [CIRCUIT, { ...CIRCUIT, id: 8, name: 'review' }],
      runs: [RUN_DONE, RUN_RUNNING, failed],
      queue: QUEUE,
    });
    openProbeDestination('circuits');
    await screen.findByTestId('run-card-12');
    // Activity shows only the live run.
    const cards = screen.getAllByTestId(/^run-card-/);
    expect(cards.map((card) => card.getAttribute('data-run-state'))).toEqual(['running']);
    expect(screen.queryByTestId('run-card-11')).toBeNull();
    expect(screen.queryByTestId('run-card-13')).toBeNull();
    expect(screen.queryByTestId('queue-run-21')).toBeNull();
    expect(screen.queryByTestId('circuit-name-input')).toBeNull();
    expect(screen.getByTestId('circuits-status').textContent).toContain('2 queued');
    expect(screen.getByTestId('circuit-activity-queue-summary').textContent).toContain('Waiting in queue');
    expect(screen.queryByTestId('circuit-trigger-7')).toBeNull();
    expect(screen.queryByTestId('circuit-edit-flow-7')).toBeNull();
    expect(screen.getByTestId('circuits-view-queue').textContent).toContain('(2)');
    fireEvent.click(screen.getByTestId('circuits-view-history'));
    // History is attention-first: failed before completed.
    const historyCards = screen.getAllByTestId(/^run-card-/);
    expect(historyCards.map((card) => card.getAttribute('data-testid'))).toEqual(
      expect.arrayContaining([expect.stringContaining('run-card-13'), expect.stringContaining('run-card-11')])
    );
    expect(screen.getByTestId('run-card-11')).toBeTruthy();
    expect(screen.getByTestId('run-card-13')).toBeTruthy();
    expect(screen.queryByTestId('run-card-12')).toBeNull();
    fireEvent.click(screen.getByTestId('circuits-view-manage'));
    expect(screen.getByTestId('circuit-name-input')).toBeTruthy();
    expect(screen.queryAllByTestId(/^run-card-/)).toHaveLength(0);
  });

  it('lists circuits with their run ledger', async () => {
    mockBackend();
    openProbeDestination('circuits');

    expect(await screen.findByText('nightly-sweep')).toBeTruthy();
    // Run state renders in the humanised vocabulary (#1468); the raw DB
    // string stays available on `data-run-state` for machine assertions.
    expect(screen.getByTestId('run-state-12').textContent).toBe('Running');
    expect(screen.queryByTestId('run-state-11')).toBeNull();
    expect(screen.queryByTestId('circuit-name-input')).toBeNull();
    fireEvent.click(screen.getByTestId('circuits-view-history'));
    expect(await screen.findByTestId('run-state-11').then((el) => el.textContent)).toBe('Completed');
    expect(screen.getByTestId('run-card-11').getAttribute('data-run-state')).toBe('completed');
    // Steps are a vertical timeline, not a joined one-line string (#1468).
    expect(screen.queryByText(/trigger:completed/)).toBeNull();
    // The ledger and complete mesh queue load through one IPC payload.
    expect(invoke).toHaveBeenCalledWith('list_circuit_probe', { meshId: 42, limit: 10 });
  });

  it('surfaces the mesh-level run-admission detail on every queue row (#1475)', async () => {
    // The Probe's queue lists pending runs that the worker's
    // `may_admit_run` has parked against the mesh's circuit-run cap
    // (#1467). Each row should now explain *why* the run has not
    // started — without this, users see a bare "Queue" with no signal
    // that the cap is full.
    //
    // Saturate the mesh with two admitted `running` runs (cap=2 in the
    // MESH fixture) so `meshActiveRuns >= meshRunCapacity` and the
    // "all N … are busy" branch fires. With an empty ledger the
    // admission count is 0 and the wording falls into the hedged
    // "spare capacity" branch — wrong surface area for this AC.
    mockBackend({
      runs: [RUN_DONE, RUN_RUNNING, { run: { ...RUN_DONE.run, id: 13, state: 'running' }, steps: [] }],
      queue: [
        {
          run: { ...RUN_DONE.run, id: 21, state: 'pending', trigger_identity: 'issue:21:run' },
          circuit_name: 'nightly-sweep',
          queue_rank: 1,
        },
        {
          run: { ...RUN_DONE.run, id: 22, state: 'pending', trigger_identity: 'issue:22:run' },
          circuit_name: 'nightly-sweep',
          queue_rank: 2,
        },
      ],
    });
    openProbeDestination('circuits');

    // Pin the wording verbatim — the same shape #1468 uses for the
    // queued-step copy so a reword silently fails review.
    fireEvent.click(await screen.findByTestId('circuits-view-queue'));
    const reason21 = await screen.findByTestId('queue-pending-reason-21');
    expect(reason21.textContent).toContain("circuit-run slot");
    expect(reason21.textContent).toContain("this mesh's circuit-run slots");
    expect(reason21.textContent).toContain('2');

    const reason22 = screen.getByTestId('queue-pending-reason-22');
    expect(reason22.textContent).toBe(reason21.textContent);
  });

  it('reads the mesh\'s `circuit_run_capacity` from the mesh row, not from the circuit', async () => {
    // The admission copy comes from `meshRunCapacity` and `meshActiveRuns`
    // (#1467 / #1475), NOT from the circuit row. Set a mesh cap of 4 with
    // only one admitted run — the queue row must reflect the mesh number (4)
    // and the "spare capacity" wording (because we're under the cap, not full).
    useMeshStore.setState({
      meshes: [{ ...MESH, circuit_run_capacity: 4 }],
      meshesById: new Map([[MESH.id, { ...MESH, circuit_run_capacity: 4 }]]),
      selectedMeshId: MESH.id,
    });
    mockBackend({
      circuits: [CIRCUIT],
      runs: [RUN_DONE, RUN_RUNNING],
      queue: [
        {
          run: { ...RUN_DONE.run, id: 21, state: 'pending', trigger_identity: 'issue:21:run' },
          circuit_name: 'nightly-sweep',
          queue_rank: 1,
        },
      ],
    });
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-queue'));
    const reason = await screen.findByTestId('queue-pending-reason-21');
    expect(reason.textContent).toContain('allows 4 concurrent runs');
    // Belt-and-braces: the per-circuit number must not leak into the
    // mesh-level copy.
    expect(reason.textContent).not.toContain('this circuit');
  });

  it('New Circuit creates the skeleton and opens the canvas editor (#1209)', async () => {
    mockBackend();
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-manage'));
    await user.type(await screen.findByTestId('circuit-name-input'), 'review-bot');
    await user.click(screen.getByTestId('circuit-create-button'));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_circuit', {
        meshId: 42,
        name: 'review-bot',
        description: '',
        // The prompt is authored in the canvas editor's inspector now.
        initialPrompt: '',
        triggerKind: 'manual',
        triggerLabel: null,
        intervalSeconds: null,
        blueprint: 'walking_skeleton',
      });
      // The editor overlay mounts over the center workspace…
      expect(useUIStore.getState().activeCircuitEditorId).toBe(7);
    });
  });

  it('creates a GitHub-labelled circuit with its trigger label (issue #1208)', async () => {
    mockBackend();
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-manage'));
    await user.type(await screen.findByTestId('circuit-name-input'), 'issue-runner');
    await user.selectOptions(screen.getByTestId('circuit-trigger-select'), 'github_issue_label');
    // The create button stays disabled until the label is filled in.
    expect(
      (screen.getByTestId('circuit-create-button') as HTMLButtonElement).disabled
    ).toBe(true);
    await user.type(screen.getByTestId('circuit-trigger-label-input'), 'buildmesh:run');
    await user.click(screen.getByTestId('circuit-create-button'));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_circuit', expect.objectContaining({
        name: 'issue-runner',
        triggerKind: 'github_issue_label',
        triggerLabel: 'buildmesh:run',
      }));
    });
  });

  it('creates the issue-driven Autopilot review blueprint with its issue-label trigger', async () => {
    mockBackend();
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-manage'));
    await user.type(await screen.findByTestId('circuit-name-input'), 'autopilot-review');
    await user.selectOptions(
      screen.getByTestId('circuit-blueprint-select'),
      'issue_driven_autopilot_review'
    );
    expect((screen.getByTestId('circuit-trigger-select') as HTMLSelectElement).value).toBe(
      'github_issue_label'
    );
    expect((screen.getByTestId('circuit-trigger-select') as HTMLSelectElement).disabled).toBe(true);
    await user.type(screen.getByTestId('circuit-trigger-label-input'), 'buildmesh:run');
    await user.click(screen.getByTestId('circuit-create-button'));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_circuit', expect.objectContaining({
        name: 'autopilot-review',
        triggerKind: 'github_issue_label',
        triggerLabel: 'buildmesh:run',
        blueprint: 'issue_driven_autopilot_review',
      }));
    });
  });

  it('Edit Flow opens the canvas editor for that circuit (#1209)', async () => {
    mockBackend();
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-manage'));
    fireEvent.click(await screen.findByTestId('circuit-edit-flow-7'));
    expect(useUIStore.getState().activeCircuitEditorId).toBe(7);
  });

  it('Trigger Now mints a manual run', async () => {
    mockBackend();
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-manage'));
    await user.click(await screen.findByTestId('circuit-trigger-7'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('trigger_circuit_now', { circuitId: 7 });
    });
  });

  it('Trigger Now stays available on a disabled draft circuit', async () => {
    mockBackend({ circuits: [{ ...CIRCUIT, enabled: false }] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-manage'));
    const trigger = (await screen.findByTestId('circuit-trigger-7')) as HTMLButtonElement;
    expect(trigger.disabled).toBe(false);
    await user.click(trigger);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('trigger_circuit_now', { circuitId: 7 });
    });
  });

  it('toggling enable writes the flag', async () => {
    mockBackend();
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-manage'));
    const toggle = (await screen.findByTestId('circuit-enabled-7')) as HTMLInputElement;
    expect(toggle.checked).toBe(true);
    await user.click(toggle);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('set_circuit_enabled', {
        circuitId: 7,
        enabled: false,
      });
    });
  });

  it('confirms before deleting a circuit', async () => {
    mockBackend();
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-manage'));
    await user.click(await screen.findByTestId('circuit-delete-7'));
    expect(invoke).not.toHaveBeenCalledWith('delete_circuit', { circuitId: 7 });
    await user.click(screen.getByTestId('circuit-confirm-delete-7'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('delete_circuit', { circuitId: 7 });
    });
  });

  it('shows the complete queue nearest-first with reorder, top/bottom and cancel controls', async () => {
    mockBackend({ queue: QUEUE });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    expect(screen.queryByTestId('circuit-queue')).toBeNull();
    await user.click(await screen.findByTestId('circuits-view-queue'));
    const queue = await screen.findByTestId('circuit-queue');
    expect(
      Array.from(queue.querySelectorAll('[data-testid^="queue-run-"]')).map((row) =>
        row.getAttribute('data-testid')
      )
    ).toEqual(['queue-run-21', 'queue-run-22']);
    expect(queue.textContent).toContain('Next to start first');

    await user.click(screen.getByLabelText('Move run 22 up'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('move_circuit_run', { runId: 22, direction: 'up' });
    });
    await user.click(screen.getByLabelText('Move run 22 to top'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('move_circuit_run', { runId: 22, direction: 'top' });
    });
    await waitFor(() => {
      expect((screen.getByLabelText('Cancel run 21') as HTMLButtonElement).disabled).toBe(false);
    });
    await user.click(screen.getByLabelText('Cancel run 21'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('cancel_circuit_run', { runId: 21 });
    });
  });

  it('supports queue bulk selection and bulk cancel', async () => {
    mockBackend({ queue: QUEUE });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-queue'));
    await screen.findByTestId('circuit-queue');
    await user.click(screen.getByTestId('queue-select-21'));
    await user.click(screen.getByTestId('queue-cancel-selected'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('cancel_circuit_runs', { runIds: [21] });
    });
  });

  it('filters history by search text and attention-only', async () => {
    mockBackend({ runs: [
      { ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed', trigger_identity: 'issue:9:run' } },
      RUN_DONE,
    ] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    expect(screen.getByTestId('run-card-9')).toBeTruthy();
    expect(screen.getByTestId('run-card-11')).toBeTruthy();

    await user.click(screen.getByTestId('history-attention-toggle'));
    expect(screen.getByTestId('run-card-9')).toBeTruthy();
    expect(screen.queryByTestId('run-card-11')).toBeNull();

    await user.click(screen.getByTestId('history-attention-toggle'));
    await user.type(screen.getByTestId('history-search-input'), 'manual:1724000000000');
    expect(screen.queryByTestId('run-card-9')).toBeNull();
    expect(screen.getByTestId('run-card-11')).toBeTruthy();
  });

  /**
   * The bulk acknowledgement is one mesh-scoped IPC behind a two-step
   * confirmation — the count in the label is the number of runs History is
   * actually asking about, and one click must not dismiss anything.
   */
  it('acknowledges all outstanding attention with one confirmed mesh-scoped call', async () => {
    mockBackend({ runs: [
      { ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed', trigger_identity: 'issue:9:run' } },
      { ...RUN_DONE, run: { ...RUN_DONE.run, id: 8, state: 'failed', trigger_identity: 'issue:8:run' } },
      RUN_DONE,
    ] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));

    const dismissAll = await screen.findByTestId('history-dismiss-all');
    expect(dismissAll.textContent).toContain('Dismiss all (2)');

    // First click only arms the confirmation — nothing is dismissed yet.
    await user.click(dismissAll);
    await screen.findByTestId('history-dismiss-all-confirm');
    expect(invoke).not.toHaveBeenCalledWith('dismiss_all_circuit_run_attention', expect.anything());

    await user.click(screen.getByTestId('history-dismiss-all-confirm'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('dismiss_all_circuit_run_attention', { meshId: 42 });
    });
    // The backend picks the rows in SQL; the UI must not send an id list that
    // could drift from the predicate it is counting.
    expect(invoke).not.toHaveBeenCalledWith('dismiss_circuit_run_attention', expect.anything());
  });

  it('lets a confirmed bulk dismissal be abandoned', async () => {
    mockBackend({ runs: [
      { ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed', trigger_identity: 'issue:9:run' } },
    ] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    await user.click(await screen.findByTestId('history-dismiss-all'));
    await user.click(screen.getByTestId('history-dismiss-all-cancel'));

    await screen.findByTestId('history-dismiss-all');
    expect(invoke).not.toHaveBeenCalledWith('dismiss_all_circuit_run_attention', expect.anything());
  });

  it('withholds the bulk action when the unbounded count is unavailable', async () => {
    // A windowed count is exactly the understatement the backend count exists to
    // prevent, so a failed count must hide the control rather than label it with a
    // number that does not describe what the command will do.
    mockBackend({
      runs: [{ ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed' } }],
      countAttentionFails: true,
    });
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    // The ledger still renders — a failed count must not blank the tab.
    await screen.findByTestId('run-card-9');
    expect(screen.queryByTestId('history-dismiss-all')).toBeNull();
  });

  /**
   * Scoping the count to its mesh id is not sufficient on its own: on an
   * A→B→A round trip the original mesh's id matches again, so a count fetched
   * before A's runs could have changed would pass the guard and sit above a
   * confirmation that dismisses those changed runs. The count must be dropped
   * on every switch, not merely re-scoped.
   */
  it('does not resurrect a mesh’s old count after an A→B→A round trip', async () => {
    mockBackend({
      runs: [{ ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed' } }],
      outstandingAttention: 7,
    });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    expect(await screen.findByTestId('history-dismiss-all')).toHaveProperty(
      'textContent',
      expect.stringContaining('7'),
    );

    const fallback = vi.mocked(invoke).getMockImplementation();
    // Mesh 42 (A) is asked twice through this override: the initial count of 7
    // came from mockBackend before it was installed, so the first call this
    // override sees is the return trip. That one is slow, and when it finally
    // answers the backlog is 2 — not the 7 fetched on the first visit.
    let aCountCalls = 0;
    let releaseReturn!: () => void;
    const returnPending = new Promise<void>((resolve) => { releaseReturn = resolve; });
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      const meshId = (args as { meshId?: number })?.meshId;
      if (cmd === 'count_circuit_run_attention' && meshId === 42) {
        aCountCalls += 1;
        if (aCountCalls === 1) return returnPending.then(() => 2);
        return Promise.resolve(7);
      }
      if (meshId === 43) return Promise.reject(new Error('mesh B is unreachable'));
      return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
    });

    const otherMesh = { ...MESH, id: 43, name: 'other mesh' };
    const select = (id: number) => act(() => useMeshStore.setState({
      meshes: [MESH, otherMesh],
      meshesById: new Map([[MESH.id, MESH], [otherMesh.id, otherMesh]]),
      selectedMeshId: id,
    }));

    select(43);
    await waitFor(() => {
      expect(vi.mocked(invoke).mock.calls.some(([, args]) =>
        (args as { meshId?: number })?.meshId === 43)).toBe(true);
    });
    select(42);

    // Back on A, whose refresh is still in flight: the old 7 must not return.
    expect(screen.queryByTestId('history-dismiss-all')).toBeNull();

    await act(async () => {
      releaseReturn();
      await returnPending;
    });

    // A's own fresh count is what may now be shown — 2, not the stale 7.
    await waitFor(() => {
      expect(screen.getByTestId('history-dismiss-all')).toHaveProperty(
        'textContent',
        expect.stringContaining('2'),
      );
    });
  });

  /**
   * A count belongs to the mesh that produced it. Between a mesh switch and the
   * new fetch landing, mesh A's count is still in state — rendering it above
   * mesh B's runs would have the user confirm a number that describes neither
   * the list nor the action the handler would take.
   */
  it('never shows one mesh’s bulk count over another mesh’s runs', async () => {
    mockBackend({
      runs: [{ ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed' } }],
      outstandingAttention: 7,
    });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    expect(await screen.findByTestId('history-dismiss-all')).toHaveProperty(
      'textContent',
      expect.stringContaining('7'),
    );

    // Hold mesh B's fetch open so the switch is observed mid-flight.
    const fallback = vi.mocked(invoke).getMockImplementation();
    let releaseMeshB!: () => void;
    const meshBPending = new Promise<void>((resolve) => { releaseMeshB = resolve; });
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      const meshId = (args as { meshId?: number })?.meshId;
      if (meshId !== 43) return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
      // Mesh B's fetch is held open, then reports a backlog of its own.
      if (cmd === 'list_circuit_probe') return meshBPending.then(() => ({ circuits: [], queue: [] }));
      if (cmd === 'count_circuit_run_attention') {
        return meshBPending.then(() => 0);
      }
      return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
    });

    const otherMesh = { ...MESH, id: 43, name: 'other mesh' };
    act(() => useMeshStore.setState({
      meshes: [MESH, otherMesh],
      meshesById: new Map([[MESH.id, MESH], [otherMesh.id, otherMesh]]),
      selectedMeshId: otherMesh.id,
    }));

    // Mesh A's 7 must not survive into mesh B's view, pending or not.
    await waitFor(() => {
      expect(vi.mocked(invoke).mock.calls.filter(([cmd, args]) =>
        cmd === 'list_circuit_probe' && (args as { meshId?: number })?.meshId === 43)).toHaveLength(1);
    });
    expect(screen.queryByTestId('history-dismiss-all')).toBeNull();

    await act(async () => {
      releaseMeshB();
      await meshBPending;
    });
    expect(screen.queryByTestId('history-dismiss-all')).toBeNull();
  });

  it('does not fall back to the previous mesh’s count when the new one fails to load', async () => {
    mockBackend({
      runs: [{ ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed' } }],
      outstandingAttention: 7,
    });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    expect(await screen.findByTestId('history-dismiss-all')).toBeTruthy();

    const fallback = vi.mocked(invoke).getMockImplementation();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if ((args as { meshId?: number })?.meshId === 43) {
        return Promise.reject(new Error('mesh B is unreachable'));
      }
      return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
    });

    const otherMesh = { ...MESH, id: 43, name: 'other mesh' };
    act(() => useMeshStore.setState({
      meshes: [MESH, otherMesh],
      meshesById: new Map([[MESH.id, MESH], [otherMesh.id, otherMesh]]),
      selectedMeshId: otherMesh.id,
    }));

    // Mesh A's 7 must not linger over mesh B, which failed to report anything.
    await waitFor(() => {
      expect(screen.queryByTestId('history-dismiss-all')).toBeNull();
    });
  });

  it('discards an armed confirmation when the mesh changes', async () => {
    mockBackend({
      runs: [{ ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed' } }],
      outstandingAttention: 3,
    });
    const user = userEvent.setup();
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));

    await user.click(await screen.findByTestId('history-dismiss-all'));
    await screen.findByTestId('history-dismiss-all-confirm');

    const otherMesh = { ...MESH, id: 43, name: 'other mesh' };
    act(() => useMeshStore.setState({
      meshes: [MESH, otherMesh],
      meshesById: new Map([[MESH.id, MESH], [otherMesh.id, otherMesh]]),
      selectedMeshId: otherMesh.id,
    }));

    // The confirmation was armed against mesh 42; it must not still be showing
    // a number whose action would now target mesh 43.
    await waitFor(() => {
      expect(screen.queryByTestId('history-dismiss-all-confirm')).toBeNull();
    });
  });

  /**
   * A bulk acknowledgement changes no run state, so no `circuit-run-updated`
   * event accompanies it. Without this subscription a second window — or any
   * view that did not perform the clearing — would keep showing the old count
   * and the now-acknowledged runs.
   */
  it('refreshes when attention is acknowledged elsewhere', async () => {
    mockBackend({ runs: [RUN_DONE] });
    openProbeDestination('circuits');
    await waitFor(() => expect(vi.mocked(listen)).toHaveBeenCalledWith(
      'circuit-runs-attention-dismissed',
      expect.any(Function),
    ));

    const before = vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_circuit_probe').length;
    await act(async () => {
      await emit('circuit-runs-attention-dismissed', { mesh_id: MESH.id, changed: 3 });
    });
    await waitFor(() => {
      expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_circuit_probe').length)
        .toBeGreaterThan(before);
    });
  });

  it('offers no bulk dismissal when nothing is asking for attention', async () => {
    mockBackend({ runs: [RUN_DONE] });
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    await screen.findByTestId('run-card-11');
    expect(screen.queryByTestId('history-dismiss-all')).toBeNull();
  });

  /**
   * The ledger payload is capped at 50 attention runs per circuit, so counting
   * it would read "Dismiss 50" while the command acknowledged every outstanding
   * run on the mesh — including runs the user never saw in the list. The label
   * must come from the backend's unbounded count.
   */
  it('labels the bulk dismissal with the backend count, not the windowed list', async () => {
    mockBackend({
      runs: [{ ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed' } }],
      outstandingAttention: 137,
    });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));

    // One failed run is loaded, but the mesh genuinely has 137 outstanding.
    expect(await screen.findByTestId('history-dismiss-all')).toHaveProperty(
      'textContent',
      expect.stringContaining('137'),
    );
    await user.click(screen.getByTestId('history-dismiss-all'));
    expect(await screen.findByTestId('history-dismiss-all-confirm')).toHaveProperty(
      'textContent',
      expect.stringContaining('137'),
    );
  });

  it('reorders queue ids purely for drag-drop', async () => {
    const { reorderQueueIds } = await import('../../src/components/Probe/CircuitsProbeTab');
    expect(reorderQueueIds([21, 22, 23], 23, 21)).toEqual([23, 21, 22]);
    expect(reorderQueueIds([21, 22], 21, 21)).toEqual([21, 22]);
    expect(reorderQueueIds([21, 22], 99, 21)).toEqual([21, 22]);
  });

  it('cancels the whole queue with one batch IPC, not one call per run', async () => {
    mockBackend({ queue: QUEUE });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-queue'));
    await screen.findByTestId('circuit-queue');
    await user.click(screen.getByTestId('queue-cancel-all'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('cancel_circuit_runs', { runIds: [21, 22] });
    });
    // No N+1 per-run cancels for a batch action.
    expect(invoke).not.toHaveBeenCalledWith('cancel_circuit_run', { runId: 21 });
    expect(invoke).not.toHaveBeenCalledWith('cancel_circuit_run', { runId: 22 });
  });

  it('labels History attention from history runs only, not live paused runs', async () => {
    const RUN_PAUSED: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 16, state: 'paused' },
      steps: [],
    };
    mockBackend({ runs: [RUN_PAUSED, RUN_DONE] });
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    // One quiet completed run, zero history attention — the live paused run
    // must not inflate this label.
    expect(screen.getByTestId('history-attention-toggle')).toBeTruthy();
    expect(screen.getByText(/Needs attention \(0\)/)).toBeTruthy();
  });

  it('keeps History and Queue toolbars outside the scroll body', async () => {
    mockBackend({ queue: QUEUE });
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    const body = screen.getByTestId('circuits-probe-body');
    expect(body.contains(screen.getByTestId('history-search-input'))).toBe(false);
    expect(body.contains(screen.getByTestId('circuit-history-filters'))).toBe(false);

    fireEvent.click(screen.getByTestId('circuits-view-queue'));
    await screen.findByTestId('circuit-queue');
    expect(body.contains(screen.getByTestId('queue-cancel-all'))).toBe(false);
    expect(body.contains(screen.getByTestId('queue-select-all'))).toBe(false);
  });

  it('reloads the queue after a stale reorder failure instead of leaving it stale', async () => {
    mockBackend({ queue: QUEUE });
    const fallback = vi.mocked(invoke).getMockImplementation();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'reorder_circuit_queue') {
        return Promise.reject(new Error('queue changed while reordering - refresh and retry'));
      }
      return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
    });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await user.click(await screen.findByTestId('circuits-view-queue'));
    await screen.findByTestId('circuit-queue');
    // Drive the failure through the supported Top jump (same error path as
    // drag-drop reorder): the banner shows the domain message and the tab
    // refetches so the stale order does not sit on screen.
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'move_circuit_run') {
        return Promise.reject(new Error('queue changed while reordering - refresh and retry'));
      }
      return fallback?.(cmd, args) ?? Promise.resolve({ cmd });
    });
    await user.click(screen.getByLabelText('Move run 21 to bottom'));
    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('refresh and retry');
    });
    await waitFor(() => {
      const probes = vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_circuit_probe');
      expect(probes.length).toBeGreaterThan(1);
    });
  });

  it('shows the empty state when no circuits exist', async () => {
    mockBackend({ circuits: [], runs: [] });
    openProbeDestination('circuits');

    expect(await screen.findByText('No circuits yet')).toBeTruthy();
  });

  describe('list framing and copy (#2462)', () => {
    it('drops the Recent outcomes header when History has no finished runs', async () => {
      // One circuit with only a live run: the panel has rows to model, but no
      // finished outcome to list. The header used to render above an empty
      // list, directly under the empty state.
      mockBackend({ circuits: [CIRCUIT], runs: [RUN_RUNNING] });
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-history'));

      expect(await screen.findByText('No finished runs yet')).toBeTruthy();
      expect(screen.queryByText('Recent outcomes · attention first')).toBeNull();
      expect(screen.queryByTestId('circuit-row')).toBeNull();
    });

    it('keeps the Recent outcomes header once History has a finished run', async () => {
      mockBackend({ circuits: [CIRCUIT], runs: [RUN_DONE, RUN_RUNNING] });
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-history'));

      expect(await screen.findByTestId('run-card-11')).toBeTruthy();
      expect(screen.getByText('Recent outcomes · attention first')).toBeTruthy();
    });

    it('labels the Create a circuit disclosure once, with no repeated heading', async () => {
      mockBackend();
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-manage'));

      // The summary is the form's only heading; a second "Create a circuit"
      // line inside the expanded panel duplicated it verbatim.
      const labels = screen.getAllByText('Create a circuit');
      expect(labels).toHaveLength(1);
      expect(labels[0].tagName).toBe('SUMMARY');
      expect(screen.getByTestId('circuit-create-form').textContent).not.toContain('Create a circuit');
    });

    it('counts idle circuits in the singular for a single circuit', async () => {
      // The idle count is the fallback when no attention/active/queued part is
      // present, so the circuit needs a finished run rather than a live one.
      mockBackend({ circuits: [CIRCUIT], runs: [RUN_DONE] });
      const releaseProbe = deferCircuitProbe();
      openProbeDestination('circuits');

      expect((await screen.findByTestId('circuits-status')).textContent).toBe('Loading circuits…');
      await act(async () => { releaseProbe(); });
      // Wait for the settled status line before asserting the count: reading
      // the status while the probe is still loading reported the fallback as a
      // wrong plural (#2487).
      await waitFor(() => expect(screen.getByTestId('circuits-status').textContent).toBe('1 circuit idle'));
    });

    it('keeps the plural idle count for more than one circuit', async () => {
      mockBackend({ circuits: [CIRCUIT, { ...CIRCUIT, id: 8, name: 'review' }], runs: [RUN_DONE] });
      const releaseProbe = deferCircuitProbe();
      openProbeDestination('circuits');

      expect((await screen.findByTestId('circuits-status')).textContent).toBe('Loading circuits…');
      await act(async () => { releaseProbe(); });
      // Wait for the settled status line before asserting the count: reading
      // the status while the probe is still loading reported the fallback as a
      // wrong plural (#2487).
      await waitFor(() => expect(screen.getByTestId('circuits-status').textContent).toBe('2 circuits idle'));
    });

    it('points the Manage empty state at the Create disclosure, not at Manage', async () => {
      mockBackend({ circuits: [], runs: [] });
      openProbeDestination('circuits');

      // Arriving from Activity, the hint names the tab to open.
      expect(await screen.findByText('Open Manage to create a circuit in the flow editor.')).toBeTruthy();
      fireEvent.click(screen.getByTestId('circuits-view-manage'));

      // Already on Manage: "Open Manage" would send the user nowhere.
      expect(screen.queryByText('Open Manage to create a circuit in the flow editor.')).toBeNull();
      expect(screen.getByText('Expand Create a circuit above to add one to the flow editor.')).toBeTruthy();
      expect(screen.getByText('Create a circuit').tagName).toBe('SUMMARY');
    });
  });

  // -- human-in-the-loop (#1207) ------------------------------------------------

  it('shows unresolved evidence as a warning without approval or failure controls', async () => {
    mockBackend({ runs:[{
      run:{...RUN_DONE.run,id:15,state:'running'},
      steps:[{...RUN_DONE.steps[0],id:3,run_id:15,node_id:'spawn',status:'unverified',outcome:null,
        error_message:'Owned work completion is unavailable.',completed_at:null}],
    }] });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('run-toggle-15'));
    expect((await screen.findByTestId('run-step-15-spawn')).textContent).toContain('Unverified Checkpoint');
    expect(screen.queryByTestId('run-error-15')).toBeNull();
    expect(screen.queryByTestId('approve-15-spawn')).toBeNull();
    expect(screen.getByTestId('run-step-15-spawn').querySelector('pre')?.className).toContain('text-status-warning');
  });

  it('shows the blocked badge with an Approve button for parked gates', async () => {
    const RUN_BLOCKED: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 15, state: 'running' },
      steps: [
        {
          id: 3,
          run_id: 15,
          node_id: 'approval',
          agent_node_id: null,
          status: 'blocked',
          attempt: 1,
          outcome: null,
          error_message: 'Waiting for your approval. This gate does not expire while you are away.',
          started_at: '2026-08-22 10:05:00',
          completed_at: null,
        },
      ],
    };
    mockBackend({ runs: [RUN_BLOCKED] });
    const user = userEvent.setup();
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('run-toggle-15'));

    expect(
      await screen.findByTestId('blocked-badge-15-approval'),
    ).toBeTruthy();
    expect(screen.queryByTestId('run-error-15')).toBeNull();
    expect(screen.getByTestId('run-step-15-approval').querySelector('pre')?.className).toContain('text-status-warning');
    await user.click(screen.getByTestId('approve-15-approval'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('approve_circuit_step', {
        runId: 15,
        nodeId: 'approval',
      });
    });
  });

  it('offers Pause, Resume, and Cancel on live runs', async () => {
    const RUN_PAUSED: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 16, state: 'paused' },
      steps: [],
    };
    mockBackend({ runs: [RUN_RUNNING, RUN_PAUSED] });
    const user = userEvent.setup();
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('run-toggle-12'));

    await user.click(await screen.findByTestId('run-pause-12'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('pause_circuit_run', { runId: 12 });
    });

    await user.click(screen.getByTestId('run-resume-16'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('resume_circuit_run', { runId: 16 });
    });

    await waitFor(() => {
      expect((screen.getByTestId('run-cancel-12') as HTMLButtonElement).disabled).toBe(false);
    });
    await user.click(screen.getByTestId('run-cancel-12'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('cancel_circuit_run', { runId: 12 });
    });
  });
});

/**
 * Readable run diagnostics (issue #1468).
 *
 * The tab used to compress a run into one truncated
 * `node:status -> node:status -> ...` span. These tests pin the
 * replacement's contract: a vertical timeline that survives long chains,
 * plain-English queue reasons, wrapping (never clipping) for unbounded
 * text, and exactly one scroll owner for the body.
 */
describe('CircuitsProbeTab run diagnostics (#1468)', () => {
  /** A realistic long chain — the issue's own example, plus the tail. */
  const LONG_CHAIN_NODES = [
    'trigger',
    'implementer',
    'implementation_classifier',
    'open_pull_request',
    'reviewer',
    'review_classifier',
    'address_feedback',
    'address_feedback_classifier',
    'rerun_review',
    'merge_gate',
    'merge',
    'close_agents',
  ];

  function step(
    over: Partial<CircuitRunDetail['steps'][number]> & { node_id: string; status: string }
  ): CircuitRunDetail['steps'][number] {
    return {
      id: 0,
      run_id: 0,
      agent_node_id: null,
      attempt: 1,
      outcome: null,
      error_message: null,
      started_at: null,
      completed_at: null,
      ...over,
    };
  }

  it('renders a long step chain as a vertical timeline, every node readable', async () => {
    const RUN_LONG: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 20, state: 'running' },
      steps: LONG_CHAIN_NODES.map((node_id, i) =>
        step({
          node_id,
          status: i < 10 ? 'completed' : i === 10 ? 'running' : 'pending_slot',
          outcome: i < 10 ? 'completed' : null,
        })
      ),
    };
    mockBackend({ runs: [RUN_LONG] });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('run-toggle-20'));

    expect((await screen.findByTestId('run-toggle-20')).getAttribute('aria-expanded')).toBe('true');
    const timeline = await screen.findByTestId('run-steps-20');
    expect(timeline.tagName).toBe('OL');

    // Every node id is present in full. The old one-liner clipped the
    // tail, which is exactly where the active step lives.
    for (const node of LONG_CHAIN_NODES) {
      const row = screen.getByTestId(`run-step-20-${node}`);
      expect(row.textContent).toContain(node);
      // Wrap, never clip: `truncate` would hide the node id at 240px.
      expect(row.querySelector('.truncate')).toBeNull();
    }

    // Timeline rows stack vertically, so length costs height (which the
    // body scrolls) not width (which it must not).
    expect(timeline.className).toContain('flex-col');
    // The headline still summarises without needing the timeline.
    expect(screen.getByTestId('run-state-20').textContent).toBe('Running');
    expect(screen.getByTestId('run-activity-20').textContent).toContain('merge');
    expect(screen.getByTestId('run-progress-20').textContent).toBe('10/12 steps');
  });

  it('explains a queued step instead of showing raw pending_slot', async () => {
    const RUN_QUEUED: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 21, state: 'running' },
      steps: [
        step({ node_id: 'trigger', status: 'completed', outcome: 'completed' }),
        step({ node_id: 'reviewer', status: 'pending_slot' }),
      ],
    };
    // A sibling run holding a running step must NOT change the explanation:
    // ADR 0042 retired the per-circuit step budget, so only the agent lease
    // can hold the reviewer back.
    const RUN_HOGGING: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 22, state: 'running' },
      steps: [step({ node_id: 'implementer', status: 'running' })],
    };
    mockBackend({ runs: [RUN_QUEUED, RUN_HOGGING] });
    openProbeDestination('circuits');

    const activity = await screen.findByTestId('run-activity-21');
    expect(activity.textContent).toContain('Queued');
    expect(activity.textContent).toContain('reviewer');
    // The raw scheduler token never reaches the user.
    expect(activity.textContent).not.toContain('pending_slot');
    expect(screen.getByTestId('run-reason-21').textContent).toBe(
      "Waiting for a circuit agent slot — this run's agent lease, or the app-wide agent pool, has no free slot."
    );
    expect(screen.getByTestId('run-reason-21').textContent).not.toMatch(/step slot/);
  });

  it('names the reviewer gate a run is parked on and keeps Approve working', async () => {
    const RUN_BLOCKED: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 24, state: 'running' },
      steps: [
        step({ node_id: 'implementer', status: 'completed', outcome: 'completed' }),
        step({ node_id: 'review_gate', status: 'blocked' }),
        // A parallel branch is still moving: the approval must still win
        // the headline, because it is the only state needing the user.
        step({ node_id: 'watchdog', status: 'running' }),
      ],
    };
    mockBackend({ runs: [RUN_BLOCKED] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    const activity = await screen.findByTestId('run-activity-24');
    expect(activity.textContent).toContain('Waiting for approval');
    expect(activity.textContent).toContain('review_gate');
    expect(screen.getByTestId('run-reason-24').textContent).toContain('approve');

    await user.click(screen.getByTestId('approve-24-review_gate'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('approve_circuit_step', {
        runId: 24,
        nodeId: 'review_gate',
      });
    });
  });

  it('wraps long trigger identities and error text rather than clipping them', async () => {
    const LONG_TRIGGER =
      'issue:1468:buildmesh:automation:circuits:probe:readable-diagnostics:long-label';
    const LONG_ERROR =
      'classifier verdict could not be parsed: expected one of green|red|working but the ' +
      'harness emitted a 4096-byte transcript with no verdict banner, so the step failed closed';
    const RUN_FAILED: CircuitRunDetail = {
      run: {
        ...RUN_DONE.run,
        id: 25,
        state: 'failed',
        trigger_identity: LONG_TRIGGER,
      },
      steps: [
        step({
          node_id: 'implementation_classifier',
          status: 'failed',
          outcome: 'failed',
          error_message: LONG_ERROR,
        }),
      ],
    };
    mockBackend({ runs: [RUN_FAILED] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    // Failed runs live in History now — switch views first.
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    // A collapsed failed card gives a plain-language summary and a clear action.
    expect(await screen.findByText(/could not read the agent's report/i)).toBeTruthy();
    expect(screen.getByText('Resolve failed step')).toBeTruthy();
    const collapsedExcerpt = screen.getByTestId('run-error-summary-25');
    expect(collapsedExcerpt.textContent).toContain('classifier verdict could not be parsed');
    expect(collapsedExcerpt.className).toContain('line-clamp-2');
    expect(screen.getByTestId('run-toggle-25').getAttribute('aria-controls')).toBe('run-detail-25');
    expect((document.getElementById('run-detail-25') as HTMLElement).hidden).toBe(true);
    fireEvent.click(screen.getByTestId('run-toggle-25'));
    // The expanded timeline keeps the complete classifier error available.
    const collapsedError = await screen.findByTestId('run-error-25');
    expect(collapsedError.textContent).toContain('classifier verdict could not be parsed');
    expect(collapsedError.className).toContain('break-words');
    expect(collapsedError.className).not.toContain('truncate');

    // Trigger identity is unspaced, so it needs `break-all` — a
    // word-boundary break has nowhere to land and would overflow.
    const trigger = screen.getByTestId('run-trigger-25');
    expect(trigger.textContent).toBe(LONG_TRIGGER);
    expect(trigger.className).toContain('break-all');
    expect(trigger.getAttribute('title')).toBe(LONG_TRIGGER);

    // The full error text is a wrapping pre block, not a clipped span.
    const pre = screen
      .getByTestId('run-step-25-implementation_classifier')
      .querySelector('pre');
    expect(pre?.textContent).toBe(LONG_ERROR);
    expect(pre?.className).toContain('whitespace-pre-wrap');
    expect(pre?.className).toContain('break-words');
  });

  it('surfaces outcome, pass and duration per step', async () => {
    const RUN_RETRIED: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 26, state: 'running' },
      steps: [
        step({
          node_id: 'review_classifier',
          status: 'completed',
          // Gate steps finish `completed` but carry the real verdict —
          // that's the branch the run actually took.
          outcome: 'red',
          attempt: 3,
          started_at: '2026-08-22 10:05:00',
          completed_at: '2026-08-22 10:06:30',
        }),
      ],
    };
    mockBackend({ runs: [RUN_RETRIED] });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('run-toggle-26'));

    const row = await screen.findByTestId('run-step-26-review_classifier');
    expect(row.getAttribute('data-step-status')).toBe('completed');
    expect(row.textContent).toContain('Done');
    expect(row.textContent).toContain('red');
    expect(row.textContent).toContain('pass 3');
    // 90s reads as "1m 30s", not "90.0s".
    expect(row.textContent).toContain('1m 30s');
    expect(screen.getByTestId('run-retries-26').textContent).toContain('retried');
  });

  it('shows an approved review per step and reveals the reviewer report', async () => {
    const REVIEW_GRAPH = {
      version: 3,
      nodes: [
        { id: 'trigger', type: { type: 'manual' } },
        {
          id: 'reviewer',
          type: {
            type: 'spawn_agent_node', prompt: '', name: 'Code reviewer', provider: null,
            model: null, effort: null, extra_args: null, timeout_seconds: null,
          },
        },
        { id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
      ],
      edges: [],
    };
    const RUN_REVIEW: CircuitRunDetail = {
      run: {
        ...RUN_DONE.run,
        id: 27,
        context_json: JSON.stringify({
          'node.verdict.review_verdict': 'approved',
          'node.verdict.review_verdict_attempt': '2',
          'node.verdict.evaluated_output': 'No remaining findings. Approved.',
        }),
      },
      steps: [
        step({ node_id: 'reviewer', status: 'completed', outcome: 'completed', attempt: 2 }),
        step({ node_id: 'verdict', status: 'completed', outcome: 'completed', attempt: 2 }),
      ],
    };
    mockBackend({
      circuits: [{ ...CIRCUIT, graph_json: JSON.stringify(REVIEW_GRAPH) }],
      runs: [RUN_REVIEW],
    });
    const user = userEvent.setup();
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));

    // The headline answers "did it end well?" without opening the card.
    expect((await screen.findByTestId('run-state-27')).textContent).toBe('Review approved');

    await user.click(screen.getByTestId('run-toggle-27'));
    const verdictRow = screen.getByTestId('run-step-27-verdict');
    expect(verdictRow.textContent).toContain('Review');
    expect(verdictRow.textContent).toContain('pass 2');
    expect(screen.getByTestId('run-step-verdict-27-verdict').textContent).toBe('APPROVED');
    // A spawned agent reads by its configured name, not its circuit node id.
    expect(screen.getByTestId('run-step-27-reviewer').textContent).toContain('Code reviewer');
    expect(screen.getByTestId('run-review-report-27-verdict').textContent)
      .toContain('No remaining findings. Approved.');
  });

  it('distinguishes an exhausted review from an approved one', async () => {
    const REVIEW_GRAPH = {
      version: 3,
      nodes: [
        { id: 'trigger', type: { type: 'manual' } },
        { id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
        { id: 'retry', type: { type: 'retry_limit', max_retries: 1 } },
      ],
      edges: [],
    };
    const RUN_EXHAUSTED: CircuitRunDetail = {
      run: {
        ...RUN_DONE.run,
        id: 28,
        state: 'failed',
        context_json: JSON.stringify({
          'node.verdict.review_verdict': 'changes_requested',
          'node.verdict.review_verdict_attempt': '2',
        }),
      },
      steps: [
        step({ node_id: 'verdict', status: 'completed', outcome: 'working', attempt: 2 }),
        step({ node_id: 'retry', status: 'failed', outcome: 'failed', attempt: 2 }),
      ],
    };
    mockBackend({
      circuits: [{ ...CIRCUIT, graph_json: JSON.stringify(REVIEW_GRAPH) }],
      runs: [RUN_EXHAUSTED],
    });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    fireEvent.click(await screen.findByTestId('run-toggle-28'));

    const card = await screen.findByTestId('run-card-28');
    expect(screen.getByTestId('run-state-28').textContent).toBe('Review limit reached');
    expect(card.textContent).toContain('Ran out of review attempts after pass 2.');

    // A failed run opens by default, so the timeline is already visible.
    expect(screen.getByTestId('run-step-verdict-28-verdict').textContent).toBe('CHANGES REQUESTED');
    expect(screen.getByTestId('run-step-verdict-28-retry').textContent).toBe('LIMIT REACHED');
  });

  describe('run identity and timing', () => {
    const ISSUE_GRAPH = {
      version: 3,
      nodes: [
        { id: 'trigger', type: { type: 'github_issue_label', label: 'buildmesh:run' } },
        {
          id: 'implementer',
          type: {
            type: 'spawn_agent_node', prompt: '', name: null, provider: null,
            model: null, effort: null, extra_args: null, timeout_seconds: null,
          },
        },
        {
          id: 'reviewer',
          type: {
            type: 'spawn_agent_node', prompt: '', name: null, provider: null,
            model: null, effort: null, extra_args: null, timeout_seconds: null,
          },
        },
        { id: 'review_classifier', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
      ],
      edges: [],
    };
    const agent = (id: number, name: string): AgentNode => ({
      id, mesh_id: 42, name, path: '/repo', branch: 'main', env: null,
      provider: 'claude', status: 'running', use_worktree: false, position: 0,
      created_at: '2026-08-22 10:00:00', scratchpad: '', sandbox: false, is_pinned: false,
    });

    it('names the run after its implementation node, never the reviewer or the run id', async () => {
      seedAgentNodes([agent(900, 'ai-loading-spinner'), agent(901, 'ebony-unhelpful-cudgel')]);
      const RUN: CircuitRunDetail = {
        run: { ...RUN_DONE.run, id: 41, state: 'running', trigger_identity: 'issue:2041:buildmesh:run' },
        steps: [
          step({ node_id: 'implementer', status: 'completed', outcome: 'completed', agent_node_id: 900,
            started_at: '2026-08-22 10:05:00', completed_at: '2026-08-22 10:30:00' }),
          // The reviewer is the active step, which used to win the link.
          step({ node_id: 'reviewer', status: 'running', agent_node_id: 901, started_at: '2026-08-22 10:31:00' }),
        ],
      };
      mockBackend({ circuits: [{ ...CIRCUIT, graph_json: JSON.stringify(ISSUE_GRAPH) }], runs: [RUN] });
      openProbeDestination('circuits');

      expect((await screen.findByTestId('run-subject-41')).textContent).toBe('ai-loading-spinner');
      // The run id is still there, but as a small tag, not the headline.
      expect(screen.getByTestId('run-id-41').textContent).toBe('#41');
      // Keep the subject stable while linking directly to the active reviewer.
      expect(screen.getByTestId('run-agent-41').textContent).toContain('ebony-unhelpful-cudgel');
      fireEvent.click(screen.getByTestId('run-agent-open-41'));
      expect(useAgentNodeStore.getState().activeNodeId).toBe(901);
      fireEvent.click(screen.getByTestId('run-toggle-41'));
      fireEvent.click(screen.getByTestId('run-step-agent-open-41-reviewer'));
      expect(useAgentNodeStore.getState().activeNodeId).toBe(901);
      expect(vi.mocked(invoke)).not.toHaveBeenCalledWith('reopen_agent_node', expect.anything());
    });

    it.each(['activity', 'history'] as const)('restores a closed reviewer from %s and prevents duplicate restores', async (view) => {
      const restored = { ...agent(901, 'saved-reviewer'), status: 'suspended' as const };
      // Cover both an absent row and a retained archived row in the frontend.
      seedAgentNodes(view === 'history' ? [{ ...restored, status: 'archived' }] : []);
      const run: CircuitRunDetail = {
        run: { ...RUN_DONE.run, id: 41, state: view === 'activity' ? 'running' : 'completed' },
        steps: [step({ node_id: 'reviewer', agent_node_id: 901, status: 'completed', outcome: 'completed' })],
      };
      mockBackend({ circuits: [{ ...CIRCUIT, graph_json: JSON.stringify(ISSUE_GRAPH) }], runs: [run] });
      const backend = vi.mocked(invoke).getMockImplementation()!;
      let resolve!: (node: AgentNode) => void;
      const pending = new Promise<AgentNode>(done => { resolve = done; });
      vi.mocked(invoke).mockImplementation((cmd, args) => cmd === 'reopen_agent_node' ? pending : backend(cmd, args));
      openProbeDestination('circuits');
      if (view === 'history') fireEvent.click(await screen.findByTestId('circuits-view-history'));
      fireEvent.click(await screen.findByTestId('run-toggle-41'));
      const open = screen.getByTestId('run-step-agent-open-41-reviewer');
      fireEvent.click(open);
      fireEvent.click(open);
      expect((open as HTMLButtonElement).disabled).toBe(true);
      expect(open.textContent).toBe('Opening…');
      await waitFor(() => expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'reopen_agent_node')).toEqual([
        ['reopen_agent_node', { nodeId: 901 }],
      ]));
      await act(async () => { resolve(restored); await pending; });
      expect(useAgentNodeStore.getState().nodesById[901].status).toBe('suspended');
      expect(useAgentNodeStore.getState().activeNodeId).toBe(901);
      expect(useUIStore.getState().viewMode).toBe('single');
      expect(useMeshStore.getState().selectedMeshId).toBe(42);
    });

    it.each([
      ['mesh switch', 'resolve'], ['mesh switch', 'reject'],
      ['unmount', 'resolve'], ['unmount', 'reject'],
    ] as const)('does not navigate or show stale errors after %s when restoration %s', async (transition, result) => {
      mockBackend({ runs: [RUN_DONE] });
      const backend = vi.mocked(invoke).getMockImplementation()!;
      let resolve!: (node: AgentNode) => void;
      let reject!: (error: Error) => void;
      const pending = new Promise<AgentNode>((done, fail) => { resolve = done; reject = fail; });
      vi.mocked(invoke).mockImplementation((cmd, args) => cmd === 'reopen_agent_node' ? pending : backend(cmd, args));
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-history'));
      fireEvent.click(await screen.findByTestId('run-toggle-11'));
      fireEvent.click(screen.getByTestId('run-step-agent-open-11-spawn'));
      await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith('reopen_agent_node', { nodeId: 900 }));
      const activeNodeId = useAgentNodeStore.getState().activeNodeId;
      const viewMode = useUIStore.getState().viewMode;
      if (transition === 'unmount') {
        act(() => useUIStore.setState({ probeOpen: false }));
        expect(screen.queryByTestId('circuits-probe-tab')).toBeNull();
      } else {
        const otherMesh = { ...MESH, id: 43, name: 'other' };
        act(() => useMeshStore.setState({ meshes: [MESH, otherMesh],
          meshesById: new Map([[42, MESH], [43, otherMesh]]), selectedMeshId: 43 }));
      }
      await act(async () => {
        if (result === 'resolve') resolve({ ...agent(900, 'restored'), status: 'suspended' });
        else reject(new Error('Stale restore failure'));
        await pending.catch(() => undefined);
      });
      expect(useAgentNodeStore.getState().activeNodeId).toBe(activeNodeId);
      expect(useUIStore.getState().viewMode).toBe(viewMode);
      expect(useMeshStore.getState().selectedMeshId).toBe(transition === 'mesh switch' ? 43 : 42);
      expect(screen.queryByTestId('circuits-focus-notice')).toBeNull();
      expect(useAgentNodeStore.getState().nodesById[900]?.status).toBe(result === 'resolve' ? 'suspended' : undefined);
    });

    it('keeps a recognisable name once the implementation node has been closed', async () => {
      seedAgentNodes([]);
      const RUN: CircuitRunDetail = {
        run: {
          ...RUN_DONE.run, id: 42, state: 'completed',
          context_json: JSON.stringify({ 'issue.number': '2041', 'issue.title': 'Spinner overlaps the log' }),
        },
        steps: [step({ node_id: 'implementer', status: 'completed', outcome: 'completed', agent_node_id: 900 })],
      };
      mockBackend({ circuits: [{ ...CIRCUIT, graph_json: JSON.stringify(ISSUE_GRAPH) }], runs: [RUN] });
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-history'));

      expect((await screen.findByTestId('run-subject-42')).textContent).toBe('#2041 Spinner overlaps the log');
    });

    it('falls back to the run number when nothing identifies the work', async () => {
      mockBackend({ runs: [{ ...RUN_DONE, run: { ...RUN_DONE.run, id: 43, state: 'completed' }, steps: [] }] });
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-history'));
      expect((await screen.findByTestId('run-subject-43')).textContent).toBe('Run #43');
    });

    it('shows wall-clock start and finish, measured from when Autopilot began rather than from queueing', async () => {
      const RUN: CircuitRunDetail = {
        run: {
          ...RUN_DONE.run, id: 44, state: 'completed',
          created_at: '2026-08-22 09:20:00', // minted, then queued for 45 minutes
          updated_at: '2026-08-22 10:20:11',
        },
        steps: [
          step({ node_id: 'trigger', status: 'completed', outcome: 'completed',
            started_at: '2026-08-22 10:05:00', completed_at: '2026-08-22 10:05:00' }),
          step({ node_id: 'spawn', status: 'completed', outcome: 'completed',
            started_at: '2026-08-22 10:05:00', completed_at: '2026-08-22 10:20:11' }),
        ],
      };
      mockBackend({ runs: [RUN] });
      const user = userEvent.setup();
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-history'));
      fireEvent.click(await screen.findByTestId('run-toggle-44'));

      const times = (await screen.findByTestId('run-times-44')).textContent ?? '';
      const now = new Date();
      const wall = (iso: string) => formatWallClock(Date.parse(iso), now);
      expect(times).toContain(`Started ${wall('2026-08-22T10:05:00Z')}`);
      expect(times).toContain(`finished ${wall('2026-08-22T10:20:11Z')}`);
      // 10:05:00 to 10:20:11, not 09:20:00 to 10:20:11.
      expect(times).toContain('took 15m 11s');
      expect(times).not.toContain('1h');

      // The expanded card also shows each step's wall-clock times.
      const spawn = screen.getByTestId('run-step-times-44-spawn').textContent ?? '';
      expect(spawn).toContain(wall('2026-08-22T10:05:00Z'));
      expect(spawn).toContain(wall('2026-08-22T10:20:11Z'));
    });

    it('says a run never started instead of inventing a duration', async () => {
      const RUN: CircuitRunDetail = {
        run: { ...RUN_DONE.run, id: 45, state: 'cancelled' },
        steps: [],
      };
      mockBackend({ runs: [RUN] });
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-history'));
      expect((await screen.findByTestId('run-times-45')).textContent).toBe('Never started');
    });

    it('tucks a leftover note on a finished step away instead of showing it like an error', async () => {
      const RUN: CircuitRunDetail = {
        run: { ...RUN_DONE.run, id: 46, state: 'completed' },
        steps: [step({
          node_id: 'confirm_publish', status: 'completed', outcome: 'completed',
          error_message: 'Waiting for your approval. This gate does not expire while you are away.',
        })],
      };
      mockBackend({ runs: [RUN] });
      const user = userEvent.setup();
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-history'));
      await user.click(await screen.findByTestId('run-toggle-46'));

      const note = screen.getByTestId('run-step-note-46-confirm_publish');
      expect(note.tagName).toBe('DETAILS');
      expect((note as HTMLDetailsElement).open).toBe(false);
      // It is not promoted to the card headline as a failure either.
      expect(screen.queryByTestId('run-error-46')).toBeNull();
    });
  });

  it('links a run to its Agent Node and focuses it on the canvas', async () => {
    const agent: AgentNode = {
      id: 900, mesh_id: 42, name: 'impl-agent', path: '/repo', branch: 'main', env: null,
      provider: 'claude', status: 'running', use_worktree: false, position: 0,
      created_at: '2026-08-22 10:00:00', scratchpad: '', sandbox: false, is_pinned: false,
    };
    seedAgentNodes([agent]);
    mockBackend({
      runs: [{
        ...RUN_DONE,
        run: { ...RUN_DONE.run, id: 29, state: 'running', source_agent_node_id: 900 },
        steps: [],
      }],
    });
    openProbeDestination('circuits');

    // The headline names the node; the row beneath it is just the way there.
    expect((await screen.findByTestId('run-subject-29')).textContent).toBe('impl-agent');
    expect(screen.getByTestId('run-agent-29').textContent).toContain('impl-agent');
    fireEvent.click(screen.getByTestId('run-agent-open-29'));
    expect(useAgentNodeStore.getState().activeNodeId).toBe(900);
    expect(useMeshStore.getState().selectedMeshId).toBe(42);
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('reveals a linked run in the right view, expanded and highlighted', async () => {
    mockBackend({ runs: [RUN_DONE, RUN_RUNNING] });
    openProbeDestination('circuits');
    await screen.findByTestId('run-card-12');

    act(() => { useUIStore.getState().focusCircuitRun(11); });

    const card = await screen.findByTestId('run-card-11');
    expect(card.getAttribute('data-run-focused')).toBe('true');
    expect(screen.getByTestId('run-toggle-11').getAttribute('aria-expanded')).toBe('true');
    // The request is consumed so a later refresh cannot re-trigger it.
    expect(useUIStore.getState().pendingCircuitRunFocus).toBeNull();
  });

  it('falls back to a filtered History when the linked run is outside the window', async () => {
    mockBackend({ runs: [RUN_DONE, RUN_RUNNING] });
    openProbeDestination('circuits');
    await screen.findByTestId('run-card-12');

    act(() => { useUIStore.getState().focusCircuitRun(999); });

    expect(await screen.findByTestId('circuits-focus-notice')).toBeTruthy();
    expect((screen.getByTestId('history-search-input') as HTMLInputElement).value).toBe('999');
    expect(useUIStore.getState().pendingCircuitRunFocus).toBeNull();
  });

  it('resolves a focus request issued before the tab mounts', async () => {
    mockBackend({ runs: [RUN_DONE, RUN_RUNNING] });
    // The production entry point: the kebab sets the focus and opens the tab
    // in one action, so the request exists before CircuitsProbeTab mounts and
    // before the first snapshot resolves. It must not be misreported as
    // out-of-window and consumed while `rows` is still empty.
    act(() => { useUIStore.getState().focusCircuitRun(11); });
    render(<ProbePanel />);

    const card = await screen.findByTestId('run-card-11');
    expect(card.getAttribute('data-run-focused')).toBe('true');
    expect(screen.getByTestId('run-toggle-11').getAttribute('aria-expanded')).toBe('true');
    expect(screen.queryByTestId('circuits-focus-notice')).toBeNull();
  });

  it('clears History filters so the linked run is guaranteed to render', async () => {
    mockBackend({ runs: [RUN_DONE, RUN_RUNNING] });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    fireEvent.change(screen.getByTestId('history-search-input'), { target: { value: 'no-match' } });
    fireEvent.click(screen.getByTestId('history-attention-toggle'));
    expect(screen.queryByTestId('run-card-11')).toBeNull();

    act(() => { useUIStore.getState().focusCircuitRun(11); });

    expect(await screen.findByTestId('run-card-11')).toBeTruthy();
    expect((screen.getByTestId('history-search-input') as HTMLInputElement).value).toBe('');
    expect((screen.getByTestId('history-attention-toggle') as HTMLInputElement).checked).toBe(false);
  });

  it('renders wait, capacity and recovery history with provenance in the expanded run', async () => {
    mockBackend({ runs: [RUN_DONE, RUN_RUNNING] });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    await screen.findByTestId('run-card-11');
    fireEvent.click(screen.getByTestId('run-toggle-11'));

    fireEvent.click(await screen.findByText('Circuit Run History'));

    const continuation = await screen.findByTestId('history-entry-3');
    expect(continuation.dataset.historyKind).toBe('review_continuation');
    expect(continuation.dataset.disposition).toBe('applied');
    expect(continuation.textContent).toContain('this run follows run #88');
    expect(screen.getByTestId('history-entry-1').dataset.disposition).toBe('waiting');
    expect(screen.getByText('Source: circuit_worker.admission · waiting')).toBeTruthy();
  });

  it('reports a restore failure and allows retry for an unavailable agent', async () => {
    mockBackend({
      runs: [{
        ...RUN_DONE,
        run: { ...RUN_DONE.run, id: 31, state: 'running', source_agent_node_id: 404 },
        steps: [],
      }],
    });
    const backend = vi.mocked(invoke).getMockImplementation()!;
    let fail = true;
    vi.mocked(invoke).mockImplementation((cmd, args) => {
      if (cmd === 'reopen_agent_node') return fail
        ? Promise.reject(new Error('Agent history unavailable'))
        : Promise.resolve({ id: 404, mesh_id: 42, name: 'restored', status: 'suspended', provider: 'claude' });
      return backend(cmd, args);
    });
    seedAgentNodes([]);
    openProbeDestination('circuits');

    const link = await screen.findByTestId('run-agent-31');
    expect(link.textContent).toContain('#404');
    fireEvent.click(screen.getByTestId('run-agent-open-31'));
    expect((await screen.findByTestId('circuits-focus-notice')).textContent).toContain('Agent history unavailable');
    expect(useAgentNodeStore.getState().nodesById[404]).toBeUndefined();
    fail = false;
    fireEvent.click(screen.getByTestId('run-agent-open-31'));
    await waitFor(() => expect(useAgentNodeStore.getState().activeNodeId).toBe(404));
    expect(screen.queryByTestId('circuits-focus-notice')).toBeNull();
  });

  it('reports an exhausted review without blaming the worker', async () => {
    const REVIEW_CIRCUIT: AutopilotCircuit = {
      ...CIRCUIT,
      graph_json: JSON.stringify({
        version: 2,
        nodes: [
          { id: 'review_classifier', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
          { id: 'review_retry', type: { type: 'retry_limit', max_retries: 3 } },
        ],
        edges: [],
      }),
    };
    // An exhausted review fails the run by design: the retry gate finishes
    // with a `failed` *outcome* (its status stays completed) and no step
    // records an error, so the generic fallback must not fire.
    const RUN_EXHAUSTED: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 32, state: 'failed' },
      steps: [
        step({ node_id: 'review_classifier', status: 'completed', outcome: 'completed' }),
        step({ node_id: 'review_retry', status: 'completed', outcome: 'failed', attempt: 3 }),
      ],
    };
    mockBackend({ circuits: [REVIEW_CIRCUIT], runs: [RUN_EXHAUSTED] });
    openProbeDestination('circuits');
    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    fireEvent.click(await screen.findByTestId('run-toggle-32'));

    expect((await screen.findByTestId('run-state-32')).textContent).toBe('Review limit reached');
    // The review line below already explains the ending; a reason line
    // claiming the worker failed the run would contradict it.
    expect(screen.queryByTestId('run-reason-32')).toBeNull();
    expect(screen.getByText(/No final approval is recorded/)).toBeTruthy();
  });

  it('collapses runs by default and preserves deliberate disclosure across views', async () => {
    mockBackend({ runs: [RUN_DONE, RUN_RUNNING] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await waitFor(() => {
      expect(screen.getByTestId('run-toggle-12').getAttribute('aria-expanded')).toBe('false');
    });
    await user.click(screen.getByTestId('circuits-view-history'));
    expect(screen.getByTestId('run-toggle-11').getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByTestId('run-steps-11')).toBeNull();

    // One click opens the completed run's ledger…
    await user.click(screen.getByTestId('run-toggle-11'));
    expect(screen.getByTestId('run-toggle-11').getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByTestId('run-steps-11')).toBeTruthy();

    await user.click(screen.getByTestId('circuits-view-activity'));
    expect(screen.getByTestId('run-toggle-12').getAttribute('aria-expanded')).toBe('false');
    await user.click(screen.getByTestId('run-toggle-12'));
    expect(screen.getByTestId('run-toggle-12').getAttribute('aria-expanded')).toBe('true');
  });

  it('keeps the disclosure control accessible', async () => {
    mockBackend({ runs: [RUN_DONE] });
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    const toggle = await screen.findByTestId('run-toggle-11');
    expect(toggle.tagName).toBe('BUTTON');
    expect(toggle.getAttribute('aria-controls')).toBe('run-detail-11');
    expect((document.getElementById('run-detail-11') as HTMLElement).hidden).toBe(true);
    // Controls live outside the disclosure button — a button nested in a
    // button is invalid HTML and breaks keyboard semantics.
    expect(toggle.querySelector('button')).toBeNull();
    fireEvent.click(toggle);
    expect(toggle.getAttribute('aria-controls')).toBe('run-detail-11');
    expect((document.getElementById('run-detail-11') as HTMLElement).hidden).toBe(false);
  });

  it('uses tab semantics for the mutually exclusive views', async () => {
    mockBackend();
    openProbeDestination('circuits');

    const tablist = screen.getByRole('tablist', { name: 'Circuit views' });
    expect(tablist).toBeTruthy();
    expect(screen.getByRole('tab', { name: /Activity/ }).getAttribute('aria-selected')).toBe('true');
    expect(screen.getByRole('tab', { name: /History/ }).getAttribute('aria-selected')).toBe('false');
    await userEvent.setup().click(screen.getByRole('tab', { name: /History/ }));
    expect(screen.getByTestId('circuits-probe-body').getAttribute('aria-labelledby')).toBe('circuits-tab-history');
    expect(screen.getByRole('tab', { name: /History/ }).getAttribute('aria-selected')).toBe('true');
  });

  it('supports roving keyboard navigation across circuit views', async () => {
    mockBackend();
    openProbeDestination('circuits');

    const user = userEvent.setup();
    const activityTab = screen.getByRole('tab', { name: /Activity/ });
    activityTab.focus();
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: /History/ }).getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(screen.getByRole('tab', { name: /History/ }));
    await user.keyboard('{ArrowDown}');
    expect(screen.getByRole('tab', { name: 'Manage' }).getAttribute('aria-selected')).toBe('true');
    await user.keyboard('{Home}');
    expect(screen.getByRole('tab', { name: /Activity/ }).getAttribute('aria-selected')).toBe('true');
  });

  it('advances a live run duration without waiting for a ledger event', async () => {
    // A step can churn for minutes without a state transition, so no
    // `circuit-run-updated` arrives. Baselining the clock on fetch left the
    // elapsed time frozen, which made a stalled run look freshly started.
    //
    // `advanceTimersByTimeAsync` rather than RTL's `findBy*`: the awaited
    // queries drive their own timers, which fights an explicit fake clock.
    vi.useFakeTimers();
    try {
      vi.setSystemTime(new Date('2026-09-01T12:00:10Z'));
      const RUN_LIVE: CircuitRunDetail = {
        run: {
          ...RUN_DONE.run,
          id: 30,
          state: 'running',
          // Queued for 40 minutes before a slot freed: that wait is not run time.
          created_at: '2026-09-01 11:20:00',
          updated_at: '2026-09-01 12:00:00',
        },
        steps: [step({ node_id: 'implementer', status: 'running', started_at: '2026-09-01 12:00:00' })],
      };
      mockBackend({ runs: [RUN_LIVE] });
      openProbeDestination('circuits');
      // Flush the load IPC without letting wall-clock time move.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      expect(screen.getByTestId('run-card-30').textContent).toContain('10.0s');
      expect(screen.getByTestId('run-card-30').textContent).not.toContain('50m');
      expect(vi.getTimerCount(), 'the 1s tick must be registered').toBeGreaterThan(0);

      // Five seconds of wall clock, zero backend events. `advanceTimersByTime`
      // moves the mocked `Date` too, so setting the system time again here
      // would double-count the gap.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5000);
      });
      expect(screen.getByTestId('run-card-30').textContent).toContain('15.0s');
    } finally {
      vi.useRealTimers();
    }
  });

  it('clears the per-card ticker on unmount and never polls the ledger on a timer (issue #1751)', async () => {
    // The 1s clock lives in the run card (`LiveRunDuration`), not the
    // tab: a minute idle must not refetch the ledger, and unmounting
    // must leave zero timers behind.
    vi.useFakeTimers();
    try {
      vi.setSystemTime(new Date('2026-09-01T12:00:10Z'));
      const RUN_LIVE: CircuitRunDetail = {
        run: {
          ...RUN_DONE.run,
          id: 31,
          state: 'running',
          created_at: '2026-09-01 12:00:00',
          updated_at: '2026-09-01 12:00:00',
        },
        steps: [step({ node_id: 'implementer', status: 'running' })],
      };
      mockBackend({ runs: [RUN_LIVE] });
      useUIStore.getState().openProbeTab('circuits');
      const { unmount } = render(<ProbePanel />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByTestId('run-card-31')).toBeTruthy();
      const probes = () =>
        vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_circuit_probe').length;
      expect(probes()).toBe(1);

      // A minute idle: the label ticks on, the ledger is not refetched.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(60_000);
      });
      expect(probes()).toBe(1);
      expect(screen.getByTestId('run-card-31').textContent).toContain('1m');

      unmount();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not tick when every visible run is terminal', async () => {
    // Terminal runs render a static reading fixed by `updated_at` with no
    // interval at all (issue #1751), so a tab showing finished work ticks
    // never.
    vi.useFakeTimers();
    try {
      mockBackend({ runs: [RUN_DONE] });
      openProbeDestination('circuits');
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      fireEvent.click(screen.getByTestId('circuits-view-history'));
      expect(screen.getByTestId('run-card-11')).toBeTruthy();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('gives the body exactly one scroll owner and no sideways escape', async () => {
    mockBackend();
    openProbeDestination('circuits');

    const root = await screen.findByTestId('circuits-probe-tab');
    const body = screen.getByTestId('circuits-probe-body');

    // The root is layout-only; the body owns the scroll. Two stacked
    // scrollers is the nested-scrolling defect #1468 names.
    expect(root.className).not.toMatch(/overflow-(y-)?auto/);
    expect(root.className).toContain('min-h-0');
    expect(body.className).toContain('overflow-y-auto');
    // `overflow-y-auto` alone computes `overflow-x: auto`, which is how a
    // long diagnostic used to scroll the whole tab sideways.
    expect(body.className).toContain('overflow-x-hidden');

    // Nothing between the root and the body re-introduces a scroller.
    const nested = root.querySelectorAll('[class*="overflow-y-auto"], [class*="overflow-auto"]');
    expect(nested.length).toBe(1);
    expect(nested[0]).toBe(body);

    expect(body.contains(screen.getByTestId('circuits-view-manage'))).toBe(false);
    fireEvent.click(screen.getByTestId('circuits-view-manage'));
    expect(body.contains(screen.getByTestId('circuit-name-input'))).toBe(true);
  });
});

describe('Autopilot Circuits IPC contract (ADR-0010 seam)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockResolvedValue([]);
  });

  // The wrappers are the ONLY sanctioned path to these commands; pin
  // each one's exact snake_case command name + camelCase arg keys so a
  // rename breaks here instead of at runtime ("command not found").
  it('every wrapper targets its registered Rust command with camelCase args', async () => {
    await listCircuitsWithRuns(42, 10);
    expect(invoke).toHaveBeenLastCalledWith('list_circuits_with_runs', { meshId: 42, limit: 10 });

    await listCircuitQueue(42);
    expect(invoke).toHaveBeenLastCalledWith('list_circuit_queue', { meshId: 42 });

    await listCircuitProbe(42, 10);
    expect(invoke).toHaveBeenLastCalledWith('list_circuit_probe', { meshId: 42, limit: 10 });

    await createCircuit(42, 'n', 'd', 'p');
    expect(invoke).toHaveBeenLastCalledWith('create_circuit', {
      meshId: 42,
      name: 'n',
      description: 'd',
      initialPrompt: 'p',
      triggerKind: 'manual',
      triggerLabel: null,
      intervalSeconds: null,
      blueprint: 'walking_skeleton',
    });

    // Milestone-3 trigger vocabulary rides the same command (#1208).
    await createCircuit(42, 'paced', '', '', 'interval', undefined, 120);
    expect(invoke).toHaveBeenLastCalledWith('create_circuit', {
      meshId: 42,
      name: 'paced',
      description: '',
      initialPrompt: '',
      triggerKind: 'interval',
      triggerLabel: null,
      intervalSeconds: 120,
      blueprint: 'walking_skeleton',
    });

    await setCircuitEnabled(7, false);
    expect(invoke).toHaveBeenLastCalledWith('set_circuit_enabled', {
      circuitId: 7,
      enabled: false,
    });

    await deleteCircuit(7);
    expect(invoke).toHaveBeenLastCalledWith('delete_circuit', { circuitId: 7 });

    await triggerCircuitNow(7);
    expect(invoke).toHaveBeenLastCalledWith('trigger_circuit_now', { circuitId: 7 });

    await listCircuitRuns(7, 5);
    expect(invoke).toHaveBeenLastCalledWith('list_circuit_runs', { circuitId: 7, limit: 5 });

    await pauseCircuitRun(11);
    expect(invoke).toHaveBeenLastCalledWith('pause_circuit_run', { runId: 11 });

    await resumeCircuitRun(11);
    expect(invoke).toHaveBeenLastCalledWith('resume_circuit_run', { runId: 11 });

    await cancelCircuitRun(11);
    expect(invoke).toHaveBeenLastCalledWith('cancel_circuit_run', { runId: 11 });

    await cancelCircuitRuns([11, 12]);
    expect(invoke).toHaveBeenLastCalledWith('cancel_circuit_runs', { runIds: [11, 12] });

    await moveCircuitRun(11, 'up');
    expect(invoke).toHaveBeenLastCalledWith('move_circuit_run', {
      runId: 11,
      direction: 'up',
    });

    await moveCircuitRun(11, 'top');
    expect(invoke).toHaveBeenLastCalledWith('move_circuit_run', {
      runId: 11,
      direction: 'top',
    });

    await reorderCircuitQueue(42, [22, 21]);
    expect(invoke).toHaveBeenLastCalledWith('reorder_circuit_queue', {
      meshId: 42,
      orderedRunIds: [22, 21],
    });

    await approveCircuitStep(11, 'gate');
    expect(invoke).toHaveBeenLastCalledWith('approve_circuit_step', { runId: 11, nodeId: 'gate' });

    // Canvas editor seams (issue #1209).
    await getCircuit(7);
    expect(invoke).toHaveBeenLastCalledWith('get_circuit', { circuitId: 7 });

    await updateCircuitGraph(9, '{"version":1,"nodes":[],"edges":[]}');
    expect(invoke).toHaveBeenLastCalledWith('update_circuit_graph', {
      circuitId: 9,
      graphJson: '{"version":1,"nodes":[],"edges":[]}',
    });
  });
});
