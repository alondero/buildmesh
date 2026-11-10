import { describe, expect, it } from 'vitest';
import { railRows } from '../../src/components/Circuits/runOverviewModel';
import {
  reviewRounds,
  runAgents,
  runHeadline,
  runStages,
  type HeadlineInput,
} from '../../src/components/Circuits/runProgressModel';
import type { AutopilotCircuitRunStep } from '../../src/types/generated/AutopilotCircuitRunStep';
import type { CircuitGraph } from '../../src/types/generated/CircuitGraph';
import type { CircuitNodeKind } from '../../src/types/generated/CircuitNodeKind';
import type { CircuitRunOverview } from '../../src/types/generated/CircuitRunOverview';

const awaitSource: CircuitNodeKind = { type: 'await_agent_turn', target_node_id: '$source' };
const check: CircuitNodeKind = { type: 'collaborator_check', require_approval: true };
const any: CircuitNodeKind = { type: 'any_completed' };
const inject = (target: string): CircuitNodeKind => ({ type: 'inject_pty', prompt: 'p', target_node_id: target });

/** The built-in node Review circuit (`CircuitGraph::agent_review_with_provider`). */
const review: CircuitGraph = {
  version: 3,
  nodes: [
    { id: 'trigger', type: { type: 'manual' } },
    { id: 'await_source', type: awaitSource },
    { id: 'confirm_source', type: check },
    { id: 'source_ready', type: any },
    { id: 'publish', type: inject('$source') },
    { id: 'await_publish', type: awaitSource },
    { id: 'confirm_publish', type: check },
    { id: 'publish_ready', type: any },
    { id: 'reviewer', type: { type: 'spawn_agent_node', prompt: 'r', name: 'Code reviewer', provider: null, model: null, effort: null, extra_args: null, timeout_seconds: null } },
    { id: 'review_round', type: any },
    { id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
    { id: 'feedback', type: inject('$source') },
    { id: 'await_fixes', type: awaitSource },
    { id: 'retry', type: { type: 'retry_limit', max_retries: 5 } },
    { id: 're_review', type: inject('reviewer') },
    { id: 'close_approved', type: { type: 'close_agent_node', target_node_id: 'reviewer' } },
    { id: 'merge', type: inject('$source') },
    { id: 'approved', type: { type: 'notify', message: 'ok' } },
    { id: 'exhausted', type: { type: 'notify', message: 'limit' } },
  ],
  edges: ([
    ['trigger', 'await_source'], ['await_source', 'source_ready', 'completed'], ['await_source', 'confirm_source', 'blocked'],
    ['confirm_source', 'source_ready'], ['source_ready', 'publish'], ['publish', 'await_publish'],
    ['await_publish', 'publish_ready', 'completed'], ['await_publish', 'confirm_publish', 'blocked'],
    ['confirm_publish', 'publish_ready'], ['publish_ready', 'reviewer'], ['reviewer', 'review_round'],
    ['review_round', 'verdict'], ['verdict', 'feedback', 'working'], ['feedback', 'await_fixes'],
    ['await_fixes', 'retry', 'completed'], ['retry', 're_review', 'completed'], ['retry', 'exhausted', 'failed'],
    ['re_review', 'review_round'], ['verdict', 'close_approved', 'completed'], ['close_approved', 'merge'],
    ['merge', 'approved'],
  ] as const).map(([from, to, on]) => ({ from, to, condition: on === undefined ? 'always' as const : { on_outcome: on } })),
};

const step = (node_id: string, status: string, over: Partial<AutopilotCircuitRunStep> = {}): AutopilotCircuitRunStep => ({
  id: 1, run_id: 9, node_id, agent_node_id: null, status, attempt: 1,
  outcome: status === 'completed' ? 'completed' : null, error_message: null, started_at: null, completed_at: null, ...over,
});

const prepared = [
  step('trigger', 'completed'), step('await_source', 'completed', { agent_node_id: 1 }), step('source_ready', 'completed'),
  step('publish', 'completed', { agent_node_id: 1 }), step('await_publish', 'completed', { agent_node_id: 1 }),
  step('publish_ready', 'completed'), step('reviewer', 'completed', { agent_node_id: 2 }),
];

function overview(state: string, steps: AutopilotCircuitRunStep[], context: Record<string, string> = {}, limit: number | undefined = 5) {
  return {
    run: {
      id: 9, circuit_id: 3, mesh_id: 1, source_agent_node_id: 1, trigger_identity: 'manual:1', state,
      context_json: JSON.stringify(context), created_at: '', updated_at: '', attention_dismissed_at: null,
    },
    steps, graph: review, review_round_limit: limit,
  } satisfies Pick<CircuitRunOverview, 'run' | 'steps' | 'graph' | 'review_round_limit'>;
}

const roundOne = { 'node.verdict.review_verdict': 'changes_requested', 'node.verdict.review_verdict_attempt': '1', 'node.verdict.evaluated_output': 'Fix the disposal path.' };
const reviewingRoundTwo = overview('running', [...prepared,
  step('review_round', 'completed', { attempt: 2 }), step('verdict', 'running', { attempt: 2 }),
  step('feedback', 'completed', { agent_node_id: 1 }), step('await_fixes', 'completed', { agent_node_id: 1 }),
  step('retry', 'completed'), step('re_review', 'completed', { agent_node_id: 2 })], roundOne);
const fixingRoundOne = overview('running', [...prepared,
  step('review_round', 'completed'), step('verdict', 'completed', { outcome: 'working' }),
  step('feedback', 'completed', { agent_node_id: 1 }), step('await_fixes', 'running', { agent_node_id: 1 })], roundOne);

const label = (id: string) => id;
const stagesOf = (o: ReturnType<typeof overview>) =>
  runStages(o.graph, railRows(o.graph, o.steps, o.run.state), label).map((stage) => `${stage.label}:${stage.state}`);
const headline = (o: ReturnType<typeof overview>, over: Partial<HeadlineInput> = {}) => runHeadline({
  overview: o, rows: railRows(o.graph, o.steps, o.run.state), rounds: reviewRounds(o, JSON.parse(o.run.context_json)),
  review: null, roleOf: label, agentName: (id) => (id === 1 ? 'fix-terminal-blank' : `agent ${id}`), ...over,
});

describe('runStages', () => {
  it('reads a review circuit as Prepare, Review and Finish with the current stage marked', () => {
    expect(stagesOf(reviewingRoundTwo)).toEqual(['Prepare:done', 'Review:current', 'Finish:upcoming']);
    expect(stagesOf(overview('running', [step('trigger', 'completed'), step('await_source', 'running')])))
      .toEqual(['Prepare:current', 'Review:upcoming', 'Finish:upcoming']);
  });

  it('keeps the reviewer and the whole loop in the Review stage', () => {
    const stages = runStages(review, railRows(review, reviewingRoundTwo.steps, 'running'), label);
    expect(stages.find((stage) => stage.key === 'review')?.nodeIds).toEqual(['reviewer', 'verdict', 'feedback', 'await_fixes', 're_review']);
    expect(stages.find((stage) => stage.key === 'prepare')?.nodeIds).toEqual(['await_source', 'confirm_source', 'publish', 'await_publish', 'confirm_publish']);
  });

  it('marks every stage done once the review is approved and merged', () => {
    const done = overview('completed', [...prepared, step('review_round', 'completed'), step('verdict', 'completed'),
      step('close_approved', 'completed'), step('merge', 'completed'), step('approved', 'completed')]);
    expect(stagesOf(done)).toEqual(['Prepare:done', 'Review:done', 'Finish:done']);
  });

  it('shows the visible steps a plain circuit can still take as its stages', () => {
    const plain: CircuitGraph = {
      version: 3,
      nodes: [{ id: 'trigger', type: { type: 'manual' } }, { id: 'nudge', type: inject('x') }, { id: 'done', type: { type: 'notify', message: '' } }],
      edges: [{ from: 'trigger', to: 'nudge', condition: 'always' }, { from: 'nudge', to: 'done', condition: 'always' }],
    };
    const rows = railRows(plain, [step('trigger', 'completed'), step('nudge', 'running')], 'running');
    expect(runStages(plain, rows, (id) => id.toUpperCase()).map((stage) => `${stage.label}:${stage.state}`)).toEqual(['NUDGE:current', 'DONE:upcoming']);
  });
});

describe('reviewRounds', () => {
  it('counts earlier rounds as changes requested and the current one as under review', () => {
    const rounds = reviewRounds(reviewingRoundTwo, roundOne)!;
    expect(rounds.current).toBe(2);
    expect(rounds.limit).toBe(5);
    expect(rounds.rounds.map((round) => round.state)).toEqual(['changes_requested', 'reviewing', 'upcoming', 'upcoming', 'upcoming']);
  });

  it('keeps the previous round’s report while the next round runs, labelled with its own round', () => {
    expect(reviewRounds(reviewingRoundTwo, roundOne)!.latest).toEqual({
      round: 1, label: 'Changes requested', tone: 'warning', report: 'Fix the disposal path.', byPerson: false,
    });
  });

  it('shows the implementation agent fixing feedback before the next round', () => {
    expect(reviewRounds(fixingRoundOne, roundOne)!.rounds.map((round) => round.state).slice(0, 3))
      .toEqual(['changes_requested', 'fixing', 'upcoming']);
  });

  it('marks the rounds an approved run never needed as unused', () => {
    const context = { 'node.verdict.review_verdict': 'approved', 'node.verdict.review_verdict_attempt': '2' };
    const approved = overview('completed', [...prepared, step('verdict', 'completed', { attempt: 2 })], context, 3);
    expect(reviewRounds(approved, context)!.rounds.map((round) => round.state)).toEqual(['changes_requested', 'approved', 'unused']);
  });

  it('reads an unlimited allowance as no limit and does not invent rounds', () => {
    const rounds = reviewRounds(overview('running', [...prepared, step('verdict', 'running', { attempt: 3 })], {}, 0), {})!;
    expect(rounds.limit).toBeNull();
    expect(rounds.rounds.map((round) => round.state)).toEqual(['changes_requested', 'changes_requested', 'reviewing']);
  });

  it('falls back to the gate’s routing outcome when the context was pruned', () => {
    const pruned = overview('failed', [...prepared, step('verdict', 'completed', { attempt: 5, outcome: 'working' }),
      step('retry', 'completed', { outcome: 'failed' })], {}, 5);
    const rounds = reviewRounds(pruned, {})!;
    expect(rounds.rounds.map((round) => round.state)).toEqual(Array(5).fill('changes_requested'));
    expect(rounds.latest).toMatchObject({ round: 5, label: 'Changes requested', report: null });
  });

  it('is null for a circuit without a review gate', () => {
    expect(reviewRounds({ ...overview('running', []), graph: { version: 3, nodes: [], edges: [] } }, {})).toBeNull();
  });
});

describe('runHeadline', () => {
  it('names who is doing what right now', () => {
    expect(headline(reviewingRoundTwo)).toEqual({ text: 'Code reviewer is reviewing round 2', tone: 'active' });
    expect(headline(fixingRoundOne).text).toBe('fix-terminal-blank is fixing round 1 feedback');
    expect(headline(overview('running', [step('trigger', 'completed'), step('await_source', 'running', { agent_node_id: 1 })])).text)
      .toBe('fix-terminal-blank is working');
  });

  it('puts a person’s decision first', () => {
    const blocked = overview('running', [step('trigger', 'completed'), step('await_source', 'completed', { outcome: 'blocked' }), step('confirm_source', 'blocked')]);
    expect(headline(blocked)).toEqual({ text: 'Waiting for your approval', tone: 'attention' });
    const unverified = overview('running', [step('trigger', 'completed'), step('await_source', 'unverified')]);
    expect(headline(unverified)).toEqual({ text: 'Confirm whether await_source finished', tone: 'attention' });
  });

  it('says a paused run is paused, and what it was doing', () => {
    expect(headline({ ...reviewingRoundTwo, run: { ...reviewingRoundTwo.run, state: 'paused' } }))
      .toEqual({ text: 'Paused — Code reviewer is reviewing round 2', tone: 'attention' });
  });

  it('reads a finished run by its review result, else by how it ended', () => {
    const done = overview('completed', prepared);
    expect(headline(done, { review: { label: 'Review approved', needsAttention: false } })).toEqual({ text: 'Review approved', tone: 'success' });
    expect(headline(done)).toEqual({ text: 'Run completed', tone: 'success' });
    expect(headline(overview('failed', [step('trigger', 'completed'), step('await_source', 'failed')]))).toEqual({ text: 'Failed at await_source', tone: 'error' });
    expect(headline(overview('pending', []))).toEqual({ text: 'Waiting for a free run slot', tone: 'muted' });
  });
});

describe('runAgents', () => {
  it('lists the implementation agent first, then each spawned role, marking who is working now', () => {
    const rows = railRows(review, reviewingRoundTwo.steps, 'running');
    expect(runAgents(reviewingRoundTwo, rows, (id) => (id === 'reviewer' ? 'Code reviewer' : id))).toEqual([
      { agentNodeId: 1, role: 'Implementation agent', active: false },
      { agentNodeId: 2, role: 'Code reviewer', active: true },
    ]);
    const fixing = runAgents(fixingRoundOne, railRows(review, fixingRoundOne.steps, 'running'), (id) => id);
    expect(fixing.map((agent) => agent.active)).toEqual([true, false]);
  });
});
