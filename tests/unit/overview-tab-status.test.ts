import { describe, expect, it, vi } from 'vitest';
import { overviewTabStatus } from '../../src/components/AgentNodeView/overviewTabStatus';
import type { CircuitAgentOwnership } from '../../src/types/generated/CircuitAgentOwnership';
import type { CircuitRunOverview } from '../../src/types/generated/CircuitRunOverview';
import type { RunOverviewState } from '../../src/components/Circuits/useCircuitRunOverview';

const ownership: CircuitAgentOwnership = {
  node_id: 42, run_id: 91, circuit_id: 3, circuit_name: 'Parser review', state: 'running', parent_node_id: null,
};
const overview: CircuitRunOverview = {
  circuit_name: 'Parser review', review_round_limit: 5,
  run: { id: 91, circuit_id: 3, mesh_id: 7, source_agent_node_id: 42, trigger_identity: 'manual:42',
    state: 'running', context_json: '{"retry.attempt":"2"}', created_at: '', updated_at: '', attention_dismissed_at: null },
  graph: { version: 3, nodes: [{ id: 'await_source', type: { type: 'await_agent_turn', target_node_id: 'source' } }], edges: [] },
  steps: [{ id: 1, run_id: 91, node_id: 'await_source', status: 'running', attempt: 1, agent_node_id: 42,
    outcome: null, error_message: null, started_at: '', completed_at: null }],
  attention: { revision: 1, checkpoints: [], pushes: [] },
};
const resource = (overrides: Partial<RunOverviewState> = {}): RunOverviewState => ({ overview, error: null, refresh: vi.fn(), ...overrides });

describe('Overview tab circuit status', () => {
  it('has nothing to say for a card without a circuit run', () => {
    expect(overviewTabStatus(undefined, resource({ overview: null }))).toBeNull();
  });

  it('labels the tab with run state and round, and keeps the detail for the tooltip', () => {
    const status = overviewTabStatus(ownership, resource());
    expect(status?.label).toBe('Running · 2/5');
    expect(status?.detail).toContain('Parser review');
    expect(status?.detail).toContain('Round 2 of 5');
    expect(status?.detail).toContain('Await task');
  });

  it('omits the round when the run has none', () => {
    const status = overviewTabStatus(ownership, resource({
      overview: { ...overview, review_round_limit: null, run: { ...overview.run, context_json: '{}' } },
    }));
    expect(status?.label).toBe('Running');
  });

  it('shortens an unlimited round limit', () => {
    const status = overviewTabStatus(ownership, resource({
      overview: { ...overview, review_round_limit: 0, run: { ...overview.run, context_json: '{"retry.attempt":"7"}' } },
    }));
    expect(status?.label).toBe('Running · 7/∞');
    expect(status?.detail).toContain('Round 7 of unlimited');
  });

  it('keeps the known ownership state while details load, and reports a failed refresh', () => {
    const loading = overviewTabStatus(ownership, resource({ overview: null }));
    expect(loading?.label).toBe('Running');
    expect(loading?.detail).toContain('Loading circuit details');
    const failed = overviewTabStatus(ownership, resource({ overview: null, error: 'Connection lost' }));
    expect(failed?.label).toBe('Running');
    expect(failed?.detail).toContain('Could not refresh circuit: Connection lost');
    expect(failed?.detail).not.toContain('Loading');
  });

  it('follows the live run state over the ownership snapshot', () => {
    const status = overviewTabStatus(ownership, resource({ overview: { ...overview, run: { ...overview.run, state: 'paused' } } }));
    expect(status?.label).toBe('Paused · 2/5');
  });
});
