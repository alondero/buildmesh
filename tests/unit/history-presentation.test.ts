import { describe, expect, it } from 'vitest';
import {
  historyTitle,
  isKeyEvent,
  splitHistory,
  stepReference,
} from '../../src/components/Circuits/historyPresentation';
import type { CircuitHistoryEntry } from '../../src/types/generated/CircuitHistoryEntry';

const entry = (over: Partial<CircuitHistoryEntry>): CircuitHistoryEntry => ({
  id: 1,
  node_id: null,
  attempt: null,
  kind: 'run_transition',
  detail: 'running',
  source: 'circuit_worker',
  disposition: 'applied',
  observed_at: '2026-10-05T19:30:52.975Z',
  ...over,
});

describe('which history entries are key events', () => {
  it('keeps the entries that say what happened to the run', () => {
    for (const kind of ['run_transition', 'operator_attestation', 'operator_retry', 'checkpoint_reason', 'review_extension',
      'review_continuation', 'queue_wait', 'evidence_recheck', 'classification']) {
      expect(isKeyEvent(entry({ kind })), kind).toBe(true);
    }
  });

  it('hides the machine bookkeeping behind the technical toggle', () => {
    for (const kind of ['observation', 'observation_readiness', 'evidence_window_changed',
      'step_capacity_wait', 'native_hook_received', 'effect_intent', 'effect_possible_dispatch',
      'effect_target', 'effect_reconciled', 'effect_result', 'configuration_pinned']) {
      expect(isKeyEvent(entry({ kind })), kind).toBe(false);
    }
  });

  it('keeps only the step transitions that need a person to look', () => {
    for (const detail of ['failed', 'unverified', 'blocked', 'cancelled']) {
      expect(isKeyEvent(entry({ kind: 'step_transition', detail })), detail).toBe(true);
    }
    for (const detail of ['running', 'completed', 'pending_slot']) {
      expect(isKeyEvent(entry({ kind: 'step_transition', detail })), detail).toBe(false);
    }
  });

  it('shows an unknown future kind rather than silently hiding it', () => {
    expect(isKeyEvent(entry({ kind: 'something_new' }))).toBe(true);
  });

  it('splits a log into the shown entries and a count of what is behind the toggle', () => {
    const rows = [
      entry({ id: 1, kind: 'run_transition', detail: 'running' }),
      entry({ id: 2, kind: 'observation' }),
      entry({ id: 3, kind: 'step_transition', detail: 'completed', node_id: 'x' }),
      entry({ id: 4, kind: 'run_transition', detail: 'failed' }),
    ];
    const split = splitHistory(rows, false);
    expect(split.shown.map((row) => row.id)).toEqual([1, 4]);
    expect(split.hiddenCount).toBe(2);
    const all = splitHistory(rows, true);
    expect(all.shown.map((row) => row.id)).toEqual([1, 2, 3, 4]);
    expect(all.hiddenCount).toBe(0);
  });
});

describe('titles and step references', () => {
  it('titles run and step transitions in plain words', () => {
    expect(historyTitle(entry({ kind: 'run_transition', detail: 'failed' }))).toBe('Run: Failed');
    expect(historyTitle(entry({ kind: 'step_transition', detail: 'unverified', node_id: 'reviewer' }))).toBe('Unverified Checkpoint');
    expect(historyTitle(entry({ kind: 'step_transition', detail: 'blocked', node_id: 'gate' }))).toBe('Needs approval');
  });

  it('keeps the established wording for the other kinds', () => {
    expect(historyTitle(entry({ kind: 'review_continuation' }))).toBe('Continued a failed review');
    expect(historyTitle(entry({ kind: 'effect_possible_dispatch' }))).toBe('Action may have been sent');
    expect(historyTitle(entry({ kind: 'operator_retry' }))).toBe('Step retried by an operator');
    expect(historyTitle(entry({ kind: 'something_new' }))).toBe('something_new');
  });

  it('refers to a step by its role and pass, falling back to the node id', () => {
    const label = (id: string) => (id === 'review_classifier' ? 'Review' : id);
    expect(stepReference(entry({ node_id: 'review_classifier', attempt: 2 }), label)).toBe('Review · pass 2');
    expect(stepReference(entry({ node_id: 'review_classifier', attempt: 1 }), label)).toBe('Review');
    expect(stepReference(entry({ node_id: 'open_pr', attempt: null }))).toBe('open_pr');
    expect(stepReference(entry({ node_id: null, attempt: null }))).toBeNull();
  });
});
