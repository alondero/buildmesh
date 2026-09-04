/**
 * CircuitRunCard — one circuit run rendered as a readable diagnostic
 * (issue #1468).
 *
 * Replaces the old one-line ledger dump. The tab used to render an entire
 * run as `steps.map(s => `${s.node_id}:${s.status}`).join(' → ')` inside a
 * `truncate`d span, which meant:
 *   - the chain was clipped at the Probe's 240px minimum width, and the
 *     clipped tail is exactly where a stuck run's current step lives;
 *   - `pending_slot` was shown raw, with no hint that it means "every
 *     slot is busy";
 *   - `title` carried the *trigger identity* while the span's text was
 *     the *step chain*, so the tooltip explained the wrong thing.
 *
 * Shape
 * -----
 * A collapsed card answers "what is this run doing, and why?" in two
 * lines — run id, state, the active/queued step, its reason, duration and
 * progress. Expanding reveals the trigger identity and the full per-step
 * timeline (status, outcome, attempt, duration, agent node, error text).
 *
 * Runs that still need something default to expanded, terminal runs to
 * collapsed: the newest failing or parked run is the one the user opened
 * the tab for. `expanded` is lifted to the parent so a `circuit-run-updated`
 * refetch can't reset a card the user deliberately opened or closed.
 *
 * Every layout choice here is wrap-first, never truncate-first — the card
 * has to stay readable at `PROBE_PANEL_BOUNDS.MIN_WIDTH` (240px).
 */

import type { CircuitRunDetail } from '../../lib/tauri';
import {
  formatDurationMs,
  runDurationMs,
  statusTextClass,
  stepDurationMs,
} from '../Circuits/circuitGraphModel';
import {
  activityStatusToken,
  runActivity,
  runStateLabel,
  runStepProgress,
  stepStatusLabel,
  type CircuitCapacity,
} from '../Circuits/runDiagnostics';

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
}: CircuitRunCardProps) {
  const { run, steps } = detail;
  const activity = runActivity(run, steps, capacity);
  const progress = runStepProgress(steps);
  const duration = runDurationMs(run, now);
  const blockedSteps = steps.filter((s) => s.status === 'blocked');
  const retried = steps.filter((s) => s.attempt > 1);
  // The run row carries no error column; the ledger's first errored step
  // is the run's failure reason.
  const firstError = steps.find((s) => s.error_message !== null && s.error_message !== '') ?? null;

  const panelId = `run-detail-${run.id}`;

  return (
    <li
      className="rounded-md border border-border-subtle bg-bg-card/40"
      data-testid={`run-card-${run.id}`}
      data-run-state={run.state}
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
            className={`text-xs ${statusTextClass(run.state)} ${
              run.state === 'running' ? 'animate-pulse' : ''
            }`}
            data-testid={`run-state-${run.id}`}
          >
            {runStateLabel(run.state)}
          </span>
          {duration !== null && (
            <span className="text-2xs text-text-muted shrink-0">
              {formatDurationMs(duration)}
            </span>
          )}
          {progress.total > 0 && (
            <span
              className="text-2xs text-text-muted shrink-0"
              data-testid={`run-progress-${run.id}`}
            >
              {progress.finished}/{progress.total} steps
            </span>
          )}
        </span>
        {/* Activity line — the fact the old one-liner buried. Wraps
            rather than clips: a long node id is the whole point. */}
        <span
          className="mt-0.5 flex items-baseline gap-1 flex-wrap text-2xs"
          data-testid={`run-activity-${run.id}`}
        >
          <span className={statusTextClass(activityStatusToken(activity.kind, run.state))}>
            {activity.label}
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
      {!expanded && firstError !== null && (
        <p
          className="px-2 pb-1.5 text-2xs text-status-error line-clamp-2 break-words"
          data-testid={`run-error-${run.id}`}
        >
          ⚠ {firstError.error_message}
        </p>
      )}

      {expanded && (
        <div id={panelId} className="px-2 pb-2 border-t border-border-subtle pt-1.5">
          <dl className="text-2xs mb-1.5">
            <dt className="text-text-muted">Triggered by</dt>
            {/* `break-all`, not `break-words`: trigger identities are
                unspaced (`issue:1468:buildmesh:run`) so a word-boundary
                break has nowhere to land and would overflow instead. */}
            <dd
              className="font-mono text-text-secondary break-all"
              data-testid={`run-trigger-${run.id}`}
              title={run.trigger_identity}
            >
              {run.trigger_identity}
            </dd>
          </dl>

          {retried.length > 0 && (
            <p className="text-2xs text-status-warning mb-1.5" data-testid={`run-retries-${run.id}`}>
              {retried.length === 1
                ? `1 step was retried (${retried[0].node_id}, attempt ${retried[0].attempt}).`
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
                return (
                  <li
                    key={s.node_id}
                    className="rounded-sm border border-border-subtle bg-bg-card p-1.5 text-2xs"
                    data-testid={`run-step-${run.id}-${s.node_id}`}
                    data-step-status={s.status}
                  >
                    <div className="flex items-baseline gap-1.5 flex-wrap">
                      <span className="font-mono text-text-primary break-words min-w-0">
                        {s.node_id}
                      </span>
                      <span
                        className={`${statusTextClass(s.status)} ${
                          s.status === 'running' ? 'animate-pulse' : ''
                        } shrink-0`}
                      >
                        {stepStatusLabel(s.status)}
                      </span>
                      {/* Gate steps finish `completed` but carry the real
                          verdict (`green`/`red`/`working`) in `outcome` —
                          that's the branch the run took. */}
                      {s.outcome !== null && s.outcome !== s.status && (
                        <span className="text-text-secondary shrink-0">· {s.outcome}</span>
                      )}
                      {s.attempt > 1 && (
                        <span className="text-status-warning shrink-0">· attempt {s.attempt}</span>
                      )}
                      {stepDuration !== null && (
                        <span className="text-text-muted shrink-0">
                          · {formatDurationMs(stepDuration)}
                        </span>
                      )}
                    </div>
                    {s.agent_node_id !== null && (
                      <p className="text-text-muted mt-0.5">agent node #{s.agent_node_id}</p>
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
