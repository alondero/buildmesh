/**
 * runNextSteps — what happened to a run that needs a person, and what they can
 * do about it, in plain words.
 *
 * Failures reach the ledger as scheduler vocabulary ("piloted agent node was
 * closed", "Prompt delivery is unverified: The harness did not confirm ...").
 * That text is kept as detail, but the first thing a person reads should say what
 * it means and what to do. Pure and React-free so the wording is unit-tested.
 */

import type { CheckpointAction } from '../../types/generated/CheckpointAction';
import type { RunRecovery } from '../../types/generated/RunRecovery';

export interface Problem {
  /** One plain sentence: what happened. */
  happened: string;
  /** The raw recorded text when the sentence above reworded it. */
  detail: string | null;
}

export function describeProblem(input: {
  state: string;
  role: string;
  status: string;
  error: string | null;
}): Problem {
  const error = (input.error ?? '').trim();
  if (input.status === 'unverified') {
    return {
      happened: `Autopilot can't confirm yet that the ${input.role} step finished. This is a pause, not a failure.`,
      detail: error === '' ? null : error,
    };
  }
  if (/piloted agent node (was closed|\d+ is no longer available)/i.test(error)) {
    return {
      happened: `The ${input.role} step could not finish because the agent it was working with was closed.`,
      detail: error,
    };
  }
  if (/prompt delivery/i.test(error)) {
    return {
      happened: `Autopilot could not confirm that the agent received the prompt for the ${input.role} step.`,
      detail: error,
    };
  }
  if (/pull request/i.test(error)) {
    return {
      happened: `The ${input.role} step found no open pull request for the agent's branch.`,
      detail: error,
    };
  }
  if (/classif|harness report|assistant report/i.test(error)) {
    return {
      happened: `Autopilot could not read the agent's report to decide whether the ${input.role} step was finished.`,
      detail: error,
    };
  }
  return { happened: `The ${input.role} step failed.`, detail: error === '' ? null : error };
}

export interface ActionAvailability {
  available: boolean;
  reason: string | null;
}

export interface RecoveryGuidance {
  retry: ActionAvailability;
  continue: ActionAvailability;
  /** Short bullets: what each action does, or why it is unavailable. */
  todo: string[];
}

export function recoveryGuidance(recovery: RunRecovery | null): RecoveryGuidance {
  if (recovery === null) {
    return {
      retry: { available: false, reason: null },
      continue: { available: false, reason: null },
      todo: ['Open the agent node to see what happened.', 'Trigger the circuit again to start a new run.'],
    };
  }
  const find = (action: 'retry' | 'continue'): ActionAvailability => {
    const option = recovery.options.find((candidate) => candidate.action === action);
    return {
      available: option?.available ?? false,
      reason: option?.available === false ? (option.unavailable_reason ?? null) : null,
    };
  };
  const retry = find('retry');
  const cont = find('continue');
  const todo: string[] = [];
  todo.push(retry.available ? 'Retry runs the step again.' : `To retry: ${retry.reason ?? 'not available.'}`);
  todo.push(
    cont.available
      ? 'Choose "I\'ve done this — continue" if you have already done what the step does yourself.'
      : `To continue: ${cont.reason ?? 'not available.'}`,
  );
  return { retry, continue: cont, todo };
}

const CHECKPOINT_LABELS: Record<CheckpointAction, string> = {
  recheck: 'Check again',
  completed: "I've done this — mark it done",
  not_performed: "It didn't happen",
  retry: 'Try it again',
};

export function checkpointActionLabel(action: CheckpointAction): string {
  return CHECKPOINT_LABELS[action];
}
