import { useEffect, useRef, useState } from 'react';
import {
  approveCircuitStep,
  cancelCircuitRun,
  isAuthError,
  listCircuitRuns,
  recordCircuitOutcome,
  recoverCircuitRun,
} from '../api';
import type { NodeCircuitRun } from '../api';
import type { CheckpointAction } from '../../types/generated/CheckpointAction';
import type { NodeCircuitNeed } from '../../types/generated/NodeCircuitNeed';
import { checkpointActionLabel, checkpointActionNeedsEvidence, recoveryGuidance } from '../../components/Circuits/runNextSteps';
import { canCancel, circuitStatusLine, needHeading } from '../circuitRun';
import { Sheet } from '../ui';
import { useVisibilityPolling } from '../useVisibilityPolling';

/** The one decision a sheet is asking the person to confirm. */
type Decision =
  | { kind: 'approve'; need: NodeCircuitNeed }
  | { kind: 'record'; need: NodeCircuitNeed; action: CheckpointAction }
  | { kind: 'recover'; action: 'continue' }
  | { kind: 'cancel' };

const sameStep = (a: NodeCircuitNeed, b: NodeCircuitNeed) =>
  a.kind === b.kind && a.step.node_id === b.step.node_id && a.step.attempt === b.step.attempt;

/**
 * A sheet may only act on what is still true. The list refreshes under an open
 * sheet, and an approval has no revision guard of its own, so a decision whose
 * subject has moved on is dropped instead of being sent against a new state.
 */
function stillOffered(decision: Decision, run: NodeCircuitRun): boolean {
  switch (decision.kind) {
    case 'approve':
    case 'record':
      return run.needs_you.some((need) => sameStep(need, decision.need));
    case 'recover':
      return run.recovery?.options.some((option) => option.action === decision.action && option.available) ?? false;
    case 'cancel':
      return canCancel(run);
  }
}

function decisionTitle(decision: Decision): string {
  switch (decision.kind) {
    case 'approve':
      return `Approve the ${decision.need.step.role}?`;
    case 'record':
      return checkpointActionLabel(decision.action, decision.need.starts_next_review);
    case 'recover':
      return "I've done this — continue";
    case 'cancel':
      return 'Cancel this run?';
  }
}

/**
 * The node screen's Circuit Run: where the run is, and what it needs from you.
 *
 * Eligibility is the desktop's, delivered by the server: a button appears only
 * for an action the backend currently offers, and the backend still refuses a
 * stale or ineligible request. Renders nothing for a node with no live run.
 */
export function NodeCircuitRunPanel({
  nodeId,
  onAuthFailed,
  onOpenTerminal,
}: {
  nodeId: number;
  onAuthFailed: () => void;
  onOpenTerminal: () => void;
}) {
  const [run, setRun] = useState<NodeCircuitRun | null>(null);
  // An action that failed is an alert; a status read that failed is only a
  // note, because this panel is secondary to the node screen it sits on.
  const [error, setError] = useState('');
  const [readError, setReadError] = useState('');
  const [notice, setNotice] = useState('');
  const [decision, setDecision] = useState<Decision | null>(null);
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const active = useRef(true);
  const version = useRef(0);
  const acting = useRef(false);
  useEffect(() => {
    active.current = true;
    return () => { active.current = false; };
  }, []);

  async function refresh(isLatest: () => boolean = () => true, signal?: AbortSignal) {
    if (acting.current) return;
    const request = ++version.current;
    const owns = () => active.current && request === version.current && isLatest();
    try {
      // `signal` is the polling hook's dispose signal (issue #1576): an
      // unmounted panel cancels its own in-flight run read.
      const runs = await listCircuitRuns(signal);
      if (!owns()) return;
      setRun(runs.find((candidate) => candidate.node_id === nodeId) ?? null);
      setReadError('');
    } catch (e) {
      if (!owns()) return;
      if (isAuthError(e)) onAuthFailed();
      else setReadError((e as Error).message);
    }
  }
  useVisibilityPolling(refresh, 5000);

  async function act(work: () => Promise<void>, done: string) {
    if (acting.current) return;
    acting.current = true;
    // Anything fetched before this click describes the world we are changing.
    ++version.current;
    setBusy(true);
    setError('');
    setNotice('');
    try {
      await work();
      if (!active.current) return;
      setDecision(null);
      setNote('');
      setNotice(done);
    } catch (e) {
      if (!active.current) return;
      if (isAuthError(e)) onAuthFailed();
      else setError((e as Error).message);
    } finally {
      acting.current = false;
      if (active.current) {
        setBusy(false);
        void refresh();
      }
    }
  }

  const openDecision = run !== null && decision !== null && stillOffered(decision, run) ? decision : null;
  if (run === null) {
    // A cancelled run leaves the list, so its confirmation has to outlive the run.
    if (error) return <p role="alert" className="inline-error">{error}</p>;
    if (notice) return <p role="status" className="field-hint">{notice}</p>;
    return readError ? <p className="health-note">Circuit Run status is unavailable: {readError}</p> : null;
  }

  const record = (need: NodeCircuitNeed, action: CheckpointAction, reason: string) =>
    act(
      () =>
        recordCircuitOutcome({
          run_id: run.run_id,
          node_id: need.step.node_id,
          attempt: need.step.attempt,
          expected_revision: run.revision,
          action,
          reason,
        }),
      'Recorded. The run will carry on.',
    );
  const recover = (action: 'retry' | 'continue', reason: string) =>
    act(
      () =>
        recoverCircuitRun({
          run_id: run.run_id,
          node_id: run.recovery!.node_id,
          attempt: run.recovery!.attempt,
          expected_revision: run.revision,
          action,
          reason,
        }),
      action === 'retry' ? 'Retrying the step.' : 'Recorded. The run will carry on.',
    );
  const guidance = recoveryGuidance(run.recovery ?? null);

  function confirm(current: Decision) {
    switch (current.kind) {
      case 'approve':
        return act(() => approveCircuitStep(run!.run_id, current.need.step.node_id), 'Approved. The run will carry on.');
      case 'record':
        return record(current.need, current.action, note);
      case 'recover':
        return recover('continue', note);
      case 'cancel':
        return act(() => cancelCircuitRun(run!.run_id), 'Run cancelled.');
    }
  }

  const needsEvidence = openDecision?.kind === 'record' && checkpointActionNeedsEvidence(openDecision.action);

  return (
    <section className="circuit-run" aria-label="Circuit Run" data-testid="circuit-run-panel">
      <h2>Circuit Run</h2>
      <p data-testid="circuit-run-line">{circuitStatusLine(run)}</p>
      {run.review?.summary && (
        <details>
          <summary>Reviewer’s latest report</summary>
          <p className="circuit-report">{run.review.summary}</p>
        </details>
      )}
      {run.needs_you.map((need) => (
        <article className="circuit-need" key={`${need.kind}:${need.step.node_id}:${need.step.attempt}`} data-testid={`circuit-need-${need.kind}`}>
          <p><strong>{needHeading(need)}</strong></p>
          {need.detail && need.kind !== 'input' && (
            <details>
              <summary>Technical detail</summary>
              <p className="circuit-report">{need.detail}</p>
            </details>
          )}
          {need.kind === 'input' && (
            <>
              <p className="field-hint">{need.detail ?? 'The agent is waiting for an answer.'} Answer it in the terminal.</p>
              {(need.agent_node_id === null || need.agent_node_id === nodeId) && (
                <button className="btn-primary" onClick={onOpenTerminal}>Open terminal to answer</button>
              )}
            </>
          )}
          {need.kind === 'approval' && (
            <button className="btn-primary" disabled={busy} onClick={() => setDecision({ kind: 'approve', need })}>Approve…</button>
          )}
          {need.kind === 'unverified' && (
            <div className="action-grid">
              {need.actions.map((action) => (
                <button
                  key={action}
                  className="btn-ghost"
                  disabled={busy}
                  onClick={() =>
                    // Checking again changes nothing, so it needs no statement.
                    action === 'recheck'
                      ? void record(need, action, 'Checked again from the phone.')
                      : setDecision({ kind: 'record', need, action })
                  }
                >
                  {checkpointActionLabel(action, need.starts_next_review)}
                </button>
              ))}
            </div>
          )}
        </article>
      ))}
      {run.state === 'failed' && (
        <div className="circuit-need" data-testid="circuit-recovery">
          <ul>
            {guidance.todo.map((line) => <li key={line}>{line}</li>)}
          </ul>
          <div className="action-grid">
            {guidance.retry.available && (
              <button className="btn-primary" disabled={busy} onClick={() => void recover('retry', '')}>Retry this step</button>
            )}
            {guidance.continue.available && (
              <button className="btn-ghost" disabled={busy} onClick={() => setDecision({ kind: 'recover', action: 'continue' })}>
                I’ve done this — continue
              </button>
            )}
          </div>
        </div>
      )}
      {canCancel(run) && (
        <button className="btn-ghost" disabled={busy} onClick={() => setDecision({ kind: 'cancel' })}>Cancel run</button>
      )}
      {notice && <p role="status" className="field-hint">{notice}</p>}
      {error && !openDecision && <p role="alert" className="inline-error">{error}</p>}
      {readError && <p className="health-note">Could not refresh this run: {readError}</p>}
      {openDecision && (
        <Sheet label={decisionTitle(openDecision)} onClose={() => setDecision(null)} dismissible={!busy}>
          <h3>{decisionTitle(openDecision)}</h3>
          {openDecision.kind === 'cancel' && <p>The run stops and its helper agents are closed. Your implementation agent stays open.</p>}
          {openDecision.kind === 'approve' && <p>The run continues past this gate as soon as you confirm.</p>}
          {(openDecision.kind === 'record' || openDecision.kind === 'recover') && (
            <label>
              {needsEvidence ? 'Your decision and the evidence for it' : 'What did you do or check? (optional)'}
              <textarea className="field" rows={3} value={note} disabled={busy} onChange={(e) => setNote(e.target.value)} />
            </label>
          )}
          {error && <p role="alert" className="inline-error">{error}</p>}
          <div className="action-grid">
            <button className="btn-ghost" disabled={busy} onClick={() => setDecision(null)}>Back</button>
            <button
              className="btn-primary"
              disabled={busy || (needsEvidence && note.trim() === '')}
              onClick={() => void confirm(openDecision)}
            >
              {busy ? 'Sending…' : 'Confirm'}
            </button>
          </div>
        </Sheet>
      )}
    </section>
  );
}
