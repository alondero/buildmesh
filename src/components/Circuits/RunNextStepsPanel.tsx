/**
 * RunNextStepsPanel — the answer to "it hit an error, what do I do?".
 *
 * Shown on a run card whenever the run needs a person: a failed run with a
 * failing step, or a running run with a step Autopilot cannot yet confirm.
 * It says in plain words what happened, what is safe to do, and offers the
 * actions that really advance the run:
 *
 *  - failed run: **Retry this step** or **I've done this — continue**, both
 *    reopening the run at the failed step (backend: `recover_failed_circuit_run`);
 *  - unverified step: check again, mark it done, say it did not happen, or try
 *    it again (backend: `record_circuit_outcome`).
 *
 * What is offered comes from the backend's own eligibility decision, so a button
 * is shown only when the command will accept it; when an action is unavailable the
 * reason says how to unblock it. Continuing is an attestation and needs a note.
 */

import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  circuitRunAttention,
  recordCircuitOutcome,
  recoverFailedCircuitRun,
} from '../../lib/tauri/circuitEvidence';
import type { AutopilotCircuitRun } from '../../types/generated/AutopilotCircuitRun';
import type { AutopilotCircuitRunStep } from '../../types/generated/AutopilotCircuitRunStep';
import type { CheckpointAction } from '../../types/generated/CheckpointAction';
import type { CircuitRunAttention } from '../../types/generated/CircuitRunAttention';
import { checkpointActionLabel, describeProblem, recoveryGuidance } from './runNextSteps';
import { runFailureStep } from './runStepPresentation';

/** A backend answer we can rely on, or `null` (a mock or a skewed build). */
function usable(value: unknown): CircuitRunAttention | null {
  const candidate = value as Partial<CircuitRunAttention> | null;
  return candidate !== null &&
    typeof candidate === 'object' &&
    typeof candidate.revision === 'number' &&
    Array.isArray(candidate.checkpoints)
    ? (candidate as CircuitRunAttention)
    : null;
}

/** Does this run currently need a person? Cheap, so the card can decide to mount. */
export function runNeedsNextSteps(
  run: Pick<AutopilotCircuitRun, 'state'>,
  steps: ReadonlyArray<Pick<AutopilotCircuitRunStep, 'status' | 'error_message'>>,
): boolean {
  if (run.state === 'failed') return runFailureStep(steps) !== null;
  return run.state === 'running' && steps.some((step) => step.status === 'unverified');
}

interface RunNextStepsProps {
  run: AutopilotCircuitRun;
  steps: ReadonlyArray<AutopilotCircuitRunStep>;
  /** A circuit node id in the card's own words (`Review`). */
  roleLabel: (nodeId: string) => string;
  /** Parent-level busy flag (another card action is in flight). */
  busy: boolean;
}

export function RunNextSteps({ run, steps, roleLabel, busy }: RunNextStepsProps) {
  const [attention, setAttention] = useState<CircuitRunAttention | null>(null);
  const [note, setNote] = useState('');
  const [noting, setNoting] = useState<string | null>(null);
  /** The recorded result awaiting its note: which step, which outcome. */
  const [recording, setRecording] = useState<{ nodeId: string; attempt: number; action: CheckpointAction } | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const failed = run.state === 'failed';

  useEffect(() => {
    let current = true;
    const subscription = listen<unknown>('circuit-run-updated', ({ payload }) => {
      if (
        current &&
        typeof payload === 'object' &&
        payload !== null &&
        'run_id' in payload &&
        payload.run_id === run.id
      ) {
        setRefresh((value) => value + 1);
      }
    });
    void subscription.catch(() => {});
    return () => {
      current = false;
      void subscription.then((unlisten) => unlisten(), () => {});
    };
  }, [run.id]);

  useEffect(() => {
    let current = true;
    circuitRunAttention(run.id).then(
      (next) => {
        if (current) setAttention(usable(next));
      },
      () => {
        if (current) setAttention(null);
      },
    );
    return () => {
      current = false;
    };
  }, [run.id, run.updated_at, refresh]);

  const failure = runFailureStep(steps);
  const recovery = attention?.recovery ?? null;
  const unverified = steps.filter((step) => step.status === 'unverified');
  const guidance = recoveryGuidance(recovery);
  const problemStep = failed ? failure : (unverified[0] ?? null);
  if (problemStep === null) return null;
  const problem = describeProblem({
    state: run.state,
    role: roleLabel(problemStep.node_id),
    status: problemStep.status,
    error: problemStep.error_message,
  });
  const disabled = busy || pending;

  async function act(work: () => Promise<void>) {
    setPending(true);
    setError(null);
    try {
      await work();
      setNote('');
      setNoting(null);
      setRecording(null);
      setRefresh((value) => value + 1);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setPending(false);
    }
  }

  const retry = () =>
    act(() =>
      recoverFailedCircuitRun({
        run_id: run.id,
        node_id: recovery!.node_id,
        attempt: recovery!.attempt,
        expected_revision: attention!.revision,
        action: 'retry',
        reason: note,
      }),
    );
  const proceed = () =>
    act(() =>
      recoverFailedCircuitRun({
        run_id: run.id,
        node_id: recovery!.node_id,
        attempt: recovery!.attempt,
        expected_revision: attention!.revision,
        action: 'continue',
        reason: note,
      }),
    );
  const record = (nodeId: string, attempt: number, action: CheckpointAction) =>
    act(() =>
      recordCircuitOutcome({
        run_id: run.id,
        node_id: nodeId,
        attempt,
        expected_revision: attention!.revision,
        action,
        // Checking again changes nothing, so it needs no justification.
        reason: note.trim() === '' && action === 'recheck' ? 'Checked again from the run card.' : note,
      }),
    );

  const frame = failed
    ? 'border-status-error/40 bg-status-error/5'
    : 'border-status-warning/40 bg-status-warning/5';

  return (
    <section
      className={`mx-2 mb-1.5 rounded-md border p-2 text-2xs ${frame}`}
      data-testid={`run-next-${run.id}`}
      aria-label="What to do next"
    >
      <p className="font-semibold text-text-primary" data-testid={`run-next-happened-${run.id}`}>
        {problem.happened}
      </p>
      {problem.detail !== null && (
        <details className="mt-0.5">
          <summary className="cursor-pointer text-text-muted">Technical detail</summary>
          <p
            className="mt-0.5 whitespace-pre-wrap break-words font-mono text-text-secondary"
            data-testid={failed ? `run-error-${run.id}` : `run-next-detail-${run.id}`}
          >
            {problem.detail}
          </p>
        </details>
      )}

      {failed && (
        <>
          <ul className="mt-1 list-disc pl-4 text-text-secondary" data-testid={`run-next-todo-${run.id}`}>
            {guidance.todo.map((line) => (
              <li key={line} className="break-words">
                {line}
              </li>
            ))}
          </ul>
          {recovery !== null && attention !== null && (
            <div className="mt-1.5 flex items-center gap-1.5 flex-wrap">
              {guidance.retry.available && (
                <button
                  type="button"
                  disabled={disabled}
                  onClick={() => void retry()}
                  data-testid={`run-recover-retry-${run.id}`}
                  className="px-1.5 py-1 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40"
                >
                  Retry this step
                </button>
              )}
              {guidance.continue.available && (
                <button
                  type="button"
                  disabled={disabled}
                  aria-expanded={noting === 'continue'}
                  onClick={() => setNoting(noting === 'continue' ? null : 'continue')}
                  data-testid={`run-recover-continue-${run.id}`}
                  className="px-1.5 py-1 rounded-md bg-text-muted/10 text-text-primary hover:bg-text-muted/20 disabled:opacity-40"
                >
                  I&apos;ve done this — continue
                </button>
              )}
            </div>
          )}
          {noting === 'continue' && (
            <div className="mt-1.5 space-y-1">
              <label className="block text-text-secondary">
                What did you do? This is recorded in the run history.
                <textarea
                  value={note}
                  onChange={(event) => setNote(event.target.value)}
                  disabled={disabled}
                  data-testid={`run-recover-note-${run.id}`}
                  rows={2}
                  className="mt-0.5 block w-full min-w-0 rounded-sm border border-border-subtle bg-bg-card p-1 text-text-primary"
                />
              </label>
              <button
                type="button"
                disabled={disabled || note.trim() === ''}
                onClick={() => void proceed()}
                data-testid={`run-recover-confirm-${run.id}`}
                className="px-1.5 py-1 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40"
              >
                Confirm and continue
              </button>
            </div>
          )}
        </>
      )}

      {!failed && attention !== null && (
        <div className="mt-1.5 space-y-1.5">
          {attention.checkpoints.map((checkpoint) => (
            <div key={`${checkpoint.node_id}:${checkpoint.attempt}`} data-testid={`run-checkpoint-${run.id}-${checkpoint.node_id}`}>
              <p className="text-text-secondary">{roleLabel(checkpoint.node_id)}</p>
              <div className="mt-0.5 flex items-center gap-1.5 flex-wrap">
                {checkpoint.actions.map((action) => {
                  // Checking again changes nothing and acts at once; recording a
                  // result is a statement, so it asks what you checked first.
                  const needsNote = action !== 'recheck';
                  const open =
                    recording?.nodeId === checkpoint.node_id &&
                    recording.attempt === checkpoint.attempt &&
                    recording.action === action;
                  return (
                    <button
                      key={action}
                      type="button"
                      disabled={disabled}
                      aria-expanded={needsNote ? open : undefined}
                      onClick={() =>
                        needsNote
                          ? setRecording(open ? null : { nodeId: checkpoint.node_id, attempt: checkpoint.attempt, action })
                          : void record(checkpoint.node_id, checkpoint.attempt, action)
                      }
                      data-testid={`run-checkpoint-${run.id}-${checkpoint.node_id}-${action}`}
                      className="px-1.5 py-1 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40"
                    >
                      {checkpointActionLabel(action)}
                    </button>
                  );
                })}
              </div>
            </div>
          ))}
          {recording !== null && (
            <div className="space-y-1">
              <label className="block text-text-secondary">
                What did you check? This is saved in the run history and does not grant approval.
                <textarea
                  value={note}
                  onChange={(event) => setNote(event.target.value)}
                  disabled={disabled}
                  data-testid={`run-checkpoint-note-${run.id}`}
                  rows={2}
                  className="mt-0.5 block w-full min-w-0 rounded-sm border border-border-subtle bg-bg-card p-1 text-text-primary"
                />
              </label>
              <button
                type="button"
                disabled={disabled || note.trim() === ''}
                onClick={() => void record(recording.nodeId, recording.attempt, recording.action)}
                data-testid={`run-checkpoint-confirm-${run.id}`}
                className="px-1.5 py-1 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40"
              >
                Confirm: {checkpointActionLabel(recording.action)}
              </button>
            </div>
          )}
          {attention.checkpoints.length === 0 && (
            <p className="text-text-muted">Autopilot is still re-checking on its own.</p>
          )}
        </div>
      )}

      {error !== null && (
        <p role="alert" className="mt-1 break-words text-status-error" data-testid={`run-next-error-${run.id}`}>
          {error}
        </p>
      )}
    </section>
  );
}
