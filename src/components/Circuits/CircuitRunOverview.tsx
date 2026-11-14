/**
 * CircuitRunOverview — the agent node card's Overview tab: where the Circuit
 * Run driving this card is, at a glance, and what it needs from a person.
 *
 * It reads top to bottom in the order a person asks:
 *
 *  1. **What is happening?** The run state, one plain sentence naming who is
 *     doing what ("Code reviewer is reviewing round 2"), elapsed time and the
 *     pull request, with Pause/Resume/Cancel beside it.
 *  2. **Does it need me?** Approvals, unverified checkpoints, failed-step
 *     recovery and moving a stuck step on, directly under the headline.
 *  3. **Where is it?** The stage tracker (Prepare → Review → Finish for a
 *     review circuit), and for a review its rounds and the latest verdict,
 *     whose full report opens on request.
 *  4. **Who is involved?** The agents the run has used, each one click away.
 *  5. **Details**, collapsed: the graph's visible steps in flow order (wiring
 *     stays hidden) with their times, agent, report and notes; the run
 *     history; the run id.
 *
 * Every action comes from the backend's own eligibility (`attention`), so a
 * button is offered only when the command will accept it, and it is sent
 * against the revision that was on screen.
 */

import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { CircuitRunOverview as Overview } from '../../types/generated/CircuitRunOverview';
import type { CircuitStepPush } from '../../types/generated/CircuitStepPush';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { StepPushOutcome } from '../../types/generated/StepPushOutcome';
import { approveCircuitStep, cancelCircuitRun, pauseCircuitRun, resumeCircuitRun } from '../../lib/tauri';
import { pushCircuitStep } from '../../lib/tauri/circuitEvidence';
import { formatError } from '../../lib/errorUtils';
import { useUIStore } from '../../stores/uiStore';
import { useMeshStore } from '../../stores/meshStore';
import { LiveRunDuration } from '../Probe/CircuitRunCard';
import { CircuitEvidenceHistory } from './CircuitEvidenceHistory';
import { continueCircuitReview, setCircuitReviewRoundLimit } from '../../lib/tauri/circuit';
import { ReviewExtension } from './ReviewExtension';
import { ReviewRoundLimit } from './ReviewRoundLimit';
import { RunNextSteps, runNeedsNextSteps } from './RunNextStepsPanel';
import { ReviewRoundsPanel, RunStageTracker, SectionHeading } from './RunProgressSummary';
import { formatDurationMs, ledgerTimestampMs, statusTextClass, stepDurationMs } from './circuitGraphModel';
import { runStateLabel, stepStatusLabel } from './circuitVocabulary';
import { canContinueReview, reviewCircuitMetadata, reviewResult } from './runDiagnostics';
import { formatWallClock, runStartedMs } from './runIdentity';
import { lastRoundGuidance } from './runNextSteps';
import { distinctRailLabels, pushOutcomeLabel, railRows, type RailRow, type RailState } from './runOverviewModel';
import { reviewRounds, runAgents, runHeadline, runStages, type RunAgent } from './runProgressModel';
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

/** Translucent status chip (DESIGN.md "Pills, badges, chips"), keyed by run state. */
const STATE_PILL: Record<string, string> = {
  running: 'bg-accent-cyan/10 text-accent-cyan ring-accent-cyan/30',
  paused: 'bg-status-warning/10 text-status-warning ring-status-warning/30',
  completed: 'bg-status-success/10 text-status-success ring-status-success/30',
  failed: 'bg-status-error/10 text-status-error ring-status-error/30',
};
const NEUTRAL_PILL = 'bg-text-muted/10 text-text-secondary ring-border-strong';

const PR_URL = /^https:\/\/github\.com\/[^/]+\/[^/]+\/pull\/\d+$/;

const GHOST_BUTTON = 'inline-flex h-6 items-center rounded-md border border-border-strong px-2 text-meta text-text-secondary hover:bg-bg-card-hover hover:text-text-primary disabled:opacity-40';

export function CircuitRunOverview({ overview, error, onRefresh, onChanged, onOpenAgent, agentName }: CircuitRunOverviewProps) {
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const [stepsOpen, setStepsOpen] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [toggled, setToggled] = useState<Record<string, boolean>>({});
  const stepsId = useId();

  const run = overview?.run;
  const currentRunId = useRef<number | null>(null);
  useEffect(() => {
    currentRunId.current = run?.id ?? null;
    return () => { currentRunId.current = null; };
  }, [run?.id]);
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
      <div className="flex-1 bg-bg-card p-3 text-meta text-text-muted" data-testid="run-overview-loading">
        {error === null ? 'Loading the circuit run…' : <span className="flex flex-wrap items-center gap-2">
          <span role="alert" className="min-w-0 break-words text-status-error">{error}</span>
          <button type="button" onClick={onRefresh} className="min-h-6 text-accent-cyan hover:underline">Retry</button>
        </span>}
      </div>
    );
  }

  const railLabels = distinctRailLabels(rows.map((row) => row.nodeId), (nodeId) => nodeRoleLabel(nodeId, kinds.get(nodeId)));
  const roleLabel = (nodeId: string) => railLabels.get(nodeId) ?? nodeRoleLabel(nodeId, kinds.get(nodeId));
  const plainRole = (nodeId: string) => nodeRoleLabel(nodeId, kinds.get(nodeId));
  const openAgent = (nodeId: number) => {
    setActionError(null);
    Promise.resolve(onOpenAgent(nodeId)).catch((cause: unknown) => setActionError(formatError(cause)));
  };
  const review = reviewResult(overview, reviewCircuitMetadata({ graph_json: JSON.stringify(overview.graph) }), context);
  const rounds = reviewRounds(overview, context);
  const stages = runStages(overview.graph, rows, plainRole);
  const headline = runHeadline({ overview, rows, rounds, review, roleOf: plainRole, agentName });
  const agents = runAgents(overview, rows, plainRole);
  const roundLimit = overview.review_round_limit ?? null;
  const live = run.state === 'running' || run.state === 'paused';
  const blocked = live ? overview.steps.filter((step) => step.status === 'blocked') : [];
  const prUrl = PR_URL.test(context['pr.url'] ?? '') ? context['pr.url'] : null;
  const doneCount = rows.filter((row) => row.state === 'done').length;
  const started = runStartedMs(overview.steps);

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

  const limitControl = roundLimit !== null && (live || run.state === 'pending')
    ? <ReviewRoundLimit label="Round limit" value={roundLimit} busy={busy} onChange={(limit) => void act(() => setCircuitReviewRoundLimit(run.id, limit))} />
    : undefined;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto bg-bg-card text-meta" data-testid={`run-overview-${run.id}`} data-run-state={run.state}>
      <div className="flex max-w-3xl flex-col gap-4 p-3">
        {/* 1. What is happening */}
        <header className="space-y-1.5">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span data-testid="run-overview-state"
              className={`inline-flex items-center gap-1 rounded-md px-1.5 py-px text-micro font-semibold ring-1 ring-inset ${STATE_PILL[run.state] ?? NEUTRAL_PILL}`}>
              {run.state === 'running' && <span aria-hidden className="h-1.5 w-1.5 rounded-pill bg-accent-cyan animate-pulse" />}
              {runStateLabel(run.state)}
            </span>
            <span className="min-w-0 break-words text-text-secondary">{overview.circuit_name}</span>
            {live && (
              <span className="ml-auto flex flex-wrap items-center gap-1">
                {run.state === 'running' && (
                  <button type="button" disabled={busy} onClick={() => void act(() => pauseCircuitRun(run.id))} className={GHOST_BUTTON}>
                    Pause
                  </button>
                )}
                {run.state === 'paused' && (
                  <button type="button" disabled={busy} onClick={() => void act(() => resumeCircuitRun(run.id))}
                    className="inline-flex h-6 items-center rounded-md border border-accent-cyan bg-bg-selection px-2 text-meta font-medium text-text-primary hover:bg-bg-card-hover disabled:opacity-40">
                    Resume
                  </button>
                )}
                <button type="button" disabled={busy || confirmCancel} onClick={() => setConfirmCancel(true)}
                  className="inline-flex h-6 items-center rounded-md px-2 text-meta text-status-error hover:bg-status-error/10 disabled:opacity-40">
                  Cancel…
                </button>
              </span>
            )}
          </div>
          <p className="text-label font-semibold text-text-primary break-words" data-testid="run-overview-headline" data-tone={headline.tone}>
            {headline.text}
          </p>
          <p className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-text-secondary">
            <span className="inline-flex flex-wrap gap-x-2"><LiveRunDuration run={run} steps={overview.steps} compact /></span>
            {prUrl && (
              <a href={prUrl} onClick={(event) => { event.preventDefault(); void openUrl(prUrl); }}
                className="inline-flex min-h-6 items-center text-accent-cyan hover:underline">
                Pull request{context['pr.number'] ? ` #${context['pr.number']}` : ''}
              </a>
            )}
          </p>
          {review !== null && <p className="text-text-secondary break-words">{review.detail}</p>}
          {confirmCancel && (
            <div className="flex flex-wrap items-center gap-2 rounded-md border border-status-error/30 bg-status-error/5 p-2">
              <span className="text-text-primary">Cancel this run? Its agents are retired and it cannot be resumed.</span>
              <span className="flex gap-1">
                <button type="button" disabled={busy} data-testid="run-overview-cancel-confirm"
                  onClick={() => { setConfirmCancel(false); void act(() => cancelCircuitRun(run.id)); }}
                  className="inline-flex h-6 items-center rounded-md bg-status-error/15 px-2 text-meta font-medium text-status-error hover:bg-status-error/25 disabled:opacity-40">
                  Cancel run
                </button>
                <button type="button" autoFocus onClick={() => setConfirmCancel(false)} className={GHOST_BUTTON}>
                  Keep it
                </button>
              </span>
            </div>
          )}
          {error !== null && <div className="flex flex-wrap items-center gap-2">
            <p role="alert" className="min-w-0 break-words text-status-error">Could not refresh: {error}</p>
            <button type="button" onClick={onRefresh} className="min-h-6 text-accent-cyan hover:underline">Retry</button>
          </div>}
          {actionError !== null && <p role="alert" className="text-status-error break-words">{actionError}</p>}
        </header>

        {/* 2. Anything that needs a person */}
        {canContinueReview(overview) && (
          <ReviewExtension key={`extension-${run.id}`} runId={run.id} busy={busy}
            onContinue={(additionalRounds) => void act(async () => {
              const selectedMeshId = useMeshStore.getState().selectedMeshId;
              const nextRunId = await continueCircuitReview(run.id, additionalRounds);
              if (nextRunId !== run.id && currentRunId.current === run.id
                && useMeshStore.getState().selectedMeshId === selectedMeshId) {
                useUIStore.getState().focusCircuitRun(nextRunId);
              }
            })} />
        )}
        {(blocked.length > 0 || runNeedsNextSteps(run, overview.steps) || overview.attention.pushes.length > 0) && (
          <div className="space-y-2">
            {blocked.map((step) => (
              <section key={step.node_id} aria-label="Approval needed" data-testid={`run-overview-approval-${step.node_id}`}
                className="rounded-md border border-status-warning/40 bg-status-warning/5 p-2">
                <p className="font-semibold text-text-primary">
                  {kinds.get(step.node_id)?.type === 'collaborator_check' && blocked.length === 1
                    ? 'This run needs your approval to continue.'
                    : `${roleLabel(step.node_id)} needs your approval to continue.`}
                </p>
                {step.error_message !== null && step.error_message.trim() !== '' && (
                  <p className="mt-1 whitespace-pre-wrap break-words text-text-secondary">{step.error_message}</p>
                )}
                <button type="button" disabled={busy || run.state !== 'running'}
                  onClick={() => void act(() => approveCircuitStep(run.id, step.node_id))}
                  className="mt-2 inline-flex h-6 items-center rounded-md bg-status-warning/25 px-2 font-semibold text-status-warning hover:bg-status-warning/40 disabled:opacity-40">
                  Approve
                </button>
              </section>
            ))}
            {runNeedsNextSteps(run, overview.steps) && (
              <div className="-mx-3"><RunNextSteps run={run} steps={overview.steps} roleLabel={roleLabel} busy={busy} /></div>
            )}
            {overview.attention.pushes.map((push) => {
              const kind = kinds.get(push.node_id);
              if (kind === undefined) return null;
              const control = (
                <StepPushControl push={push} kind={kind} revision={overview.attention.revision} runId={run.id}
                  role={overview.attention.pushes.length > 1 ? roleLabel(push.node_id) : null}
                  onPushed={() => { onChanged(); onRefresh(); }} onRefused={onRefresh} />
              );
              return push.overrides !== null ? <div key={push.node_id}>{control}</div> : (
                <ManualPush key={push.node_id} role={overview.attention.pushes.length > 1 ? roleLabel(push.node_id) : 'this step'}>{control}</ManualPush>
              );
            })}
          </div>
        )}

        {/* 3. Where the run is */}
        {stages.length > 0 && (
          <RunStageTracker stages={stages}
            detail={rounds === null ? undefined : { review: `Round ${rounds.current}${rounds.limit === null ? '' : ` of ${rounds.limit}`}` }} />
        )}
        {rounds !== null ? <ReviewRoundsPanel key={overview.run.id} rounds={rounds} control={limitControl} overview={overview} /> : limitControl}

        {/* 4. Who is involved */}
        {agents.length > 0 && <AgentList agents={agents} agentName={agentName} onOpen={openAgent} />}

        {/* 5. Details */}
        <div className="space-y-1 border-t border-border-subtle pt-2">
          <button type="button" onClick={() => setStepsOpen(!stepsOpen)} aria-expanded={stepsOpen} aria-controls={stepsId}
            data-testid="run-overview-steps-toggle"
            className="flex min-h-6 w-full items-center gap-1.5 rounded-sm text-left text-text-secondary hover:text-text-primary">
            <span aria-hidden className={`w-3 text-center text-text-muted transition-transform ${stepsOpen ? 'rotate-90' : ''}`}>▸</span>
            <span className="font-medium">All steps</span>
            <span className="text-text-muted">{doneCount} of {rows.length} done</span>
          </button>
          <div id={stepsId} hidden={!stepsOpen}>
            {stepsOpen && (
              <ol aria-label="Circuit steps" className="ml-1 flex flex-col" data-testid={`run-overview-rail-${run.id}`}>
                {rows.map((row) => {
                  const expanded = toggled[row.nodeId] ?? row.state === 'current';
                  return (
                    <RailItem key={row.nodeId} row={row} label={roleLabel(row.nodeId)} context={context}
                      expanded={expanded} onToggle={() => setToggled((all) => ({ ...all, [row.nodeId]: !expanded }))}
                      runId={run.id} onOpenAgent={openAgent} agentName={agentName} />
                  );
                })}
              </ol>
            )}
          </div>
          <details onToggle={(event) => setHistoryOpen(event.currentTarget.open)}>
            <summary className="flex min-h-6 cursor-pointer list-none items-center gap-1.5 text-text-secondary hover:text-text-primary [&::-webkit-details-marker]:hidden">
              <span aria-hidden className={`w-3 text-center text-text-muted transition-transform ${historyOpen ? 'rotate-90' : ''}`}>▸</span>
              <span className="font-medium">Run history</span>
            </summary>
            {historyOpen && (
              <div className="mt-1">
                <CircuitEvidenceHistory runId={run.id} updatedAt={run.updated_at} nodeLabel={roleLabel} />
              </div>
            )}
          </details>
          <p className="pt-1 text-micro text-text-muted">
            Run <span className="font-mono">#{run.id}</span>
            {started !== null && ` · started ${formatWallClock(started, new Date())}`}
          </p>
        </div>
      </div>
    </div>
  );
}

function AgentList({ agents, agentName, onOpen }: {
  agents: readonly RunAgent[];
  agentName: (nodeId: number) => string | null;
  onOpen: (nodeId: number) => void;
}) {
  const headingId = useId();
  return (
    <section aria-labelledby={headingId} className="space-y-1" data-testid="run-overview-agents">
      <SectionHeading id={headingId}>Agents</SectionHeading>
      <ul className="space-y-0.5">
        {agents.map((agent) => {
          const name = agentName(agent.agentNodeId) ?? `Agent #${agent.agentNodeId}`;
          return (
            <li key={agent.agentNodeId} className="flex min-h-6 items-center gap-2" data-testid={`run-overview-agent-${agent.agentNodeId}`}>
              <span aria-hidden className={`h-2 w-2 shrink-0 rounded-pill ${agent.active ? 'bg-accent-cyan animate-pulse' : 'border border-border-strong'}`} />
              <span className="min-w-0 flex-1 break-words">
                <span className="font-mono text-text-primary">{name}</span>
                <span className="text-text-secondary"> · {agent.role}</span>
                {agent.active && <span className="text-accent-cyan"> · working now</span>}
              </span>
              <button type="button" onClick={() => onOpen(agent.agentNodeId)} aria-label={`Open ${name}`}
                className="inline-flex h-6 shrink-0 items-center rounded-md px-2 text-accent-cyan hover:bg-accent-cyan/10">
                Open
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

/** A step a person may move on although nothing is holding it: offered, not pressed on them. */
function ManualPush({ role, children }: { role: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    <div>
      <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} aria-controls={id}
        className="inline-flex min-h-6 items-center gap-1 text-text-secondary hover:text-text-primary">
        <span aria-hidden className={`w-3 text-center text-text-muted transition-transform ${open ? 'rotate-90' : ''}`}>▸</span>
        Already done? Move {role} on by hand
      </button>
      <div id={id} hidden={!open} className="mt-1">{open && children}</div>
    </div>
  );
}

interface RailItemProps {
  row: RailRow;
  label: string;
  context: Record<string, string>;
  expanded: boolean;
  onToggle: () => void;
  runId: number;
  onOpenAgent: (nodeId: number) => void;
  agentName: (nodeId: number) => string | null;
}

function RailItem({ row, label, context, expanded, onToggle, runId, onOpenAgent, agentName }: RailItemProps) {
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
        className="flex min-h-6 w-full flex-wrap items-baseline gap-x-1.5 rounded-sm py-0.5 text-left hover:bg-bg-card-hover disabled:cursor-default disabled:hover:bg-transparent">
        <span aria-hidden className={`w-3 shrink-0 text-center ${glyph.className}`}>{glyph.glyph}</span>
        <span className="sr-only">{glyph.label}:</span>
        <span className={`min-w-0 break-words ${row.state === 'remaining' || row.state === 'skipped' ? 'text-text-muted' : 'text-text-primary'}`}>
          {label}
        </span>
        {pass !== null && <span className="text-text-muted">· {pass}</span>}
        {verdict !== null ? (
          <span className={`font-semibold ${verdictTextClass(verdict.tone)}`}>{verdict.label}</span>
        ) : step !== null && row.state !== 'done' ? (
          <span className={statusTextClass(step.status)}>{stepStatusLabel(step.status)}</span>
        ) : row.state === 'skipped' ? (
          <span className="text-text-muted">Skipped</span>
        ) : null}
        {duration !== null && <span className="ml-auto text-text-muted">{formatDurationMs(duration)}</span>}
      </button>
      {hasDetail && expanded && (
        <div id={detailId} className="mb-1.5 ml-4 space-y-1 text-text-secondary">
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
function StepPushControl({ push, kind, role, revision, runId, onPushed, onRefused }: {
  push: CircuitStepPush;
  kind: CircuitNodeKind;
  /** Named only when several steps can be moved on and the reader must tell them apart. */
  role: string | null;
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
      className={`rounded-md border p-2 ${push.overrides !== null ? 'border-status-warning/30 bg-status-warning/5' : 'border-border-subtle bg-bg-base/60'}`}>
      {push.overrides !== null && (
        <p className="mb-1 break-words text-text-primary">
          {role !== null && <span className="font-semibold">{role} · </span>}Autopilot is waiting: {push.overrides}
        </p>
      )}
      <p className="mb-1.5 text-text-secondary">If you know better, say what happened and the run moves on:</p>
      {push.ends_review && !push.starts_next_review && (
        <p className="mb-1.5 text-text-secondary">{lastRoundGuidance(push.review_again)}</p>
      )}
      <div className="flex flex-wrap items-center gap-1">
        {push.outcomes.map((outcome) => (
          <button key={outcome} type="button" disabled={pending} aria-pressed={choice === outcome}
            data-testid={`run-overview-push-${push.node_id}-${outcome}`}
            onClick={() => setChoice(choice === outcome ? null : outcome)}
            className={`inline-flex h-6 items-center rounded-md px-2 disabled:opacity-40 ${choice === outcome ? 'bg-accent-cyan/30 text-text-primary' : 'bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25'}`}>
            {pushOutcomeLabel(kind, outcome, push.starts_next_review, push.ends_review)}
          </button>
        ))}
      </div>
      {choice !== null && (
        <div className="mt-2 space-y-1">
          {push.overrides !== null && (
            <p className="text-text-secondary">
              This overrides what Autopilot observed. If the agent is really still busy, the next step starts too early.
            </p>
          )}
          <label className="block text-text-secondary">
            Note <span className="text-text-muted">(optional, saved in the run history)</span>
            <textarea value={note} onChange={(event) => setNote(event.target.value)} disabled={pending} rows={2}
              data-testid={`run-overview-push-note-${push.node_id}`}
              className="mt-0.5 block w-full min-w-0 rounded-sm border border-border-subtle bg-bg-input p-1 text-text-primary" />
          </label>
          <button type="button" disabled={pending} onClick={() => void confirm(choice)}
            data-testid={`run-overview-push-confirm-${push.node_id}`}
            className="inline-flex h-6 items-center rounded-md border border-accent-cyan bg-bg-selection px-2 font-medium text-text-primary hover:bg-bg-card-hover disabled:opacity-40">
            {pending ? 'Moving on…' : `Move on: ${pushOutcomeLabel(kind, choice, push.starts_next_review, push.ends_review)}`}
          </button>
        </div>
      )}
      {error !== null && <p role="alert" className="mt-1 break-words text-status-error">{error}</p>}
    </section>
  );
}
