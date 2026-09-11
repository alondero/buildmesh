/**
 * RunHistoryDrawer — the editor's run-history drawer step vocabulary.
 *
 * The drawer reads through the same `runStepPresentation` helpers as the
 * Probe's run cards, so this pins that it renders human roles, `pass N`, and
 * gate verdicts instead of raw circuit node ids and `attempt N`.
 */

import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { RunHistoryDrawer } from '../../src/components/Circuits/RunHistoryDrawer';
import type { CircuitGraph } from '../../src/types/generated/CircuitGraph';
import type { CircuitRunDetail } from '../../src/types/generated/CircuitRunDetail';

const GRAPH = {
  nodes: [
    {
      id: 'reviewer',
      type: {
        type: 'spawn_agent_node', prompt: '', name: 'Code reviewer', provider: null,
        model: null, effort: null, extra_args: null, timeout_seconds: null,
      },
    },
    { id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
  ],
} as unknown as CircuitGraph;

const RUN: CircuitRunDetail = {
  run: {
    id: 5, circuit_id: 1, mesh_id: 42, source_agent_node_id: null,
    trigger_identity: 'manual:1', state: 'failed',
    context_json: JSON.stringify({
      'node.verdict.review_verdict': 'changes_requested',
      'node.verdict.review_verdict_attempt': '2',
    }),
    created_at: '2026-08-22 10:05:00', updated_at: '2026-08-22 10:06:00',
  },
  steps: [
    {
      id: 1, run_id: 5, node_id: 'reviewer', agent_node_id: null, status: 'completed',
      attempt: 2, outcome: 'completed', error_message: null,
      started_at: '2026-08-22 10:00:00', completed_at: '2026-08-22 10:01:00',
    },
    {
      id: 2, run_id: 5, node_id: 'verdict', agent_node_id: null, status: 'completed',
      attempt: 2, outcome: 'working', error_message: null,
      started_at: null, completed_at: null,
    },
  ],
};

describe('RunHistoryDrawer', () => {
  it('renders human roles, passes, and gate verdicts', () => {
    render(<RunHistoryDrawer runs={[RUN]} selectedRunId={5} onSelectRun={() => {}} graph={GRAPH} />);

    const reviewer = screen.getByTestId('run-step-5-reviewer');
    expect(reviewer.textContent).toContain('Code reviewer');
    expect(reviewer.textContent).toContain('pass 2');
    // 60s reads as "1m 0s", not "60.0s".
    expect(reviewer.textContent).toContain('1m 0s');

    const verdict = screen.getByTestId('run-step-5-verdict');
    expect(verdict.textContent).toContain('Review');
    expect(verdict.textContent).toContain('CHANGES REQUESTED');
    // The raw routing token is not the headline once a verdict is available.
    expect(verdict.textContent).not.toContain('working');
  });

  it('shows the empty state when no run is selected', () => {
    render(<RunHistoryDrawer runs={[RUN]} selectedRunId={null} onSelectRun={() => {}} graph={GRAPH} />);
    expect(screen.queryByTestId('run-step-5-reviewer')).toBeNull();
  });
});
