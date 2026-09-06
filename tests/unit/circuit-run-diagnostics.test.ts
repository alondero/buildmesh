/**
 * Tests for the circuit run-diagnostics helpers (issue #1468) — the
 * plain-English reading of a run's ledger that the Probe's run cards render.
 *
 * Split from `circuit-graph-model.test.ts` alongside the module itself: the
 * copy and the capacity reasoning are their own concern, and pinning the
 * exact wording here keeps a reworded label from silently changing what a
 * stuck run tells the user.
 */

import { describe, it, expect } from 'vitest';
import { formatDurationMs, runDurationMs } from '../../src/components/Circuits/circuitGraphModel';
import {
  activityStatusToken,
  countActiveRuns,
  countRunningSteps,
  pendingAdmissionDetail,
  queuedReason,
  runActivity,
  runStateLabel,
  runStepProgress,
  stepStatusLabel,
  type CircuitCapacity,
} from '../../src/components/Circuits/runDiagnostics';
import { isTerminalRunState } from '../../src/components/Circuits/circuitGraphModel';

/**
 * Run diagnostics (issue #1468) — the plain-English reading of a ledger.
 * These are the wording rules the Probe's run cards render, kept pure so
 * the copy is testable without mounting the panel.
 */
describe('run diagnostics', () => {
  const st = (node_id: string, status: string) => ({ node_id, status, error_message: null });


  describe('formatDurationMs', () => {
    it('scales from tenths of a second up to hours', () => {
      expect(formatDurationMs(400)).toBe('0.4s');
      expect(formatDurationMs(12_400)).toBe('12.4s');
      // The reason this tier exists: a 20-minute run must not read
      // "1200.0s".
      expect(formatDurationMs(90_000)).toBe('1m 30s');
      expect(formatDurationMs(252_000)).toBe('4m 12s');
      expect(formatDurationMs(3_960_000)).toBe('1h 6m');
    });

    it('clamps a negative gap to zero rather than printing "-3.0s"', () => {
      // Clock skew between two SQLite writes shouldn't render as a
      // negative duration.
      expect(formatDurationMs(-3000)).toBe('0.0s');
    });

    it('does not straddle the minute boundary', () => {
      // Regression: the tier used to be chosen on raw ms (`safe < 60_000`)
      // while the label printed `toFixed(1)`, so 59_999ms passed the test
      // but rendered "60.0s" — the sequence read "59.8s, 60.0s, 1m 0s".
      // The boundary is now decided in deciseconds, the unit it prints, so
      // a value that would render as 60.0s is already in the minute tier.
      expect(formatDurationMs(59_940)).toBe('59.9s');
      expect(formatDurationMs(59_999)).toBe('1m 0s');
      expect(formatDurationMs(60_000)).toBe('1m 0s');
    });

    it('never formats a non-finite input as "NaNh NaNm"', () => {
      // `Math.max(0, NaN)` is NaN, not 0 — the clamp does not sanitise, and
      // NaN fails every `<` comparison, so it fell through to the hours
      // branch and printed "NaNh NaNm". The duration helpers return null
      // rather than NaN, so this guards future callers.
      expect(formatDurationMs(NaN)).toBe('0.0s');
      expect(formatDurationMs(Infinity)).toBe('0.0s');
      expect(formatDurationMs(-Infinity)).toBe('0.0s');
    });
  });

  describe('runDurationMs', () => {
    const NOW = new Date('2026-08-22T10:10:00Z');

    it('measures a terminal run between created_at and updated_at', () => {
      expect(
        runDurationMs(
          { state: 'completed', created_at: '2026-08-22 10:05:00', updated_at: '2026-08-22 10:07:00' },
          NOW
        )
      ).toBe(120_000);
    });

    it('measures a live run against now, not its last transition', () => {
      // `updated_at` is when the run last CHANGED. Measuring a running run
      // against it would freeze the clock and make a hung run look like it
      // finished in two minutes.
      expect(
        runDurationMs(
          { state: 'running', created_at: '2026-08-22 10:05:00', updated_at: '2026-08-22 10:07:00' },
          NOW
        )
      ).toBe(300_000);
    });

    it('treats paused as live — a paused run is still open', () => {
      expect(
        runDurationMs(
          { state: 'paused', created_at: '2026-08-22 10:05:00', updated_at: '2026-08-22 10:07:00' },
          NOW
        )
      ).toBe(300_000);
    });

    it('returns null on an unparseable timestamp instead of NaN', () => {
      expect(runDurationMs({ state: 'completed', created_at: 'nonsense', updated_at: 'x' }, NOW)).toBeNull();
    });

    it('reads a zoneless SQLite timestamp as UTC, not local time', () => {
      // The trap: V8's Date.parse accepts "2026-08-22 10:05:00" and reads
      // it as LOCAL time. The skew cancels when you subtract two ledger
      // timestamps (so stepDurationMs never noticed) but NOT when you
      // compare one against `now` — which is exactly what a running run
      // does. On a UTC+1 host this test read 65 minutes instead of 5.
      expect(
        runDurationMs(
          { state: 'running', created_at: '2026-08-22 10:05:00', updated_at: '2026-08-22 10:05:00' },
          new Date('2026-08-22T10:05:30Z')
        )
      ).toBe(30_000);
      // An explicitly-zoned ISO string keeps working — appending "Z" to it
      // would produce "…ZZ" → NaN.
      expect(
        runDurationMs(
          { state: 'running', created_at: '2026-08-22T10:05:00Z', updated_at: '2026-08-22T10:05:00Z' },
          new Date('2026-08-22T10:05:30Z')
        )
      ).toBe(30_000);
    });
  });

  describe('label vocabularies', () => {
    it('renames the scheduler-internal statuses users could not decode', () => {
      // `pending_slot` is the token issue #1468 was filed over.
      expect(stepStatusLabel('pending_slot')).toBe('Queued');
      expect(stepStatusLabel('blocked')).toBe('Needs approval');
      expect(stepStatusLabel('completed')).toBe('Done');
      // A run's `pending` means "admitted, waiting its turn".
      expect(runStateLabel('pending')).toBe('Queued');
    });

    it('passes an unknown status through rather than inventing a label', () => {
      // Forward-compatible: a status this build has never heard of should
      // still be legible, not blank.
      expect(stepStatusLabel('teleported')).toBe('teleported');
      expect(runStateLabel('teleported')).toBe('teleported');
    });

    it('knows which run states the worker never leaves', () => {
      expect(isTerminalRunState('completed')).toBe(true);
      expect(isTerminalRunState('failed')).toBe(true);
      expect(isTerminalRunState('paused')).toBe(false);
      expect(isTerminalRunState('running')).toBe(false);
      expect(isTerminalRunState('pending')).toBe(false);
    });
  });

  describe('runActivity', () => {
    const FREE: CircuitCapacity = {
      concurrencyLimit: 2,
      runningSteps: 0,
      meshRunCapacity: 2,
      meshActiveRuns: 0,
    };

    it('puts an approval gate ahead of a running step', () => {
      // A blocked gate is the only state that needs the USER to move, so
      // it wins the headline even while another branch is executing.
      const activity = runActivity(
        { state: 'running' },
        [st('implementer', 'running'), st('review_gate', 'blocked')],
        FREE
      );
      expect(activity.kind).toBe('awaiting_approval');
      expect(activity.nodeId).toBe('review_gate');
    });

    it('names the running step when nothing is parked', () => {
      const activity = runActivity({ state: 'running' }, [st('implementer', 'running')], FREE);
      expect(activity).toMatchObject({ kind: 'running', label: 'Running', nodeId: 'implementer' });
    });

    it('names the failed step on a failed run', () => {
      const activity = runActivity(
        { state: 'failed' },
        [
          st('trigger', 'completed'),
          { node_id: 'classifier', status: 'failed', error_message: 'boom' },
        ],
        FREE
      );
      expect(activity).toMatchObject({ kind: 'terminal', label: 'Failed', nodeId: 'classifier' });
      // The card renders the message itself, so the detail line stays quiet
      // rather than repeating "look at the error" above it.
      expect(activity.detail).toBeNull();
    });

    it('still says something when a failed run has no error text anywhere', () => {
      // The run row has no error column of its own, so a failure that never
      // reached a step (or a step that died without recording a message)
      // used to render "Failed" and nothing else.
      const noMessage = runActivity(
        { state: 'failed' },
        [{ node_id: 'implementer', status: 'failed', error_message: null }],
        FREE
      );
      expect(noMessage.detail).toContain('recorded no error message');

      const noSteps = runActivity({ state: 'failed' }, [], FREE);
      expect(noSteps.nodeId).toBeNull();
      expect(noSteps.detail).toContain('before any step was recorded');

      // Failed run, steps present, but none of them is the failure — the
      // worker failed the run itself.
      const workerFailed = runActivity(
        { state: 'failed' },
        [{ node_id: 'trigger', status: 'completed', error_message: null }],
        FREE
      );
      expect(workerFailed.detail).toContain('No step recorded a failure');
    });

    it('reports paused with the step it was parked on, and how to continue', () => {
      const activity = runActivity({ state: 'paused' }, [st('implementer', 'running')], FREE);
      expect(activity.label).toBe('Paused');
      expect(activity.nodeId).toBe('implementer');
      expect(activity.detail).toContain('Resume');
    });

    it('explains a pending run as waiting on mesh-level run admission (#1475)', () => {
      // A `state === 'pending'` run with an empty ledger used to render
      // "Waiting to start" and nothing else. After #1467 the run is
      // parked against the mesh's circuit-run budget, so the activity
      // detail names that budget and how many concurrent runs the mesh
      // allows — the run card user can act on that.
      const fullMesh = runActivity(
        { state: 'pending' },
        [],
        { concurrencyLimit: 2, runningSteps: 0, meshRunCapacity: 2, meshActiveRuns: 2 }
      );
      expect(fullMesh).toMatchObject({ kind: 'idle', label: 'Waiting to start', nodeId: null });
      expect(fullMesh.detail).toBe(
        'Waiting for a circuit-run slot — all 2 of this mesh\'s circuit-run slots are busy.'
      );

      // Singular copy when the mesh allows only one run.
      const singularFull = runActivity(
        { state: 'pending' },
        [],
        { concurrencyLimit: 1, runningSteps: 0, meshRunCapacity: 1, meshActiveRuns: 1 }
      );
      expect(singularFull.detail).toBe(
        'Waiting for a circuit-run slot — this mesh allows 1 concurrent run, and that slot is busy.'
      );
    });

    it('carries the capacity explanation on a queued step', () => {
      const activity = runActivity({ state: 'running' }, [st('reviewer', 'pending_slot')], {
        concurrencyLimit: 1,
        runningSteps: 1,
      });
      expect(activity.kind).toBe('queued');
      expect(activity.nodeId).toBe('reviewer');
      expect(activity.detail).not.toBeNull();
    });
  });

  describe('queuedReason', () => {
    it('blames the circuit budget when its step slots are all busy', () => {
      expect(
        queuedReason({
          concurrencyLimit: 1,
          runningSteps: 1,
          meshRunCapacity: 2,
          meshActiveRuns: 1,
        })
      ).toBe(
        'Waiting for a slot — this circuit runs one step at a time, and that slot is busy.'
      );
      expect(
        queuedReason({
          concurrencyLimit: 2,
          runningSteps: 2,
          meshRunCapacity: 2,
          meshActiveRuns: 1,
        })
      ).toContain("all 2 of this circuit's step slots are busy");
    });

    it('blames the circuit agent lease when circuit slots are free', () => {
      // `schedule_ready` only has two reasons to park a step, so with
      // circuit capacity to spare the circuit-agent lease is the binding
      // one by elimination. (#1467 makes this a fact, not an inference.)
      expect(
        queuedReason({
          concurrencyLimit: 2,
          runningSteps: 0,
          meshRunCapacity: 2,
          meshActiveRuns: 1,
        })
      ).toContain('circuit agent slot');
    });

    it('does not claim a zero limit is "busy"', () => {
      // A 0 limit would divide the copy by a nonsense number; fall back
      // to the circuit-agent explanation rather than "All 0 slots are busy".
      expect(
        queuedReason({
          concurrencyLimit: 0,
          runningSteps: 0,
          meshRunCapacity: 2,
          meshActiveRuns: 1,
        })
      ).toContain('circuit agent slot');
    });

    it('never claims one budget is THE reason', () => {
      // Both budgets can be exhausted at once, and the ledger records
      // neither — so the copy names *a* constraint it can see, and hedges
      // the other. Asserting the absence of an exclusive claim is the point:
      // "waiting for a slot" is true in every case, the detail is evidence.
      const circuitFull = queuedReason({
        concurrencyLimit: 2,
        runningSteps: 2,
        meshRunCapacity: 2,
        meshActiveRuns: 1,
      });
      const circuitFree = queuedReason({
        concurrencyLimit: 4,
        runningSteps: 1,
        meshRunCapacity: 2,
        meshActiveRuns: 1,
      });
      for (const copy of [circuitFull, circuitFree]) {
        expect(copy.startsWith('Waiting for a slot')).toBe(true);
      }
      expect(circuitFull).toContain("this circuit's step slots are busy");
      // The circuit-agent branch says WHY it concluded that, so the reader can judge.
      expect(circuitFree).toContain('spare step slots');
      expect(circuitFree).toContain('circuit agent slot');
      expect(circuitFree).not.toContain('Autopilot');
    });
  });

  describe('activityStatusToken', () => {
    it('maps every activity kind onto a token the colour vocabulary knows', () => {
      expect(activityStatusToken('running', 'running')).toBe('running');
      expect(activityStatusToken('awaiting_approval', 'running')).toBe('blocked');
      expect(activityStatusToken('queued', 'running')).toBe('pending_slot');
      // Terminal borrows the run's own state so Completed and Failed differ.
      expect(activityStatusToken('terminal', 'completed')).toBe('completed');
      expect(activityStatusToken('terminal', 'failed')).toBe('failed');
      expect(activityStatusToken('idle', 'paused')).toBe('paused');
      expect(activityStatusToken('idle', 'pending')).toBe('pending');
    });
  });

  describe('countRunningSteps / runStepProgress', () => {
    it('counts running steps across every visible run of a circuit', () => {
      expect(
        countRunningSteps([
          { steps: [{ status: 'running' }, { status: 'completed' }] },
          { steps: [{ status: 'running' }] },
          { steps: [{ status: 'pending_slot' }] },
        ])
      ).toBe(2);
    });

    it('counts every terminal step as finished, failures included', () => {
      // A failed step is done moving; excluding it would leave a failed
      // run reading "2/4 steps" forever.
      expect(
        runStepProgress([
          { status: 'completed' },
          { status: 'failed' },
          { status: 'cancelled' },
          { status: 'running' },
          { status: 'pending_slot' },
        ])
      ).toEqual({ finished: 3, total: 5 });
    });
  });

  describe('countActiveRuns (issue #1467 / #1475)', () => {
    it('counts running + paused runs across the mesh', () => {
      // Mirrors `db::count_active_circuit_runs` semantics — `running`
      // and `paused` admit, `pending` and terminal states do not.
      expect(
        countActiveRuns([
          { run: { state: 'running' } },
          { run: { state: 'paused' } },
          { run: { state: 'pending' } },
          { run: { state: 'completed' } },
          { run: { state: 'failed' } },
          { run: { state: 'cancelled' } },
        ])
      ).toBe(2);
    });

    it('counts an empty ledger as zero rather than undefined', () => {
      // A mesh with no circuits still has a zero admission count —
      // mirrors the backend's empty-aggregate behaviour.
      expect(countActiveRuns([])).toBe(0);
    });
  });

  describe('pendingAdmissionDetail (issue #1475)', () => {
    it('names the mesh budget and the configured cap when full', () => {
      expect(
        pendingAdmissionDetail({
          concurrencyLimit: 2,
          runningSteps: 0,
          meshRunCapacity: 2,
          meshActiveRuns: 2,
        })
      ).toBe(
        "Waiting for a circuit-run slot — all 2 of this mesh's circuit-run slots are busy."
      );
    });

    it('uses singular wording when the mesh allows exactly one run', () => {
      // `meshRunCapacity === 1` is a legitimate configuration, so the
      // wording must NOT read "all 1 slots".
      expect(
        pendingAdmissionDetail({
          concurrencyLimit: 1,
          runningSteps: 0,
          meshRunCapacity: 1,
          meshActiveRuns: 1,
        })
      ).toBe(
        'Waiting for a circuit-run slot — this mesh allows 1 concurrent run, and that slot is busy.'
      );
    });

    it('hedges a zero capacity rather than claiming a busy budget', () => {
      // A `meshRunCapacity === 0` should not happen on a healthy mesh
      // (the column is `1..=8` per #1467), but render defensively — the
      // same shape `queuedReason` uses for a zero per-circuit limit.
      expect(
        pendingAdmissionDetail({
          concurrencyLimit: 2,
          runningSteps: 0,
          meshRunCapacity: 0,
          meshActiveRuns: 0,
        })
      ).toContain('not configured');
    });

    it('says why the run is still pending when the mesh has spare capacity', () => {
      // A `pending` run with `meshActiveRuns < meshRunCapacity` is a
      // transient mid-tick state. State what we see — admission is
      // re-checked every 2 s — rather than fabricate a budget reason.
      expect(
        pendingAdmissionDetail({
          concurrencyLimit: 2,
          runningSteps: 0,
          meshRunCapacity: 2,
          meshActiveRuns: 1,
        })
      ).toContain('allows 2 concurrent runs');
    });

    it('uses a visually distinct vocabulary from the queued-step copy (#1467 AC)', () => {
      // Per #1467's own AC: the three capacity concepts must read
      // unambiguously as different budgets. The pending copy says
      // "circuit-run slot(s)", the queued copy says "step slots" or
      // "circuit agent slot" — no overlap.
      const pendingCopy = pendingAdmissionDetail({
        concurrencyLimit: 2,
        runningSteps: 0,
        meshRunCapacity: 2,
        meshActiveRuns: 2,
      });
      const queuedCopy = queuedReason({
        concurrencyLimit: 2,
        runningSteps: 2,
        meshRunCapacity: 2,
        meshActiveRuns: 1,
      });
      expect(pendingCopy).toContain('circuit-run');
      expect(pendingCopy).not.toContain('step slot');
      expect(queuedCopy).toContain('step slot');
      expect(queuedCopy).not.toContain('circuit-run');
    });
  });
});
