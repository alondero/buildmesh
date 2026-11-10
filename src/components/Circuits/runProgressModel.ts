/**
 * runProgressModel — a Circuit Run's progress as a person reads it, rather
 * than as the stepper records it: which stage the run is in, where its review
 * rounds stand and what the latest review said, what is happening right now in
 * one sentence, and which agents it has involved.
 *
 * The rail (`runOverviewModel.railRows`) is every visible circuit node. This
 * module folds that rail into the few facts that lead the Overview; the rail
 * itself stays available as the detailed step list.
 *
 * Pure and React-free so its rules are unit-tested without mounting a card
 * (`tests/unit/run-progress-model.test.ts`).
 */

import type { AutopilotCircuitRunStep } from '../../types/generated/AutopilotCircuitRunStep';
import type { CircuitGraph } from '../../types/generated/CircuitGraph';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { CircuitRunOverview } from '../../types/generated/CircuitRunOverview';
import { isTerminalRunState } from './circuitVocabulary';
import type { RailRow } from './runOverviewModel';
import { stepVerdict, type VerdictTone } from './runStepPresentation';

type Graph = Pick<CircuitGraph, 'nodes' | 'edges'>;
type OverviewLike = Pick<CircuitRunOverview, 'run' | 'steps' | 'graph'> & { review_round_limit?: number };

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

export type StageState = 'done' | 'current' | 'failed' | 'upcoming' | 'skipped';

export interface RunStage {
  key: string;
  label: string;
  state: StageState;
  /** The rail rows (circuit node ids) the stage stands for. */
  nodeIds: string[];
}

/** The review gate of a review circuit, or `null` for any other circuit. */
export function reviewVerdictNode(graph: Graph): { id: string; reviewerId: string | null } | null {
  const node = graph.nodes.find((candidate) => candidate.type.type === 'review_verdict');
  if (node === undefined || node.type.type !== 'review_verdict') return null;
  return { id: node.id, reviewerId: node.type.target_node_id ?? null };
}

/** Every circuit node on a cycle through `id`: reachable from it and leading back to it. */
function loopThrough(graph: Graph, id: string): Set<string> {
  const reach = (start: string, next: (nodeId: string) => string[]) => {
    const seen = new Set<string>();
    const pending = [start];
    while (pending.length > 0) {
      const current = pending.pop()!;
      for (const to of next(current)) if (!seen.has(to)) { seen.add(to); pending.push(to); }
    }
    return seen;
  };
  const forward = reach(id, (from) => graph.edges.filter((edge) => edge.from === from).map((edge) => edge.to));
  const backward = reach(id, (to) => graph.edges.filter((edge) => edge.to === to).map((edge) => edge.from));
  return new Set([...forward].filter((nodeId) => backward.has(nodeId)));
}

function rawStageState(rows: readonly RailRow[]): StageState | 'partial' {
  if (rows.some((row) => row.state === 'current')) return 'current';
  if (rows.some((row) => row.state === 'failed')) return 'failed';
  if (rows.every((row) => row.state === 'done' || row.state === 'skipped')) {
    return rows.some((row) => row.state === 'done') ? 'done' : 'skipped';
  }
  return rows.some((row) => row.state === 'done') ? 'partial' : 'upcoming';
}

/**
 * The run's stages, in flow order. A review circuit reads as Prepare, Review
 * and Finish: the review stage is the reviewer and the loop through the review
 * gate, everything before it prepares, everything after it finishes. Any other
 * circuit has no stage vocabulary of its own, so each visible step it can
 * still take is a stage. A stage part-way done is current until a later stage
 * starts, and done after that.
 */
export function runStages(graph: Graph, rows: readonly RailRow[], labelOf: (nodeId: string) => string): RunStage[] {
  const verdict = reviewVerdictNode(graph);
  let groups: Array<{ key: string; label: string; rows: RailRow[] }>;
  if (verdict === null) {
    groups = rows.filter((row) => row.state !== 'skipped')
      .map((row) => ({ key: row.nodeId, label: labelOf(row.nodeId), rows: [row] }));
  } else {
    const review = loopThrough(graph, verdict.id);
    review.add(verdict.id);
    if (verdict.reviewerId !== null) review.add(verdict.reviewerId);
    const first = rows.findIndex((row) => review.has(row.nodeId));
    const prepare = first < 0 ? [] : rows.slice(0, first).filter((row) => !review.has(row.nodeId));
    const inReview = rows.filter((row) => review.has(row.nodeId));
    const finish = rows.filter((row) => !review.has(row.nodeId) && !prepare.includes(row));
    groups = [
      { key: 'prepare', label: 'Prepare', rows: prepare },
      { key: 'review', label: 'Review', rows: inReview },
      { key: 'finish', label: 'Finish', rows: finish },
    ].filter((group) => group.rows.length > 0);
  }
  const raw = groups.map((group) => rawStageState(group.rows));
  return groups.map((group, index) => {
    let state = raw[index];
    if (state === 'partial') {
      const laterStarted = raw.slice(index + 1).some((later) => later !== 'upcoming' && later !== 'skipped');
      state = laterStarted ? 'done' : 'current';
    }
    return { key: group.key, label: group.label, state, nodeIds: group.rows.map((row) => row.nodeId) };
  });
}

// ---------------------------------------------------------------------------
// Review rounds
// ---------------------------------------------------------------------------

export type RoundState =
  | 'changes_requested'
  | 'approved'
  | 'needs_attention'
  | 'reviewing'
  /** The implementation agent is addressing the previous round's findings. */
  | 'fixing'
  | 'upcoming'
  /** The run ended before this allowed round was needed. */
  | 'unused';

export interface ReviewRound {
  round: number;
  state: RoundState;
}

export interface LatestReview {
  round: number;
  label: string;
  tone: VerdictTone;
  /** The reviewer's report (or a person's note), when the run still carries it. */
  report: string | null;
  /** A person recorded this verdict rather than the reviewer. */
  byPerson: boolean;
}

export interface ReviewRounds {
  /** The round the run is on, or ended on. */
  current: number;
  /** Allowed rounds; `null` when unlimited or unknown. */
  limit: number | null;
  rounds: ReviewRound[];
  latest: LatestReview | null;
}

const IN_FLIGHT = new Set(['running', 'pending_slot', 'queued', 'unverified']);

const VERDICTS: Record<string, { label: string; tone: VerdictTone }> = {
  approved: { label: 'Approved', tone: 'success' },
  changes_requested: { label: 'Changes requested', tone: 'warning' },
  blocked: { label: 'Needs attention', tone: 'error' },
};

const TONE_STATE: Record<VerdictTone, RoundState> = {
  success: 'approved',
  warning: 'changes_requested',
  error: 'needs_attention',
};

function roundLimit(overview: OverviewLike, context: Record<string, string>): number | null {
  const parse = (value: string | undefined) => {
    const parsed = value === undefined ? Number.NaN : Number.parseInt(value, 10);
    return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
  };
  if (overview.review_round_limit !== undefined) return overview.review_round_limit > 0 ? overview.review_round_limit : null;
  if (context['review.round_limit'] !== undefined) return parse(context['review.round_limit']);
  if (context['retry.max_retries'] !== undefined) return parse(context['retry.max_retries']);
  const retry = overview.graph.nodes.find((node) => node.type.type === 'retry_limit');
  return retry?.type.type === 'retry_limit' ? parse(String(retry.type.max_retries)) : null;
}

/** The newest verdict the run context still carries, with the round it belongs to. */
function latestReview(
  verdictId: string,
  verdictStep: AutopilotCircuitRunStep | undefined,
  kind: CircuitNodeKind | undefined,
  context: Record<string, string>,
): LatestReview | null {
  const round = Number.parseInt(context[`node.${verdictId}.review_verdict_attempt`] ?? '', 10);
  const recorded = VERDICTS[context[`node.${verdictId}.review_verdict`] ?? ''];
  const byPerson = context[`node.${verdictId}.review_verdict_source`] === 'operator';
  if (recorded !== undefined && Number.isFinite(round) && round > 0) {
    const report = context[`node.${verdictId}.evaluated_output`];
    return { round, ...recorded, report: report === undefined || report.trim() === '' ? null : report, byPerson };
  }
  // Pruned context: the step's own routing outcome still says what it decided.
  if (verdictStep?.status !== 'completed') return null;
  const verdict = stepVerdict(verdictStep, kind, context);
  if (verdict === null) return null;
  const label = verdict.tone === 'success' ? 'Approved' : verdict.tone === 'warning' ? 'Changes requested' : 'Needs attention';
  return { round: verdictStep.attempt, label, tone: verdict.tone, report: null, byPerson };
}

/**
 * Where a review circuit's rounds stand, or `null` for any other circuit.
 *
 * The review gate's attempt is the round. The gate only runs again after it
 * asked for changes, so every earlier round requested changes; the current
 * round reads from the gate's own state. Once a round asked for changes and
 * the run is still live, the next round is the implementation agent fixing
 * them. Rounds the run never needed read as upcoming, or unused once it ended.
 */
export function reviewRounds(overview: OverviewLike, context: Record<string, string>): ReviewRounds | null {
  const verdict = reviewVerdictNode(overview.graph);
  if (verdict === null) return null;
  const kind = overview.graph.nodes.find((node) => node.id === verdict.id)?.type;
  const step = overview.steps.find((candidate) => candidate.node_id === verdict.id);
  const ended = isTerminalRunState(overview.run.state);
  const limit = roundLimit(overview, context);
  const exhausted = overview.steps.some((candidate) =>
    candidate.outcome === 'failed'
    && overview.graph.nodes.find((node) => node.id === candidate.node_id)?.type.type === 'retry_limit');

  const rounds: ReviewRound[] = [];
  let current = 1;
  if (step === undefined) {
    const started = overview.steps.some((candidate) => candidate.node_id === verdict.reviewerId);
    rounds.push({ round: 1, state: ended ? 'unused' : started ? 'reviewing' : 'upcoming' });
  } else {
    current = step.attempt;
    for (let round = 1; round < current; round += 1) rounds.push({ round, state: 'changes_requested' });
    let state: RoundState;
    if (IN_FLIGHT.has(step.status)) state = ended ? 'needs_attention' : 'reviewing';
    else if (step.status === 'blocked' || step.status === 'failed' || step.status === 'cancelled') state = 'needs_attention';
    else {
      const decided = stepVerdict(step, kind, context);
      state = decided === null ? 'needs_attention' : TONE_STATE[decided.tone];
    }
    rounds.push({ round: current, state });
    const roomForAnother = limit === null || current < limit;
    if (state === 'changes_requested' && !ended && !exhausted && roomForAnother) {
      rounds.push({ round: current + 1, state: 'fixing' });
    }
  }
  const total = Math.max(limit ?? 0, rounds.length);
  for (let round = rounds.length + 1; round <= total; round += 1) {
    rounds.push({ round, state: ended || rounds.some((r) => r.state === 'approved') ? 'unused' : 'upcoming' });
  }
  return { current, limit, rounds, latest: latestReview(verdict.id, step, kind, context) };
}

/** Plain words for a round's state; the tracker's colour is never the only cue. */
export function roundStateLabel(state: RoundState): string {
  switch (state) {
    case 'changes_requested': return 'Changes requested';
    case 'approved': return 'Approved';
    case 'needs_attention': return 'Needs attention';
    case 'reviewing': return 'Reviewing now';
    case 'fixing': return 'Next, once fixes are in';
    case 'upcoming': return 'Not started';
    case 'unused': return 'Not used';
  }
}

// ---------------------------------------------------------------------------
// Who is involved, and what is happening now
// ---------------------------------------------------------------------------

export interface RunAgent {
  agentNodeId: number;
  /** What the agent is to this run: the implementation agent, or the spawned role. */
  role: string;
  /** The agent is the one working on the run's current step. */
  active: boolean;
}

function targetOf(kind: CircuitNodeKind | undefined): string | null {
  return kind !== undefined && 'target_node_id' in kind ? kind.target_node_id ?? null : null;
}

/** The agent node a circuit node acts on: its own, or the one its target spawned. */
function agentFor(nodeId: string, overview: OverviewLike, kinds: ReadonlyMap<string, CircuitNodeKind>): number | null {
  const step = overview.steps.find((candidate) => candidate.node_id === nodeId);
  if (step?.agent_node_id != null) return step.agent_node_id;
  const target = targetOf(kinds.get(nodeId));
  if (target === '$source') return overview.run.source_agent_node_id;
  if (target === null) return null;
  return overview.steps.find((candidate) => candidate.node_id === target)?.agent_node_id ?? null;
}

/**
 * The agents a run has involved, implementation agent first, each with what it
 * is to the run and whether the current step is on it.
 */
export function runAgents(overview: OverviewLike, rows: readonly RailRow[], roleOf: (nodeId: string) => string): RunAgent[] {
  const kinds = new Map(overview.graph.nodes.map((node) => [node.id, node.type] as const));
  const roles = new Map<number, string>();
  const source = overview.run.source_agent_node_id;
  if (source !== null) roles.set(source, 'Implementation agent');
  for (const step of overview.steps) {
    if (step.agent_node_id === null || roles.has(step.agent_node_id)) continue;
    const kind = kinds.get(step.node_id);
    roles.set(step.agent_node_id, kind?.type === 'spawn_agent_node' ? roleOf(step.node_id) : 'Agent');
  }
  const active = new Set(rows.filter((row) => row.state === 'current')
    .map((row) => agentFor(row.nodeId, overview, kinds))
    .filter((id): id is number => id !== null));
  return [...roles].map(([agentNodeId, role]) => ({ agentNodeId, role, active: active.has(agentNodeId) }));
}

export type HeadlineTone = 'active' | 'attention' | 'success' | 'error' | 'muted';

export interface RunHeadline {
  text: string;
  tone: HeadlineTone;
}

export interface HeadlineInput {
  overview: OverviewLike;
  rows: readonly RailRow[];
  rounds: ReviewRounds | null;
  /** The finished review's own reading (`runDiagnostics.reviewResult`), when there is one. */
  review: { label: string; needsAttention: boolean } | null;
  roleOf: (nodeId: string) => string;
  agentName: (agentNodeId: number) => string | null;
}

/**
 * One sentence for what the run is doing: who is working on what, what it is
 * waiting for, or how it ended. The step list carries the detail; this is the
 * line a person reads first.
 */
export function runHeadline({ overview, rows, rounds, review, roleOf, agentName }: HeadlineInput): RunHeadline {
  const { run } = overview;
  const kinds = new Map(overview.graph.nodes.map((node) => [node.id, node.type] as const));
  if (run.state === 'pending') return { text: 'Waiting for a free run slot', tone: 'muted' };
  if (isTerminalRunState(run.state)) {
    if (review !== null) return { text: review.label, tone: review.needsAttention ? 'attention' : 'success' };
    if (run.state === 'completed') return { text: 'Run completed', tone: 'success' };
    if (run.state === 'cancelled') return { text: 'Run cancelled', tone: 'muted' };
    const failed = rows.find((row) => row.state === 'failed');
    return { text: failed === undefined ? 'Run failed' : `Failed at ${roleOf(failed.nodeId)}`, tone: 'error' };
  }
  // Read a person's decisions from the ledger itself, as the approval and
  // checkpoint cards do: a step waiting on a person may not be on the rail.
  if (overview.steps.some((step) => step.status === 'blocked')) return { text: 'Waiting for your approval', tone: 'attention' };
  const unverified = overview.steps.find((step) => step.status === 'unverified');
  if (unverified !== undefined) return { text: `Confirm whether ${roleOf(unverified.node_id)} finished`, tone: 'attention' };
  const row = rows.find((candidate) => candidate.state === 'current');
  if (row === undefined) {
    return run.state === 'paused' ? { text: 'Paused', tone: 'attention' } : { text: 'Moving to the next step', tone: 'active' };
  }
  const activity = describeActivity(row, overview, kinds, rounds, roleOf, agentName);
  return run.state === 'paused'
    ? { text: `Paused — ${activity}`, tone: 'attention' }
    : { text: activity, tone: 'active' };
}

function describeActivity(
  row: RailRow,
  overview: OverviewLike,
  kinds: ReadonlyMap<string, CircuitNodeKind>,
  rounds: ReviewRounds | null,
  roleOf: (nodeId: string) => string,
  agentName: (agentNodeId: number) => string | null,
): string {
  // Who a step acts on, by the name a person knows them by: a spawned role
  // ("Code reviewer") when it has one, else the agent's own name.
  const actor = (nodeId: string | null): string => {
    if (nodeId === '$source') {
      const source = overview.run.source_agent_node_id;
      return (source === null ? null : agentName(source)) ?? 'The implementation agent';
    }
    if (nodeId === null) return 'The agent';
    const kind = kinds.get(nodeId);
    if (kind?.type === 'spawn_agent_node' && kind.name?.trim()) return kind.name.trim();
    const agent = agentFor(nodeId, overview, kinds);
    return (agent === null ? null : agentName(agent)) ?? roleOf(nodeId);
  };
  const target = targetOf(row.kind);
  const fixing = rounds?.rounds.find((round) => round.state === 'fixing');
  switch (row.kind.type) {
    case 'review_verdict':
      return `${actor(target)} is reviewing${rounds === null ? '' : ` round ${rounds.current}`}`;
    case 'await_agent_turn':
      return fixing !== undefined && target === '$source'
        ? `${actor(target)} is fixing round ${fixing.round - 1} feedback`
        : `${actor(target)} is working`;
    case 'inject_pty':
      return fixing !== undefined && target === '$source'
        ? `Sending round ${fixing.round - 1} feedback to ${actor(target)}`
        : `Sending instructions to ${actor(target)}`;
    case 'spawn_agent_node':
      return `Starting ${actor(row.nodeId)}`;
    case 'close_agent_node':
      return `Closing ${actor(target)}`;
    case 'collaborator_check':
      return 'Checking whether approval is needed';
    case 'deterministic_verification':
      return 'Running verification checks';
    case 'llm_turn_classifier':
      return 'Checking whether the task is complete';
    case 'notify':
      return 'Wrapping up';
    case 'github_action':
      return 'Running a GitHub action';
    default:
      return `${roleOf(row.nodeId)} in progress`;
  }
}
