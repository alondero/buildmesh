/**
 * historyPresentation — which Circuit Run History entries a person needs, and
 * how each reads at a glance.
 *
 * The ledger records every observation, wait window and effect state because
 * recovery needs them, which makes a long run hundreds of entries deep. Almost
 * all of it is bookkeeping. The default view keeps the events that say what
 * happened to the run and which step needs a person; the rest sits behind a
 * toggle with its count. Pure and React-free so the rules are unit-testable.
 */

import type { CircuitHistoryEntry } from '../../types/generated/CircuitHistoryEntry';
import { runStateLabel, stepStatusLabel } from './circuitVocabulary';
import { stepPassLabel } from './runStepPresentation';

const KIND_TITLES: Record<string, string> = {
  continuation_effect: 'Continuation prompt',
  queue_wait: 'Waiting for admission',
  step_capacity_wait: 'Step capacity wait changed',
  configuration_pinned: 'Pinned run configuration',
  evidence_window_changed: 'Evidence wait changed',
  review_continuation: 'Continued a failed review',
  observation_readiness: 'Session observation',
  review_extension: 'Review rounds added',
  run_transition: 'Run state',
  step_transition: 'Step state',
  effect_intent: 'Action intended',
  effect_possible_dispatch: 'Action may have been sent',
  effect_result: 'Action result',
  effect_reconciled: 'Action reconciled by read-only check',
  effect_target: 'Action target recorded',
  operator_attestation: 'Operator-recorded outcome (attestation)',
  operator_retry: 'Step retried by an operator',
  checkpoint_reason: 'Checkpoint explanation',
  observation: 'Observed evidence',
  evidence_recheck: 'Evidence recheck requested',
  native_hook_received: 'Native evidence received',
  classification: 'Report interpretation',
};

/** Bookkeeping the default view leaves out. */
const TECHNICAL_KINDS = new Set([
  'observation',
  'observation_readiness',
  'evidence_window_changed',
  'step_capacity_wait',
  'native_hook_received',
  'effect_intent',
  'effect_possible_dispatch',
  'effect_target',
  'effect_reconciled',
  'effect_result',
  'configuration_pinned',
]);

/** Step states that need a person to look; the routine ones are in the step list. */
const PROBLEM_STEP_STATES = new Set(['failed', 'unverified', 'blocked', 'cancelled']);

/** An unknown kind is shown: hiding something new would hide a problem. */
export function isKeyEvent(entry: Pick<CircuitHistoryEntry, 'kind' | 'detail'>): boolean {
  if (entry.kind === 'step_transition') return PROBLEM_STEP_STATES.has(entry.detail);
  return !TECHNICAL_KINDS.has(entry.kind);
}

export function splitHistory(
  entries: readonly CircuitHistoryEntry[],
  showTechnical: boolean,
): { shown: CircuitHistoryEntry[]; hiddenCount: number } {
  if (showTechnical) return { shown: [...entries], hiddenCount: 0 };
  const shown = entries.filter(isKeyEvent);
  return { shown, hiddenCount: entries.length - shown.length };
}

export function historyTitle(entry: Pick<CircuitHistoryEntry, 'kind' | 'detail'>): string {
  if (entry.kind === 'run_transition') return `Run: ${runStateLabel(entry.detail)}`;
  if (entry.kind === 'step_transition') return stepStatusLabel(entry.detail);
  return KIND_TITLES[entry.kind] ?? entry.kind;
}

/** `Review · pass 2` — the step an entry is about, in the card's own words. */
export function stepReference(
  entry: Pick<CircuitHistoryEntry, 'node_id' | 'attempt'>,
  nodeLabel: (nodeId: string) => string = (nodeId) => nodeId,
): string | null {
  if (entry.node_id === null) return null;
  const pass = entry.attempt === null ? null : stepPassLabel({ attempt: entry.attempt });
  return pass === null ? nodeLabel(entry.node_id) : `${nodeLabel(entry.node_id)} · ${pass}`;
}
