/**
 * runOverviewModel — a Circuit Run as the agent node card's Overview tab reads
 * it: the run's graph laid out in flow order, with each step marked done,
 * current, remaining or skipped, and the wording for moving a stuck step on.
 *
 * Pure and React-free so the rules are unit-tested without mounting the card.
 */

import type { AutopilotCircuitRunStep } from '../../types/generated/AutopilotCircuitRunStep';
import type { CircuitGraph } from '../../types/generated/CircuitGraph';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { CircuitRunOverview } from '../../types/generated/CircuitRunOverview';
import type { StepPushOutcome } from '../../types/generated/StepPushOutcome';
import { categoryOf } from './circuitGraphModel';
import { isTerminalRunState } from './circuitVocabulary';

export type RailState = 'done' | 'current' | 'failed' | 'remaining' | 'skipped';

export interface RailRow {
  nodeId: string;
  kind: CircuitNodeKind;
  state: RailState;
  /** The step ledger row, once the run has reached this node. */
  step: AutopilotCircuitRunStep | null;
}

/**
 * Circuit node ids in the order a person reads the flow: each node after
 * everything that leads into it. Edges that close a loop are ignored for
 * ordering, so a review loop reads top to bottom rather than wrapping back.
 * Ties keep the blueprint's own node order.
 */
export function flowOrder(graph: Pick<CircuitGraph, 'nodes' | 'edges'>): string[] {
  const position = new Map(graph.nodes.map((node, index) => [node.id, index] as const));
  const outgoing = new Map<string, string[]>();
  for (const edge of graph.edges) {
    if (!position.has(edge.from) || !position.has(edge.to)) continue;
    outgoing.set(edge.from, [...(outgoing.get(edge.from) ?? []), edge.to]);
  }
  const loopEdges = new Set<string>();
  const visiting = new Set<string>();
  const visited = new Set<string>();
  const visit = (id: string) => {
    visiting.add(id);
    for (const to of outgoing.get(id) ?? []) {
      if (visiting.has(to)) loopEdges.add(`${id}->${to}`);
      else if (!visited.has(to)) visit(to);
    }
    visiting.delete(id);
    visited.add(id);
  };
  const hasIncoming = new Set(graph.edges.map((edge) => edge.to));
  for (const node of graph.nodes) if (!hasIncoming.has(node.id) && !visited.has(node.id)) visit(node.id);
  for (const node of graph.nodes) if (!visited.has(node.id)) visit(node.id);

  const remainingInputs = new Map<string, number>(graph.nodes.map((node) => [node.id, 0]));
  for (const edge of graph.edges) {
    if (position.has(edge.from) && position.has(edge.to) && !loopEdges.has(`${edge.from}->${edge.to}`)) {
      remainingInputs.set(edge.to, (remainingInputs.get(edge.to) ?? 0) + 1);
    }
  }
  const byPosition = (a: string, b: string) => position.get(a)! - position.get(b)!;
  const ready = graph.nodes.map((node) => node.id).filter((id) => remainingInputs.get(id) === 0);
  const order: string[] = [];
  while (ready.length > 0) {
    ready.sort(byPosition);
    const id = ready.shift()!;
    order.push(id);
    for (const to of outgoing.get(id) ?? []) {
      if (loopEdges.has(`${id}->${to}`)) continue;
      const left = remainingInputs.get(to)! - 1;
      remainingInputs.set(to, left);
      if (left === 0) ready.push(to);
    }
  }
  return order;
}

/** Triggers, joins and retry counters are wiring, not work a person follows. */
function isShownOnRail(kind: CircuitNodeKind): boolean {
  return categoryOf(kind) !== 'trigger' && categoryOf(kind) !== 'join' && kind.type !== 'retry_limit';
}

const IN_FLIGHT = new Set(['running', 'pending_slot', 'queued', 'blocked', 'unverified']);

/**
 * The rail: every visible node in flow order with where the run stands on it.
 * A node the run has not reached is `skipped` when the run can no longer get
 * there — the run ended, or every path into it came from a step whose
 * outcome took another branch and which cannot run again — and `remaining`
 * otherwise. A review loop's merge step stays `remaining` while the loop can
 * still come back round to the review that decides it.
 */
export function railRows(
  graph: Pick<CircuitGraph, 'nodes' | 'edges'>,
  steps: readonly AutopilotCircuitRunStep[],
  runState: string,
): RailRow[] {
  const kinds = new Map(graph.nodes.map((node) => [node.id, node.type] as const));
  const stepByNode = new Map(steps.map((step) => [step.node_id, step] as const));
  const order = flowOrder(graph);
  const rank = new Map(order.map((id, index) => [id, index] as const));
  const ended = isTerminalRunState(runState);
  // What can still run: steps in flight, nodes an already-matched edge is
  // about to start, and everything reachable from those (loops included).
  const routes = (edge: CircuitGraph['edges'][number]) => {
    const source = stepByNode.get(edge.from);
    return source?.status === 'completed'
      && (edge.condition === 'always' || edge.condition.on_outcome === source.outcome);
  };
  const live = new Set<string>();
  const pending = [
    ...steps.filter((step) => IN_FLIGHT.has(step.status)).map((step) => step.node_id),
    ...graph.edges.filter((edge) => !stepByNode.has(edge.to) && routes(edge)).map((edge) => edge.to),
  ];
  while (pending.length > 0) {
    const id = pending.pop()!;
    if (live.has(id)) continue;
    live.add(id);
    for (const edge of graph.edges) if (edge.from === id) pending.push(edge.to);
  }
  const bypassed = new Set<string>();
  for (const id of order) {
    if (stepByNode.has(id)) continue;
    // Only edges from earlier in the flow decide; a loop edge back into this
    // node does not make it unreachable.
    const inputs = graph.edges.filter((edge) => edge.to === id && (rank.get(edge.from) ?? Infinity) < rank.get(id)!);
    const dead = inputs.length > 0 && inputs.every((edge) => {
      if (bypassed.has(edge.from)) return true;
      const source = stepByNode.get(edge.from);
      return source?.status === 'completed' && !routes(edge) && !live.has(edge.from);
    });
    if (dead) bypassed.add(id);
  }
  return order
    .filter((id) => isShownOnRail(kinds.get(id)!))
    .map((id) => {
      const step = stepByNode.get(id) ?? null;
      let state: RailState;
      if (step === null) state = ended || bypassed.has(id) ? 'skipped' : 'remaining';
      else if (step.status === 'completed') state = 'done';
      else if (step.status === 'failed') state = 'failed';
      else if (step.status === 'cancelled') state = 'skipped';
      else state = IN_FLIGHT.has(step.status) && !ended ? 'current' : 'skipped';
      return { nodeId: id, kind: kinds.get(id)!, state, step };
    });
}

/**
 * The label each rail row reads by. Kind labels repeat (two inject steps both
 * read "Send feedback"), so a label shared by more than one row carries its
 * node id to tell them apart.
 */
export function distinctRailLabels(nodeIds: readonly string[], labelOf: (nodeId: string) => string): Map<string, string> {
  const uses = new Map<string, number>();
  for (const id of nodeIds) uses.set(labelOf(id), (uses.get(labelOf(id)) ?? 0) + 1);
  return new Map(nodeIds.map((id) => {
    const label = labelOf(id);
    return [id, uses.get(label)! > 1 && label !== id ? `${label} (${id})` : label] as const;
  }));
}

/**
 * Does the run need a person? Drives the Overview tab's badge, so it only
 * counts what a person must do: an approval, an unverified checkpoint, or an
 * unacknowledged failure that can be recovered. A step that is merely
 * waiting (even one that could be moved on) does not raise the badge.
 */
export function overviewNeedsYou(overview: Pick<CircuitRunOverview, 'run' | 'steps' | 'attention'>): boolean {
  const { run, steps, attention } = overview;
  if (run.state === 'failed') return run.attention_dismissed_at === null && attention.recovery !== undefined;
  if (run.state !== 'running' && run.state !== 'paused') return false;
  return attention.checkpoints.length > 0 || steps.some((step) => step.status === 'blocked');
}

/** What moving a step on to `outcome` means for this kind of step. */
export function pushOutcomeLabel(
  kind: CircuitNodeKind,
  outcome: StepPushOutcome,
  startsNextReview = false,
): string {
  switch (outcome) {
    case 'completed':
      if (startsNextReview) return 'Start the next review';
      return kind.type === 'llm_turn_classifier' ? 'Task is complete' : 'Agent has finished';
    case 'working':
      return 'Still working';
    case 'blocked':
      return 'Needs a person';
  }
}
