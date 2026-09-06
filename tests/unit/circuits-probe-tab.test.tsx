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

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { AutopilotCircuit } from '../../src/types/generated/AutopilotCircuit';
import type { CircuitRunDetail } from '../../src/types/generated/CircuitRunDetail';
import type { CircuitQueueEntry } from '../../src/types/generated/CircuitQueueEntry';
import { seedAgentNodes } from './helpers/seedAgentNodes';
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
  moveCircuitRun,
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
  concurrency_limit: 1,
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

function mockBackend(overrides: {
  circuits?: AutopilotCircuit[];
  runs?: CircuitRunDetail[];
  queue?: CircuitQueueEntry[];
} = {}) {
  const circuits = overrides.circuits ?? [CIRCUIT];
  const runs = overrides.runs ?? [RUN_DONE, RUN_RUNNING];
  const queue = overrides.queue ?? [];
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'list_circuits') return Promise.resolve(circuits);
    if (cmd === 'list_circuit_runs') return Promise.resolve(runs);
    if (cmd === 'list_circuit_queue') return Promise.resolve(queue);
    if (cmd === 'list_circuit_probe') {
      return Promise.resolve({
        circuits: circuits.map((circuit) => ({
          circuit,
          runs: runs.filter((r) => r.run.circuit_id === circuit.id),
        })),
        queue,
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
      cmd === 'move_circuit_run' ||
      cmd === 'approve_circuit_step'
    ) {
      return Promise.resolve(args && cmd === 'create_circuit' ? CIRCUIT : undefined);
    }
    if (cmd === 'trigger_circuit_now') return Promise.resolve(13);
    // Default-fall-through so failures surface the unexpected command.
    return Promise.resolve({ cmd });
  });
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

describe('CircuitsProbeTab', () => {
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

  it('keeps failures visible in Activity and retains them in History', async () => {
    mockBackend({ runs: [
      { ...RUN_DONE, run: { ...RUN_DONE.run, id: 9, state: 'failed' } },
      RUN_DONE, RUN_RUNNING,
    ] });
    openProbeDestination('circuits');
    await screen.findByTestId('run-card-12');
    expect(screen.getByTestId('run-card-9')).toBeTruthy();
    fireEvent.click(screen.getByTestId('circuits-view-history'));
    expect(screen.getByTestId('run-card-9')).toBeTruthy();
  });

  it('prioritizes attention across circuits and separates history, queue, and configuration', async () => {
    const failed = { ...RUN_DONE, run: { ...RUN_DONE.run, id: 13, circuit_id: 8, state: 'failed' } };
    mockBackend({
      circuits: [CIRCUIT, { ...CIRCUIT, id: 8, name: 'review' }],
      runs: [RUN_DONE, RUN_RUNNING, failed],
      queue: QUEUE,
    });
    openProbeDestination('circuits');
    await screen.findByTestId('run-card-13');
    const cards = screen.getAllByTestId(/^run-card-/);
    expect(cards.map((card) => card.getAttribute('data-run-state'))).toEqual(['failed', 'running']);
    expect(screen.queryByTestId('run-card-11')).toBeNull();
    expect(screen.queryByTestId('queue-run-21')).toBeNull();
    expect(screen.queryByTestId('circuit-name-input')).toBeNull();
    expect(screen.getByTestId('circuits-status').textContent).toContain('2 queued');
    expect(screen.getByTestId('circuit-activity-queue-summary').textContent).toContain('Waiting in queue');
    expect(screen.getByTestId('circuit-trigger-7')).toBeTruthy();
    expect(screen.getByTestId('circuit-edit-flow-7')).toBeTruthy();
    expect(screen.getByTestId('circuits-view-queue').textContent).toContain('(2)');
    fireEvent.click(screen.getByTestId('circuits-view-history'));
    expect(screen.getByTestId('run-card-11')).toBeTruthy();
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
    // (#1467 / #1475), NOT from the per-circuit `concurrency_limit`. Set
    // a circuit-level cap of 1 and a mesh cap of 4 with only one admitted
    // run — the queue row must reflect the mesh number (4) and the
    // "spare capacity" wording (because we're under the cap, not full).
    useMeshStore.setState({
      meshes: [{ ...MESH, circuit_run_capacity: 4 }],
      meshesById: new Map([[MESH.id, { ...MESH, circuit_run_capacity: 4 }]]),
      selectedMeshId: MESH.id,
    });
    mockBackend({
      circuits: [{ ...CIRCUIT, concurrency_limit: 1 }],
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
        concurrencyLimit: 1,
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

  it('creates the issue-driven Autopilot review blueprint with two agent slots', async () => {
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
        concurrencyLimit: 2,
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

  it('shows the complete queue nearest-first with reorder and cancel controls', async () => {
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
    await waitFor(() => {
      expect((screen.getByLabelText('Cancel run 21') as HTMLButtonElement).disabled).toBe(false);
    });
    await user.click(screen.getByLabelText('Cancel run 21'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('cancel_circuit_run', { runId: 21 });
    });
  });

  it('shows the empty state when no circuits exist', async () => {
    mockBackend({ circuits: [], runs: [] });
    openProbeDestination('circuits');

    expect(await screen.findByText('No circuits yet')).toBeTruthy();
  });

  // -- human-in-the-loop (#1207) ------------------------------------------------

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
          error_message: null,
          started_at: '2026-08-22 10:05:00',
          completed_at: null,
        },
      ],
    };
    mockBackend({ runs: [RUN_BLOCKED] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    expect(
      await screen.findByTestId('blocked-badge-15-approval'),
    ).toBeTruthy();
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
    expect(screen.getByTestId('run-activity-20').textContent).toContain('Running');
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
    // concurrency_limit 1 with a running step elsewhere on the circuit =>
    // the circuit's own step budget is what's holding the reviewer back.
    const RUN_HOGGING: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 22, state: 'running' },
      steps: [step({ node_id: 'implementer', status: 'running' })],
    };
    mockBackend({
      circuits: [{ ...CIRCUIT, concurrency_limit: 1 }],
      runs: [RUN_QUEUED, RUN_HOGGING],
    });
    openProbeDestination('circuits');

    const activity = await screen.findByTestId('run-activity-21');
    expect(activity.textContent).toContain('Queued');
    expect(activity.textContent).toContain('reviewer');
    // The raw scheduler token never reaches the user.
    expect(activity.textContent).not.toContain('pending_slot');
    expect(screen.getByTestId('run-reason-21').textContent).toBe(
      'Waiting for a slot — this circuit runs one step at a time, and that slot is busy.'
    );
  });

  it('attributes a queued step to the circuit agent lease when circuit slots are free', async () => {
    const RUN_QUEUED: CircuitRunDetail = {
      run: { ...RUN_DONE.run, id: 23, state: 'running' },
      steps: [step({ node_id: 'implementer', status: 'pending_slot' })],
    };
    mockBackend({ circuits: [{ ...CIRCUIT, concurrency_limit: 2 }], runs: [RUN_QUEUED] });
    openProbeDestination('circuits');

    expect((await screen.findByTestId('run-reason-23')).textContent).toContain(
      'waiting on a circuit agent slot'
    );
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

    // Failed runs open by default, and a failure is never hidden.
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

  it('surfaces outcome, attempt and duration per step', async () => {
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

    const row = await screen.findByTestId('run-step-26-review_classifier');
    expect(row.getAttribute('data-step-status')).toBe('completed');
    expect(row.textContent).toContain('Done');
    expect(row.textContent).toContain('red');
    expect(row.textContent).toContain('attempt 3');
    // 90s reads as "1m 30s", not "90.0s".
    expect(row.textContent).toContain('1m 30s');
    expect(screen.getByTestId('run-retries-26').textContent).toContain('retried');
  });

  it('opens live and failed runs, collapses completed runs, and honours a manual toggle', async () => {
    mockBackend({ runs: [RUN_DONE, RUN_RUNNING] });
    const user = userEvent.setup();
    openProbeDestination('circuits');

    await waitFor(() => {
      expect(screen.getByTestId('run-toggle-12').getAttribute('aria-expanded')).toBe('true');
    });
    await user.click(screen.getByTestId('circuits-view-history'));
    expect(screen.getByTestId('run-toggle-11').getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByTestId('run-steps-11')).toBeNull();

    // One click opens the completed run's ledger…
    await user.click(screen.getByTestId('run-toggle-11'));
    expect(screen.getByTestId('run-toggle-11').getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByTestId('run-steps-11')).toBeTruthy();

    await user.click(screen.getByTestId('circuits-view-activity'));
    expect(screen.getByTestId('run-toggle-12').getAttribute('aria-expanded')).toBe('true');
    await user.click(screen.getByTestId('run-toggle-12'));
    expect(screen.getByTestId('run-toggle-12').getAttribute('aria-expanded')).toBe('false');
  });

  it('keeps the disclosure control accessible', async () => {
    mockBackend({ runs: [RUN_DONE] });
    openProbeDestination('circuits');

    fireEvent.click(await screen.findByTestId('circuits-view-history'));
    const toggle = await screen.findByTestId('run-toggle-11');
    expect(toggle.tagName).toBe('BUTTON');
    expect(toggle.getAttribute('aria-controls')).toBe('run-detail-11');
    // Controls live outside the disclosure button — a button nested in a
    // button is invalid HTML and breaks keyboard semantics.
    expect(toggle.querySelector('button')).toBeNull();
  });

  it('uses tab semantics for the mutually exclusive views', async () => {
    mockBackend();
    openProbeDestination('circuits');

    const tablist = screen.getByRole('tablist', { name: 'Circuit views' });
    expect(tablist).toBeTruthy();
    expect(screen.getByRole('tab', { name: /Activity/ }).getAttribute('aria-selected')).toBe('true');
    expect(screen.getByRole('tab', { name: 'History' }).getAttribute('aria-selected')).toBe('false');
    await userEvent.setup().click(screen.getByRole('tab', { name: 'History' }));
    expect(screen.getByTestId('circuits-probe-body').getAttribute('aria-labelledby')).toBe('circuits-tab-history');
    expect(screen.getByRole('tab', { name: 'History' }).getAttribute('aria-selected')).toBe('true');
  });

  it('supports roving keyboard navigation across circuit views', async () => {
    mockBackend();
    openProbeDestination('circuits');

    const user = userEvent.setup();
    const activityTab = screen.getByRole('tab', { name: /Activity/ });
    activityTab.focus();
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'History' }).getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(screen.getByRole('tab', { name: 'History' }));
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
          created_at: '2026-09-01 12:00:00',
          updated_at: '2026-09-01 12:00:00',
        },
        steps: [step({ node_id: 'implementer', status: 'running' })],
      };
      mockBackend({ runs: [RUN_LIVE] });
      openProbeDestination('circuits');
      // Flush the load IPC without letting wall-clock time move.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      expect(screen.getByTestId('run-card-30').textContent).toContain('10.0s');
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

  it('does not tick when every visible run is terminal', async () => {
    // The interval is gated on there being a live run, so a tab showing
    // finished work re-renders never.
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
    expect(body.contains(screen.getByTestId('circuit-name-input'))).toBe(false);
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

    await createCircuit(42, 'n', 'd', 2, 'p');
    expect(invoke).toHaveBeenLastCalledWith('create_circuit', {
      meshId: 42,
      name: 'n',
      description: 'd',
      concurrencyLimit: 2,
      initialPrompt: 'p',
      triggerKind: 'manual',
      triggerLabel: null,
      intervalSeconds: null,
      blueprint: 'walking_skeleton',
    });

    // Milestone-3 trigger vocabulary rides the same command (#1208).
    await createCircuit(42, 'paced', '', 1, '', 'interval', undefined, 120);
    expect(invoke).toHaveBeenLastCalledWith('create_circuit', {
      meshId: 42,
      name: 'paced',
      description: '',
      concurrencyLimit: 1,
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

    await moveCircuitRun(11, 'up');
    expect(invoke).toHaveBeenLastCalledWith('move_circuit_run', {
      runId: 11,
      direction: 'up',
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
