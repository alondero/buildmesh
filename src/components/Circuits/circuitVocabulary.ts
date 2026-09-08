/**
 * Runtime Circuit ledger vocabulary (issue #1660).
 *
 * Wire unions come from the ts-rs twins of `autopilot::circuit::vocabulary`.
 * Display labels and IN-list helpers live here so the canvas, diagnostics,
 * and Probe never re-spell `pending_slot` / terminal run states.
 */

import type { RunState } from '../../types/generated/RunState';
import type { StepStatus } from '../../types/generated/StepStatus';
import type { CapacityBind } from '../../types/generated/CapacityBind';

/** Stored token for stepper `Queued`. */
export const STEP_STATUS_QUEUED: StepStatus = 'pending_slot';

/** Historical step-status alias still present on some ledger rows. */
export const STEP_STATUS_QUEUED_LEGACY = 'queued';

export const TERMINAL_RUN_STATES = ['completed', 'failed', 'cancelled'] as const satisfies readonly RunState[];

export const ADMITTED_RUN_STATES = ['running', 'paused'] as const satisfies readonly RunState[];

export const LIVE_RUN_STATES = ['pending', 'running', 'paused'] as const satisfies readonly RunState[];

export function isTerminalRunState(state: string): boolean {
  return (TERMINAL_RUN_STATES as readonly string[]).includes(state);
}

export function isAdmittedRunState(state: string): boolean {
  return (ADMITTED_RUN_STATES as readonly string[]).includes(state);
}

export function isQueuedStepStatus(status: string): boolean {
  return status === STEP_STATUS_QUEUED || status === STEP_STATUS_QUEUED_LEGACY;
}

export function isTerminalStepStatus(status: string): boolean {
  return status === 'completed' || status === 'failed' || status === 'cancelled';
}

/** Display label for a run state. */
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
    case 'cancelled':
      return 'Cancelled';
    default:
      return state;
  }
}

/**
 * Display label for a step status. `pending_slot` is scheduler-internal
 * shorthand for "eligible, but every slot is taken".
 */
export function stepStatusLabel(status: string): string {
  switch (status) {
    case STEP_STATUS_QUEUED:
    case STEP_STATUS_QUEUED_LEGACY:
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
 * Which budget binds a queued step. Must match
 * `autopilot::circuit::capacity::queued_step_bind`.
 */
export function queuedStepBind(concurrencyLimit: number, runningSteps: number): CapacityBind {
  if (concurrencyLimit > 0 && runningSteps >= concurrencyLimit) {
    return 'circuit_step_slots';
  }
  return 'circuit_agent_lease';
}

/**
 * Which budget binds a parked run. Must match
 * `autopilot::circuit::capacity::pending_run_bind`. Returns `null` when
 * no bind is in effect (free run slot) — only `mesh_run_admission`
 * ever binds a pending run.
 */
export function pendingRunBind(meshRunCapacity: number, meshActiveRuns: number): CapacityBind | null {
  if (meshRunCapacity > 0 && meshActiveRuns >= meshRunCapacity) {
    return 'mesh_run_admission';
  }
  return null;
}
