/**
 * CircuitRunOverview — the agent node card's Overview tab: where the Circuit
 * Run driving this card is, at a glance, and what it needs from a person.
 *
 * Top to bottom: the run headline and its controls; anything waiting on a
 * person (approvals, unverified checkpoints, failed-step recovery); the step
 * rail, which lays the run's graph out in flow order with each step done,
 * current, remaining or skipped; and the run history behind a disclosure.
 * A step the run is stuck on can be moved on from its rail row: the person
 * says what the outcome was, overriding stale evidence (`push_circuit_step`).
 *
 * Every action comes from the backend's own eligibility (`attention`), so a
 * button is offered only when the command will accept it, and it is sent
 * against the revision that was on screen.
 */

import { useMemo, useState } from 'react';
import type { CircuitRunOverview as Overview } from '../../types/generated/CircuitRunOverview';
import type { CircuitStepPush } from '../../types/generated/CircuitStepPush';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { StepPushOutcome } from '../../types/generated/StepPushOutcome';
import { approveCircuitStep, cancelCircuitRun, pauseCircuitRun, resumeCircuitRun } from '../../lib/tauri';
import { pushCircuitStep } from '../../lib/tauri/circuitEvidence';
import { formatError } from '../../lib/errorUtils';
import { LiveRunDuration } from '../Probe/CircuitRunCard';
import { CircuitEvidenceHistory } from './CircuitEvidenceHistory';
import { setCircuitReviewRoundLimit } from '../../lib/tauri/circuit';
import { ReviewRoundLimit, reviewRoundLabel } from './ReviewRoundLimit';
import { RunNextSteps, runNeedsNextSteps } from './RunNextStepsPanel';
import { formatDurationMs, ledgerTimestampMs, statusTextClass, stepDurationMs } from './circuitGraphModel';
import { runStateLabel, stepStatusLabel } from './circuitVocabulary';
import { reviewCircuitMetadata, reviewResult } from './runDiagnostics';
import { formatWallClock } from './runIdentity';
import { lastRoundGuidance } from './runNextSteps';
import { distinctRailLabels, pushOutcomeLabel, railRows, type RailRow, type RailState } from './runOverviewModel';
import { nodeRoleLabel, parseRunContext, reviewReport, stepPassLabel, stepVerdict, verdictTextClass } from './runStepPresentation';

interface CircuitRunOverviewProps {
  overview: Overview | null;
  error: string | null;
  onRefresh: () => void;
  /** Called after this panel changed the run, so related state can catch up. */
  onChanged: () => void;
  /** Focus (or reopen) an agent; a rejection is shown on the panel. */
  onOpenAgent: (nodeId: number) => void | Promise<void>;
  agentName: (nodeId: number) => string | null;
}

const GLYPHS: Record<RailState, { glyph: string; className: string; label: string }> = {
  done: { glyph: '✓', className: 'text-status-success', label: 'Done' },
  current: { glyph: '●', className: 'text-accent-cyan animate-pulse', label: 'Now' },
  failed: { glyph: '✕', className: 'text-status-error', label: 'Failed' },
  remaining: { glyph: '○', className: 'text-text-muted', label: 'Not started' },
  skipped: { glyph: '–', className: 'text-text-muted', label: 'Skipped' },
};

export function CircuitRunOverview({ overview, error, onRefresh, onChanged, onOpenAgent, agentName }: CircuitRunOverviewProps) {
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [toggled, setToggled] = useState<Record<string, boolean>>({});

  const run = overview?.run;
  const context = useMemo(() => parseRunContext(run?.context_json ?? '{}'), [run?.context_json]);
  const kinds = useMemo(
    () => new Map((overview?.graph.nodes ?? []).map((node) => [node.id, node.type] as const)),
    [overview?.graph.nodes],
  );
  const rows = useMemo(
    () => (overview ? railRows(overview.graph, overview.steps, overview.run.state) : []),
    [overview],
  );

  if (overview === null || run === undefined) {
    return (
      <div className="flex-1 bg-bg-card p-3 text-xs text-text-muted" data-testid="run-overview-loading">
        {error === null ? 'Loading the circuit run…' : <span className="flex flex-wrap items-center gap-2">
          <span role="alert" className="min-w-0 break-words text-status-error">{error}</span>
          <button type="button" onClick={onRefresh} className="min-h-6 text-accent-cyan hover:underline">Retry</button>
        </span>}
      </div>
    );
  }

  const railLabels = distinctRailLabels(rows.map((row) => row.nodeId), (nodeId) => nodeRoleLabel(nodeId, kinds.get(nodeId)));
  const roleLabel = (nodeId: string) => railLabels.get(nodeId) ?? nodeRoleLabel(nodeId, kinds.get(nodeId));
  const openAgent = (nodeId: number) => {
    setActionError(null);
    Promise.resolve(onOpenAgent(nodeId)).catch((cause: unknown) => setActionError(formatError(cause)));
  };
  const detail = { run, steps: overview.steps };
  const review = reviewResult(detail, reviewCircuitMetadata({ graph_json: JSON.stringify(overview.graph) }), context);
  const roundLimit = overview.review_round_limit ?? null;
  const blocked = run.state === 'running' || run.state === 'paused'
    ? overview.steps.filter((step) => step.status === 'blocked')
    : [];
  const pushes = new Map(overview.attention.pushes.map((push) => [`${push.node_id}:${push.attempt}`, push] as const));
  const live = run.state === 'running' || run.state === 'paused';

  async function act(work: () => Promise<unknown>) {
    setBusy(true);
    setActionError(null);
    try {
      await work();
      onChanged();
    } catch (cause) {
      setActionError(formatError(cause));
    } finally {
      setBusy(false);
      onRefresh();
    }
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto bg-bg-card p-2 text-xs" data-testid={`run-overview-${run.id}`} data-run-state={run.state}>
      <header className="mb-2 flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <span className="font-semibold text-text-primary break-words min-w-0">{overview.circuit_name}</span>
        <span className={`${statusTextClass(review?.needsAttention ? 'failed' : run.state)}`} data-testid="run-overview-state">
          {review?.label ?? runStateLabel(run.state)}
        </span>
        {context['retry.attempt'] && context['retry.max_retries'] && (
          <span className="text-2xs text-text-muted">{reviewRoundLabel(context)}</span>
        )}
        <span className="inline-flex flex-wrap gap-x-2 text-2xs text-text-muted"><LiveRunDuration run={run} steps={overview.steps} compact /></span>
        <span className="text-2xs font-mono text-text-muted">#{run.id}</span>
      </header>

      {roundLimit !== null && (live || run.state === 'pending') && (
        <div className="mb-2">
          <ReviewRoundLimit value={roundLimit} busy={busy} onChange={(limit) => void act(() => setCircuitReviewRoundLimit(run.id, limit))} />
        </div>
      )}

      {review !== null && <p className="mb-2 text-2xs text-text-secondary break-words">{review.detail}</p>}
      {error !== null && <div className="mb-2 flex flex-wrap items-center gap-2 text-2xs">
        <p role="alert" className="min-w-0 break-words text-status-error">Could not refresh: {error}</p>
        <button type="button" onClick={onRefresh} className="min-h-6 text-accent-cyan hover:underline">Retry</button>
      </div>}

      {live && (
        <div className="mb-2 flex flex-wrap items-center gap-1">
          {run.state === 'running' && (
            <button type="button" disabled={busy} onClick={() => void act(() => pauseCircuitRun(run.id))}
              className="rounded-md bg-text-muted/10 px-1.5 py-0.5 text-2xs text-text-secondary hover:text-text-primary disabled:opacity-40">
              Pause
            </button>
          )}
          {run.state === 'paused' && (
            <button type="button" disabled={busy} onClick={() => void act(() => resumeCircuitRun(run.id))}
              className="rounded-md bg-accent-cyan/15 px-1.5 py-0.5 text-2xs text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40">
              Resume
            </button>
          )}
          {confirmCancel ? (
            <span className="inline-flex flex-wrap items-center gap-1 text-2xs">
              <span className="text-status-error">Cancel this run?</span>
              <button type="button" disabled={busy} data-testid="run-overview-cancel-confirm"
                onClick={() => { setConfirmCancel(false); void act(() => cancelCircuitRun(run.id)); }}
                className="rounded-md bg-status-error/15 px-1.5 py-0.5 text-status-error hover:bg-status-error/25 disabled:opacity-40">
                Cancel run
              </button>
              <button type="button" onClick={() => setConfirmCancel(false)} className="rounded-md px-1.5 py-0.5 text-text-muted hover:text-text-primary">
                Keep it
              </button>
            </span>
          ) : (
            <button type="button" disabled={busy} onClick={() => setConfirmCancel(true)}
              className="rounded-md px-1.5 py-0.5 text-2xs text-status-error hover:bg-status-error/10 disabled:opacity-40">
              Cancel…
            </button>
          )}
        </div>
      )}

      {blocked.map((step) => (
        <section key={step.node_id} aria-label="Approval needed" data-testid={`run-overview-approval-${step.node_id}`}
          className="mb-2 rounded-md border border-status-warning/40 bg-status-warning/5 p-2 text-2xs">
          <p className="font-semibold text-text-primary">{roleLabel(step.node_id)} needs your approval to continue.</p>
          <button type="button" disabled={busy || run.state !== 'running'}
            onClick={() => void act(() => approveCircuitStep(run.id, step.node_id))}
            className="mt-1 rounded-md bg-status-warning/25 px-1.5 py-1 font-semibold text-status-warning hover:bg-status-warning/40 disabled:opacity-40">
            Approve
          </button>
        </section>
      ))}

      {runNeedsNextSteps(run, overview.steps) && (
        <div className="-mx-2"><RunNextSteps run={run} steps={overview.steps} roleLabel={roleLabel} busy={busy} /></div>
      )}

      {actionError !== null && <p role="alert" className="mb-2 text-2xs text-status-error break-words">{actionError}</p>}

      <ol aria-label="Circuit steps" className="flex flex-col" data-testid={`run-overview-rail-${run.id}`}>
        {rows.map((row) => {
          const expanded = toggled[row.nodeId] ?? row.state === 'current';
          const push = row.step === null ? undefined : pushes.get(`${row.nodeId}:${row.step.attempt}`);
          return (
            <RailItem key={row.nodeId} row={row} label={roleLabel(row.nodeId)} context={context}
              expanded={expanded} onToggle={() => setToggled((all) => ({ ...all, [row.nodeId]: !expanded }))}
              push={push} revision={overview.attention.revision} runId={run.id}
              onPushed={() => { onChanged(); onRefresh(); }} onRefused={onRefresh}
              onOpenAgent={openAgent} agentName={agentName} />
          );
        })}
      </ol>

      <details className="mt-2" onToggle={(event) => setHistoryOpen(event.currentTarget.open)}>
        <summary className="cursor-pointer text-2xs text-text-secondary">Run history</summary>
        {historyOpen && (
          <div className="mt-1">
            <CircuitEvidenceHistory runId={run.id} updatedAt={run.updated_at} nodeLabel={roleLabel} />
          </div>
        )}
      </details>
    </div>
  );
}

interface RailItemProps {
  row: RailRow;
  label: string;
  context: Record<string, string>;
  expanded: boolean;
  onToggle: () => void;
  push: CircuitStepPush | undefined;
  revision: number;
  runId: number;
  onPushed: () => void;
  /** A refused push usually means the run moved on: reread it for the new revision. */
  onRefused: () => void;
  onOpenAgent: (nodeId: number) => void;
  agentName: (nodeId: number) => string | null;
}

function RailItem({ row, label, context, expanded, onToggle, push, revision, runId, onPushed, onRefused, onOpenAgent, agentName }: RailItemProps) {
  const { step } = row;
  const glyph = GLYPHS[row.state];
  const verdict = step === null ? null : stepVerdict(step, row.kind, context);
  const pass = step === null ? null : stepPassLabel(step);
  const duration = step === null ? null : stepDurationMs(step);
  const report = step === null ? null : reviewReport(step, row.kind, context);
  const detailId = `rail-detail-${runId}-${row.nodeId}`;
  const hasDetail = step !== null;
  return (
    <li data-testid={`rail-row-${row.nodeId}`} data-rail-state={row.state} data-step-status={step?.status}
      className={`border-l-2 pl-2 ${row.state === 'current' ? 'border-accent-cyan' : 'border-border-subtle'}`}>
      <button type="button" onClick={onToggle} disabled={!hasDetail} aria-expanded={hasDetail ? expanded : undefined}
        aria-controls={hasDetail ? detailId : undefined}
        className="flex w-full flex-wrap items-baseline gap-x-1.5 rounded-sm py-1 text-left hover:bg-bg-card-hover disabled:cursor-default disabled:hover:bg-transparent">
        <span aria-hidden className={`w-3 shrink-0 text-center ${glyph.className}`}>{glyph.glyph}</span>
        <span className="sr-only">{glyph.label}:</span>
        <span className={`min-w-0 break-words ${row.state === 'remaining' || row.state === 'skipped' ? 'text-text-muted' : 'text-text-primary'}`}>
          {label}
        </span>
        {pass !== null && <span className="text-2xs text-text-muted">· {pass}</span>}
        {verdict !== null ? (
          <span className={`text-2xs font-semibold ${verdictTextClass(verdict.tone)}`}>{verdict.label}</span>
        ) : step !== null && row.state !== 'done' ? (
          <span className={`text-2xs ${statusTextClass(step.status)}`}>{stepStatusLabel(step.status)}</span>
        ) : row.state === 'skipped' ? (
          <span className="text-2xs text-text-muted">Skipped</span>
        ) : null}
        {duration !== null && <span className="ml-auto text-2xs text-text-muted">{formatDurationMs(duration)}</span>}
      </button>
      {hasDetail && expanded && (
        <div id={detailId} className="mb-1.5 ml-4 space-y-1 text-2xs text-text-secondary">
          <StepTimes started={step.started_at} completed={step.completed_at} />
          {step.outcome !== null && step.outcome !== step.status && verdict === null && (
            <p>Outcome: <span className="text-text-primary">{step.outcome}</span></p>
          )}
          {step.agent_node_id !== null && (
            <p>
              Agent <span className="font-mono">{agentName(step.agent_node_id) ?? `#${step.agent_node_id}`}</span>{' '}
              <button type="button" onClick={() => onOpenAgent(step.agent_node_id!)} className="text-accent-cyan hover:underline">
                Open
              </button>
            </p>
          )}
          {report !== null && (
            <details>
              <summary className="cursor-pointer text-accent-cyan">
                {context[`node.${step.node_id}.review_verdict_source`] === 'operator' ? 'View human review note' : 'View review report'}
              </summary>
              <pre className="mt-0.5 whitespace-pre-wrap break-words font-mono">{report}</pre>
            </details>
          )}
          {step.error_message !== null && step.error_message.trim() !== '' && (
            <pre className={`whitespace-pre-wrap break-words font-mono ${step.status === 'completed' ? 'text-text-secondary' : step.status === 'failed' ? 'text-status-error' : 'text-status-warning'}`}>
              {step.error_message}
            </pre>
          )}
          {push !== undefined && (
            <StepPushControl push={push} kind={row.kind} revision={revision} runId={runId} onPushed={onPushed} onRefused={onRefused} />
          )}
        </div>
      )}
    </li>
  );
}

function StepTimes({ started, completed }: { started: string | null; completed: string | null }) {
  const now = new Date();
  const startedMs = started === null ? Number.NaN : ledgerTimestampMs(started);
  if (!Number.isFinite(startedMs)) return null;
  const completedMs = completed === null ? Number.NaN : ledgerTimestampMs(completed);
  return (
    <p className="text-text-muted">
      Started {formatWallClock(startedMs, now)}
      {Number.isFinite(completedMs) ? ` · finished ${formatWallClock(completedMs, now)}` : ''}
    </p>
  );
}

/**
 * Move a stuck step on. Choosing an outcome opens a short confirmation with
 * an optional note; when stale evidence is holding the step, the confirmation
 * names it, because the push sets it aside.
 */
function StepPushControl({ push, kind, revision, runId, onPushed, onRefused }: {
  push: CircuitStepPush;
  kind: CircuitNodeKind;
  revision: number;
  runId: number;
  onPushed: () => void;
  onRefused: () => void;
}) {
  const [choice, setChoice] = useState<StepPushOutcome | null>(null);
  const [note, setNote] = useState('');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function confirm(outcome: StepPushOutcome) {
    setPending(true);
    setError(null);
    try {
      await pushCircuitStep({
        run_id: runId,
        node_id: push.node_id,
        attempt: push.attempt,
        expected_revision: revision,
        outcome,
        reason: note,
      });
      setChoice(null);
      setNote('');
      onPushed();
    } catch (cause) {
      setError(formatError(cause));
      onRefused();
    } finally {
      setPending(false);
    }
  }

  return (
    <section aria-label="Move this step on" data-testid={`run-overview-push-${push.node_id}`}
      className="rounded-md border border-border-subtle bg-bg-base/60 p-1.5">
      {push.overrides !== null && (
        <p className="mb-1 text-status-warning break-words">Autopilot is waiting: {push.overrides}</p>
      )}
      <p className="mb-1 text-text-muted">If you know better, say what happened and the run moves on:</p>
      {push.ends_review && !push.starts_next_review && (
        <p className="mb-1 text-text-secondary">{lastRoundGuidance(push.review_again)}</p>
      )}
      <div className="flex flex-wrap items-center gap-1">
        {push.outcomes.map((outcome) => (
          <button key={outcome} type="button" disabled={pending} aria-pressed={choice === outcome}
            data-testid={`run-overview-push-${push.node_id}-${outcome}`}
            onClick={() => setChoice(choice === outcome ? null : outcome)}
            className={`rounded-md px-1.5 py-1 disabled:opacity-40 ${choice === outcome ? 'bg-accent-cyan/30 text-text-primary' : 'bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25'}`}>
            {pushOutcomeLabel(kind, outcome, push.starts_next_review, push.ends_review)}
          </button>
        ))}
      </div>
      {choice !== null && (
        <div className="mt-1.5 space-y-1">
          {push.overrides !== null && (
            <p className="text-text-secondary">
              This overrides what Autopilot observed. If the agent is really still busy, the next step starts too early.
            </p>
          )}
          <label className="block text-text-secondary">
            Note <span className="text-text-muted">(optional, saved in the run history)</span>
            <textarea value={note} onChange={(event) => setNote(event.target.value)} disabled={pending} rows={2}
              data-testid={`run-overview-push-note-${push.node_id}`}
              className="mt-0.5 block w-full min-w-0 rounded-sm border border-border-subtle bg-bg-card p-1 text-text-primary" />
          </label>
          <button type="button" disabled={pending} onClick={() => void confirm(choice)}
            data-testid={`run-overview-push-confirm-${push.node_id}`}
            className="rounded-md bg-accent-cyan/15 px-1.5 py-1 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40">
            {pending ? 'Moving on…' : `Move on: ${pushOutcomeLabel(kind, choice, push.starts_next_review, push.ends_review)}`}
          </button>
        </div>
      )}
      {error !== null && <p role="alert" className="mt-1 break-words text-status-error">{error}</p>}
    </section>
  );
}
