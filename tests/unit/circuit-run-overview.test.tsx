import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { emit } from '@tauri-apps/api/event';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { NodeCard } from '../../src/components/AgentNodeView/NodeCard';
import { CircuitRunOverview } from '../../src/components/Circuits/CircuitRunOverview';
import { setCircuitReviewRoundLimit } from '../../src/lib/tauri/circuit';
import { circuitRunOverview, pushCircuitStep } from '../../src/lib/tauri/circuitEvidence';
import { approveCircuitStep, cancelCircuitRun, pauseCircuitRun, resumeCircuitRun } from '../../src/lib/tauri';
import type { CircuitRunOverview as Overview } from '../../src/types/generated/CircuitRunOverview';
import type { AutopilotCircuitRunStep } from '../../src/types/generated/AutopilotCircuitRunStep';
import type { CircuitGraph } from '../../src/types/generated/CircuitGraph';

vi.mock('@dnd-kit/core', () => ({
  useDraggable: () => ({ setNodeRef: vi.fn(), listeners: {}, attributes: {}, isDragging: false }),
  useDroppable: () => ({ setNodeRef: vi.fn() }),
}));
vi.mock('../../src/components/AgentNodeView/nodeDrag', () => ({ NodeDropCue: () => null }));
const { disposeUtility } = vi.hoisted(() => ({ disposeUtility: vi.fn() }));
vi.mock('../../src/components/Terminal/Terminal', () => ({
  AgentTerminal: ({ nodeId }: { nodeId: number }) => <textarea aria-label={`Agent ${nodeId}`} />,
  terminalManager: { getInstance: () => null },
}));
vi.mock('../../src/components/Terminal/BuildRunTerminalRegistry', () => ({
  buildRunTerminalManager: { dispose: disposeUtility, getInstance: () => null },
}));
vi.mock('../../src/components/AgentNodeView/GridNodeHeader', () => ({
  GridNodeHeader: ({ titleNodeId }: { titleNodeId: number }) => <div>Title {titleNodeId}</div>,
}));
vi.mock('../../src/components/Circuits/CircuitEvidenceHistory', () => ({
  CircuitEvidenceHistory: () => <div>History</div>,
}));
vi.mock('../../src/lib/tauri/circuitEvidence', () => ({
  circuitRunOverview: vi.fn(),
  pushCircuitStep: vi.fn(),
  circuitRunAttention: vi.fn(() => new Promise(() => {})),
  recordCircuitOutcome: vi.fn(),
  recoverFailedCircuitRun: vi.fn(),
  circuitRunHistory: vi.fn(),
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => ({
  ...await importOriginal<typeof import('../../src/lib/tauri')>(),
  approveCircuitStep: vi.fn(() => Promise.resolve()),
  pauseCircuitRun: vi.fn(() => Promise.resolve()),
  resumeCircuitRun: vi.fn(() => Promise.resolve()),
  cancelCircuitRun: vi.fn(() => Promise.resolve()),
}));

vi.mock('../../src/lib/tauri/circuit', async (importOriginal) => ({
  ...await importOriginal<typeof import('../../src/lib/tauri/circuit')>(),
  setCircuitReviewRoundLimit: vi.fn(),
}));

const node = (id: number, overrides: Partial<AgentNode> = {}): AgentNode => ({
  id, mesh_id: 1, name: `Agent ${id}`, status: 'running', provider: 'terminal',
  path: '/repo', branch: 'main', env: 'windows', use_worktree: false, position: id,
  created_at: '', cli_session_id: null, worktree_name: null, is_pinned: false,
  source_issue: null, source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
  source_pr_pinned_sha: null, signal_health: null, worktree_path: null, ...overrides,
});

const graph: CircuitGraph = {
  version: 3,
  nodes: [
    { id: 'trigger', type: { type: 'manual' } },
    { id: 'implement', type: { type: 'spawn_agent_node', prompt: 'do it', name: 'Implement', provider: null, model: null, effort: null, extra_args: null, timeout_seconds: null } },
    { id: 'await', type: { type: 'await_agent_turn', target_node_id: 'implement' } },
    { id: 'done', type: { type: 'notify', message: 'done' } },
    { id: 'nudge', type: { type: 'notify', message: 'nudge' } },
  ],
  edges: [
    { from: 'trigger', to: 'implement', condition: 'always' },
    { from: 'implement', to: 'await', condition: 'always' },
    { from: 'await', to: 'done', condition: { on_outcome: 'completed' } },
    { from: 'await', to: 'nudge', condition: { on_outcome: 'working' } },
  ],
};

const step = (node_id: string, status: string, over: Partial<AutopilotCircuitRunStep> = {}): AutopilotCircuitRunStep => ({
  id: 1, run_id: 10, node_id, agent_node_id: null, status, attempt: 1,
  outcome: status === 'completed' ? 'completed' : null, error_message: null,
  started_at: '2026-10-15 10:00:00', completed_at: status === 'completed' ? '2026-10-15 10:04:00' : null, ...over,
});

const overview = (over: Partial<Overview> = {}): Overview => ({
  run: {
    id: 10, circuit_id: 3, mesh_id: 1, source_agent_node_id: null, trigger_identity: 'manual:1',
    state: 'running', context_json: '{}', created_at: '2026-10-15 10:00:00',
    updated_at: '2026-10-15 10:05:00', attention_dismissed_at: null,
  },
  steps: [
    step('trigger', 'completed'),
    step('implement', 'completed', { agent_node_id: 1 }),
    step('await', 'running'),
  ],
  circuit_name: 'Fix loop',
  graph,
  attention: {
    revision: 41,
    checkpoints: [],
    pushes: [{
      node_id: 'await', attempt: 1, outcomes: ['completed', 'working'],
      overrides: 'The harness reports unfinished child or background work. Waiting for its completion evidence.',
    }],
  },
  ...over,
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => { resolve = r; });
  return { promise, resolve };
}

function card() {
  return <NodeCard nodeId={1} isActive
    onActivate={(id, utility, mode) => useNodeActivityStore.getState().activateNode(id, utility, mode)} />;
}

beforeEach(() => {
  vi.mocked(setCircuitReviewRoundLimit).mockReset();
  vi.mocked(circuitRunOverview).mockReset();
  vi.mocked(pushCircuitStep).mockReset();
  disposeUtility.mockClear();
  useAgentNodeStore.setState({
    nodeIds: [1], nodesById: { 1: node(1) }, activeNodeId: 1, closingNodeIds: new Set(), semanticTurns: {},
    circuitOwnerships: { 1: { node_id: 1, run_id: 10, circuit_id: 3, circuit_name: 'Fix loop', state: 'running', parent_node_id: null } },
  });
  useNodeActivityStore.setState({ selections: {}, utilities: {}, groups: [] });
});

describe('Overview tab on a circuit card', () => {
  it('changes review rounds on a paused run, shows pending and errors, and refreshes saved values', async () => {
    const review = overview();
    review.review_round_limit = 5;
    review.run.state = 'paused';
    review.run.context_json = '{"retry.attempt":"4","retry.max_retries":"5"}';
    review.graph = { version: 3, nodes: [
      { id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
      { id: 'retry', type: { type: 'retry_limit', max_retries: 5 } },
    ], edges: [{ from: 'verdict', to: 'retry', condition: 'always' }, { from: 'retry', to: 'verdict', condition: 'always' }] };
    const pending = deferred<void>();
    vi.mocked(setCircuitReviewRoundLimit).mockReturnValueOnce(pending.promise);
    const onRefresh = vi.fn();
    const onChanged = vi.fn();
    const props = { error: null, onRefresh, onChanged, onOpenAgent: vi.fn(), agentName: () => null };
    const view = render(<CircuitRunOverview {...props} overview={review} />);
    const select = screen.getByRole('combobox', { name: 'Review rounds' }) as HTMLSelectElement;
    expect(select.value).toBe('5');
    expect(within(select).getAllByRole('option').map((option) => option.textContent)).toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9', 'Unlimited']);
    fireEvent.change(select, { target: { value: '1' } });
    expect(setCircuitReviewRoundLimit).toHaveBeenCalledWith(10, 1);
    expect(select.disabled).toBe(true);
    await act(async () => pending.resolve());
    expect(onChanged).toHaveBeenCalledOnce();
    expect(onRefresh).toHaveBeenCalledOnce();
    review.run.context_json = '{"retry.attempt":"4","review.round_limit":"1"}';
    review.review_round_limit = 1;
    view.rerender(<CircuitRunOverview {...props} overview={{ ...review }} />);
    expect(select.value).toBe('1');
    vi.mocked(setCircuitReviewRoundLimit).mockRejectedValueOnce(new Error('Could not save rounds'));
    fireEvent.change(select, { target: { value: '0' } });
    expect((await screen.findByRole('alert')).textContent).toContain('Could not save rounds');
    expect(select.value).toBe('1');
    expect(select.disabled).toBe(false);
    review.run.context_json = '{"retry.attempt":"12","retry.max_retries":"unlimited","review.round_limit":"0"}';
    review.review_round_limit = 0;
    view.rerender(<CircuitRunOverview {...props} overview={{ ...review }} />);
    expect(select.value).toBe('0');
    expect(screen.getByText('Round 12 of unlimited')).toBeTruthy();
    view.rerender(<CircuitRunOverview {...props} overview={{ ...review, run: { ...review.run, state: 'completed' } }} />);
    expect(screen.queryByRole('combobox', { name: 'Review rounds' })).toBeNull();
  });
  it('leads the tab strip, never steals the terminal, and switches back without disposing anything', async () => {
    vi.mocked(circuitRunOverview).mockResolvedValue(overview());
    render(card());
    const tabs = screen.getAllByRole('tab');
    expect(tabs[0].getAttribute('aria-label')).toContain('Circuit run overview');
    expect(tabs[0].getAttribute('aria-selected')).toBe('false');
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();

    fireEvent.click(tabs[0]);
    expect(await screen.findByTestId('run-overview-10')).toBeTruthy();
    expect(screen.queryByLabelText('Agent 1')).toBeNull();
    expect(screen.getByRole('tabpanel').getAttribute('aria-labelledby')).toBe('activity-1-overview');

    fireEvent.click(screen.getByRole('tab', { name: /^Agent/ }));
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();
    expect(disposeUtility).not.toHaveBeenCalled();
  });

  it('carries circuit status on the Overview tab instead of a strip above the terminal', async () => {
    vi.mocked(circuitRunOverview).mockResolvedValue(overview({
      review_round_limit: 5,
      run: { ...overview().run, context_json: '{"retry.attempt":"2"}' },
    }));
    render(card());
    const tab = screen.getAllByRole('tab')[0];
    await waitFor(() => expect(tab.textContent).toContain('Running · 2/5'));
    expect(tab.getAttribute('title')).toContain('Fix loop');
    expect(tab.getAttribute('title')).toContain('Round 2 of 5');
    // Let the card's lazy chunks settle before asserting the strip is absent.
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 100)); });
    expect(screen.queryByRole('region', { name: 'Node review circuit' })).toBeNull();
    expect(screen.queryByText('View circuit')).toBeNull();
    expect(screen.getByLabelText('Agent 1')).toBeTruthy();
    fireEvent.click(tab);
    expect(await screen.findByTestId('run-overview-10')).toBeTruthy();
    expect(circuitRunOverview).toHaveBeenCalledTimes(1);
  });

  it('puts the terminal directly under the title bar when no circuit is attached', async () => {
    useAgentNodeStore.setState({ circuitOwnerships: {} });
    render(card());
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 100)); });
    expect(screen.queryByRole('tablist')).toBeNull();
    expect(screen.queryByText('No circuit attached')).toBeNull();
    const panel = document.getElementById('activity-panel-1');
    expect(panel?.previousElementSibling?.textContent).toBe('Title 1');
  });

  it('keeps showing the Overview when a click inside it activates the card', async () => {
    vi.mocked(circuitRunOverview).mockResolvedValue(overview());
    render(<NodeCard nodeId={1} isActive={false}
      onActivate={(id, utility, mode) => useNodeActivityStore.getState().activateNode(id, utility, mode)} />);
    fireEvent.click(screen.getAllByRole('tab')[0]);
    act(() => useAgentNodeStore.getState().setActiveNode(null));
    fireEvent.click(await screen.findByText('Fix loop'));
    expect(screen.getByTestId('run-overview-10')).toBeTruthy();
    expect(useAgentNodeStore.getState().activeNodeId).toBe(1);
  });

  it('badges the tab only when the run needs a person', async () => {
    vi.mocked(circuitRunOverview).mockResolvedValue(overview());
    const view = render(card());
    await waitFor(() => expect(circuitRunOverview).toHaveBeenCalledWith(10));
    expect(screen.queryByTestId('activity-overview-badge-1')).toBeNull();
    view.unmount();

    vi.mocked(circuitRunOverview).mockResolvedValue(overview({
      steps: [step('trigger', 'completed'), step('implement', 'blocked')],
    }));
    render(card());
    expect(await screen.findByTestId('activity-overview-badge-1')).toBeTruthy();
  });

  it('survives a backend answer that is not an overview', async () => {
    // A mock or a skewed build can answer with anything; the card must not crash.
    vi.mocked(circuitRunOverview).mockResolvedValue({} as Overview);
    render(card());
    fireEvent.click(screen.getAllByRole('tab')[0]);
    expect(await screen.findByText(/did not return a circuit run overview/i)).toBeTruthy();
  });

  it('refreshes for its own run only, and an older read never overwrites a newer one', async () => {
    const first = deferred<Overview>();
    const second = deferred<Overview>();
    vi.mocked(circuitRunOverview).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    render(card());
    fireEvent.click(screen.getAllByRole('tab')[0]);
    await act(async () => { await emit('circuit-run-updated', { run_id: 99, state: 'running' }); });
    expect(circuitRunOverview).toHaveBeenCalledTimes(1);
    await act(async () => { await emit('circuit-run-updated', { run_id: 10, state: 'paused' }); });
    expect(circuitRunOverview).toHaveBeenCalledTimes(2);

    const paused = overview();
    paused.run = { ...paused.run, state: 'paused' };
    await act(async () => { second.resolve(paused); });
    await act(async () => { first.resolve(overview()); });
    expect(screen.getByTestId('run-overview-state').textContent).toBe('Paused');
  });
});

describe('Overview panel', () => {
  const props = {
    error: null, onRefresh: vi.fn(), onChanged: vi.fn(), onOpenAgent: vi.fn(),
    agentName: (id: number) => `Agent ${id}`,
  };

  it('offers Retry for a failed first read and for a failed refresh', () => {
    const onRefresh = vi.fn();
    const view = render(<CircuitRunOverview {...props} onRefresh={onRefresh} overview={null} error="Connection lost" />);
    expect(screen.getByRole('alert').textContent).toContain('Connection lost');
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRefresh).toHaveBeenCalledOnce();
    view.rerender(<CircuitRunOverview {...props} onRefresh={onRefresh} overview={overview()} error="Connection lost" />);
    expect(screen.getByRole('alert').textContent).toContain('Could not refresh: Connection lost');
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRefresh).toHaveBeenCalledTimes(2);
  });

  it('lays the graph out as a rail: done, current, then what is still ahead', () => {
    render(<CircuitRunOverview {...props} overview={overview()} />);
    const rail = screen.getByRole('list', { name: 'Circuit steps' });
    const rows = within(rail).getAllByRole('listitem');
    expect(rows.map((row) => [row.getAttribute('data-testid'), row.getAttribute('data-rail-state')])).toEqual([
      ['rail-row-implement', 'done'],
      ['rail-row-await', 'current'],
      ['rail-row-done', 'remaining'],
      ['rail-row-nudge', 'remaining'],
    ]);
  });

  it('moves a stuck step on with an optional note, naming the evidence it overrides', async () => {
    vi.mocked(pushCircuitStep).mockResolvedValue(undefined);
    const onChanged = vi.fn();
    render(<CircuitRunOverview {...props} onChanged={onChanged} overview={overview()} />);
    const push = screen.getByTestId('run-overview-push-await');
    expect(push.textContent).toContain('Autopilot is waiting: The harness reports unfinished child or background work.');
    expect(within(push).getAllByRole('button').map((button) => button.textContent)).toEqual(['Agent has finished', 'Still working']);

    fireEvent.click(screen.getByTestId('run-overview-push-await-completed'));
    expect(push.textContent).toContain('This overrides what Autopilot observed.');
    fireEvent.change(screen.getByTestId('run-overview-push-note-await'), { target: { value: 'The build finished an hour ago.' } });
    fireEvent.click(screen.getByTestId('run-overview-push-confirm-await'));

    await waitFor(() => expect(onChanged).toHaveBeenCalled());
    expect(pushCircuitStep).toHaveBeenCalledWith({
      run_id: 10, node_id: 'await', attempt: 1, expected_revision: 41,
      outcome: 'completed', reason: 'The build finished an hour ago.',
    });
  });

  it('names a completed push that re-enters review as the next review', () => {
    const review = overview();
    review.attention.pushes[0].starts_next_review = true;
    render(<CircuitRunOverview {...props} overview={review} />);
    expect(screen.getByTestId('run-overview-push-await-completed').textContent).toBe('Start the next review');
    fireEvent.click(screen.getByTestId('run-overview-push-await-completed'));
    expect(screen.getByTestId('run-overview-push-confirm-await').textContent).toBe('Move on: Start the next review');
  });

  it('names the last allowed round as recording the round, and points at Review again', () => {
    const review = overview();
    review.attention.pushes[0].starts_next_review = false;
    review.attention.pushes[0].ends_review = true;
    review.attention.pushes[0].review_again = true;
    render(<CircuitRunOverview {...props} overview={review} />);
    expect(screen.getByTestId('run-overview-push-await-completed').textContent).toBe('Record the last round');
    expect(screen.getByText('Recording this stops the review. Review again adds one more round after the run stops.')).toBeTruthy();
    fireEvent.click(screen.getByTestId('run-overview-push-await-completed'));
    expect(screen.getByTestId('run-overview-push-confirm-await').textContent).toBe('Move on: Record the last round');
  });

  it('points a custom last round at manual recovery instead of Review again', () => {
    const review = overview();
    review.attention.pushes[0].starts_next_review = false;
    review.attention.pushes[0].ends_review = true;
    review.attention.pushes[0].review_again = false;
    render(<CircuitRunOverview {...props} overview={review} />);
    expect(screen.getByTestId('run-overview-push-await-completed').textContent).toBe('Record the last round');
    expect(screen.getByText('Recording this stops the review. This custom circuit needs manual recovery. Open its implementation agent and inspect the failed step.')).toBeTruthy();
    expect(screen.queryByText('Recording this stops the review. Review again adds one more round after the run stops.')).toBeNull();
  });

  it('shows a refused push, keeps the choice open and rereads the run so a retry uses its new revision', async () => {
    vi.mocked(pushCircuitStep).mockRejectedValue('The run changed. Refresh it before acting.');
    props.onRefresh.mockClear();
    props.onChanged.mockClear();
    render(<CircuitRunOverview {...props} overview={overview()} />);
    fireEvent.click(screen.getByTestId('run-overview-push-await-working'));
    fireEvent.click(screen.getByTestId('run-overview-push-confirm-await'));
    expect((await screen.findByRole('alert')).textContent).toContain('The run changed.');
    expect(screen.getByTestId('run-overview-push-confirm-await')).toBeTruthy();
    expect(props.onRefresh).toHaveBeenCalledTimes(1);
    expect(props.onChanged).not.toHaveBeenCalled();
  });

  it('runs the live controls against this run and tells the card it changed', async () => {
    const run = overview().run;
    props.onChanged.mockClear();
    const { rerender } = render(<CircuitRunOverview {...props} overview={overview()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Pause' }));
    await waitFor(() => expect(pauseCircuitRun).toHaveBeenCalledWith(10));
    await waitFor(() => expect(props.onChanged).toHaveBeenCalledTimes(1));

    rerender(<CircuitRunOverview {...props} overview={overview({ run: { ...run, state: 'paused' } })} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Resume' }));
    await waitFor(() => expect(resumeCircuitRun).toHaveBeenCalledWith(10));

    fireEvent.click(await screen.findByRole('button', { name: 'Cancel…' }));
    expect(cancelCircuitRun).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('run-overview-cancel-confirm'));
    await waitFor(() => expect(cancelCircuitRun).toHaveBeenCalledWith(10));
  });

  it('asks for approval on a blocked step and approves that step', async () => {
    const steps = [...overview().steps, step('gate', 'blocked')];
    render(<CircuitRunOverview {...props} overview={overview({ steps })} />);
    const approval = screen.getByTestId('run-overview-approval-gate');
    fireEvent.click(within(approval).getByRole('button', { name: 'Approve' }));
    await waitFor(() => expect(approveCircuitStep).toHaveBeenCalledWith(10, 'gate'));
  });

  it('hands unverified checkpoints and failed-run recovery to the shared next-steps panel', () => {
    const unverified = overview({ steps: [step('trigger', 'completed'), step('await', 'unverified')] });
    const { rerender } = render(<CircuitRunOverview {...props} overview={unverified} />);
    expect(screen.getByTestId('run-next-10')).toBeTruthy();

    const run = overview().run;
    const failed = overview({
      run: { ...run, state: 'failed' },
      steps: [step('trigger', 'completed'), step('await', 'failed', { outcome: 'failed', error_message: 'Agent exited' })],
    });
    rerender(<CircuitRunOverview {...props} overview={failed} />);
    expect(screen.getByTestId('run-next-10')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Pause' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Cancel…' })).toBeNull();
  });
});
