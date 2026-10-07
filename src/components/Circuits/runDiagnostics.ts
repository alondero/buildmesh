/**
 * runDiagnostics — the plain-English reading of a circuit run's ledger
 * (issues #1468 / #1475).
 *
 * Split out of `circuitGraphModel.ts` in review: that module is documented as
 * "pure helpers behind the canvas editor", and the wording rules below exist
 * for the Probe's run cards. Two unrelated reasons to edit one file is the
 * Divergent Change smell, so the policy/wording layer lives here while
 * `circuitGraphModel` keeps the graph AST plus the timestamp, duration and
 * colour primitives both surfaces share.
 *
 * It sits under `Circuits/` rather than `Probe/` on purpose: this is circuit
 * domain vocabulary, and the editor's run-history drawer is a second consumer.
 * Everything here is pure, so the copy is unit-testable without mounting a
 * component.
 */

import { isTerminalRunState, ledgerTimestampMs, parseGraph } from './circuitGraphModel';
import type { StepLike } from './circuitGraphModel';
import {
  indexNodes,
  parseRunContext,
  reviewPassCount,
  type NodeIndex,
} from './runStepPresentation';
import type { CircuitRunDetail } from '../../types/generated/CircuitRunDetail';
import type { CircuitWithRuns } from '../../types/generated/CircuitWithRuns';
import {
  isAdmittedRunState,
  isTerminalStepStatus,
  pendingRunBind,
  STEP_STATUS_QUEUED,
} from './circuitVocabulary';

export { runStateLabel, stepStatusLabel } from './circuitVocabulary';

export type CircuitProbeView = 'activity' | 'history' | 'queue' | 'manage';

export interface ReviewCircuitMetadata {
  verdictNodeId: string;
  retryNodeIds: readonly string[];
  /** The step that asks the implementation agent to merge after approval. */
  mergeNodeId?: string;
  supportsContinuation?: boolean;
}

const MERGE_HAND_OFF_NODE_ID = 'merge';
/** Steps the issue-review blueprint adds once the merge is checked on GitHub. */
const MERGE_VERIFIED_NODE_ID = 'close_implementer';
const MERGE_UNCONFIRMED_NODE_IDS = ['merge_unconfirmed', 'merge_blocked'];

/** Review semantics come from the persisted graph, not `is_preset`. */
export function reviewCircuitMetadata(
  circuit: Pick<CircuitWithRuns['circuit'], 'graph_json'>
): ReviewCircuitMetadata | null {
  return circuitGraphFacts(circuit).reviewCircuit;
}

/** Extensions use the stock review-only snapshot even for issue circuits. */
export function reviewCircuitForRun(detail: CircuitRunDetail, circuit: ReviewCircuitMetadata | null): ReviewCircuitMetadata | null {
  const context = parseRunContext(detail.run.context_json);
  return context['review.extended'] === '1' || context['recovery.from_run_id']
    ? { verdictNodeId: 'verdict', retryNodeIds: ['retry'], mergeNodeId: MERGE_HAND_OFF_NODE_ID, supportsContinuation: true }
    : circuit;
}

export interface CircuitGraphFacts {
  reviewCircuit: ReviewCircuitMetadata | null;
  /** Circuit `node_id` → blueprint node, for the run card's role/verdict
   *  labels. Parsed alongside the review metadata so each graph_json is
   *  read once per snapshot. */
  nodeIndex: NodeIndex;
}

/** Parse one persisted graph into everything the Probe row needs. */
export function circuitGraphFacts(
  circuit: Pick<CircuitWithRuns['circuit'], 'graph_json'>
): CircuitGraphFacts {
  try {
    const graph = parseGraph(circuit.graph_json);
    const verdict = graph.nodes.find((node) => node.type.type === 'review_verdict');
    const merge = graph.nodes.find((node) => node.id === MERGE_HAND_OFF_NODE_ID && node.type.type === 'inject_pty');
    return {
      reviewCircuit: verdict === undefined
        ? null
        : {
            verdictNodeId: verdict.id,
            ...(merge === undefined ? {} : { mergeNodeId: merge.id }),
            ...(graph.blueprint === 'issue_driven_autopilot_review' ? { supportsContinuation: true } : {}),
            retryNodeIds: graph.nodes
              .filter((node) => node.type.type === 'retry_limit')
              .map((node) => node.id),
          },
      nodeIndex: indexNodes(graph.nodes),
    };
  } catch {
    return { reviewCircuit: null, nodeIndex: new Map() };
  }
}

/** Historical completion meant the graph stopped, not that a review approved. */
export function reviewResult(detail: CircuitRunDetail, reviewCircuit: ReviewCircuitMetadata | null = null): { label: string; detail: string; needsAttention: boolean } | null {
  if (reviewCircuit === null || !isTerminalRunState(detail.run.state)) return null;
  const review = detail.steps.find((s) => s.node_id === reviewCircuit.verdictNodeId);
  if (!review) return null;
  const exhausted = detail.steps.some((s) =>
    reviewCircuit.retryNodeIds.includes(s.node_id) && s.outcome === 'failed');
  // The persisted ReviewVerdict step already carries the typed routing
  // outcome. Do not reconstruct approval from arbitrary context_json keys.
  const approved = detail.run.state === 'completed' && !exhausted && review.outcome === 'completed';
  // Which pass the recorded verdict belongs to, when the context still
  // carries it. Retention empties `context_json` on pruned runs, so this is
  // a flourish on the run-level summary, never a requirement.
  const pass = reviewPassCount(parseRunContext(detail.run.context_json), reviewCircuit.verdictNodeId);
  // Runs approved before the merge hand-off existed never delivered it.
  const mergeRequested = reviewCircuit.mergeNodeId !== undefined
    && detail.steps.some((s) => s.node_id === reviewCircuit.mergeNodeId && s.status === 'completed');
  const stepDone = (nodeId: string) => detail.steps.some((s) => s.node_id === nodeId && s.status === 'completed');
  // Approved is not merged: say which of the two happened when the run knows.
  if (approved && stepDone(MERGE_VERIFIED_NODE_ID)) {
    return {
      label: 'Review approved',
      detail: `${pass === null ? '' : `Approved on pass ${pass}. `}GitHub confirmed the squash-merge and the implementation agent was closed.`,
      needsAttention: false,
    };
  }
  if (approved && MERGE_UNCONFIRMED_NODE_IDS.some(stepDone)) {
    return {
      label: 'Merge needs attention',
      detail: `${pass === null ? '' : `Approved on pass ${pass}. `}The squash-merge was not confirmed on GitHub, so the implementation agent was left open. Open it, finish the merge, then close it yourself.`,
      needsAttention: true,
    };
  }
  return approved
    ? {
        label: 'Review approved',
        detail: `${pass === null ? '' : `Approved on pass ${pass}. `}${mergeRequested
          ? 'The implementation agent was asked to squash-merge the pull request and was handed back. Check its report to confirm the merge.'
          : 'Check the current PR head and required checks before merging.'}`,
        needsAttention: false,
      }
    : {
        label: exhausted ? 'Review limit reached' : 'Review needs attention',
        detail: `${exhausted && pass !== null ? `Ran out of review attempts after pass ${pass}. ` : ''}No final approval is recorded. ${canContinueReview(detail, reviewCircuit) ? 'Inspect the latest findings. If the work is ready, add one review round to this run on the same worktree. If the approach needs changing, open the implementation agent first.' : 'Open the implementation agent, or resume its saved session from Archive. Address the failed step and latest findings before starting a fresh review. If the session is unavailable, recover from the PR branch.'}`,
        needsAttention: true,
      };
}

export function canContinueReview(detail: CircuitRunDetail, reviewCircuit: ReviewCircuitMetadata | null): boolean {
  if (detail.run.state !== 'failed' || reviewCircuit === null) return false;
  const context = parseRunContext(detail.run.context_json);
  const supported = reviewCircuit.supportsContinuation || context['source.review_preset'] === '1' || Boolean(context['recovery.from_run_id']);
  return Boolean(supported && detail.steps.some((s) => s.node_id === reviewCircuit.verdictNodeId)
    && (detail.run.source_agent_node_id !== null || detail.steps.some((s) => s.node_id === 'implementer' && s.agent_node_id !== null)));
}

/** A run needs a user's attention before the circuit can make progress. */
export function runNeedsAttention(detail: CircuitRunDetail, reviewCircuit: ReviewCircuitMetadata | null = null): boolean {
  return detail.run.state === 'failed' || detail.run.state === 'paused' || reviewResult(detail, reviewCircuit)?.needsAttention === true ||
    detail.steps.some((step) => step.status === 'blocked' || step.status === 'unverified');
}

/**
 * Activity is strictly the actively-managed set: runs holding a
 * mesh circuit-run slot (`running` + `paused`). Mirrors
 * `db::count_active_circuit_runs` — `pending` lives in the queue,
 * terminal states live in History. Failures and review-attention
 * surface in History with `runNeedsAttention`, not here.
 */
export function isActiveRunState(state: string): boolean {
  return state === 'running' || state === 'paused';
}

/**
 * Activity includes only work still in flight (holding capacity).
 * Completed, failed and cancelled runs live in History — even when they
 * need attention. Pending runs live in the queue payload and are
 * excluded here so the same run never presents twice.
 */
export function runBelongsToActivity(detail: CircuitRunDetail): boolean {
  return isActiveRunState(detail.run.state);
}

export function runBelongsToHistory(detail: CircuitRunDetail): boolean {
  return isTerminalRunState(detail.run.state);
}

function timestampValue(value: string): number {
  const parsed = ledgerTimestampMs(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function compareRunsNewestFirst(a: CircuitRunDetail | null, b: CircuitRunDetail | null): number {
  if (a === null && b === null) return 0;
  if (a === null) return 1;
  if (b === null) return -1;
  return timestampValue(b.run.updated_at) - timestampValue(a.run.updated_at) ||
    timestampValue(b.run.created_at) - timestampValue(a.run.created_at) ||
    b.run.id - a.run.id;
}

export interface CircuitProbeRow extends CircuitWithRuns {
  visibleRuns: CircuitRunDetail[];
  hasAttention: boolean;
  reviewCircuit: ReviewCircuitMetadata | null;
  /** Circuit `node_id` → blueprint node, for the run card's role labels. */
  nodeIndex: NodeIndex;
}

/** Parse each persisted graph once per backend snapshot. */
export function annotateCircuitRows(rows: CircuitWithRuns[]): CircuitProbeRow[] {
  return rows.map((row) => {
    const { reviewCircuit, nodeIndex } = circuitGraphFacts(row.circuit);
    return {
      ...row,
      reviewCircuit,
      nodeIndex,
      visibleRuns: [],
      hasAttention: false,
    };
  });
}

/** Build the stable, view-specific row model used by the Probe.
 * History sorts attention-first within each circuit so failed /
 * needs-review rows surface above quiet completions; the component may
 * additionally filter by search text / attention-only. */
export function buildCircuitProbeRows(
  rows: Array<CircuitWithRuns | CircuitProbeRow>,
  view: CircuitProbeView,
  options: { attentionOnly?: boolean; search?: string } = {}
): CircuitProbeRow[] {
  const needle = (options.search ?? '').trim().toLowerCase();
  return rows
    .map((row) => {
      const facts = 'reviewCircuit' in row
        ? { reviewCircuit: row.reviewCircuit, nodeIndex: row.nodeIndex }
        : circuitGraphFacts(row.circuit);
      const reviewCircuit = facts.reviewCircuit;
      let visibleRuns = view === 'history'
        ? row.runs.filter(runBelongsToHistory)
        : view === 'activity'
          ? row.runs.filter((run) => runBelongsToActivity(run))
          : [];
      if (view === 'history') {
        if (options.attentionOnly) {
          visibleRuns = visibleRuns.filter((run) => runNeedsAttention(run, reviewCircuitForRun(run, reviewCircuit)));
        }
        if (needle !== '') {
          visibleRuns = visibleRuns.filter((run) =>
            run.run.trigger_identity.toLowerCase().includes(needle) ||
            String(run.run.id).includes(needle) ||
            row.circuit.name.toLowerCase().includes(needle)
          );
        }
        // Attention-first, then newest — so the 63-failure case reads as
        // actionable items on top, not an endless completed tail.
        visibleRuns.sort((a, b) =>
          Number(runNeedsAttention(b, reviewCircuitForRun(b, reviewCircuit))) - Number(runNeedsAttention(a, reviewCircuitForRun(a, reviewCircuit))) ||
          compareRunsNewestFirst(a, b));
      } else {
        visibleRuns.sort(compareRunsNewestFirst);
      }
      return {
        ...row,
        visibleRuns,
        hasAttention: visibleRuns.some((run) => runNeedsAttention(run, reviewCircuitForRun(run, reviewCircuit))),
        reviewCircuit,
        nodeIndex: facts.nodeIndex,
      };
    })
    .sort((a, b) => Number(b.hasAttention) - Number(a.hasAttention) ||
      compareRunsNewestFirst(a.visibleRuns[0] ?? null, b.visibleRuns[0] ?? null) ||
      a.circuit.name.localeCompare(b.circuit.name));
}

export interface CircuitActivityStats {
  /** Alias of activeCount kept for backwards compat — Activity IS the
   * active set now, so both numbers agree. Prefer activeCount. */
  activityCount: number;
  /** Runs holding a circuit-run slot (`running` + `paused`). */
  activeCount: number;
  /** All runs needing attention (live + terminal). Drives the header
   * status line. Do NOT use for the History-only filter label — a paused
   * run would make the History checkbox lie. */
  attentionCount: number;
  /** Terminal runs needing attention (failed / unapproved review).
   * Drives the History filter checkbox so the label always matches what
   * checking the box will show. */
  historyAttentionCount: number;
  /** Live runs needing attention (paused / blocked running). */
  activeAttentionCount: number;
  /** Terminal runs in the fetched History window. */
  historyCount: number;
  queuedCount: number;
}

/** Summarize the monitoring header without rebuilding arrays in the component. */
export function circuitActivityStats(
  rows: Array<CircuitWithRuns | CircuitProbeRow>,
  queuedCount: number
): CircuitActivityStats {
  let activeCount = 0;
  let attentionCount = 0;
  let historyAttentionCount = 0;
  let activeAttentionCount = 0;
  let historyCount = 0;
  for (const row of rows) {
    const { runs, circuit } = row;
    const reviewCircuit = 'reviewCircuit' in row
      ? row.reviewCircuit
      : reviewCircuitMetadata(circuit);
    for (const detail of runs) {
      if (runBelongsToActivity(detail)) {
        activeCount += 1;
        if (runNeedsAttention(detail, reviewCircuitForRun(detail, reviewCircuit))) {
          activeAttentionCount += 1;
          attentionCount += 1;
        }
      }
      if (runBelongsToHistory(detail)) {
        historyCount += 1;
        if (runNeedsAttention(detail, reviewCircuitForRun(detail, reviewCircuit))) {
          historyAttentionCount += 1;
          attentionCount += 1;
        }
      }
    }
  }
  return { activityCount: activeCount, activeCount, attentionCount, historyAttentionCount, activeAttentionCount, historyCount, queuedCount };
}

/** Quiet threshold before a live run reads as stalled. Matches the
 * worker's lost-turn watchdog window (60s) so the UI and the worker
 * agree on what "quiet" means. */
export const RUN_STALE_AFTER_MS = 60_000;

/** Ms since `updated_at` (SQLite zoneless timestamps read as UTC). Null
 * when the timestamp is unparseable or the run is terminal. */
export function runStaleMs(
  run: { state: string; updated_at: string },
  now: Date = new Date()
): number | null {
  if (isTerminalRunState(run.state)) return null;
  const updated = ledgerTimestampMs(run.updated_at);
  if (!Number.isFinite(updated)) return null;
  return Math.max(0, now.getTime() - updated);
}

/** True when a live run has not transitioned for longer than the stale
 * window — surfaced as a "No update in Xm" badge, not a failure. */
export function isRunStale(
  run: { state: string; updated_at: string },
  now: Date = new Date(),
  thresholdMs: number = RUN_STALE_AFTER_MS
): boolean {
  const stale = runStaleMs(run, now);
  return stale !== null && stale >= thresholdMs;
}

/**
 * Two distinct capacity budgets a stuck run can be bound by, kept
 * verbally distinct in the UI copy per the AC on issue #1467:
 *
 *   1. **Mesh circuit-run admission** — `meshes.circuit_run_capacity`,
 *      the cap #1467 introduced. One slot per admitted run regardless
 *      of how many agent nodes the run's blueprint fans out to. A run
 *      in `state='pending'` is parked here until the mesh has fewer
 *      `running`/`paused` runs than the cap (issue #1475).
 *   2. **Circuit agent slots** — the run's durable blueprint lease,
 *      optionally bounded by the app-wide Circuit agent pool. A
 *      `pending_slot` step is parked here. The legacy mesh node setting
 *      is not a circuit budget, and there is no per-circuit step budget
 *      (ADR 0042 retired it).
 *
 * The Probe's two copy strings spell out which budget binds: "circuit-
 * run slots" and "circuit agent slot". A user reading "waiting for a
 * slot" should be able to tell which.
 *
 * `meshActiveRuns` is a CLIENT-SIDE OBSERVATION through a paginated
 * window (`listCircuitsWithRuns(meshId, 10)`, `listCircuitProbe`), not an
 * authoritative count. The worker reads across all runs
 * (`db::count_active_circuit_runs`). In practice the wire observation
 * covers the admitted universe — `pending` lives in the queue, not the
 * ledger — but the ten-run terminal-history cap can still drop an
 * admitted run whose terminal row landed outside the window. The hedged
 * wording in `pendingAdmissionDetail` keeps that from becoming a false
 * statement.
 */
export interface CircuitCapacity {
  /** `meshes.circuit_run_capacity` — circuit runs this mesh admits at once
   *  (issue #1467 / schema v36, default 2). Read from the mesh row the
   *  Probe already has in `meshStore`; no new IPC needed. */
  meshRunCapacity: number;
  /** Runs currently `running` or `paused` across this mesh — mirrors
   *  `db::count_active_circuit_runs` (issue #1467). Deliberately excludes
   *  `pending` because counting pending self-deadlocks: every pending
   *  run would see itself + peers, so `count < cap` is always false
   *  and no run ever admits. */
  meshActiveRuns: number;
}

/**
 * Count this mesh's `running` + `paused` runs across every circuit —
 * the frontend's stand-in for the worker's `db::count_active_circuit_runs`.
 * Same window caveat as the other client-side counts (see `CircuitCapacity`).
 *
 * `pending` is intentionally NOT counted: the backend excludes it for
 * the self-deadlock reason named on `CircuitCapacity.meshActiveRuns`,
 * and matching that here keeps the copy accurate.
 */
export function countActiveRuns(
  runs: Array<{ run: { state: string } }>
): number {
  return runs.reduce(
    (total, { run }) => total + (isAdmittedRunState(run.state) ? 1 : 0),
    0
  );
}

export type RunActivityKind =
  | 'running'
  | 'awaiting_approval'
  | 'unverified'
  | 'queued'
  | 'terminal'
  | 'idle';

/** The one-line answer to "what is this run doing right now, and why?". */
export interface RunActivity {
  kind: RunActivityKind;
  /** Plain-English headline, e.g. `Waiting for approval`. */
  label: string;
  /** Circuit node the headline is about, when there is one. Rendered
   *  separately so the card can set it in mono without the label. */
  nodeId: string | null;
  /** Why it is in that state, when we can say honestly. */
  detail: string | null;
}

/**
 * Reduce a run's ledger to its single most actionable fact.
 *
 * Priority is deliberate: an approval gate outranks a running step,
 * because a blocked gate is the only state that needs the *user* to move
 * before anything else can. A queued step outranks "waiting to start"
 * for the same reason — it carries the capacity explanation.
 */
export function runActivity(
  run: { state: string },
  steps: Array<Pick<StepLike, 'node_id' | 'status' | 'error_message'>>,
  capacity: CircuitCapacity,
  review: { needsAttention: boolean } | null = null
): RunActivity {
  const firstWith = (status: string) => steps.find((s) => s.status === status) ?? null;
  const running = firstWith('running');
  const blocked = firstWith('blocked');
  const queued = firstWith(STEP_STATUS_QUEUED);

  if (run.state === 'paused') {
    return {
      kind: 'idle',
      label: 'Paused',
      nodeId: (running ?? blocked ?? queued)?.node_id ?? null,
      detail: 'Resume to continue this run.',
    };
  }
  if (run.state === 'failed') {
    const failed = firstWith('failed');
    return {
      kind: 'terminal',
      label: 'Failed',
      nodeId: failed?.node_id ?? null,
      // A failed run whose ledger carries no error text would otherwise
      // render "Failed" and nothing else — the run row has no error column
      // of its own, so if no step recorded a message there is nothing for
      // the card to show. Say where to look instead of leaving it blank.
      //
      // An unapproved review is the exception: the review graph ends the run
      // by design (the round limit was reached, or the reviewer could not
      // give a verdict) and records that ending as a gate *outcome*, never as
      // a failed step. The card's review line already states the reason, so
      // the generic "the worker failed the run" fallback would be false.
      detail: review?.needsAttention === true
        ? null
        : failedWithoutMessageDetail(steps, failed !== null),
    };
  }
  if (run.state === 'completed') {
    return { kind: 'terminal', label: 'Completed', nodeId: null, detail: null };
  }
  if (run.state === 'cancelled') {
    return { kind: 'terminal', label: 'Cancelled', nodeId: null, detail: null };
  }
  if (blocked !== null) {
    return {
      kind: 'awaiting_approval',
      label: 'Waiting for approval',
      nodeId: blocked.node_id,
      detail: 'A collaborator gate is parked until you approve it.',
    };
  }
  const unverified = firstWith('unverified');
  if (unverified !== null) {
    return { kind: 'unverified', label: 'Unverified Checkpoint', nodeId: unverified.node_id,
      detail: unverified.error_message ?? 'Evidence is incomplete. Inspect the latest observations before continuing.' };
  }
  if (running !== null) {
    return { kind: 'running', label: 'Running', nodeId: running.node_id, detail: null };
  }
  if (queued !== null) {
    return {
      kind: 'queued',
      label: 'Queued',
      nodeId: queued.node_id,
      detail: queuedReason(),
    };
  }
  // `pending` covers the brief window between a run's admission and the
  // worker's first step emission, and is the persistent state of every
  // row the queue UI shows. Either way, the reason it has not started is
  // mesh-level run admission (#1467), so the run card surfaces the same
  // copy as the queue.
  if (run.state === 'pending') {
    return {
      kind: 'idle',
      label: 'Waiting to start',
      nodeId: null,
      detail: pendingAdmissionDetail(capacity),
    };
  }
  return { kind: 'idle', label: 'Waiting to start', nodeId: null, detail: null };
}

/**
 * Fallback copy for a failed run with no error text anywhere in its ledger
 * — an agent process that died mid-flight, or a failure raised before any
 * step existed. Returns `null` when a message IS present, because the card
 * renders that message itself and repeating "look at the error" above it
 * would be noise.
 */
function failedWithoutMessageDetail(
  steps: Array<Pick<StepLike, 'status' | 'error_message'>>,
  hasFailedStep: boolean
): string | null {
  const hasMessage = steps.some((s) => s.error_message !== null && s.error_message !== '');
  if (hasMessage) return null;
  if (hasFailedStep) {
    return 'The failed step recorded no error message — check its agent node, or the app log.';
  }
  return steps.length === 0
    ? 'The run failed before any step was recorded — check the app log.'
    : 'No step recorded a failure — the run was failed by the worker; check the app log.';
}

/**
 * What a run is parked on while `state === 'pending'` — mesh-level
 * admission against the circuit-run budget (#1467 / #1475).
 *
 * Like `queuedReason`, the copy names *a* binding constraint rather than
 * claiming an exclusive cause. The two are deliberately visually
 * distinct (`queuedReason` says "circuit agent slot"; this says
 * "circuit-run slots") so a user reading "waiting for a slot" can tell
 * which budget binds.
 *
 * Singular vs plural wording follows the integer. A `meshRunCapacity`
 * of `0` falls through to a hedged statement rather than "All 0 slots
 * are busy".
 */
export function pendingAdmissionDetail(capacity: CircuitCapacity): string {
  const { meshRunCapacity, meshActiveRuns } = capacity;
  if (meshRunCapacity <= 0) {
    return "Waiting for a circuit-run slot — this mesh's run budget is not configured.";
  }
  switch (pendingRunBind(meshRunCapacity, meshActiveRuns)) {
    case 'mesh_run_admission':
      return meshRunCapacity === 1
        ? 'Waiting for a circuit-run slot — this mesh allows 1 concurrent run, and that slot is busy.'
        : `Waiting for a circuit-run slot — all ${meshRunCapacity} of this mesh's circuit-run slots are busy.`;
    default:
      // A `pending` run with spare mesh capacity is a transient state (mid
      // worker tick) — the worker re-checks admission every 2 s. State
      // what we observe rather than fabricate an explanation.
      return meshRunCapacity === 1
        ? 'Waiting for a circuit-run slot — this mesh allows 1 concurrent run (admission is re-checked every 2 s).'
        : `Waiting for a circuit-run slot — this mesh allows ${meshRunCapacity} concurrent runs (admission is re-checked every 2 s).`;
  }
}

/**
 * What holds a queued step back. Only an agent spawn can park (ADR 0042
 * retired the per-circuit step budget), and it parks on the run's agent
 * lease or the optional app-wide agent pool.
 */
export function queuedReason(): string {
  return "Waiting for a circuit agent slot — this run's agent lease, or the app-wide agent pool, has no free slot.";
}

/** Terminal-vs-live progress through the ledger, for the card's counter. */
export function runStepProgress(steps: Array<Pick<StepLike, 'status'>>): {
  finished: number;
  total: number;
} {
  const finished = steps.filter((s) => isTerminalStepStatus(s.status)).length;
  return { finished, total: steps.length };
}

/**
 * Map an activity kind onto the status token `statusTextClass` already
 * understands, so the activity line is coloured by the same vocabulary as the
 * state chips instead of a second palette.
 */
export function activityStatusToken(kind: RunActivityKind, runState: string): string {
  switch (kind) {
    case 'running':
      return 'running';
    case 'awaiting_approval':
      return 'blocked';
    case 'unverified':
      return 'unverified';
    case 'queued':
      return STEP_STATUS_QUEUED;
    case 'terminal':
      return runState;
    default:
      return runState === 'paused' ? 'paused' : 'pending';
  }
}
