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
