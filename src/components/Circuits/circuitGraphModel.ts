/**
 * circuitGraphModel — pure helpers behind the canvas editor (issue #1209).
 *
 * The editor's React Flow surface is deliberately thin: everything that
 * can be decided without rendering lives here so it stays unit-testable.
 * Wire shapes come from the ts-rs generated AST twins
 * (`src/types/generated/CircuitGraph.ts` & co) — never hand-declared.
 *
 * Positions are NOT part of the blueprint AST: the graph_json stays
 * canonical in Rust, and layout is derived (Dagre) with an optional
 * session-scoped position map layered on top when the user drags cards.
 */

import dagre from '@dagrejs/dagre';
import type { CircuitGraph } from '../../types/generated/CircuitGraph';
import type { CircuitNode } from '../../types/generated/CircuitNode';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { CircuitEdge } from '../../types/generated/CircuitEdge';
import type { EdgeCondition } from '../../types/generated/EdgeCondition';
import type { StepOutcome } from '../../types/generated/StepOutcome';

export type NodeCategory = 'trigger' | 'action' | 'gate' | 'join';
export type KindDiscriminator = CircuitNodeKind['type'];

/** One palette entry: a node kind plus its presentation grouping. */
export interface NodeKindSpec {
  discriminator: KindDiscriminator;
  label: string;
  category: NodeCategory;
}

// Palette order mirrors the spec: Triggers cyan, Actions green,
// Gates amber, Joins violet.
export const NODE_SPECS: readonly NodeKindSpec[] = [
  { discriminator: 'manual', label: 'Manual Trigger', category: 'trigger' },
  { discriminator: 'interval', label: 'Interval Trigger', category: 'trigger' },
  { discriminator: 'github_issue_label', label: 'Issue Label Trigger', category: 'trigger' },
  { discriminator: 'github_pull_request_label', label: 'PR Label Trigger', category: 'trigger' },
  { discriminator: 'spawn_agent_node', label: 'Spawn Agent Node', category: 'action' },
  { discriminator: 'inject_pty', label: 'Inject PTY', category: 'action' },
  { discriminator: 'github_action', label: 'GitHub Action', category: 'action' },
  { discriminator: 'set_node_status', label: 'Set Node Status', category: 'action' },
  { discriminator: 'notify', label: 'Notify', category: 'action' },
  { discriminator: 'llm_turn_classifier', label: 'LLM Turn Classifier', category: 'gate' },
  {
    discriminator: 'deterministic_verification',
    label: 'Deterministic Verification',
    category: 'gate',
  },
  { discriminator: 'collaborator_check', label: 'Collaborator Check', category: 'gate' },
  { discriminator: 'retry_limit', label: 'Retry Limit', category: 'gate' },
  { discriminator: 'all_completed', label: 'All Completed', category: 'join' },
  { discriminator: 'any_completed', label: 'Any Completed', category: 'join' },
];

const SPEC_BY_DISCRIMINATOR = new Map(NODE_SPECS.map((s) => [s.discriminator, s]));

export function specFor(discriminator: KindDiscriminator): NodeKindSpec {
  return SPEC_BY_DISCRIMINATOR.get(discriminator)!;
}

export function categoryOf(kind: CircuitNodeKind): NodeCategory {
  return specFor(kind.type).category;
}

/** Tailwind accent tokens per palette category (App.css @theme). */
export function categoryAccent(category: NodeCategory): {
  text: string;
  bg: string;
  border: string;
} {
  switch (category) {
    case 'trigger':
      return { text: 'text-accent-cyan', bg: 'bg-accent-cyan', border: 'border-accent-cyan' };
    case 'action':
      return { text: 'text-status-success', bg: 'bg-status-success', border: 'border-status-success' };
    case 'gate':
      return { text: 'text-status-warning', bg: 'bg-status-warning', border: 'border-status-warning' };
    case 'join':
      return { text: 'text-accent-violet', bg: 'bg-accent-violet', border: 'border-accent-violet' };
  }
}

/** Sensible starting config for a freshly dropped/quick-connected node.
 *  Callers pass a palette discriminator string; unknown values are
 *  impossible via the catalogue but typed loosely at this seam. */
export function defaultKind(discriminator: string): CircuitNodeKind {
  switch (discriminator) {
    case 'manual':
      return { type: 'manual' };
    case 'interval':
      return { type: 'interval', interval_seconds: 300 };
    case 'github_issue_label':
      return { type: 'github_issue_label', label: '' };
    case 'github_pull_request_label':
      return { type: 'github_pull_request_label', label: '' };
    case 'spawn_agent_node':
      return { type: 'spawn_agent_node', prompt: '', name: null };
    case 'inject_pty':
      return { type: 'inject_pty', prompt: '' };
    case 'github_action':
      return { type: 'github_action', action: 'add_label', label: null, comment: null };
    case 'set_node_status':
      return { type: 'set_node_status', status: 'completed' };
    case 'notify':
      return { type: 'notify', message: '' };
    case 'llm_turn_classifier':
      return { type: 'llm_turn_classifier' };
    case 'deterministic_verification':
      return { type: 'deterministic_verification', command: '' };
    case 'collaborator_check':
      return { type: 'collaborator_check', require_approval: true };
    case 'retry_limit':
      return { type: 'retry_limit', max_retries: 3 };
    case 'all_completed':
      return { type: 'all_completed' };
    case 'any_completed':
      return { type: 'any_completed' };
    default:
      throw new Error(`unknown node discriminator: ${discriminator}`);
  }
}

/** Unique per-kind id (`notify_1`, `notify_2`, …) for a new node. */
export function makeNodeId(discriminator: string, existingIds: Iterable<string>): string {
  const taken = new Set(existingIds);
  let n = 1;
  while (taken.has(`${discriminator}_${n}`)) n += 1;
  return `${discriminator}_${n}`;
}

function truncate(s: string, max: number): string {
  const t = s.replace(/\s+/g, ' ').trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
}

/** One-line config summary for the node card's second row. */
export function configSummary(kind: CircuitNodeKind): string {
  switch (kind.type) {
    case 'manual':
      return 'fire by hand';
    case 'interval':
      return `every ${kind.interval_seconds}s`;
    case 'github_issue_label':
      return `label "${kind.label || '?'}"`;
    case 'github_pull_request_label':
      return `label "${kind.label || '?'}"`;
    case 'spawn_agent_node': {
      const name = kind.name ? ` “${kind.name}”` : '';
      return `${truncate(kind.prompt, 40) || '(no prompt)'}${name}`;
    }
    case 'inject_pty':
      return truncate(kind.prompt, 48) || '(no prompt)';
    case 'github_action': {
      const detail = kind.label ? ` "${kind.label}"` : kind.comment ? ` "${kind.comment}"` : '';
      return `${kind.action}${detail}`;
    }
    case 'set_node_status':
      return `status → ${kind.status}`;
    case 'notify':
      return truncate(kind.message, 48) || '(no message)';
    case 'llm_turn_classifier':
      return 'classify each turn';
    case 'deterministic_verification':
      return truncate(kind.command, 48) || '(no command)';
    case 'collaborator_check':
      return kind.require_approval ? 'requires approval' : 'auto-pass';
    case 'retry_limit':
      return `max ${kind.max_retries} retries`;
    case 'all_completed':
      return 'continue when all done';
    case 'any_completed':
      return 'continue when any done';
  }
}

export function parseGraph(json: string): CircuitGraph {
  const parsed: unknown = JSON.parse(json);
  if (
    typeof parsed !== 'object' ||
    parsed === null ||
    !Array.isArray((parsed as CircuitGraph).nodes) ||
    !Array.isArray((parsed as CircuitGraph).edges)
  ) {
    throw new Error('invalid circuit graph_json: expected {version, nodes, edges}');
  }
  const graph = parsed as CircuitGraph;
  // The Rust boundary defaults a missing edge condition to `always`
  // (`#[serde(default)]`); hand-edited JSON gets the same grace here.
  return {
    ...graph,
    edges: graph.edges.map((e) => ({ ...e, condition: e.condition ?? 'always' })),
  };
}

/**
 * Order-insensitive serialization for dirty tracking: nodes sorted by
 * id, edges by their key. Raw `JSON.stringify` would flag an add+delete
 * pair or an equivalent reorder as unsaved changes.
 */
export function stableGraphJson(graph: CircuitGraph): string {
  return JSON.stringify({
    version: graph.version,
    nodes: [...graph.nodes].sort((a, b) => a.id.localeCompare(b.id)),
    edges: [...graph.edges].sort((a, b) => edgeKey(a).localeCompare(edgeKey(b))),
  });
}

// ---------------------------------------------------------------------------
// Fuzzy search — quick-connect menu ranking.
// ---------------------------------------------------------------------------

/** Subsequence match score; higher is better, null means "no match". */
export function fuzzyScore(query: string, text: string): number | null {
  if (query === '') return 0;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  const exact = t.indexOf(q);
  if (exact !== -1) {
    // Contiguous hit: earlier + shorter is better.
    return 1000 - exact - (t.length - q.length) * 0.1;
  }
  let score = 0;
  let ti = 0;
  let streak = 0;
  for (let qi = 0; qi < q.length; qi++) {
    const found = t.indexOf(q[qi], ti);
    if (found === -1) return null;
    streak = found === ti && qi > 0 ? streak + 1 : 0;
    score += 10 - Math.min(found - ti, 9) + streak * 2;
    ti = found + 1;
  }
  return score;
}

/** Catalogue entries matching `query`, best first (all of them when empty). */
export function fuzzyFilterSpecs(query: string): NodeKindSpec[] {
  const scored = NODE_SPECS.map((spec) => ({
    spec,
    score: Math.max(fuzzyScore(query, spec.label) ?? -Infinity, fuzzyScore(query, spec.discriminator) ?? -Infinity),
  })).filter((s) => s.score > -Infinity);
  scored.sort((a, b) => b.score - a.score || a.spec.label.localeCompare(b.spec.label));
  return query === '' ? [...NODE_SPECS] : scored.map((s) => s.spec);
}

// ---------------------------------------------------------------------------
// Mustache autocomplete — context namespace chips.
// ---------------------------------------------------------------------------

/** Context paths offered by `{{` autocomplete. Mirrors the stepper's
 *  `CircuitContext` namespaces; verification/retry resolve empty until
 *  their gates populate them. */
export const MUSTACHE_PATHS: readonly string[] = [
  'circuit.id',
  'circuit.name',
  'circuit.mesh_id',
  'circuit.run_id',
  'node.id',
  'issue.number',
  'issue.title',
  'issue.body',
  'issue.author',
  'issue.url',
  'issue.labels',
  'pr.number',
  'pr.title',
  'pr.body',
  'pr.author',
  'pr.url',
  'pr.head_ref',
  'pr.labels',
  'verification.outcome',
  'verification.command',
  'retry.attempt',
  'retry.max_retries',
];

/**
 * Insert `{{ path }}` at a caret sitting just after typed `{{` (the
 * autocomplete trigger). Returns the new text and caret position.
 */
export function insertMustache(
  text: string,
  caret: number,
  path: string
): { text: string; caret: number } {
  const before = text.slice(0, caret);
  const openStart = before.lastIndexOf('{{');
  const insertion = `{{ ${path} }}`;
  return {
    text: `${before.slice(0, Math.max(openStart, 0))}${insertion}${text.slice(caret)}`,
    caret: Math.max(openStart, 0) + insertion.length,
  };
}

// ---------------------------------------------------------------------------
// Gates: named outcome ports + edge condition badges.
// ---------------------------------------------------------------------------

/**
 * Named outcome source handles for gate nodes that route by outcome;
 * null for every other kind (single anonymous handle). Order is the
 * handle's top-to-bottom render order.
 */
export function sourceOutcomes(kind: CircuitNodeKind): StepOutcome[] | null {
  switch (kind.type) {
    case 'llm_turn_classifier':
      return ['completed', 'blocked', 'working'];
    case 'deterministic_verification':
      return ['green', 'red'];
    default:
      return null;
  }
}

export function conditionLabel(condition: EdgeCondition): string {
  return condition === 'always' ? 'Always' : `OnOutcome(${condition.on_outcome})`;
}

/** Stable key identifying one edge (parallel edges differ by condition). */
export function edgeKey(edge: Pick<CircuitEdge, 'from' | 'to' | 'condition'>): string {
  return edge.condition === 'always'
    ? `${edge.from}->${edge.to}:always`
    : `${edge.from}->${edge.to}:on:${edge.condition.on_outcome}`;
}

/**
 * Edge condition implied by the source handle a connection starts from:
 * a named gate outcome port routes `OnOutcome(...)`, anything else is
 * unconditional. Shared by quick-connect auto-wiring and manual connects.
 */
export function conditionFromHandle(
  sourceKind: CircuitNodeKind | undefined,
  handleId: string | null
): EdgeCondition {
  if (sourceKind === undefined || handleId === null) return 'always';
  const outcomes = sourceOutcomes(sourceKind);
  return outcomes !== null && (outcomes as string[]).includes(handleId)
    ? { on_outcome: handleId as StepOutcome }
    : 'always';
}

/**
 * Tailwind text class for one ledger status — the single status→colour
 * vocabulary shared by node cards and the run-history drawer.
 */
export function statusTextClass(status: string): string {
  switch (status) {
    case 'completed':
      return 'text-status-success';
    case 'running':
      return 'text-accent-cyan';
    case 'blocked':
    case 'paused':
      return 'text-status-warning';
    case 'failed':
    case 'cancelled':
      return 'text-status-error';
    default:
      return 'text-text-muted';
  }
}

// ---------------------------------------------------------------------------
// Run observation — status overlays + traversed-path highlighting.
// ---------------------------------------------------------------------------

/** The slice of an `AutopilotCircuitRunStep` the overlays need. */
export interface StepLike {
  node_id: string;
  status: string;
  outcome: string | null;
  error_message: string | null;
  started_at: string | null;
  completed_at: string | null;
}

/** Wall-clock step duration from the ledger timestamps, ms or null.
 *  Tolerates both SQLite's "YYYY-MM-DD HH:MM:SS" (treated as UTC) and
 *  plain ISO-8601 — appending "Z" unconditionally would corrupt an
 *  already-ISO timestamp into a silent NaN. */
export function stepDurationMs(step: Pick<StepLike, 'started_at' | 'completed_at'>): number | null {
  if (step.started_at === null || step.completed_at === null) return null;
  const toMs = (s: string): number => {
    const direct = Date.parse(s);
    if (!Number.isNaN(direct)) return direct;
    return Date.parse(s.replace(' ', 'T') + 'Z');
  };
  const start = toMs(step.started_at);
  const end = toMs(step.completed_at);
  if (Number.isNaN(start) || Number.isNaN(end)) return null;
  return end - start;
}

/**
 * Edge keys traversed by a run: each finished step routes along edges
 * whose condition matches its recorded outcome; an Always edge carries
 * any outcome once its source step has actually started (queued steps
 * light nothing — the path hasn't been walked yet).
 */
export function traversedEdgeKeys(
  steps: Array<Pick<StepLike, 'node_id' | 'status' | 'outcome'>>,
  edges: CircuitEdge[]
): Set<string> {
  const keys = new Set<string>();
  for (const step of steps) {
    if (step.status === 'queued' || step.status === 'pending_slot') continue;
    for (const edge of edges) {
      if (edge.from !== step.node_id) continue;
      if (
        edge.condition === 'always' ||
        (step.outcome !== null && edge.condition.on_outcome === step.outcome)
      ) {
        keys.add(edgeKey(edge));
      }
    }
  }
  return keys;
}

/**
 * Derive the persisted blueprint AST from the editor's working copy of
 * React Flow nodes/edges (the canonical shape stays the Rust one).
 */
export function toGraph(
  nodes: Array<{ data: { circuitNode: CircuitNode } }>,
  edges: Array<{ source: string; target: string; data?: { condition?: EdgeCondition } }>
): CircuitGraph {
  return {
    version: 1,
    nodes: nodes.map((n) => n.data.circuitNode),
    edges: edges.map((e) => ({
      from: e.source,
      to: e.target,
      condition: e.data?.condition ?? 'always',
    })),
  };
}

// ---------------------------------------------------------------------------
// Dagre auto-layout.
// ---------------------------------------------------------------------------

/** Card size used for spacing; the real card may vary a few px. */
export const NODE_SIZE = { width: 220, height: 64 };

/**
 * Lay out the whole graph with Dagre in `direction`, returning a
 * position per node id (disconnected nodes included).
 */
export function layoutPositions(
  graph: Pick<CircuitGraph, 'nodes' | 'edges'>,
  direction: 'LR' | 'TB'
): Map<string, { x: number; y: number }> {
  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: direction, nodesep: 40, ranksep: 80, marginx: 20, marginy: 20 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const node of graph.nodes) {
    g.setNode(node.id, { width: NODE_SIZE.width, height: NODE_SIZE.height });
  }
  for (const edge of graph.edges) {
    g.setEdge(edge.from, edge.to);
  }
  dagre.layout(g);

  const positions = new Map<string, { x: number; y: number }>();
  for (const node of graph.nodes) {
    const laid = g.node(node.id) as { x: number; y: number } | undefined;
    // Dagre centres on the node; React Flow wants the top-left corner.
    positions.set(node.id, {
      x: (laid?.x ?? 0) - NODE_SIZE.width / 2,
      y: (laid?.y ?? 0) - NODE_SIZE.height / 2,
    });
  }
  return positions;
}
