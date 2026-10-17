import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { NodeCircuitRunPanel } from '../../src/mobile/screens/CircuitRun';
import NodeList, { NodeRow } from '../../src/mobile/screens/NodeList';
import { circuitStatusLine } from '../../src/mobile/circuitRun';
import type { AgentNode, Mesh, NodeCircuitRun } from '../../src/mobile/api';
import type { NodeCircuitNeed } from '../../src/types/generated/NodeCircuitNeed';

const response = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

const gate = { node_id: 'confirm_source', role: 'Approval gate', attempt: 1, status: 'blocked' };
const approval: NodeCircuitNeed = { kind: 'approval', step: gate, detail: null, actions: [], agent_node_id: null };
const unverified = (actions: NodeCircuitNeed['actions']): NodeCircuitNeed => ({
  kind: 'unverified',
  step: { node_id: 'verdict', role: 'Review', attempt: 2, status: 'unverified' },
  detail: null,
  actions,
  starts_next_review: false,
  ends_review: false,
  agent_node_id: null,
});
const circuit = (over: Partial<NodeCircuitRun> = {}): NodeCircuitRun => ({
  node_id: 17,
  run_id: 7,
  blueprint_name: 'Review Blueprint',
  state: 'running',
  step: gate,
  review: { round: 2, max_rounds: 3, verdict: null, summary: null },
  needs_you: [approval],
  revision: 12,
  ...over,
});

let runs: NodeCircuitRun[];
let posts: Array<{ url: string; body: unknown }>;
let postReply: () => Promise<Response>;
let fetcher: ReturnType<typeof vi.fn>;

beforeEach(() => {
  runs = [circuit()];
  posts = [];
  postReply = () => Promise.resolve(response({ ok: true, run_id: 7 }));
  fetcher = vi.fn((url: string, init?: RequestInit) => {
    if (init?.method === 'POST') {
      posts.push({ url, body: init.body === undefined ? undefined : JSON.parse(String(init.body)) });
      return postReply();
    }
    return Promise.resolve(response(runs));
  });
  vi.stubGlobal('fetch', fetcher);
});
afterEach(() => {
  vi.unstubAllGlobals();
});

const panel = (over: { onAuthFailed?: () => void; onOpenTerminal?: () => void } = {}) =>
  render(<NodeCircuitRunPanel nodeId={17} onAuthFailed={over.onAuthFailed ?? vi.fn()} onOpenTerminal={over.onOpenTerminal ?? vi.fn()} />);

it('says where the run is in plain words, with no scheduler tokens', async () => {
  panel();
  expect(await screen.findByTestId('circuit-run-line')).toHaveProperty(
    'textContent',
    'Needs you: Review Blueprint · Round 2 of 3 · Approval gate: Needs approval',
  );
  expect(screen.getByText('Approval gate needs your approval')).toBeTruthy();
});

it('shows the latest verdict and the reviewer’s report when the pass has one', async () => {
  runs = [circuit({
    needs_you: [],
    step: { node_id: 'feedback', role: 'Send feedback', attempt: 1, status: 'running' },
    review: { round: 1, max_rounds: 3, verdict: 'changes_requested', summary: 'The retry loop never exits.' },
  })];
  panel();
  expect((await screen.findByTestId('circuit-run-line')).textContent).toBe(
    'Running: Review Blueprint · Round 1 of 3 · Changes requested · Send feedback: Running',
  );
  expect(screen.getByText('The retry loop never exits.')).toBeTruthy();
});

it('renders nothing for a node with no run, and ignores other nodes’ runs', async () => {
  runs = [circuit({ node_id: 99 })];
  const view = panel();
  await waitFor(() => expect(fetcher).toHaveBeenCalled());
  expect(view.container.querySelector('[data-testid="circuit-run-panel"]')).toBeNull();
});

it('treats a desktop without the route as having no circuit support, not as a failure', async () => {
  fetcher.mockImplementation(() => Promise.resolve(response({ error: 'not found' })));
  const view = panel();
  await waitFor(() => expect(fetcher).toHaveBeenCalled());
  expect(view.container.textContent).toBe('');
});

it('notes, without an alert, that status is unavailable when the desktop sends an unexpected body', async () => {
  fetcher.mockImplementation(() => Promise.resolve(response({ ticket: 'x' })));
  panel();
  expect(await screen.findByText(/Circuit Run status is unavailable/)).toBeTruthy();
  expect(screen.queryByRole('alert')).toBeNull();
});

it('asks before approving, sends the literal gate id once, and reflects the run moving on', async () => {
  let finish!: (value: Response) => void;
  postReply = () => new Promise<Response>((resolve) => { finish = resolve; });
  panel();
  fireEvent.click(await screen.findByRole('button', { name: 'Approve…' }));
  expect(posts).toEqual([]);
  expect(screen.getByRole('dialog', { name: 'Approve the Approval gate?' })).toBeTruthy();

  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(finish).toBeTypeOf('function'));
  expect(posts).toEqual([{ url: '/api/circuit-runs/7/approve', body: { node_id: 'confirm_source' } }]);
  expect((screen.getByRole('button', { name: 'Sending…' }) as HTMLButtonElement).disabled).toBe(true);

  runs = [circuit({ needs_you: [], step: { ...gate, status: 'completed' } })];
  await act(async () => finish(response({ ok: true, run_id: 7 })));
  expect(await screen.findByText('Approved. The run will carry on.')).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Approve…' })).toBeNull();
  expect(posts).toHaveLength(1);
});

it('keeps the sheet open with the desktop’s refusal and does not log the phone out', async () => {
  const auth = vi.fn();
  postReply = () => Promise.resolve(response({ error: 'step confirm_source is not waiting for approval (status running)' }, 409));
  panel({ onAuthFailed: auth });
  fireEvent.click(await screen.findByRole('button', { name: 'Approve…' }));
  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', expect.stringContaining('not waiting for approval'));
  expect(auth).not.toHaveBeenCalled();
  expect(screen.getByRole('dialog')).toBeTruthy();
});

it('drops a confirmation whose gate has since been resolved elsewhere', async () => {
  panel();
  fireEvent.click(await screen.findByRole('button', { name: 'Approve…' }));
  expect(screen.getByRole('dialog')).toBeTruthy();
  runs = [circuit({ needs_you: [], step: { ...gate, status: 'completed' } })];
  act(() => window.dispatchEvent(new Event('online')));
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  expect(posts).toEqual([]);
});

it('offers exactly the checkpoint actions the desktop listed; recheck acts at once with the revision', async () => {
  runs = [circuit({ needs_you: [unverified(['recheck', 'review_pass', 'review_fail'])], step: null })];
  panel();
  expect(await screen.findByRole('button', { name: 'Check again' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: "It didn't happen" })).toBeNull();

  fireEvent.click(screen.getByRole('button', { name: 'Check again' }));
  await waitFor(() => expect(posts).toHaveLength(1));
  expect(posts[0]).toEqual({
    url: '/api/circuit-runs/7/outcome',
    body: {
      run_id: 7, node_id: 'verdict', attempt: 2, expected_revision: 12, action: 'recheck',
      reason: 'Checked again from the phone.',
    },
  });
});

it('names a review re-entry as the next review pass', async () => {
  runs = [circuit({
    needs_you: [{ ...unverified(['recheck', 'completed']), starts_next_review: true }],
    step: null,
  })];
  panel();
  expect(await screen.findByRole('button', { name: 'Start the next review' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Check again' })).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Start the next review' }));
  expect(screen.getByRole('dialog', { name: 'Start the next review' })).toBeTruthy();
});

it('names the last allowed round as recording the round, and points at Review again', async () => {
  runs = [circuit({
    needs_you: [{ ...unverified(['recheck', 'completed']), starts_next_review: false, ends_review: true }],
    step: null,
  })];
  panel();
  expect(await screen.findByRole('button', { name: 'Record the last round' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Check again' })).toBeTruthy();
  expect(screen.getByText('Recording this stops the review. Review again adds one more round after the run stops.')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Record the last round' }));
  expect(screen.getByRole('dialog', { name: 'Record the last round' })).toBeTruthy();
  expect(screen.getAllByText('Recording this stops the review. Review again adds one more round after the run stops.')).toHaveLength(2);
  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  expect(await screen.findByText('Recorded. The review stops.')).toBeTruthy();
});

it('requires evidence for a review verdict before it can be sent', async () => {
  runs = [circuit({ needs_you: [unverified(['recheck', 'review_pass'])], step: null })];
  panel();
  fireEvent.click(await screen.findByRole('button', { name: 'Review passed' }));
  const confirm = screen.getByRole('button', { name: 'Confirm' }) as HTMLButtonElement;
  expect(confirm.disabled).toBe(true);
  fireEvent.change(screen.getByLabelText(/Your decision and the evidence/), { target: { value: 'Read the diff; tests pass.' } });
  expect(confirm.disabled).toBe(false);
  fireEvent.click(confirm);
  await waitFor(() => expect(posts).toHaveLength(1));
  expect(posts[0].body).toMatchObject({ action: 'review_pass', reason: 'Read the diff; tests pass.', expected_revision: 12 });
});

it('offers Run Recovery only where the desktop says it is available, and explains the rest', async () => {
  runs = [circuit({
    state: 'failed',
    needs_you: [{ kind: 'failed', step: { node_id: 'verdict', role: 'Review', attempt: 1, status: 'failed' }, detail: 'reviewer vanished', actions: [], agent_node_id: null }],
    recovery: {
      node_id: 'verdict', attempt: 1, status: 'failed', error: 'reviewer vanished',
      options: [
        { action: 'retry', available: false, unavailable_reason: 'The agent for this step was closed.' },
        { action: 'continue', available: false, unavailable_reason: 'A review approval cannot be recorded on the reviewer’s behalf.' },
      ],
    },
  })];
  panel();
  expect(await screen.findByText(/To retry: The agent for this step was closed\./)).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Retry this step' })).toBeNull();
  expect(screen.queryByRole('button', { name: /continue/ })).toBeNull();
  expect(screen.queryByRole('button', { name: 'Cancel run' })).toBeNull();
});

it('retries a failed step and records a continue with the step the desktop named', async () => {
  runs = [circuit({
    state: 'failed',
    needs_you: [],
    recovery: {
      node_id: 'publish', attempt: 1, status: 'failed', error: 'prompt delivery failed',
      options: [{ action: 'retry', available: true }, { action: 'continue', available: true }],
    },
  })];
  panel();
  fireEvent.click(await screen.findByRole('button', { name: 'Retry this step' }));
  await waitFor(() => expect(posts).toHaveLength(1));
  expect(posts[0]).toEqual({
    url: '/api/circuit-runs/7/recover',
    body: { run_id: 7, node_id: 'publish', attempt: 1, expected_revision: 12, action: 'retry', reason: '' },
  });

  fireEvent.click(await screen.findByRole('button', { name: 'I’ve done this — continue' }));
  fireEvent.change(screen.getByLabelText(/What did you do or check/), { target: { value: 'Pushed by hand.' } });
  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(posts).toHaveLength(2));
  expect(posts[1].body).toMatchObject({ action: 'continue', reason: 'Pushed by hand.', node_id: 'publish' });
});

it('cancels only after confirmation and keeps the confirmation after the run leaves the list', async () => {
  needsNothing();
  panel();
  fireEvent.click(await screen.findByRole('button', { name: 'Cancel run' }));
  expect(posts).toEqual([]);
  runs = [];
  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(posts).toEqual([{ url: '/api/circuit-runs/7/cancel', body: undefined }]));
  expect(await screen.findByText('Run cancelled.')).toBeTruthy();
});

function needsNothing() {
  runs = [circuit({ needs_you: [], step: { node_id: 'verdict', role: 'Review', attempt: 1, status: 'running' } })];
}

it('answers a question in the terminal rather than inventing a reply channel', async () => {
  const open = vi.fn();
  runs = [circuit({
    needs_you: [{ kind: 'input', step: { node_id: 'await_source', role: 'Await task', attempt: 1, status: 'running' }, detail: 'Waiting for a permission decision', actions: [], agent_node_id: 17 }],
  })];
  panel({ onOpenTerminal: open });
  fireEvent.click(await screen.findByRole('button', { name: 'Open terminal to answer' }));
  expect(open).toHaveBeenCalledTimes(1);
  expect(posts).toEqual([]);
});

it('does not let a refresh that started before a click overwrite the result of the click', async () => {
  let stale!: (value: Response) => void;
  fetcher.mockImplementationOnce(() => new Promise<Response>((resolve) => { stale = resolve; }));
  panel();
  await waitFor(() => expect(stale).toBeTypeOf('function'));
  runs = [circuit({ needs_you: [], step: { ...gate, status: 'completed' } })];
  fetcher.mockImplementation((url: string, init?: RequestInit) =>
    init?.method === 'POST' ? Promise.resolve(response({ ok: true, run_id: 7 })) : Promise.resolve(response(runs)));
  act(() => window.dispatchEvent(new Event('online')));
  expect(await screen.findByText(/Round 2 of 3 · Approval gate: Done/)).toBeTruthy();
  await act(async () => stale(response([circuit()])));
  expect(screen.getByTestId('circuit-run-line').textContent).toContain('Approval gate: Done');
});

it('reports an authorization failure on the read, but not after unmount', async () => {
  const auth = vi.fn();
  let late!: (value: Response) => void;
  fetcher.mockImplementationOnce(() => new Promise<Response>((resolve) => { late = resolve; }));
  panel({ onAuthFailed: auth }).unmount();
  await act(async () => late(response({ error: 'Unauthorized' }, 403)));
  expect(auth).not.toHaveBeenCalled();
  fetcher.mockImplementation(() => Promise.resolve(response({ error: 'Unauthorized' }, 403)));
  panel({ onAuthFailed: auth });
  await waitFor(() => expect(auth).toHaveBeenCalledTimes(1));
});

it('puts the same line on the node card, amber only while the run needs the person', () => {
  const node = { id: 17, mesh_id: 1, name: 'Fix search', provider: 'anthropic', status: 'idle' } as AgentNode;
  const view = render(<NodeRow node={node} onClick={vi.fn()} circuitRun={circuit()} />);
  const line = screen.getByTestId('node-circuit-17');
  expect(line.textContent).toBe(circuitStatusLine(circuit()));
  expect(line.getAttribute('data-needs-you')).toBe('true');
  view.rerender(<NodeRow node={node} onClick={vi.fn()} circuitRun={circuit({ needs_you: [] })} />);
  expect(screen.getByTestId('node-circuit-17').getAttribute('data-needs-you')).toBe('false');
  view.rerender(<NodeRow node={node} onClick={vi.fn()} />);
  expect(screen.queryByTestId('node-circuit-17')).toBeNull();
});

it('keeps the node list when the circuit read fails, and shows the line when it works', async () => {
  vi.stubGlobal('WebSocket', class { onmessage?: unknown; close() {} });
  const node = { id: 17, mesh_id: 1, name: 'Fix search', provider: 'anthropic', status: 'idle' } as AgentNode;
  const mesh = { id: 1, name: 'Buildmesh' } as Mesh;
  let circuitStatus = 500;
  fetcher.mockImplementation((url: string) =>
    Promise.resolve(
      url === '/api/circuit-runs'
        ? response(circuitStatus === 200 ? runs : { error: 'boom' }, circuitStatus)
        : response(url === '/api/nodes' ? [node] : url === '/api/meshes' ? [mesh] : url === '/api/providers' ? [] : { ticket: 't' }),
    ),
  );
  const offline = vi.fn();
  render(<NodeList onOpenNode={vi.fn()} onOpenAgentNodes={vi.fn()} onOpenIssues={vi.fn()} onOffline={offline} onAuthFailed={vi.fn()} />);
  expect(await screen.findByText('Fix search')).toBeTruthy();
  expect(offline).not.toHaveBeenCalled();
  expect(screen.queryByTestId('node-circuit-17')).toBeNull();

  circuitStatus = 200;
  act(() => window.dispatchEvent(new Event('online')));
  expect((await screen.findByTestId('node-circuit-17')).textContent).toContain('Needs you: Review Blueprint');
});
