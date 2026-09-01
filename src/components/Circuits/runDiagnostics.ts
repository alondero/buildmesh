/**
 * runDiagnostics — the plain-English reading of a circuit run's ledger
 * (issue #1468).
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
 * What we can observe about why a step is parked.
 *
 * `schedule_ready` (src-tauri/src/autopilot/circuit/stepper.rs) parks a
 * step as `pending_slot` for exactly two reasons: the circuit's own
 * running-step budget is spent, or the step needs a fresh agent node and
 * the mesh's agent budget is spent. Neither reason is persisted on the
 * step row, so we derive which one is binding from the numbers the wire
 * DOES carry. Issue #1467 replaces this inference with a real
 * circuit-run capacity contract the ledger can state outright.
 *
 * **This is observation through a paginated window, and the window can
 * skew it.** `runningSteps` is counted from the runs the Probe fetched —
 * `listCircuitsWithRuns(meshId, 10)`, the ten most recent. The worker
 * counts across *all* of them (`db::count_running_circuit_steps`). An
 * in-flight run that has fallen outside the ten-run window therefore
 * makes `runningSteps` under-count, and an under-count reads as "this
 * circuit has spare step slots" — flipping the diagnosis to the mesh
 * agent budget when the circuit budget was in fact binding. That needs
 * eleven concurrent-ish runs on one circuit to happen, and the hedged
 * wording in `queuedReason` keeps it from becoming a false statement,
 * but it is a real limit of inferring scheduler state client-side and is
 * the reason #1467 should replace this rather than extend it.
 */
export interface CircuitCapacity {
  /** `autopilot_circuits.concurrency_limit` — steps this circuit may run at once. */
  concurrencyLimit: number;
  /** Steps currently `running` across every *visible* run of this circuit
   *  (see the window caveat above — this is a lower bound, not a total). */
  runningSteps: number;
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
 * A constraint we can see holding a queued step back. See `CircuitCapacity`
 * for why this is observation rather than the scheduler's own answer.
 *
 * The wording names *a* binding constraint, never "the" reason: when the
 * circuit's step budget AND the mesh agent budget are both exhausted, both are
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
  return "Waiting for a slot — this circuit has spare step slots, so it is waiting on a mesh agent slot (Autopilot's concurrency limit).";
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
