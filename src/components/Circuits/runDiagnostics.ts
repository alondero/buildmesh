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

import type { StepLike } from './circuitGraphModel';

/**
 * Display label for a run state. The DB vocabulary is lower-case and
 * `pending` reads as "nothing has happened yet" rather than "admitted,
 * waiting its turn" — which is what it means.
 */
export function runStateLabel(state: string): string {
  switch (state) {
    case 'pending':
      return 'Queued';
    case 'running':
      return 'Running';
    case 'paused':
      return 'Paused';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    default:
      return state;
  }
}

/**
 * Display label for a step status. `pending_slot` is the one that most
 * needed this: it is scheduler-internal shorthand for "eligible, but
 * every slot is taken", and issue #1468 was filed because users could
 * not tell that from the raw string.
 */
export function stepStatusLabel(status: string): string {
  switch (status) {
    case 'pending_slot':
      return 'Queued';
    case 'running':
      return 'Running';
    case 'blocked':
      return 'Needs approval';
    case 'completed':
      return 'Done';
    case 'failed':
      return 'Failed';
    case 'cancelled':
      return 'Cancelled';
    default:
      return status;
  }
}

/**
 * Three distinct capacity budgets a stuck run can be bound by, kept
 * verbally distinct in the UI copy per the AC on issue #1467:
 *
 *   1. **Mesh circuit-run admission** — `meshes.circuit_run_capacity`,
 *      the cap #1467 introduced. One slot per admitted run regardless
 *      of how many agent nodes the run's blueprint fans out to. A run
 *      in `state='pending'` is parked here until the mesh has fewer
 *      `running`/`paused` runs than the cap (issue #1475).
 *   2. **Per-circuit step slots** — `autopilot_circuits.concurrency_limit`,
 *      steps the circuit may run at once. A `pending_slot` step is
 *      parked here.
 *   3. **Circuit agent slots** — the run's durable blueprint lease,
 *      optionally bounded by the app-wide Autopilot process pool. A step
 *      needing a fresh agent with no circuit slot to spare gets parked on
 *      this too. The legacy mesh node setting is not a circuit budget.
 *
 * The Probe's three copy strings spell out which budget binds: "circuit-
 * run slots", "step slots", "circuit agent slot". A user reading
 * "waiting for a slot" should be able to tell which.
 *
 * `runningSteps` and `meshActiveRuns` are both CLIENT-SIDE OBSERVATIONS
 * through a paginated window (`listCircuitsWithRuns(meshId, 10)`,
 * `listCircuitProbe`), not authoritative counts. The worker reads across
 * all runs (`db::count_running_circuit_steps` /
 * `db::count_active_circuit_runs`). In practice both wire observations
 * cover the admitted universe — `pending` lives in the queue, not the
 * ledger — but the ten-run terminal-history cap can still drop an
 * admitted run whose terminal row landed outside the window. The hedged
 * wording in `queuedReason` and `pendingAdmissionDetail` keeps that from
 * becoming a false statement. Issue #1467's bookkeeping did not expose a
 * per-circuit running-step count (it gates run admission at the mesh
 * level), so the window caveat stays.
 */
export interface CircuitCapacity {
  /** `autopilot_circuits.concurrency_limit` — steps this circuit may run at once. */
  concurrencyLimit: number;
  /** Steps currently `running` across every *visible* run of this circuit
   *  (see the window caveat above — this is a lower bound, not a total). */
  runningSteps: number;
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

/** Count the `running` steps across a circuit's visible runs — the
 *  frontend's stand-in for the worker's `count_running_circuit_steps`.
 *  A LOWER BOUND, not a total: it only sees the fetched window. See
 *  `CircuitCapacity` for what that costs the diagnosis. */
export function countRunningSteps(runs: Array<{ steps: Array<Pick<StepLike, 'status'>> }>): number {
  return runs.reduce(
    (total, { steps }) => total + steps.filter((s) => s.status === 'running').length,
    0
  );
}

/**
 * Count this mesh's `running` + `paused` runs across every circuit —
 * the frontend's stand-in for the worker's `db::count_active_circuit_runs`.
 * Same window caveat as `countRunningSteps` (see `CircuitCapacity`).
 *
 * `pending` is intentionally NOT counted: the backend excludes it for
 * the self-deadlock reason named on `CircuitCapacity.meshActiveRuns`,
 * and matching that here keeps the copy accurate.
 */
export function countActiveRuns(
  runs: Array<{ run: { state: string } }>
): number {
  return runs.reduce(
    (total, { run }) =>
      total + (run.state === 'running' || run.state === 'paused' ? 1 : 0),
    0
  );
}

export type RunActivityKind =
  | 'running'
  | 'awaiting_approval'
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
  capacity: CircuitCapacity
): RunActivity {
  const firstWith = (status: string) => steps.find((s) => s.status === status) ?? null;
  const running = firstWith('running');
  const blocked = firstWith('blocked');
  const queued = firstWith('pending_slot');

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
      detail: failedWithoutMessageDetail(steps, failed !== null),
    };
  }
  if (run.state === 'completed') {
    return { kind: 'terminal', label: 'Completed', nodeId: null, detail: null };
  }
  if (blocked !== null) {
    return {
      kind: 'awaiting_approval',
      label: 'Waiting for approval',
      nodeId: blocked.node_id,
      detail: 'A collaborator gate is parked until you approve it.',
    };
  }
  if (running !== null) {
    return { kind: 'running', label: 'Running', nodeId: running.node_id, detail: null };
  }
  if (queued !== null) {
    return {
      kind: 'queued',
      label: 'Queued',
      nodeId: queued.node_id,
      detail: queuedReason(capacity),
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
 * distinct (`queuedReason` says "step slots" or "circuit agent slot"; this says
 * "circuit-run slots") so a user reading "waiting for a slot" can tell
 * which budget binds.
 *
 * Singular vs plural wording follows the integer. A `meshRunCapacity`
 * of `0` falls through to a hedged statement rather than "All 0 slots
 * are busy" — same shape as `queuedReason` handles `concurrencyLimit`.
 */
export function pendingAdmissionDetail(capacity: CircuitCapacity): string {
  const { meshRunCapacity, meshActiveRuns } = capacity;
  if (meshRunCapacity <= 0) {
    return "Waiting for a circuit-run slot — this mesh's run budget is not configured.";
  }
  if (meshActiveRuns >= meshRunCapacity) {
    return meshRunCapacity === 1
      ? 'Waiting for a circuit-run slot — this mesh allows 1 concurrent run, and that slot is busy.'
      : `Waiting for a circuit-run slot — all ${meshRunCapacity} of this mesh's circuit-run slots are busy.`;
  }
  // A `pending` run with spare mesh capacity is a transient state (mid
  // worker tick) — the worker re-checks admission every 2 s. State what
  // we observe rather than fabricate an explanation.
  return meshRunCapacity === 1
    ? 'Waiting for a circuit-run slot — this mesh allows 1 concurrent run (admission is re-checked every 2 s).'
    : `Waiting for a circuit-run slot — this mesh allows ${meshRunCapacity} concurrent runs (admission is re-checked every 2 s).`;
}

/**
 * A constraint we can see holding a queued step back. See `CircuitCapacity`
 * for why this is observation rather than the scheduler's own answer.
 *
 * The wording names *a* binding constraint, never "the" reason: when the
 * circuit's step budget AND its agent lease are both exhausted, both are
 * binding, and claiming one exclusively would be false. #1467 replaces this
 * with a capacity contract the ledger can state outright.
 */
export function queuedReason(capacity: CircuitCapacity): string {
  const { concurrencyLimit, runningSteps } = capacity;
  if (concurrencyLimit > 0 && runningSteps >= concurrencyLimit) {
    return concurrencyLimit === 1
      ? "Waiting for a slot — this circuit runs one step at a time, and that slot is busy."
      : `Waiting for a slot — all ${concurrencyLimit} of this circuit's step slots are busy.`;
  }
  return "Waiting for a slot — this circuit has spare step slots, so it is waiting on a circuit agent slot.";
}

/** Terminal-vs-live progress through the ledger, for the card's counter. */
export function runStepProgress(steps: Array<Pick<StepLike, 'status'>>): {
  finished: number;
  total: number;
} {
  const finished = steps.filter(
    (s) => s.status === 'completed' || s.status === 'failed' || s.status === 'cancelled'
  ).length;
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
    case 'queued':
      return 'pending_slot';
    case 'terminal':
      return runState;
    default:
      return runState === 'paused' ? 'paused' : 'pending';
  }
}
