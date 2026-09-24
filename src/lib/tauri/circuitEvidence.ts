import { _invoke } from './_invoke';
import type { CircuitEvidenceView } from '../../types/generated/CircuitEvidenceView';
import type { CheckpointRequest } from '../../types/generated/CheckpointRequest';

export const circuitRunHistory = (runId: number) =>
  _invoke<CircuitEvidenceView>('circuit_run_history', { runId });

export const recordCircuitOutcome = (request: CheckpointRequest) =>
  _invoke<void>('record_circuit_outcome', { request });
