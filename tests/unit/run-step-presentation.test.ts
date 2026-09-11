/**
 * Tests for `runStepPresentation` — the per-step reading of a
 * circuit run's ledger. These are the wording rules the Probe's run cards and
 * the editor's history drawer render, kept pure so the copy is testable
 * without mounting a component.
 */

import { describe, it, expect } from 'vitest';
import {
  indexNodes,
  linkedAgentNodeId,
  nodeRoleLabel,
  parseRunContext,
  reviewPassCount,
  reviewReport,
  stepPassLabel,
  stepSummary,
  stepVerdict,
  verdictTextClass,
} from '../../src/components/Circuits/runStepPresentation';
import type { CircuitNodeKind } from '../../src/types/generated/CircuitNodeKind';

const spawn = (name: string | null): CircuitNodeKind => ({
  type: 'spawn_agent_node',
  prompt: '',
  name,
  provider: null,
  model: null,
  effort: null,
  extra_args: null,
  timeout_seconds: null,
});

const step = (over: Partial<{
  node_id: string;
  status: string;
  outcome: string | null;
  attempt: number;
}> = {}) => ({
  node_id: 'verdict',
  status: 'completed',
  outcome: 'completed',
  attempt: 1,
  ...over,
});

describe('parseRunContext', () => {
  it('keeps string values and drops everything else', () => {
    expect(parseRunContext(JSON.stringify({
      'node.verdict.review_verdict': 'approved',
      'node.verdict.review_verdict_attempt': '2',
      nested: { nope: true },
      count: 3,
    }))).toEqual({
      'node.verdict.review_verdict': 'approved',
      'node.verdict.review_verdict_attempt': '2',
    });
  });

  it('treats unreadable and non-object payloads as no context', () => {
    expect(parseRunContext('not json')).toEqual({});
    expect(parseRunContext('[1,2,3]')).toEqual({});
    expect(parseRunContext('null')).toEqual({});
    expect(parseRunContext('')).toEqual({});
  });
});

describe('nodeRoleLabel', () => {
  it('uses a spawned agent’s configured name, falling back to its id', () => {
    expect(nodeRoleLabel('reviewer', spawn('Code reviewer'))).toBe('Code reviewer');
    expect(nodeRoleLabel('reviewer', spawn(null))).toBe('reviewer');
    expect(nodeRoleLabel('reviewer', spawn(''))).toBe('reviewer');
  });

  it('names the gate and trigger kinds a person would recognise', () => {
    expect(nodeRoleLabel('verdict', { type: 'review_verdict', target_node_id: null })).toBe('Review');
    expect(nodeRoleLabel('retry', { type: 'retry_limit', max_retries: 3 })).toBe('Retry budget');
    expect(nodeRoleLabel('check', { type: 'deterministic_verification', command: 'cargo test' })).toBe('Verification');
    expect(nodeRoleLabel('done', { type: 'all_completed' })).toBe('All completed');
  });

  it('passes through an unknown node id rather than inventing a label', () => {
    expect(nodeRoleLabel('mystery_step', undefined)).toBe('mystery_step');
  });
});

describe('stepVerdict', () => {
  const verdictKind: CircuitNodeKind = { type: 'review_verdict', target_node_id: 'reviewer' };

  it('reads the attempt-matched review verdict from the run context', () => {
    const context = {
      'node.verdict.review_verdict': 'approved',
      'node.verdict.review_verdict_attempt': '2',
    };
    expect(stepVerdict(step({ attempt: 2 }), verdictKind, context))
      .toEqual({ label: 'APPROVED', tone: 'success' });
  });

  it('maps changes-requested and blocked verdicts to their tones', () => {
    const ctx = (verdict: string) => ({
      'node.verdict.review_verdict': verdict,
      'node.verdict.review_verdict_attempt': '1',
    });
    expect(stepVerdict(step(), verdictKind, ctx('changes_requested')))
      .toEqual({ label: 'CHANGES REQUESTED', tone: 'warning' });
    expect(stepVerdict(step(), verdictKind, ctx('blocked')))
      .toEqual({ label: 'NEEDS ATTENTION', tone: 'error' });
  });

  it('ignores a verdict recorded for a different attempt', () => {
    // Pass 2 is running; the context still holds pass 1's verdict. Attributing
    // it to pass 2 would be a lie, so the routing outcome is used instead.
    const context = {
      'node.verdict.review_verdict': 'changes_requested',
      'node.verdict.review_verdict_attempt': '1',
    };
    expect(stepVerdict(step({ attempt: 2, outcome: 'completed' }), verdictKind, context))
      .toEqual({ label: 'APPROVED', tone: 'success' });
  });

  it('falls back to the routing outcome when the context is gone', () => {
    expect(stepVerdict(step({ outcome: 'completed' }), verdictKind, {}))
      .toEqual({ label: 'APPROVED', tone: 'success' });
    expect(stepVerdict(step({ outcome: 'working' }), verdictKind, {}))
      .toEqual({ label: 'CHANGES REQUESTED', tone: 'warning' });
    expect(stepVerdict(step({ outcome: 'blocked' }), verdictKind, {}))
      .toEqual({ label: 'NEEDS ATTENTION', tone: 'error' });
  });

  it('stays silent for a node kind with no specific verdict', () => {
    expect(stepVerdict(step(), undefined, {})).toBeNull();
    expect(stepVerdict(step(), spawn('impl'), {})).toBeNull();
  });

  it('reads verification and retry-budget outcomes', () => {
    const verify: CircuitNodeKind = { type: 'deterministic_verification', command: 'test' };
    const retry: CircuitNodeKind = { type: 'retry_limit', max_retries: 3 };
    expect(stepVerdict(step({ outcome: 'green' }), verify, {}))
      .toEqual({ label: 'PASSED', tone: 'success' });
    expect(stepVerdict(step({ outcome: 'red' }), verify, {}))
      .toEqual({ label: 'FAILED', tone: 'error' });
    expect(stepVerdict(step({ outcome: 'failed', status: 'failed' }), retry, {}))
      .toEqual({ label: 'LIMIT REACHED', tone: 'error' });
    expect(stepVerdict(step({ outcome: 'completed' }), retry, {})).toBeNull();
  });
});

describe('stepPassLabel', () => {
  it('prints a pass only once a node has been retried', () => {
    expect(stepPassLabel({ attempt: 1 })).toBeNull();
    expect(stepPassLabel({ attempt: 3 })).toBe('pass 3');
  });
});

describe('reviewReport', () => {
  const verdictKind: CircuitNodeKind = { type: 'review_verdict', target_node_id: 'reviewer' };

  it('returns the reviewed output for a review gate', () => {
    const context = { 'node.verdict.evaluated_output': 'Two findings remain.' };
    expect(reviewReport({ node_id: 'verdict' }, verdictKind, context)).toBe('Two findings remain.');
  });

  it('returns null for a non-review step or an empty/pruned report', () => {
    expect(reviewReport({ node_id: 'verdict' }, spawn('impl'), {
      'node.verdict.evaluated_output': 'x',
    })).toBeNull();
    expect(reviewReport({ node_id: 'verdict' }, verdictKind, {})).toBeNull();
    expect(reviewReport({ node_id: 'verdict' }, verdictKind, {
      'node.verdict.evaluated_output': '   ',
    })).toBeNull();
  });
});

describe('reviewPassCount', () => {
  it('parses the recorded verdict attempt', () => {
    expect(reviewPassCount({ 'node.verdict.review_verdict_attempt': '2' }, 'verdict')).toBe(2);
  });

  it('returns null for a missing, zero or unparseable attempt', () => {
    expect(reviewPassCount({}, 'verdict')).toBeNull();
    expect(reviewPassCount({ 'node.verdict.review_verdict_attempt': '0' }, 'verdict')).toBeNull();
    expect(reviewPassCount({ 'node.verdict.review_verdict_attempt': 'later' }, 'verdict')).toBeNull();
  });
});

describe('linkedAgentNodeId', () => {
  const s = (agent_node_id: number | null, status: string) => ({ agent_node_id, status });

  it('prefers the borrowed source agent', () => {
    expect(linkedAgentNodeId(
      { source_agent_node_id: 42 },
      [s(900, 'running')]
    )).toBe(42);
  });

  it('falls back to the agent behind the current step', () => {
    expect(linkedAgentNodeId(
      { source_agent_node_id: null },
      [s(900, 'completed'), s(901, 'running')]
    )).toBe(901);
    expect(linkedAgentNodeId(
      { source_agent_node_id: null },
      [s(900, 'completed'), s(901, 'pending_slot')]
    )).toBe(901);
  });

  it('points at the last agent a finished run drove', () => {
    expect(linkedAgentNodeId(
      { source_agent_node_id: null },
      [s(900, 'completed'), s(null, 'completed')]
    )).toBe(900);
  });

  it('returns null when no step bound an agent', () => {
    expect(linkedAgentNodeId({ source_agent_node_id: null }, [s(null, 'completed')])).toBeNull();
  });
});

describe('stepSummary and verdictTextClass', () => {
  it('composes the timeline line', () => {
    const summary = stepSummary(
      step({ attempt: 2, outcome: 'working' }),
      { type: 'review_verdict', target_node_id: 'reviewer' },
      { 'node.verdict.review_verdict': 'changes_requested', 'node.verdict.review_verdict_attempt': '2' }
    );
    expect(summary).toEqual({
      role: 'Review',
      pass: 'pass 2',
      verdict: { label: 'CHANGES REQUESTED', tone: 'warning' },
    });
  });

  it('maps tones onto the shared status colour vocabulary', () => {
    expect(verdictTextClass('success')).toBe('text-status-success');
    expect(verdictTextClass('warning')).toBe('text-status-warning');
    expect(verdictTextClass('error')).toBe('text-status-error');
  });

  it('indexes nodes by id', () => {
    const index = indexNodes([
      { id: 'a', type: { type: 'manual' } },
      { id: 'b', type: { type: 'notify', message: 'hi' } },
    ]);
    expect(index.get('a')?.type).toEqual({ type: 'manual' });
    expect(index.get('missing')).toBeUndefined();
  });
});
