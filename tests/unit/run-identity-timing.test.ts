import { describe, expect, it } from 'vitest';
import {
  formatWallClock,
  runActiveDurationMs,
  runFinishedMs,
  runStartedMs,
  runSubject,
} from '../../src/components/Circuits/runIdentity';
import { indexNodes } from '../../src/components/Circuits/runStepPresentation';
import type { AutopilotCircuitRun } from '../../src/types/generated/AutopilotCircuitRun';
import type { AutopilotCircuitRunStep } from '../../src/types/generated/AutopilotCircuitRunStep';
import type { CircuitNode } from '../../src/types/generated/CircuitNode';

function run(over: Partial<AutopilotCircuitRun> = {}): AutopilotCircuitRun {
  return {
    id: 335,
    circuit_id: 5,
    mesh_id: 1,
    source_agent_node_id: null,
    trigger_identity: 'issue:9:buildmesh',
    state: 'completed',
    context_json: '{}',
    created_at: '2026-10-05 14:09:09',
    updated_at: '2026-10-05 15:08:56',
    ...over,
  };
}

function step(over: Partial<AutopilotCircuitRunStep>): AutopilotCircuitRunStep {
  return {
    id: 1,
    run_id: 335,
    node_id: 'implementer',
    agent_node_id: null,
    status: 'completed',
    attempt: 1,
    outcome: 'completed',
    error_message: null,
    started_at: '2026-10-05 14:48:45',
    completed_at: '2026-10-05 14:50:00',
    ...over,
  };
}

const spawn = (id: string): CircuitNode => ({
  id,
  type: {
    type: 'spawn_agent_node',
    prompt: 'p',
    name: null,
    provider: null,
    model: null,
    effort: null,
    extra_args: null,
    timeout_seconds: null,
  },
});
const verdict: CircuitNode = { id: 'review_classifier', type: { type: 'review_verdict', target_node_id: 'reviewer' } };
const nodes = indexNodes([spawn('implementer'), spawn('reviewer'), verdict]);

describe('run timing excludes the time a run spent queued', () => {
  it('starts at the first step, not at the moment the trigger minted the run', () => {
    const steps = [
      step({ node_id: 'trigger', started_at: '2026-10-05 14:48:45' }),
      step({ node_id: 'implementer', started_at: '2026-10-05 14:49:10' }),
    ];
    // Created 14:09:09, but it queued for ~40 minutes before running.
    expect(runStartedMs(steps)).toBe(Date.UTC(2026, 9, 5, 14, 48, 45));
  });

  it('has no start while the ledger holds no step yet', () => {
    expect(runStartedMs([])).toBeNull();
  });

  it('measures a finished run from its start to the terminal stamp', () => {
    const steps = [step({ started_at: '2026-10-05 14:48:45' })];
    const finished = runFinishedMs(run());
    expect(finished).toBe(Date.UTC(2026, 9, 5, 15, 8, 56));
    expect(runActiveDurationMs(run(), steps, new Date('2026-10-06T00:00:00Z'))).toBe(
      (15 * 3600 + 8 * 60 + 56 - (14 * 3600 + 48 * 60 + 45)) * 1000,
    );
  });

  it('measures a live run against the clock, and reports no finish time', () => {
    const live = run({ state: 'running' });
    const steps = [step({ started_at: '2026-10-05 14:48:45' })];
    expect(runFinishedMs(live)).toBeNull();
    expect(runActiveDurationMs(live, steps, new Date('2026-10-05T14:58:45Z'))).toBe(10 * 60 * 1000);
  });

  it('has no duration for a run that has not started', () => {
    expect(runActiveDurationMs(run({ state: 'running' }), [], new Date())).toBeNull();
  });
});

describe('wall-clock formatting', () => {
  const now = new Date('2026-10-05T20:00:00Z');
  it('shows only the time for today', () => {
    expect(formatWallClock(Date.UTC(2026, 9, 5, 14, 48, 45), now, 'UTC')).toBe('14:48:45');
  });
  it('adds the date for another day', () => {
    expect(formatWallClock(Date.UTC(2026, 9, 4, 9, 5, 7), now, 'UTC')).toBe('4 Oct 09:05:07');
  });
});

describe('runSubject — which node is this run about?', () => {
  it('prefers the implementation agent over the reviewer, by name', () => {
    const steps = [
      step({ node_id: 'implementer', agent_node_id: 4970 }),
      step({ node_id: 'reviewer', agent_node_id: 4989 }),
    ];
    const names: Record<number, string> = { 4970: 'ai-loading-spinner', 4989: 'ebony-unhelpful-cudgel' };
    const subject = runSubject(run(), steps, {}, nodes, (id) => names[id] ?? null);
    expect(subject).toEqual({ label: 'ai-loading-spinner', agentNodeId: 4970 });
  });

  it('uses the borrowed source when a title-bar review started the run', () => {
    const subject = runSubject(
      run({ source_agent_node_id: 4983 }),
      [step({ node_id: 'reviewer', agent_node_id: 4993 })],
      {},
      nodes,
      (id) => (id === 4983 ? 'fix-set-vars' : 'veritable-phobic-creek'),
    );
    expect(subject).toEqual({ label: 'fix-set-vars', agentNodeId: 4983 });
  });

  it('falls back to the issue title once the agent node no longer exists', () => {
    const steps = [step({ node_id: 'implementer', agent_node_id: 4970 })];
    const subject = runSubject(
      run(),
      steps,
      { 'issue.number': '2041', 'issue.title': 'Spinner overlaps log' },
      nodes,
      () => null,
    );
    expect(subject).toEqual({ label: '#2041 Spinner overlaps log', agentNodeId: null });
  });

  it('falls back to the recorded source name, then the PR title', () => {
    expect(runSubject(run({ source_agent_node_id: 9 }), [], { 'source.name': 'Fix vars' }, nodes, () => null))
      .toEqual({ label: 'Fix vars', agentNodeId: null });
    expect(runSubject(run(), [], { 'pr.title': 'feat: x' }, nodes, () => null))
      .toEqual({ label: 'feat: x', agentNodeId: null });
  });

  it('says nothing specific when the run never bound an agent', () => {
    expect(runSubject(run(), [], {}, nodes, () => null)).toEqual({ label: null, agentNodeId: null });
  });
});
