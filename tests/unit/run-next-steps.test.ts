import { describe, expect, it } from 'vitest';
import {
  checkpointActionLabel,
  describeProblem,
  recoveryGuidance,
} from '../../src/components/Circuits/runNextSteps';
import type { RunRecovery } from '../../src/types/generated/RunRecovery';

const recovery = (over: Partial<RunRecovery> = {}): RunRecovery => ({
  node_id: 'finish',
  attempt: 1,
  status: 'failed',
  error: 'Prompt delivery failed',
  options: [
    { action: 'retry', available: true },
    { action: 'continue', available: true },
  ],
  ...over,
});

describe('describeProblem — what happened, in plain words', () => {
  it('says an agent was closed under the step rather than quoting the scheduler', () => {
    const problem = describeProblem({
      state: 'failed',
      role: 'Classifier',
      status: 'cancelled',
      error: 'piloted agent node was closed',
    });
    expect(problem.happened).toBe(
      'The Classifier step could not finish because the agent it was working with was closed.',
    );
    expect(problem.detail).toBe('piloted agent node was closed');
  });

  it('recognises the "no longer available" wording too', () => {
    const problem = describeProblem({
      state: 'failed',
      role: 'Send feedback',
      status: 'cancelled',
      error: 'Piloted agent node 4938 is no longer available (deleted, archived or missing)',
    });
    expect(problem.happened).toContain('agent it was working with was closed');
  });

  it('explains an unconfirmed prompt without the harness jargon', () => {
    const problem = describeProblem({
      state: 'failed',
      role: 'Send feedback',
      status: 'failed',
      error: 'Prompt delivery is unverified: The harness did not confirm the 831-character pasted prompt before Enter after 90.0s',
    });
    expect(problem.happened).toBe(
      'Autopilot could not confirm that the agent received the prompt for the Send feedback step.',
    );
    expect(problem.detail).toContain('831-character');
  });

  it('explains a missing pull request', () => {
    const problem = describeProblem({
      state: 'failed',
      role: 'GitHub action',
      status: 'failed',
      error: 'the implementation agent did not raise an open pull request for its branch',
    });
    expect(problem.happened).toContain('no open pull request');
  });

  it('keeps the recorded error as detail for anything it does not recognise', () => {
    const problem = describeProblem({
      state: 'failed',
      role: 'Notify',
      status: 'failed',
      error: 'something exotic went wrong',
    });
    // The raw text is kept as detail rather than headlining the card.
    expect(problem.happened).toBe('The Notify step failed.');
    expect(problem.detail).toBe('something exotic went wrong');
  });

  it('says an unverified step is waiting for evidence, not that it failed', () => {
    const problem = describeProblem({
      state: 'running',
      role: 'Review',
      status: 'unverified',
      error: 'Waiting for a usable harness report: newer input or tool activity follows the assistant report.',
    });
    expect(problem.happened).toBe(
      "Autopilot can't confirm yet that the Review step finished. This is a pause, not a failure.",
    );
    expect(problem.detail).toContain('Waiting for a usable harness report');
  });
});

describe('recoveryGuidance — what a person can do', () => {
  it('offers both actions when both are available', () => {
    const guidance = recoveryGuidance(recovery());
    expect(guidance.retry).toEqual({ available: true, reason: null });
    expect(guidance.continue).toEqual({ available: true, reason: null });
    expect(guidance.todo).toEqual([
      'Retry runs the step again.',
      "Choose \"I've done this — continue\" if you have already done what the step does yourself.",
    ]);
  });

  it('carries the reason an action is not available, so the person knows how to unblock it', () => {
    const guidance = recoveryGuidance(
      recovery({
        options: [
          { action: 'retry', available: false, unavailable_reason: 'The agent for this step was closed. Resume it from Archive, then retry.' },
          { action: 'continue', available: true },
        ],
      }),
    );
    expect(guidance.retry.available).toBe(false);
    expect(guidance.retry.reason).toContain('Resume it from Archive');
    expect(guidance.todo[0]).toBe('To retry: The agent for this step was closed. Resume it from Archive, then retry.');
  });

  it('says plainly when nothing can be done from the card', () => {
    const guidance = recoveryGuidance(
      recovery({
        options: [
          { action: 'retry', available: false, unavailable_reason: 'The agent no longer exists.' },
          { action: 'continue', available: false, unavailable_reason: 'A review approval cannot be recorded.' },
        ],
      }),
    );
    expect(guidance.todo).toEqual([
      'To retry: The agent no longer exists.',
      'To continue: A review approval cannot be recorded.',
    ]);
  });

  it('gives guidance even when the backend offered no recovery', () => {
    expect(recoveryGuidance(null).todo).toEqual([
      'Open the agent node to see what happened.',
      'Trigger the circuit again to start a new run.',
    ]);
  });
});

describe('checkpointActionLabel', () => {
  it('uses plain verbs for each recorded outcome', () => {
    expect(checkpointActionLabel('recheck')).toBe('Check again');
    expect(checkpointActionLabel('completed')).toBe("I've done this — mark it done");
    expect(checkpointActionLabel('not_performed')).toBe("It didn't happen");
    expect(checkpointActionLabel('retry')).toBe('Try it again');
  });
});
