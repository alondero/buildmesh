import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { NodeCircuitSummary } from '../../src/components/AgentNodeView/NodeCircuitSummary';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import type { CircuitAgentOwnership } from '../../src/types/generated/CircuitAgentOwnership';
import type { CircuitRunOverview } from '../../src/types/generated/CircuitRunOverview';
import type { RunOverviewState } from '../../src/components/Circuits/useCircuitRunOverview';

vi.mock('../../src/hooks/useProviderList', async importOriginal => ({
  ...await importOriginal<typeof import('../../src/hooks/useProviderList')>(),
  useProviderList: () => [],
}));

const node = { id: 42, mesh_id: 7, name: 'Fix parser', provider: 'codex', status: 'ready' } as AgentNode;
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

describe('compact node circuit summary', () => {
  it('offers review setup directly on a node without a circuit', () => {
    render(<NodeCircuitSummary node={node} resource={resource({ overview: null })} onOpenOverview={vi.fn()} />);
    expect(screen.getByText('No circuit attached')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Add review' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByText('Reviewer launch configuration')).toBeTruthy();
    expect((screen.getByLabelText('Maximum review rounds') as HTMLInputElement).value).toBe('3');
  });

  it('shows the round limit and links the live run to its Overview without offering a duplicate', () => {
    const open = vi.fn();
    render(<NodeCircuitSummary node={node} ownership={ownership} resource={resource()} onOpenOverview={open} />);
    expect(screen.getByText('Parser review')).toBeTruthy();
    expect(screen.getByText('Running')).toBeTruthy();
    expect(screen.getByText('Round 2 of 5')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Add review' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'View circuit' }));
    expect(open).toHaveBeenCalledOnce();
  });

  it('keeps status and navigation visible while loading, and retries a failed refresh', () => {
    const refresh = vi.fn();
    const props = { node, ownership, onOpenOverview: vi.fn() };
    const view = render(<NodeCircuitSummary {...props} resource={resource({ overview: null })} />);
    expect(screen.getByText('Loading circuit details…')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'View circuit' })).toBeTruthy();
    view.rerender(<NodeCircuitSummary {...props} resource={resource({ overview: null, error: 'Connection lost', refresh })} />);
    expect(screen.getByRole('alert').textContent).toContain('Connection lost');
    expect(screen.getByText('Running')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(refresh).toHaveBeenCalledOnce();
  });

  it('flags a checkpoint requiring a person and follows updated run state', () => {
    const props = { node, ownership, onOpenOverview: vi.fn() };
    const blocked = { ...overview, steps: [{ ...overview.steps[0], status: 'blocked' }] };
    const view = render(<NodeCircuitSummary {...props} resource={resource({ overview: blocked })} />);
    expect(screen.getByText('Needs attention')).toBeTruthy();
    view.rerender(<NodeCircuitSummary {...props} resource={resource({ overview: { ...overview, run: { ...overview.run, state: 'paused' } } })} />);
    expect(screen.getByText('Paused')).toBeTruthy();
    expect(screen.queryByText('Needs attention')).toBeNull();
  });

  it.each(['completed', 'failed', 'cancelled'])('retains %s history while allowing a new review', state => {
    render(<NodeCircuitSummary node={node} ownership={{ ...ownership, state }}
      resource={resource({ overview: { ...overview, run: { ...overview.run, state } } })} onOpenOverview={vi.fn()} />);
    expect(screen.getByRole('button', { name: 'View circuit' })).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Add review' }) as HTMLButtonElement).disabled).toBe(false);
  });
});
