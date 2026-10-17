import { describe, expect, it } from 'vitest';
import { distinctRailLabels, flowOrder, overviewNeedsYou, pushOutcomeLabel, railRows } from '../../src/components/Circuits/runOverviewModel';
import type { AutopilotCircuitRunStep } from '../../src/types/generated/AutopilotCircuitRunStep';
import type { CircuitGraph } from '../../src/types/generated/CircuitGraph';
import type { CircuitRunOverview } from '../../src/types/generated/CircuitRunOverview';

const spawn = (name: string) => ({ type: 'spawn_agent_node' as const, prompt: '', name, provider: null, model: null, effort: null, extra_args: null, timeout_seconds: null });
const inject = { type: 'inject_pty' as const, prompt: 'fix it', target_node_id: 'implement' };

/** The review loop: implement, review, then either merge or feed back and review again. */
const reviewLoop: CircuitGraph = {
  version: 3,
  nodes: [
    // Deliberately out of flow order: the rail must not rely on it.
    { id: 'merge', type: inject },
    { id: 'trigger', type: { type: 'manual' } },
    { id: 'implement', type: spawn('Implement') },
    { id: 'review', type: { type: 'review_verdict', target_node_id: 'implement' } },
    { id: 'feedback', type: inject },
    { id: 'retry', type: { type: 'retry_limit', max_retries: 3 } },
  ],
  edges: [
    { from: 'trigger', to: 'implement', condition: 'always' },
    { from: 'implement', to: 'review', condition: 'always' },
    { from: 'review', to: 'merge', condition: { on_outcome: 'completed' } },
    { from: 'review', to: 'retry', condition: { on_outcome: 'working' } },
    { from: 'retry', to: 'feedback', condition: { on_outcome: 'completed' } },
    { from: 'feedback', to: 'review', condition: 'always' },
  ],
};

const step = (node_id: string, status: string, outcome: string | null = status === 'completed' ? 'completed' : null, attempt = 1): AutopilotCircuitRunStep => ({
  id: 1, run_id: 1, node_id, agent_node_id: null, status, attempt, outcome,
  error_message: null, started_at: null, completed_at: null,
});

const states = (rows: ReturnType<typeof railRows>) => rows.map((row) => `${row.nodeId}:${row.state}`);

describe('flowOrder', () => {
  it('orders each node after everything leading into it, ignoring the edge that closes a loop', () => {
    expect(flowOrder(reviewLoop)).toEqual(['trigger', 'implement', 'review', 'merge', 'retry', 'feedback']);
  });

  it('keeps every node when the graph is one big cycle', () => {
    const cycle: CircuitGraph = {
      version: 3,
      nodes: [{ id: 'a', type: inject }, { id: 'b', type: inject }],
      edges: [{ from: 'a', to: 'b', condition: 'always' }, { from: 'b', to: 'a', condition: 'always' }],
    };
    expect(flowOrder(cycle)).toEqual(['a', 'b']);
  });
});

describe('railRows', () => {
  it('hides wiring and marks done, current and remaining work', () => {
    const rows = railRows(reviewLoop, [step('trigger', 'completed'), step('implement', 'completed'), step('review', 'running')], 'running');
    expect(states(rows)).toEqual(['implement:done', 'review:current', 'merge:remaining', 'feedback:remaining']);
  });

  it('keeps an untaken branch ahead while the loop can still come back to the step that decides it', () => {
    const steps = [step('trigger', 'completed'), step('implement', 'completed'), step('review', 'completed', 'working'), step('retry', 'completed')];
    expect(states(railRows(reviewLoop, steps, 'running'))).toEqual(['implement:done', 'review:done', 'merge:remaining', 'feedback:remaining']);
  });

  it('skips a branch the run did not take once nothing can decide it again', () => {
    const fork: CircuitGraph = {
      version: 3,
      nodes: [
        { id: 'trigger', type: { type: 'manual' } },
        { id: 'check', type: { type: 'llm_turn_classifier', target_node_id: null } },
        { id: 'ship', type: inject },
        { id: 'after_ship', type: inject },
        { id: 'nudge', type: inject },
      ],
      edges: [
        { from: 'trigger', to: 'check', condition: 'always' },
        { from: 'check', to: 'ship', condition: { on_outcome: 'completed' } },
        { from: 'ship', to: 'after_ship', condition: 'always' },
        { from: 'check', to: 'nudge', condition: { on_outcome: 'working' } },
      ],
    };
    const steps = [step('trigger', 'completed'), step('check', 'completed', 'working')];
    expect(states(railRows(fork, steps, 'running'))).toEqual(['check:done', 'ship:skipped', 'after_ship:skipped', 'nudge:remaining']);
  });

  it('counts a looped step by its latest pass and keeps the feedback it came through', () => {
    const steps = [
      step('trigger', 'completed'), step('implement', 'completed'), step('retry', 'completed'),
      step('feedback', 'completed'), step('review', 'unverified', null, 2),
    ];
    const rows = railRows(reviewLoop, steps, 'running');
    expect(states(rows)).toEqual(['implement:done', 'review:current', 'merge:remaining', 'feedback:done']);
    expect(rows.find((row) => row.nodeId === 'review')?.step?.attempt).toBe(2);
  });

  it('marks everything the run never reached as skipped once it has ended', () => {
    const steps = [step('trigger', 'completed'), step('implement', 'failed', 'failed')];
    expect(states(railRows(reviewLoop, steps, 'failed'))).toEqual(['implement:failed', 'review:skipped', 'merge:skipped', 'feedback:skipped']);
  });
});

describe('overviewNeedsYou', () => {
  const base = (over: { state?: string; dismissed?: number | null; steps?: AutopilotCircuitRunStep[]; checkpoints?: number; recovery?: boolean }) => ({
    run: { state: over.state ?? 'running', attention_dismissed_at: over.dismissed ?? null },
    steps: over.steps ?? [],
    attention: {
      revision: 1, pushes: [],
      checkpoints: Array.from({ length: over.checkpoints ?? 0 }, () => ({ node_id: 'review', attempt: 1, actions: ['recheck'] })),
      ...(over.recovery ? { recovery: { node_id: 'implement', attempt: 1, status: 'failed', options: [] } } : {}),
    },
  }) as unknown as Pick<CircuitRunOverview, 'run' | 'steps' | 'attention'>;

  it('asks for a person on an approval, an unverified checkpoint or an unacknowledged recoverable failure', () => {
    expect(overviewNeedsYou(base({ steps: [step('gate', 'blocked')] }))).toBe(true);
    expect(overviewNeedsYou(base({ checkpoints: 1 }))).toBe(true);
    expect(overviewNeedsYou(base({ state: 'failed', recovery: true }))).toBe(true);
  });

  it('stays quiet for work that is merely waiting, an acknowledged failure, or a finished run', () => {
    expect(overviewNeedsYou(base({ steps: [step('review', 'running')] }))).toBe(false);
    expect(overviewNeedsYou(base({ state: 'failed', recovery: true, dismissed: 1_760_000_000 }))).toBe(false);
    expect(overviewNeedsYou(base({ state: 'completed', steps: [step('gate', 'blocked')] }))).toBe(false);
  });
});

describe('pushOutcomeLabel', () => {
  it('words each outcome for the kind of step being moved on', () => {
    expect(pushOutcomeLabel({ type: 'await_agent_turn', target_node_id: null }, 'completed')).toBe('Agent has finished');
    expect(pushOutcomeLabel({ type: 'await_agent_turn', target_node_id: null }, 'completed', true)).toBe('Start the next review');
    expect(pushOutcomeLabel({ type: 'llm_turn_classifier', target_node_id: null }, 'completed', true)).toBe('Start the next review');
    expect(pushOutcomeLabel({ type: 'await_agent_turn', target_node_id: null }, 'completed', false, true)).toBe('Record the last round');
    expect(pushOutcomeLabel({ type: 'llm_turn_classifier', target_node_id: null }, 'completed', false, true)).toBe('Record the last round');
    expect(pushOutcomeLabel({ type: 'await_agent_turn', target_node_id: null }, 'completed', true, true)).toBe('Start the next review');
    expect(pushOutcomeLabel({ type: 'llm_turn_classifier', target_node_id: null }, 'completed')).toBe('Task is complete');
    expect(pushOutcomeLabel({ type: 'llm_turn_classifier', target_node_id: null }, 'working')).toBe('Still working');
    expect(pushOutcomeLabel({ type: 'llm_turn_classifier', target_node_id: null }, 'blocked')).toBe('Needs a person');
  });
});

describe('distinctRailLabels', () => {
  it('names steps that share a label by their node id so two feedback steps can be told apart', () => {
    const labels = distinctRailLabels(['implement', 'nudge', 'review', 'feedback'], (id) => (id === 'nudge' || id === 'feedback' ? 'Send feedback' : id));
    expect([...labels.values()]).toEqual(['implement', 'Send feedback (nudge)', 'review', 'Send feedback (feedback)']);
  });
});
