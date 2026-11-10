// Circuit Run overview fixtures for ui-shot-circuit-overview.steps.mjs: the
// built-in node Review circuit (`CircuitGraph::agent_review_with_provider`),
// mirrored node for node, in four states a person meets.
const K = {
  manual: { type: 'manual' },
  await: (t) => ({ type: 'await_agent_turn', target_node_id: t }),
  check: { type: 'collaborator_check', require_approval: true },
  any: { type: 'any_completed' },
  inject: (t, prompt = 'prompt') => ({ type: 'inject_pty', prompt, target_node_id: t }),
};
export const graph = {
  version: 3,
  nodes: [
    { id: 'trigger', type: K.manual },
    { id: 'await_source', type: K.await('$source') },
    { id: 'confirm_source', type: K.check },
    { id: 'source_ready', type: K.any },
    { id: 'publish', type: K.inject('$source') },
    { id: 'await_publish', type: K.await('$source') },
    { id: 'confirm_publish', type: K.check },
    { id: 'publish_ready', type: K.any },
    { id: 'reviewer', type: { type: 'spawn_agent_node', prompt: 'review', name: 'Code reviewer', provider: null, model: null, effort: null, extra_args: null, timeout_seconds: null } },
    { id: 'review_round', type: K.any },
    { id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
    { id: 'feedback', type: K.inject('$source') },
    { id: 'await_fixes', type: K.await('$source') },
    { id: 'retry', type: { type: 'retry_limit', max_retries: 5 } },
    { id: 're_review', type: K.inject('reviewer') },
    { id: 'close_approved', type: { type: 'close_agent_node', target_node_id: 'reviewer' } },
    { id: 'merge', type: K.inject('$source') },
    { id: 'approved', type: { type: 'notify', message: 'approved' } },
    { id: 'exhausted', type: { type: 'notify', message: 'exhausted' } },
  ],
  edges: [
    ['trigger', 'await_source'], ['await_source', 'source_ready', 'completed'], ['await_source', 'confirm_source', 'blocked'],
    ['confirm_source', 'source_ready'], ['source_ready', 'publish'], ['publish', 'await_publish'],
    ['await_publish', 'publish_ready', 'completed'], ['await_publish', 'confirm_publish', 'blocked'],
    ['confirm_publish', 'publish_ready'], ['publish_ready', 'reviewer'], ['reviewer', 'review_round'],
    ['review_round', 'verdict'], ['verdict', 'feedback', 'working'], ['feedback', 'await_fixes'],
    ['await_fixes', 'retry', 'completed'], ['retry', 're_review', 'completed'], ['retry', 'exhausted', 'failed'],
    ['re_review', 'review_round'], ['verdict', 'close_approved', 'completed'], ['close_approved', 'merge'],
    ['merge', 'approved'],
  ].map(([from, to, on]) => ({ from, to, condition: on ? { on_outcome: on } : 'always' })),
};

let stepId = 0;
const at = (minute) => `2026-11-10 09:${String(minute).padStart(2, '0')}:00`;
export const step = (node_id, status, over = {}) => ({
  id: ++stepId, run_id: 91, node_id, agent_node_id: null, status, attempt: 1,
  outcome: status === 'completed' ? 'completed' : null, error_message: null,
  started_at: at(over.start ?? 0), completed_at: status === 'completed' ? at(over.end ?? (over.start ?? 0) + 1) : null,
  ...over,
});

const ROUND1 = `## Review — round 1

**Verdict: CHANGES REQUESTED**

1. \`TerminalRegistry.dispose()\` is called on React unmount, so switching meshes still drops the buffer. Retain the instance and dispose only on node delete.
2. The new test asserts the buffer exists but never switches away and back — it would pass on main.
3. Nit: \`retainTerminal\` is exported but unused.`;

const prep = (agent = 1) => [
  step('trigger', 'completed', { start: 0 }),
  step('await_source', 'completed', { agent_node_id: agent, start: 0, end: 4 }),
  step('source_ready', 'completed', { start: 4 }),
  step('publish', 'completed', { agent_node_id: agent, start: 4, end: 5 }),
  step('await_publish', 'completed', { agent_node_id: agent, start: 5, end: 7 }),
  step('publish_ready', 'completed', { start: 7 }),
  step('reviewer', 'completed', { agent_node_id: 2, start: 7, end: 8 }),
];

const run = (state, context, over = {}) => ({
  id: 91, circuit_id: 3, mesh_id: 1, source_agent_node_id: 1, trigger_identity: 'manual:1762765200000', state,
  context_json: JSON.stringify(context), created_at: at(0), updated_at: at(30), attention_dismissed_at: null, ...over,
});

const base = { circuit_name: 'Review', graph, review_round_limit: 5 };
const roundOneContext = {
  'retry.attempt': '2', 'retry.max_retries': '5',
  'node.verdict.review_verdict': 'changes_requested', 'node.verdict.review_verdict_attempt': '1',
  'node.verdict.evaluated_output': ROUND1,
};

export const scenarios = {
  // Round 2: the reviewer is re-checking the fixes.
  reviewing: {
    ...base,
    run: run('running', roundOneContext),
    steps: [...prep(), step('review_round', 'completed', { attempt: 2, start: 21 }),
      step('verdict', 'running', { attempt: 2, start: 21 }),
      step('feedback', 'completed', { agent_node_id: 1, start: 12 }), step('await_fixes', 'completed', { agent_node_id: 1, start: 12, end: 20 }),
      step('retry', 'completed', { start: 20 }), step('re_review', 'completed', { agent_node_id: 2, start: 20, end: 21 })],
    attention: { revision: 7, checkpoints: [], pushes: [] },
  },
  // Round 1 found problems; the implementer is fixing them and Autopilot is unsure it finished.
  fixing: {
    ...base,
    run: run('running', { ...roundOneContext, 'retry.attempt': '1' }),
    steps: [...prep(), step('review_round', 'completed', { start: 8 }),
      step('verdict', 'completed', { start: 8, end: 12, outcome: 'working' }),
      step('feedback', 'completed', { agent_node_id: 1, start: 12 }),
      step('await_fixes', 'running', { agent_node_id: 1, start: 12 })],
    attention: { revision: 9, checkpoints: [], pushes: [{
      node_id: 'await_fixes', attempt: 1, outcomes: ['completed', 'working'],
      overrides: 'The harness reports unfinished child or background work. Waiting for its completion evidence.',
      starts_next_review: true, ends_review: false, review_again: true,
    }] },
  },
  // The source agent stopped and said it needs a person before review can start.
  approval: {
    ...base,
    run: run('running', { 'retry.attempt': '1', 'retry.max_retries': '5' }),
    steps: [step('trigger', 'completed', { start: 0 }),
      step('await_source', 'completed', { agent_node_id: 1, start: 0, end: 4, outcome: 'blocked' }),
      step('confirm_source', 'blocked', { start: 4, error_message: 'The agent asked a question: "Should I keep the legacy disposal path behind a flag?"' })],
    attention: { revision: 3, checkpoints: [], pushes: [] },
  },
  // Approved on round 3 and the merge was handed back.
  approved: {
    ...base,
    run: run('completed', { ...roundOneContext, 'retry.attempt': '3', 'node.verdict.review_verdict': 'approved',
      'node.verdict.review_verdict_attempt': '3', 'node.verdict.evaluated_output': '## Review — round 3\n\n**Verdict: APPROVED**\n\nAll earlier findings are resolved. Tests cover switching meshes away and back.' }),
    steps: [...prep(), step('review_round', 'completed', { attempt: 3, start: 30 }),
      step('verdict', 'completed', { attempt: 3, start: 30, end: 36 }),
      step('feedback', 'completed', { attempt: 2, agent_node_id: 1, start: 22 }), step('await_fixes', 'completed', { attempt: 2, agent_node_id: 1, start: 22, end: 29 }),
      step('retry', 'completed', { attempt: 2, start: 29 }), step('re_review', 'completed', { attempt: 2, agent_node_id: 2, start: 29, end: 30 }),
      step('close_approved', 'completed', { start: 36 }), step('merge', 'completed', { agent_node_id: 1, start: 36, end: 37 }),
      step('approved', 'completed', { start: 37 })],
    attention: { revision: 30, checkpoints: [], pushes: [] },
  },
};
