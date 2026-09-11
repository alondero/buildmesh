/**
 * CircuitRunCard — one circuit run rendered as a readable diagnostic
 * (issue #1468), reorganised for at-a-glance scanning and review clarity.
 *
 * The card answers three questions, in order:
 *
 *   1. **What happened?** Run id, the review verdict or run state, and the
 *      duration — the headline. For a review circuit this says
 *      "Review approved" / "Review limit reached" outright, so a user never
 *      has to infer whether the run ended well or merely stopped.
 *   2. **Why?** The activity line names the step the run is on and, when it
 *      is parked, which budget holds it.
 *   3. **Which agent node?** The linked-agent line names the Agent Node the
 *      run drives and links straight to it.
 *
 * Everything else — the dedupe trigger identity, step progress, per-step
 * durations and raw circuit node ids — is provenance, and lives behind the
 * disclosure. The expanded timeline reads as
 * `Review · pass 2 · CHANGES REQUESTED` rather than
 * `review_classifier:completed · attempt 2`, and a review step with a
 * retained report offers it behind its own toggle.
 *
 * Live and failed diagnostics default to expanded; terminal runs are
 * collapsed until the user asks for their detail. Errors remain visible in
 * the card headline even when a terminal card is collapsed.
 * `expanded` is lifted to the parent so a `circuit-run-updated` refetch
 * can't reset a card the user deliberately opened or closed.
 *
 * Every layout choice here is wrap-first, never truncate-first — the card
 * has to stay readable at `PROBE_PANEL_BOUNDS.MIN_WIDTH` (240px).
 */

import { useMemo } from 'react';
import type { CircuitRunDetail } from '../../lib/tauri';
import {
  formatDurationMs,
  runDurationMs,
  statusTextClass,
  stepDurationMs,
} from '../Circuits/circuitGraphModel';
import {
  activityStatusToken,
  isRunStale,
  runActivity,
  reviewResult,
  runStateLabel,
  runStaleMs,
  runStepProgress,
  stepStatusLabel,
  type CircuitCapacity,
  type ReviewCircuitMetadata,
} from '../Circuits/runDiagnostics';
import {
  linkedAgentNodeId,
  nodeRoleLabel,
  parseRunContext,
  reviewReport,
  stepPassLabel,
  stepVerdict,
  verdictTextClass,
  type NodeIndex,
} from '../Circuits/runStepPresentation';

interface CircuitRunCardProps {
  detail: CircuitRunDetail;
  capacity: CircuitCapacity;
  expanded: boolean;
  onToggleExpanded: () => void;
  /** Clock for duration/age labels — injected so tests can pin it. */
  now: Date;
  busy: boolean;
  onPause: () => void;
  onResume: () => void;
  onCancel: () => void;
  onApprove: (nodeId: string) => void;
  reviewCircuit: ReviewCircuitMetadata | null;
  /** Circuit `node_id` → blueprint node, for role/verdict labels. */
  nodeIndex: NodeIndex;
  /** Resolve an Agent Node's display name, or `null` when it no longer exists. */
  agentName: (nodeId: number) => string | null;
  /** Focus (and solo) an Agent Node — the card's link into the canvas. */
  onFocusAgent: (nodeId: number) => void;
  /** True for a beat after the user linked here from that agent node. */
  focused?: boolean;
}

export function CircuitRunCard({
  detail,
  capacity,
  expanded,
  onToggleExpanded,
  now,
  busy,
  onPause,
  onResume,
  onCancel,
  onApprove,
  reviewCircuit,
  nodeIndex,
  agentName,
  onFocusAgent,
  focused = false,
}: CircuitRunCardProps) {
  const { run, steps } = detail;
  const activity = runActivity(run, steps, capacity);
  const review = reviewResult(detail, reviewCircuit);
  const progress = runStepProgress(steps);
  const duration = runDurationMs(run, now);
  const stale = isRunStale(run, now);
  const staleMs = runStaleMs(run, now);
  const blockedSteps = steps.filter((s) => s.status === 'blocked');
  const retried = steps.filter((s) => s.attempt > 1);
  // The run row carries no error column; the ledger's first errored step
  // is the run's failure reason.
  const firstError = steps.find((s) => s.error_message !== null && s.error_message !== '') ?? null;
  // Parsing a potentially large `context_json` (issue/PR bodies, prompts) is
  // memoised: the duration clock re-renders a live card every second and must
  // not re-parse the blob each tick.
  const context = useMemo(() => parseRunContext(run.context_json), [run.context_json]);
  const linkedAgent = linkedAgentNodeId(run, steps);
  const linkedAgentLabel = linkedAgent === null ? null : agentName(linkedAgent);

  const panelId = `run-detail-${run.id}`;

  return (
    <li
      className={`rounded-md border bg-bg-card/40 ${
        focused ? 'border-accent-cyan ring-1 ring-accent-cyan/50' : 'border-border-subtle'
      }`}
      data-testid={`run-card-${run.id}`}
      data-run-state={run.state}
      data-run-focused={focused ? 'true' : undefined}
    >
      {/* Headline row. The whole row is the disclosure control so the hit
          target stays comfortable at narrow widths; the buttons below sit
          outside it so they don't inherit the toggle. */}
      <button
        type="button"
        onClick={onToggleExpanded}
        aria-expanded={expanded}
        aria-controls={panelId}
        data-testid={`run-toggle-${run.id}`}
        className="w-full text-left px-2 py-1.5 rounded-md hover:bg-bg-card-hover focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent-cyan"
      >
        <span className="flex items-baseline gap-1.5 flex-wrap">
          <span
            aria-hidden
            className={`text-text-muted text-2xs w-3 shrink-0 text-center transition-transform ${
              expanded ? 'rotate-90' : ''
            }`}
          >
            ▸
          </span>
          <span className="text-2xs font-mono text-text-muted shrink-0">#{run.id}</span>
          <span
            className={`text-xs ${statusTextClass(review?.needsAttention ? 'failed' : run.state)} ${
              run.state === 'running' ? 'animate-pulse' : ''
            }`}
            data-testid={`run-state-${run.id}`}
          >
            {review?.label ?? runStateLabel(run.state)}
          </span>
          {duration !== null && (
            <span className="text-2xs text-text-muted shrink-0">
              {formatDurationMs(duration)}
            </span>
          )}
          {stale && staleMs !== null && (
            <span
              className="text-2xs text-status-warning shrink-0"
              data-testid={`run-stale-${run.id}`}
              title="No state transition for a while — the worker watchdog also watches quiet turns. Cancel if this never moves."
            >
              No update in {formatDurationMs(staleMs)}
            </span>
          )}
        </span>
        {/* Activity line — the fact the old one-liner buried. Wraps
            rather than clips: a long node id is the whole point. */}
        <span
          className="mt-0.5 flex items-baseline gap-1 flex-wrap text-2xs"
          data-testid={`run-activity-${run.id}`}
        >
          <span className={statusTextClass(review?.needsAttention ? 'failed' : activityStatusToken(activity.kind, run.state))}>
            {review?.needsAttention ? 'Needs attention' : activity.label}
          </span>
          {activity.nodeId !== null && (
            <span className="font-mono text-text-secondary break-words min-w-0">
              {activity.nodeId}
            </span>
          )}
        </span>
        {activity.detail !== null && (
          <span
            className="mt-0.5 block text-2xs text-text-muted break-words"
            data-testid={`run-reason-${run.id}`}
          >
            {activity.detail}
          </span>
        )}
      </button>

      {review && <p className="px-2 pb-1.5 text-2xs text-text-secondary break-words">{review.detail}</p>}

      {/* The Agent Node this run drives — the answer to "which node is this
          run about?". Outside the disclosure button (no nested buttons), and
          always visible because it is the link back to the canvas. */}
      {linkedAgent !== null && (
        <div
          className="px-2 pb-1.5 flex items-baseline gap-1.5 flex-wrap text-2xs"
          data-testid={`run-agent-${run.id}`}
        >
          <span className="text-text-muted">Agent node:</span>
          <span className="font-mono text-text-secondary break-words min-w-0">
            {linkedAgentLabel ?? `#${linkedAgent}`}
          </span>
          {linkedAgentLabel !== null && (
            <button
              type="button"
              onClick={() => onFocusAgent(linkedAgent)}
              data-testid={`run-agent-open-${run.id}`}
              title="Show this Agent Node on the canvas"
              className="px-1 rounded-md text-accent-cyan hover:bg-accent-cyan/10"
            >
              Open
            </button>
          )}
        </div>
      )}

      {/* Controls. Outside the disclosure button — nesting a button inside
          a button is invalid HTML and breaks keyboard semantics. */}
      {(run.state === 'running' ||
        run.state === 'paused' ||
        blockedSteps.length > 0) && (
        <div className="px-2 pb-1.5 flex items-center gap-1 flex-wrap">
          {run.state === 'running' && (
            <button
              type="button"
              onClick={onPause}
              disabled={busy}
              data-testid={`run-pause-${run.id}`}
              className="px-1.5 py-0.5 text-2xs rounded-md bg-text-muted/10 text-text-muted hover:text-text-primary disabled:opacity-40"
            >
              Pause
            </button>
          )}
          {run.state === 'paused' && (
            <button
              type="button"
              onClick={onResume}
              disabled={busy}
              data-testid={`run-resume-${run.id}`}
              className="px-1.5 py-0.5 text-2xs rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40"
            >
              Resume
            </button>
          )}
          {(run.state === 'running' || run.state === 'paused') && (
            <button
              type="button"
              onClick={onCancel}
              disabled={busy}
              data-testid={`run-cancel-${run.id}`}
              className="px-1.5 py-0.5 text-2xs rounded-md text-status-error hover:bg-status-error/10 disabled:opacity-40"
            >
              Cancel
            </button>
          )}
          {blockedSteps.map((s) => (
            <span
              key={s.node_id}
              className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded-md bg-status-warning/15 text-status-warning text-2xs min-w-0"
              data-testid={`blocked-badge-${run.id}-${s.node_id}`}
            >
              <span className="font-mono break-words min-w-0">{s.node_id}</span>
              <button
                type="button"
                onClick={() => onApprove(s.node_id)}
                disabled={busy}
                aria-label={`Approve ${s.node_id} on run ${run.id}`}
                data-testid={`approve-${run.id}-${s.node_id}`}
                className="px-1 rounded-md bg-status-warning/25 hover:bg-status-warning/40 font-semibold shrink-0 disabled:opacity-40"
              >
                Approve
              </button>
            </span>
          ))}
        </div>
      )}

      {/* Collapsed runs still surface the failure — an error you have to
          expand to find is an error you miss. */}
      {firstError !== null && (
        <p
          className="px-2 pb-1.5 text-2xs text-status-error line-clamp-2 break-words"
          data-testid={`run-error-${run.id}`}
        >
          ⚠ {firstError.error_message}
        </p>
      )}

      {expanded && (
        <div id={panelId} className="px-2 pb-2 border-t border-border-subtle pt-1.5">
          {/* Provenance behind the disclosure: trigger identity (dedupe key)
              and progress. Neither answers "what happened / why", so neither
              costs a headline line. */}
          <div className="flex items-baseline gap-1.5 flex-wrap mb-1.5">
            {progress.total > 0 && (
              <span
                className="text-2xs text-text-muted shrink-0"
                data-testid={`run-progress-${run.id}`}
              >
                {progress.finished}/{progress.total} steps
              </span>
            )}
            <span
              className="text-2xs text-text-muted break-all min-w-0"
              data-testid={`run-trigger-${run.id}`}
              title={run.trigger_identity}
            >
              {run.trigger_identity}
            </span>
          </div>

          {retried.length > 0 && (
            <p className="text-2xs text-status-warning mb-1.5" data-testid={`run-retries-${run.id}`}>
              {retried.length === 1
                ? `1 step was retried (${retried[0].node_id}, pass ${retried[0].attempt}).`
                : `${retried.length} steps were retried.`}
            </p>
          )}

          {steps.length === 0 ? (
            <p className="text-2xs text-text-muted">No steps recorded yet.</p>
          ) : (
            // The timeline. One row per step, vertical so length costs
            // height (which the body scrolls) rather than width (which it
            // must not).
            <ol className="flex flex-col gap-1" data-testid={`run-steps-${run.id}`}>
              {steps.map((s) => {
                const stepDuration = stepDurationMs(s);
                const kind = nodeIndex.get(s.node_id)?.type;
                const verdict = stepVerdict(s, kind, context);
                const pass = stepPassLabel(s);
                const report = reviewReport(s, kind, context);
                const stepAgent = s.agent_node_id;
                const stepAgentLabel = stepAgent === null ? null : agentName(stepAgent);
                return (
                  <li
                    key={s.node_id}
                    className="rounded-sm border border-border-subtle bg-bg-card p-1.5 text-2xs"
                    data-testid={`run-step-${run.id}-${s.node_id}`}
                    data-step-status={s.status}
                  >
                    <div className="flex items-baseline gap-1.5 flex-wrap">
                      <span className="text-text-primary break-words min-w-0">
                        {nodeRoleLabel(s.node_id, kind)}
                      </span>
                      {pass !== null && <span className="text-text-muted shrink-0">· {pass}</span>}
                      {verdict !== null ? (
                        <span
                          className={`${verdictTextClass(verdict.tone)} font-semibold shrink-0`}
                          data-testid={`run-step-verdict-${run.id}-${s.node_id}`}
                        >
                          {verdict.label}
                        </span>
                      ) : (
                        <>
                          <span
                            className={`${statusTextClass(s.status)} ${
                              s.status === 'running' ? 'animate-pulse' : ''
                            } shrink-0`}
                          >
                            {stepStatusLabel(s.status)}
                          </span>
                          {/* Gate steps finish `completed` but carry the real
                              verdict (`green`/`red`/`working`) in `outcome` —
                              that's the branch the run took. Kept only when no
                              dedicated verdict label applies. */}
                          {s.outcome !== null && s.outcome !== s.status && (
                            <span className="text-text-secondary shrink-0">· {s.outcome}</span>
                          )}
                        </>
                      )}
                    </div>
                    <div className="mt-0.5 flex items-baseline gap-1.5 flex-wrap text-text-muted">
                      <span className="font-mono break-words min-w-0">{s.node_id}</span>
                      {stepDuration !== null && (
                        <span className="shrink-0">· {formatDurationMs(stepDuration)}</span>
                      )}
                      {stepAgent !== null && (
                        <span className="shrink-0">
                          · agent{' '}
                          <span className="font-mono text-text-secondary">
                            {stepAgentLabel ?? `#${stepAgent}`}
                          </span>
                          {stepAgentLabel !== null && (
                            <button
                              type="button"
                              onClick={() => onFocusAgent(stepAgent)}
                              data-testid={`run-step-agent-open-${run.id}-${s.node_id}`}
                              className="ml-1 text-accent-cyan hover:underline"
                            >
                              Open
                            </button>
                          )}
                        </span>
                      )}
                    </div>
                    {report !== null && (
                      <details
                        className="mt-0.5"
                        data-testid={`run-review-report-${run.id}-${s.node_id}`}
                      >
                        <summary className="cursor-pointer text-accent-cyan">
                          View review report
                        </summary>
                        <pre className="mt-0.5 whitespace-pre-wrap break-words text-text-secondary font-mono">
                          {report}
                        </pre>
                      </details>
                    )}
                    {s.error_message !== null && s.error_message !== '' && (
                      // Per-step log surface. #1219 will widen this to
                      // successful steps' captured output; the wrapping
                      // and colour it needs are already here.
                      <pre className="mt-0.5 whitespace-pre-wrap break-words text-status-error font-mono">
                        {s.error_message}
                      </pre>
                    )}
                  </li>
                );
              })}
            </ol>
          )}
        </div>
      )}
    </li>
  );
}
