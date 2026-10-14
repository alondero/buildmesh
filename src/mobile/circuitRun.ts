// Plain-words presentation of a node's Circuit Run for the phone (issue #2314).
//
// The wording comes from the same pure modules the desktop run card uses
// (`circuitVocabulary`, `runNextSteps`) so "Needs approval" and "Unverified
// Checkpoint" read identically on both. This file only composes them.

import type { NodeCircuitNeed } from '../types/generated/NodeCircuitNeed';
import type { NodeCircuitReview } from '../types/generated/NodeCircuitReview';
import type { NodeCircuitRun } from '../types/generated/NodeCircuitRun';
import { runStateLabel, stepStatusLabel } from '../components/Circuits/circuitVocabulary';
import { describeProblem } from '../components/Circuits/runNextSteps';

const VERDICT_LABELS: Record<string, string> = {
  approved: 'Approved',
  changes_requested: 'Changes requested',
  blocked: 'Needs attention',
};

export function verdictLabel(verdict: string): string {
  return VERDICT_LABELS[verdict] ?? verdict;
}

/** `Round 2 of 3 · Changes requested`; the limit and verdict are omitted when unknown. */
export function reviewLine(review: NodeCircuitReview): string {
  const round = review.max_rounds === null ? `Round ${review.round}` : `Round ${review.round} of ${review.max_rounds}`;
  return review.verdict === null ? round : `${round} · ${verdictLabel(review.verdict)}`;
}

/** Does this run need the person, as opposed to merely being visible? */
export function needsYou(run: NodeCircuitRun): boolean {
  return run.needs_you.length > 0;
}

/**
 * One line for the node card: `Needs you: Review Blueprint · Round 2 of 3 · Approval gate: Needs approval`.
 * Known raw tokens (`pending_slot`, `blocked`) are humanised; a token the
 * vocabulary does not know is shown as received.
 */
export function circuitStatusLine(run: NodeCircuitRun): string {
  const parts = [run.blueprint_name];
  if (run.review !== null) parts.push(reviewLine(run.review));
  if (run.step !== null) parts.push(`${run.step.role}: ${stepStatusLabel(run.step.status)}`);
  return `${needsYou(run) ? 'Needs you' : runStateLabel(run.state)}: ${parts.join(' · ')}`;
}

/** What the person is being asked to do, as a heading for its card. */
export function needHeading(need: NodeCircuitNeed): string {
  switch (need.kind) {
    case 'approval':
      return `${need.step.role} needs your approval`;
    case 'unverified':
      return describeProblem({ state: 'running', role: need.step.role, status: 'unverified', error: need.detail }).happened;
    case 'input':
      return `The agent is waiting for you at the ${need.step.role} step`;
    case 'failed':
      return describeProblem({ state: 'failed', role: need.step.role, status: need.step.status, error: need.detail }).happened;
  }
}

/** A run can be cancelled while it is still queued, running or paused. */
export function canCancel(run: NodeCircuitRun): boolean {
  return run.state === 'pending' || run.state === 'running' || run.state === 'paused';
}
