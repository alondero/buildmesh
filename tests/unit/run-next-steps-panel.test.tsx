import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { emit } from '@tauri-apps/api/event';
import { RunNextSteps, runNeedsNextSteps } from '../../src/components/Circuits/RunNextStepsPanel';
import {
  circuitRunAttention,
  circuitPromptPayload,
  recordCircuitOutcome,
  recoverFailedCircuitRun,
} from '../../src/lib/tauri/circuitEvidence';
import type { AutopilotCircuitRun } from '../../src/types/generated/AutopilotCircuitRun';
import type { AutopilotCircuitRunStep } from '../../src/types/generated/AutopilotCircuitRunStep';
import type { CircuitRunAttention } from '../../src/types/generated/CircuitRunAttention';

vi.mock('../../src/lib/tauri/circuitEvidence', () => ({
  circuitRunHistory: vi.fn(),
  circuitPromptPayload: vi.fn(),
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
  attention_dismissed_at: null,
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
  vi.mocked(circuitPromptPayload).mockReset();
});

describe('manual prompt delivery', () => {
  function manualPanel() {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable({ recovery: null, checkpoints: [{
      node_id: 'send', attempt: 2, actions: ['manual_delivered', 'recheck'],
      starts_next_review: false, ends_review: false, review_again: false,
    }] }));
    return renderPanel({ state: 'running' }, [step({ node_id: 'send', attempt: 2, status: 'unverified' })]);
  }

  it('copies the full prepared payload and confirms manual delivery separately', async () => {
    const text = 'Full prepared payload\r\nUnicode: café\n' + 'details '.repeat(1000);
    vi.mocked(circuitPromptPayload).mockResolvedValue(text);
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    manualPanel();
    fireEvent.click(await screen.findByRole('button', { name: 'Copy payload' }));
    await screen.findByRole('status');
    expect(writeText).toHaveBeenCalledWith(text);
    expect(circuitPromptPayload).toHaveBeenCalledWith({ run_id: 7, node_id: 'send', attempt: 2, expected_revision: 41 });
    expect(recordCircuitOutcome).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Sent manually' }));
    expect(recordCircuitOutcome).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Confirm: Sent manually' }));
    await waitFor(() => expect(recordCircuitOutcome).toHaveBeenCalledWith({
      run_id: 7, node_id: 'send', attempt: 2, expected_revision: 41, action: 'manual_delivered', reason: '',
    }));
  });

  it('shows retrieval and clipboard errors and permits retry', async () => {
    vi.mocked(circuitPromptPayload).mockRejectedValueOnce('The run changed').mockResolvedValue('payload');
    const writeText = vi.fn().mockRejectedValueOnce(new Error('Clipboard denied')).mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    manualPanel();
    const copy = await screen.findByRole('button', { name: 'Copy payload' });
    fireEvent.click(copy);
    expect((await screen.findByRole('alert')).textContent).toContain('The run changed');
    expect(writeText).not.toHaveBeenCalled();
    fireEvent.click(copy);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Clipboard denied'));
    fireEvent.click(copy);
    await screen.findByRole('status');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('keeps confirmation bound to the revision originally shown when the run refreshes', async () => {
    const view = manualPanel();
    fireEvent.click(await screen.findByRole('button', { name: 'Sent manually', exact: true }));
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable({ revision: 99, checkpoints: [{
      node_id: 'send', attempt: 2, actions: ['manual_delivered'], starts_next_review: false, ends_review: false, review_again: false,
    }] }));
    view.rerender(<RunNextSteps run={run({ state: 'running', updated_at: 'new' })} steps={[step({ node_id: 'send', attempt: 2, status: 'unverified' })]} roleLabel={roleLabel} busy={false} />);
    await waitFor(() => expect(circuitRunAttention).toHaveBeenCalledTimes(2));
    fireEvent.click(screen.getByRole('button', { name: 'Confirm: Sent manually' }));
    await waitFor(() => expect(recordCircuitOutcome).toHaveBeenCalledWith(expect.objectContaining({ action: 'manual_delivered', expected_revision: 41 })));
  });
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

  it('records a note when one is given, and never blocks the action on having one', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable());
    renderPanel();

    fireEvent.click(await screen.findByTestId('run-recover-continue-7'));
    const confirm = screen.getByTestId('run-recover-confirm-7') as HTMLButtonElement;
    // The person already said "I've done this" by pressing the button; making
    // them invent prose they have nothing useful to add is pure friction.
    expect(confirm.disabled).toBe(false);
    expect(recoverFailedCircuitRun).not.toHaveBeenCalled();

    fireEvent.change(screen.getByTestId('run-recover-note-7'), {
      target: { value: 'Pasted the finish prompt into the terminal myself' },
    });
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

  it('continues with an empty note rather than stranding the operator on a text box', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(recoverable());
    renderPanel();

    fireEvent.click(await screen.findByTestId('run-recover-continue-7'));
    fireEvent.change(screen.getByTestId('run-recover-note-7'), { target: { value: '   ' } });
    fireEvent.click(screen.getByTestId('run-recover-confirm-7'));

    await waitFor(() =>
      expect(recoverFailedCircuitRun).toHaveBeenCalledWith(
        expect.objectContaining({ action: 'continue', reason: '   ' }),
      ),
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

  it('names a review re-entry as the next review pass', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue({
      revision: 12,
      checkpoints: [{ node_id: 'await_fixes', attempt: 2, actions: ['recheck', 'completed'], starts_next_review: true }],
    });
    renderPanel({ state: 'running' }, [step({ node_id: 'await_fixes', status: 'unverified', attempt: 2, outcome: null, error_message: null })]);

    expect(await screen.findByRole('button', { name: 'Start the next review' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Check again' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Start the next review' }));
    expect(screen.getByRole('button', { name: 'Confirm: Start the next review' })).toBeTruthy();
  });

  it('names the last allowed round as recording the round, and points at Review again', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue({
      revision: 12,
      checkpoints: [{ node_id: 'await_fixes', attempt: 2, actions: ['recheck', 'completed'], starts_next_review: false, ends_review: true, review_again: true }],
    });
    renderPanel({ state: 'running' }, [step({ node_id: 'await_fixes', status: 'unverified', attempt: 2, outcome: null, error_message: null })]);

    expect(await screen.findByRole('button', { name: 'Record the last round' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Check again' })).toBeTruthy();
    expect(screen.getByText('Recording this stops the review. Review again adds one more round after the run stops.')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Record the last round' }));
    expect(screen.getByRole('button', { name: 'Confirm: Record the last round' })).toBeTruthy();
  });

  it('points a custom last round at manual recovery instead of Review again', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue({
      revision: 12,
      checkpoints: [{ node_id: 'await_fixes', attempt: 2, actions: ['recheck', 'completed'], starts_next_review: false, ends_review: true, review_again: false }],
    });
    renderPanel({ state: 'running' }, [step({ node_id: 'await_fixes', status: 'unverified', attempt: 2, outcome: null, error_message: null })]);

    expect(await screen.findByRole('button', { name: 'Record the last round' })).toBeTruthy();
    expect(screen.getByText('Recording this stops the review. This custom circuit needs manual recovery. Open its implementation agent and inspect the failed step.')).toBeTruthy();
    expect(screen.queryByText('Recording this stops the review. Review again adds one more round after the run stops.')).toBeNull();
  });

  it('recording a result offers an optional note box, and sends it when given', async () => {
    vi.mocked(circuitRunAttention).mockResolvedValue(checkpointed());
    renderPanel({ state: 'running' }, waiting);

    // The box stays short until you act: no note field yet.
    expect(screen.queryByTestId('run-checkpoint-note-7')).toBeNull();
    fireEvent.click(await screen.findByTestId('run-checkpoint-7-review_classifier-completed'));
    const confirm = screen.getByTestId('run-checkpoint-confirm-7') as HTMLButtonElement;
    // A note annotates the history when given; it never gates the record.
    expect(confirm.disabled).toBe(false);
    expect(recordCircuitOutcome).not.toHaveBeenCalled();
    fireEvent.change(screen.getByTestId('run-checkpoint-note-7'), { target: { value: 'Read the reviewer report; it is final.' } });
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

it('records an explicit human review verdict only after evidence is supplied', async () => {
  vi.mocked(circuitRunAttention).mockResolvedValue(recoverable({
    checkpoints: [{ node_id: 'verdict', attempt: 2, actions: ['recheck', 'review_pass', 'review_fail'] }],
  }));
  renderPanel({ state: 'running' }, [step({ node_id: 'verdict', status: 'unverified', attempt: 2 })]);
  fireEvent.click(await screen.findByRole('button', { name: 'Review passed' }));
  const confirm = screen.getByRole('button', { name: 'Confirm: Review passed' });
  expect(confirm.hasAttribute('disabled')).toBe(true);
  fireEvent.change(screen.getByTestId('run-checkpoint-note-7'), { target: { value: 'Inspected the diff and passing checks' } });
  fireEvent.click(confirm);
  await waitFor(() => expect(recordCircuitOutcome).toHaveBeenCalledWith({
    run_id: 7, node_id: 'verdict', attempt: 2, expected_revision: 41, action: 'review_pass',
    reason: 'Inspected the diff and passing checks',
  }));
});
