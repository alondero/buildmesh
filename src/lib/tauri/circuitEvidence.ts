import { _invoke } from './_invoke';
import type { CircuitEvidenceView } from '../../types/generated/CircuitEvidenceView';
import type { CheckpointRequest } from '../../types/generated/CheckpointRequest';
import type { CircuitRunAttention } from '../../types/generated/CircuitRunAttention';
import type { RecoveryRequest } from '../../types/generated/RecoveryRequest';

export const circuitRunHistory = (runId: number) =>
  _invoke<CircuitEvidenceView>('circuit_run_history', { runId });

export const recordCircuitOutcome = (request: CheckpointRequest) =>
  _invoke<void>('record_circuit_outcome', { request });

/** What a run needs from a person right now, without loading its history. */
export const circuitRunAttention = (runId: number) =>
  _invoke<CircuitRunAttention>('circuit_run_attention', { runId });

/** Retry a failed run's failed step, or record that it was finished by hand. */
export const recoverFailedCircuitRun = (request: RecoveryRequest) =>
  _invoke<void>('recover_failed_circuit_run', { request });

/**
 * How many runs on a mesh are still asking for attention, unbounded by the
 * Probe ledger's per-circuit window.
 *
 * The ledger deliberately caps how many attention runs it surfaces per circuit,
 * so it cannot count what the bulk acknowledgement will touch. The History
 * toolbar labels that control with this number instead.
 */
export const countCircuitRunAttention = (meshId: number) =>
  _invoke<number>('count_circuit_run_attention', { meshId });

/**
 * Acknowledge every outstanding attention run on a mesh — the History tab's
 * "Dismiss all (N)". Returns how many runs the backend actually changed.
 *
 * The rows are selected in SQL against the same predicate
 * `countCircuitRunAttention` counts with, so the confirmation names exactly the
 * runs this command touches; the UI deliberately does not send an id list that
 * could drift from it.
 */
export const dismissAllCircuitRunAttention = (meshId: number) =>
  _invoke<number>('dismiss_all_circuit_run_attention', { meshId });
