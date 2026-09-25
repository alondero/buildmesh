import { _invoke } from './_invoke';
import type { AutopilotCircuit } from '../../types/generated/AutopilotCircuit';

export const copyReviewBlueprint = (circuitId: number, name: string) =>
  _invoke<AutopilotCircuit>('copy_review_blueprint', { circuitId, name });
