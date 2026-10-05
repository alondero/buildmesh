import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { emit } from '@tauri-apps/api/event';
import { RunNextSteps, runNeedsNextSteps } from '../../src/components/Circuits/RunNextStepsPanel';
import {
  circuitRunAttention,
  recordCircuitOutcome,
  recoverFailedCircuitRun,
} from '../../src/lib/tauri/circuitEvidence';
import type { AutopilotCircuitRun } from '../../src/types/generated/AutopilotCircuitRun';
import type { AutopilotCircuitRunStep } from '../../src/types/generated/AutopilotCircuitRunStep';
import type { CircuitRunAttention } from '../../src/types/generated/CircuitRunAttention';

vi.mock('../../src/lib/tauri/circuitEvidence', () => ({
  circuitRunHistory: vi.fn(),
  circuitRunAttention: vi.fn(),
  recordCircuitOutcome: vi.fn(),
  recoverFailedCircuitRun: vi.fn(),
}));

const run = (over: Partial<AutopilotCircuitRun> = {}): AutopilotCircuitRun => ({
  id: 7,
  circuit_id: 1,
  mesh_id: 1,
  source_agent_node_id: null,
  trigger_identity: 'issue:9',
  state: 'failed',
  context_json: '{}',
  created_at: '2026-10-05 12:00:00',
  updated_at: '2026-10-05 12:30:00',
  ...over,
});

const step = (over: Partial<AutopilotCircuitRunStep>): AutopilotCircuitRunStep => ({
  id: 1,
  run_id: 7,
  node_id: 'finish',
  agent_node_id: null,
  status: 'failed',
  attempt: 1,
  outcome: 'failed',
  error_message: 'Prompt delivery failed',
  started_at: null,
  completed_at: null,
  ...over,
});

const recoverable = (over: Partial<CircuitRunAttention> = {}): CircuitRunAttention => ({
  revision: 41,
  checkpoints: [],
  recovery: {
    node_id: 'finish',
    attempt: 1,
    status: 'failed',
    error: 'Prompt delivery failed',
    options: [
      { action: 'retry', available: true },
      { action: 'continue', available: true },
    ],
  },
  ...over,
});

const roleLabel = (id: string) => (id === 'finish' ? 'Send finish prompt' : id);

function renderPanel(
  runOver: Partial<AutopilotCircuitRun> = {},
  steps: AutopilotCircuitRunStep[] = [step({})],
) {
  return render(<RunNextSteps run={run(runOver)} steps={steps} roleLabel={roleLabel} busy={false} />);
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(circuitRunAttention).mockReset();
  vi.mocked(recordCircuitOutcome).mockReset().mockResolvedValue(undefined);
  vi.mocked(recoverFailedCircuitRun).mockReset().mockResolvedValue(undefined);
});

describe('when the panel is needed', () => {
  it('is needed for a failed run with a failing step, and for a running run with an unverified step', () => {
    expect(runNeedsNextSteps({ state: 'failed' }, [step({})])).toBe(true);
    expect(runNeedsNextSteps({ state: 'running' }, [step({ status: 'unverified', error_message: 'wait' })])).toBe(true);
  });

  it('is not needed for a review that merely did not approve, a quiet run, or a finished one', () => {
    expect(runNeedsNextSteps({ state: 'failed' }, [step({ status: 'completed', error_message: null })])).toBe(false);
    expect(runNeedsNextSteps({ state: 'running' }, [step({ status: 'running', error_message: null })])).toBe(false);
    expect(runNeedsNextSteps({ state: 'completed' }, [step({ status: 'completed', error_message: null })])).toBe(false);
  });
});

describe('a failed run', () => {
  it('says what happened in plain words and keeps the raw error behind a disclosure', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable());
    renderPanel({}, [step({ error_message: 'Prompt delivery is unverified: The harness did not confirm it' })]);

    expect((await screen.findByTestId('run-next-happened-7')).textContent).toBe(
      'Autopilot could not confirm that the agent received the prompt for the Send finish prompt step.',
    );
    const detail = screen.getByTestId('run-error-7');
    expect(detail.closest('details')?.open).toBe(false);
    expect(detail.textContent).toContain('The harness did not confirm it');
  });

  it('retries the failed step with one click, against the revision the person saw', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable());
    renderPanel();

    fireEvent.click(await screen.findByTestId('run-recover-retry-7'));

    await waitFor(() =>
      expect(recoverFailedCircuitRun).toHaveBeenCalledWith({
        run_id: 7,
        node_id: 'finish',
        attempt: 1,
        expected_revision: 41,
        action: 'retry',
        reason: '',
      }),
    );
  });

  it('refreshes what it offers after acting', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable());
    renderPanel();
    fireEvent.click(await screen.findByTestId('run-recover-retry-7'));
    await waitFor(() => expect(recoverFailedCircuitRun).toHaveBeenCalled());
    await waitFor(() => expect(vi.mocked(circuitRunAttention).mock.calls.length).toBeGreaterThan(1));
  });

  it('will not record that the person finished the step without a note saying what they did', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable());
    renderPanel();

    fireEvent.click(await screen.findByTestId('run-recover-continue-7'));
    const confirm = screen.getByTestId('run-recover-confirm-7') as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    expect(recoverFailedCircuitRun).not.toHaveBeenCalled();

    fireEvent.change(screen.getByTestId('run-recover-note-7'), {
      target: { value: 'Pasted the finish prompt into the terminal myself' },
    });
    expect(confirm.disabled).toBe(false);
    fireEvent.click(confirm);

    await waitFor(() =>
      expect(recoverFailedCircuitRun).toHaveBeenCalledWith({
        run_id: 7,
        node_id: 'finish',
        attempt: 1,
        expected_revision: 41,
        action: 'continue',
        reason: 'Pasted the finish prompt into the terminal myself',
      }),
    );
  });

  it('shows why an action is unavailable instead of a button that cannot work', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(
      recoverable({
        recovery: {
          node_id: 'finish',
          attempt: 1,
          status: 'cancelled',
          error: 'piloted agent node was closed',
          options: [
            { action: 'retry', available: false, unavailable_reason: 'The agent for this step was closed. Resume it from Archive, then retry.' },
            { action: 'continue', available: true },
          ],
        },
      }),
    );
    renderPanel({}, [step({ status: 'cancelled', error_message: 'piloted agent node was closed' })]);

    expect((await screen.findByTestId('run-next-todo-7')).textContent).toContain(
      'To retry: The agent for this step was closed. Resume it from Archive, then retry.',
    );
    expect(screen.queryByTestId('run-recover-retry-7')).toBeNull();
    expect(screen.getByTestId('run-recover-continue-7')).toBeTruthy();
    expect((await screen.findByTestId('run-next-happened-7')).textContent).toContain('agent it was working with was closed');
  });

  it('shows what the backend refused and keeps the note so it can be tried again', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable());
    vi.mocked(recoverFailedCircuitRun).mockRejectedValue(new Error('The run changed. Refresh it before acting.'));
    renderPanel();

    fireEvent.click(await screen.findByTestId('run-recover-continue-7'));
    fireEvent.change(screen.getByTestId('run-recover-note-7'), { target: { value: 'did it by hand' } });
    fireEvent.click(screen.getByTestId('run-recover-confirm-7'));

    expect((await screen.findByTestId('run-next-error-7')).textContent).toContain('The run changed');
    expect((screen.getByTestId('run-recover-note-7') as HTMLTextAreaElement).value).toBe('did it by hand');
  });

  it('still tells the person what to do when the backend has no recovery to offer', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable({ recovery: undefined }));
    renderPanel();

    expect((await screen.findByTestId('run-next-todo-7')).textContent).toContain('Open the agent node to see what happened.');
    expect(screen.queryByTestId('run-recover-retry-7')).toBeNull();
    expect(screen.queryByTestId('run-recover-continue-7')).toBeNull();
  });

  it('degrades to guidance without buttons when the backend answer is unusable', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue({ cmd: 'circuit_run_attention' } as unknown as CircuitRunAttention);
    renderPanel();

    expect((await screen.findByTestId('run-next-happened-7')).textContent).toContain('Send finish prompt');
    await waitFor(() => expect(circuitRunAttention).toHaveBeenCalled());
    expect(screen.queryByTestId('run-recover-retry-7')).toBeNull();
  });

  it('reloads when this run changes elsewhere, and ignores other runs', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable());
    renderPanel();
    await screen.findByTestId('run-recover-retry-7');
    expect(circuitRunAttention).toHaveBeenCalledTimes(1);

    await act(async () => { await emit('circuit-run-updated', { run_id: 99, state: 'running' }); });
    expect(circuitRunAttention).toHaveBeenCalledTimes(1);
    await act(async () => { await emit('circuit-run-updated', { run_id: 7, state: 'pending' }); });
    await waitFor(() => expect(circuitRunAttention).toHaveBeenCalledTimes(2));
  });
});

describe('a running run waiting on evidence', () => {
  const waiting = [step({ node_id: 'review_classifier', status: 'unverified', outcome: null, error_message: 'Waiting for a usable harness report: newer input follows the report.' })];
  const checkpointed = (): CircuitRunAttention => ({
    revision: 12,
    checkpoints: [{ node_id: 'review_classifier', attempt: 1, actions: ['recheck', 'completed'] }],
  });

  it('calls it a pause, not a failure, and offers the actions right on the card', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(checkpointed());
    renderPanel({ state: 'running' }, waiting);

    expect((await screen.findByTestId('run-next-happened-7')).textContent).toContain('This is a pause, not a failure.');
    expect(await screen.findByTestId('run-checkpoint-7-review_classifier-recheck')).toBeTruthy();
    expect(screen.getByTestId('run-checkpoint-7-review_classifier-completed')).toBeTruthy();
    // It is not shown as an error.
    expect(screen.queryByTestId('run-error-7')).toBeNull();
  });

  it('checking again needs no note, because it changes nothing', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(checkpointed());
    renderPanel({ state: 'running' }, waiting);

    fireEvent.click(await screen.findByTestId('run-checkpoint-7-review_classifier-recheck'));

    await waitFor(() =>
      expect(recordCircuitOutcome).toHaveBeenCalledWith({
        run_id: 7,
        node_id: 'review_classifier',
        attempt: 1,
        expected_revision: 12,
        action: 'recheck',
        reason: 'Checked again from the run card.',
      }),
    );
  });

  it('recording a result asks what you checked first, and sends the note', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(checkpointed());
    renderPanel({ state: 'running' }, waiting);

    // The box stays short until you act: no note field yet.
    expect(screen.queryByTestId('run-checkpoint-note-7')).toBeNull();
    fireEvent.click(await screen.findByTestId('run-checkpoint-7-review_classifier-completed'));
    const confirm = screen.getByTestId('run-checkpoint-confirm-7') as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    expect(recordCircuitOutcome).not.toHaveBeenCalled();
    fireEvent.change(screen.getByTestId('run-checkpoint-note-7'), { target: { value: 'Read the reviewer report; it is final.' } });
    expect(confirm.disabled).toBe(false);
    fireEvent.click(confirm);

    await waitFor(() =>
      expect(recordCircuitOutcome).toHaveBeenCalledWith({
        run_id: 7,
        node_id: 'review_classifier',
        attempt: 1,
        expected_revision: 12,
        action: 'completed',
        reason: 'Read the reviewer report; it is final.',
      }),
    );
  });
});
