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

/** Mirrors Rust `CIRCUIT_GRAPH_VERSION`. Writers emit this; parse upgrades v1. */
export const CIRCUIT_GRAPH_VERSION = 2;

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
  { discriminator: 'close_agent_node', label: 'Close Agent Node', category: 'action' },
  { discriminator: 'notify', label: 'Notify', category: 'action' },
  { discriminator: 'llm_turn_classifier', label: 'LLM Turn Classifier', category: 'gate' },
  { discriminator: 'await_agent_turn', label: 'Wait for Task Completion', category: 'gate' },
  { discriminator: 'review_verdict', label: 'Review Verdict', category: 'gate' },
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
      return {
        type: 'spawn_agent_node',
        prompt: '',
        name: null,
        provider: null,
        model: null,
        effort: null,
        extra_args: null,
      };
    case 'inject_pty':
      return { type: 'inject_pty', prompt: '', target_node_id: null };
    case 'github_action':
      return { type: 'github_action', action: 'add_label', open_pr_policy: null, label: null, comment: null };
    case 'set_node_status':
      return { type: 'set_node_status', status: 'completed', target_node_id: null };
    case 'close_agent_node':
      return { type: 'close_agent_node', target_node_id: null };
    case 'notify':
      return { type: 'notify', message: '' };
    case 'llm_turn_classifier':
      return { type: 'llm_turn_classifier', target_node_id: null };
    case 'await_agent_turn':
      return { type: 'await_agent_turn', target_node_id: '$source' };
    case 'review_verdict':
      return { type: 'review_verdict', target_node_id: null };
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
    case 'await_agent_turn':
      return 'classify completion, including an earlier turn';
    case 'review_verdict':
      return 'approval, changes requested, or needs attention';
    case 'close_agent_node':
      return kind.target_node_id ? `close ${kind.target_node_id}` : 'close nearest agent';
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

/**
 * Compatibility marker for review blueprints written before the Rust AST
 * gained an explicit `blueprint` field. This deliberately checks only the
 * stable structural contract and never an author-editable prompt.
 */
function isLegacyIssueReviewGraph(graph: Pick<CircuitGraph, 'nodes'>): boolean {
  const kind = (id: string) => graph.nodes.find((node) => node.id === id)?.type;
  const trigger = kind('trigger');
  const reviewer = kind('reviewer');
  const reviewPrompt = kind('review_prompt');
  const closeReviewer = kind('close_reviewer');
  return (
    trigger?.type === 'github_issue_label' &&
    reviewer?.type === 'spawn_agent_node' &&
    reviewPrompt?.type === 'inject_pty' &&
    reviewPrompt.target_node_id === 'reviewer' &&
    closeReviewer?.type === 'close_agent_node' &&
    closeReviewer.target_node_id === 'reviewer'
  );
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
  // v1 blueprints upgrade in-memory to v2 (version stamp + optional
  // field defaults) so a canvas save persists the current AST without
  // looking dirty on open (issue #1356).
  const version =
    typeof graph.version === 'number' && graph.version >= CIRCUIT_GRAPH_VERSION
      ? graph.version
      : CIRCUIT_GRAPH_VERSION;
  const blueprint =
    graph.blueprint ??
    (isLegacyIssueReviewGraph(graph) ? 'issue_driven_autopilot_review' : null);
  return {
    ...graph,
    version,
    blueprint,
    nodes: graph.nodes.map((n) => ({ ...n, type: normalizeKind(n.type) })),
    edges: graph.edges.map((e) => ({ ...e, condition: e.condition ?? 'always' })),
  };
}

/** Fill v2 optional fields so a stored v1 node satisfies the generated type. */
function normalizeKind(kind: CircuitNodeKind): CircuitNodeKind {
  switch (kind.type) {
    case 'spawn_agent_node':
      return {
        type: 'spawn_agent_node',
        prompt: kind.prompt,
        name: kind.name ?? null,
        provider: kind.provider ?? null,
        model: kind.model ?? null,
        effort: kind.effort ?? null,
        extra_args: kind.extra_args ?? null,
      };
    case 'inject_pty':
      return {
        type: 'inject_pty',
        prompt: kind.prompt,
        target_node_id: kind.target_node_id ?? null,
      };
    case 'set_node_status':
      return {
        type: 'set_node_status',
        status: kind.status,
        target_node_id: kind.target_node_id ?? null,
      };
    case 'close_agent_node':
      return {
        type: 'close_agent_node',
        target_node_id: kind.target_node_id ?? null,
      };
    case 'github_action':
      return {
        type: 'github_action',
        action: kind.action,
        open_pr_policy: kind.open_pr_policy ?? null,
        label: kind.label ?? null,
        comment: kind.comment ?? null,
      };
    case 'llm_turn_classifier':
      return {
        type: 'llm_turn_classifier',
        target_node_id: kind.target_node_id ?? null,
      };
    default:
      return kind;
  }
}

/**
 * Order-insensitive serialization for dirty tracking: nodes sorted by
 * id, edges by their key. Raw `JSON.stringify` would flag an add+delete
 * pair or an equivalent reorder as unsaved changes.
 */
export function stableGraphJson(graph: CircuitGraph): string {
  return JSON.stringify({
    version: graph.version,
    blueprint: graph.blueprint ?? null,
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
  'issue.prefill',
  'autopilot.finish_prompt',
  'autopilot.wrapup_correction',
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
  'source.agent_id',
  'source.name',
  'source.path',
  'source.base_ref',
  'source.output',
];

/** Display labels + order for the namespace chips that group the
 *  autocomplete popup and the inspector's context reference. Order is
 *  the render order top-to-bottom inside the popup / drawer — circuit
 *  identity first (always-live), then triggers / actions, then gates. */
export interface MustacheGroupSpec {
  /** Top-level key this group covers (the substring before the first `.`). */
  namespace: string;
  /** Header label rendered above the chips. */
  label: string;
  /** Stable description shown in the inspector reference drawer. */
  description: string;
}

export const MUSTACHE_GROUPS: readonly MustacheGroupSpec[] = [
  { namespace: 'circuit', label: 'Circuit Metadata', description: 'Identity of this circuit and the current run.' },
  { namespace: 'source', label: 'Triggering Agent', description: 'Available when launched from an Agent Node; empty on ordinary Trigger Now runs.' },
  { namespace: 'node', label: 'Current Node', description: 'Identifier of the node whose template is being resolved.' },
  { namespace: 'issue', label: 'Issue Context', description: 'GitHub issue that fired the trigger (issue-label runs).' },
  { namespace: 'pr', label: 'Pull Request', description: 'PR payload — populated when a github_action opens or a PR-label trigger fires.' },
  { namespace: 'autopilot', label: 'Autopilot', description: 'Rendered wrap-up prompt and mesh-level Autopilot policy.' },
  { namespace: 'verification', label: 'Verification', description: 'Last verification gate outcome (Green/Red) and command.' },
  { namespace: 'retry', label: 'Retries', description: 'Current retry attempt and the configured cap.' },
  { namespace: 'spawn_output', label: 'Node Outputs', description: 'Terminal output of upstream agent nodes (one chip per reachable spawn).' },
];

/** Which canonical namespace a path belongs to. `spawn_output` is
 *  virtual — there is no top-level `spawn_output` namespace in the
 *  context map; the chip text itself encodes the node id
 *  (`node.<id>.output`). The two `isReachable` implementations
 *  (textarea autocomplete + inspector drawer) must use the same
 *  prefix key here so they cannot disagree on which paths are live. */
export function groupForPath(path: string): string {
  const prefix = path.split('.', 1)[0];
  if (prefix === 'node' && path.endsWith('.output')) return 'spawn_output';
  return prefix;
}

/**
 * Insert `{{ path }}` at a caret sitting just after typed `{{` (the
 * autocomplete trigger). Returns the new text and caret position.
 *
 * If the user has already typed the matching `}}` past the caret (or
 * has the caret anywhere inside the placeholder and a `}}` further
 * along), consume it as part of the replacement — otherwise the
 * insertion leaves a stray `}}` after the new path that becomes dead
 * text in the rendered template (`{{ path }}}`). The `}}` is only
 * consumed when it lies *between the caret and the next line break
 * or end of text*, so an unrelated `}}` later in the line is not
 * affected.
 */
export function insertMustache(
  text: string,
  caret: number,
  path: string
): { text: string; caret: number } {
  const before = text.slice(0, caret);
  const openStart = before.lastIndexOf('{{');
  // Anchor fallback: if there's no `{{` between the start of the text
  // and the caret (e.g. user clicked a chip without typing one first),
  // fall back to `caret` so `before.slice(0, anchor)` returns the
  // entire prefix verbatim instead of erasing it (issue #1359 round-3
  // review: `Math.max(-1, 0) = 0` silently dropped everything before
  // the caret when no opener was present).
  const anchor = openStart === -1 ? caret : openStart;
  // Search for `}}` starting from the caret forward. Anything past
  // the caret is preserved verbatim — including any `}}` the user
  // already typed — UNLESS that `}}` is part of the current
  // placeholder, in which case we consume it so the result is clean.
  const after = text.slice(caret);
  // `[^}\n]*` — anything except `}` and newline; then `}}`. Anchored
  // at the start of `after` (i.e. just past the caret). If a `}}`
  // appears earlier in the same line we still consume it because the
  // regex matches the longest such prefix.
  const closeMatch = after.match(/^[^}\n]*\}\}/);
  const closeLen = closeMatch !== null ? closeMatch[0].length : 0;
  const insertion = `{{ ${path} }}`;
  return {
    text: `${before.slice(0, anchor)}${insertion}${after.slice(closeLen)}`,
    caret: anchor + insertion.length,
  };
}

// ---------------------------------------------------------------------------
// Upstream reachability (issue #1359).
//
// A pure, graph-only view of "what template variables does this node
// actually have by the time it executes?". The stepper populates the
// runtime `CircuitContext` from the upstream nodes it walks through, so
// reachability is equivalent to "is there a producer upstream?":
//
//   - `issue.*`     ← an issue-label trigger upstream
//   - `pr.*`        ← a PR-label trigger upstream OR an `open_pr` action
//   - `node.<id>.*` ← every upstream `spawn_agent_node` (terminal output
//                     / status write; the worker commits these to the
//                     context after the agent yields or finishes)
//   - `verification.*` ← an upstream `deterministic_verification` gate
//   - `retry.*`     ← an upstream `retry_limit` gate
//
// `circuit.*` and `node.id` are always available — the trigger wrapper
// sets them at run creation, and `with_node` is called per step.
// ---------------------------------------------------------------------------

/** Per-kind booleans so callers can render a stable schema without
 *  leaking the BFS internals. `nodeOutputIds` lists the upstream
 *  spawn-agent node ids (one chip per id in the autocomplete / drawer). */
export interface ReachableContext {
  triggers: {
    manual: boolean;
    interval: boolean;
    issue: boolean;
    pullRequest: boolean;
  };
  pullRequest: boolean;
  nodeOutputIds: string[];
  gates: {
    verification: boolean;
    retry: boolean;
    llmClassifier: boolean;
    collaborator: boolean;
  };
  metadata: boolean;
}

const EMPTY_REACHABLE: ReachableContext = {
  triggers: { manual: false, interval: false, issue: false, pullRequest: false },
  pullRequest: false,
  nodeOutputIds: [],
  gates: { verification: false, retry: false, llmClassifier: false, collaborator: false },
  metadata: false,
};

/** Index edges by `to` so the BFS step is O(1) per node. Done once per
 *  call — graphs are tiny, so allocating a fresh index is cheaper than
 *  caching. */
function incomingByTarget(
  edges: ReadonlyArray<Pick<CircuitEdge, 'from' | 'to'>>
): Map<string, string[]> {
  const m = new Map<string, string[]>();
  for (const e of edges) {
    const list = m.get(e.to);
    if (list === undefined) m.set(e.to, [e.from]);
    else list.push(e.from);
  }
  return m;
}

function indexNodesById(
  nodes: ReadonlyArray<CircuitNode>
): Map<string, CircuitNode> {
  const m = new Map<string, CircuitNode>();
  for (const n of nodes) m.set(n.id, n);
  return m;
}

/**
 * Walk edges backwards from `nodeId` and report which context
 * namespaces a producer upstream is capable of populating. Pure —
 * operates on the AST, never on run-time state.
 *
 * **Important**: the BFS seeds from `nodeId`'s *immediate predecessors*
 * — the selected node itself is never visited, because no node can
 * read its own terminal output before it has executed. This is the
 * reachability contract `{{ node.<id>.output }}` honours: the spawn's
 * own initial-prompt template can NEVER interpolate its own output,
 * only downstream consumers (after the worker writes the context).
 *
 * Termination: the visited set keeps BFS finite on cycles (bounded or
 * not); the slice-1 validator rejects unbounded loops so any cycle the
 * editor loads has at least one bounded gate.
 *
 * Returns an all-false `ReachableContext` when `nodeId` is unknown or
 * the graph is empty — that keeps the call site branch-free.
 */
export function getReachableContext(
  nodeId: string,
  graph: Pick<CircuitGraph, 'nodes' | 'edges'>
): ReachableContext {
  const byId = indexNodesById(graph.nodes);
  if (!byId.has(nodeId)) return EMPTY_REACHABLE;

  const incoming = incomingByTarget(graph.edges);
  const reachable: ReachableContext = {
    triggers: { manual: false, interval: false, issue: false, pullRequest: false },
    pullRequest: false,
    nodeOutputIds: [],
    gates: { verification: false, retry: false, llmClassifier: false, collaborator: false },
    metadata: true, // circuit.* + node.id are always populated by the trigger wrapper
  };
  // Dedupe spawn outputs by id so a diamond that re-merges onto the
  // same spawn doesn't double-list its output chip.
  const spawnOutputs = new Set<string>();

  // Seed: every immediate predecessor of `nodeId`. We DO include the
  // node itself when it's its own predecessor (a self-loop), because
  // a verification gate or retry-limit looping back on itself IS a
  // valid reachability target. The temporal-paradox guard is enforced
  // separately for spawn outputs (a node cannot read its own
  // terminal output before it has executed).
  const queue: string[] = incoming.get(nodeId)?.slice() ?? [];
  const visited = new Set<string>();
  while (queue.length > 0) {
    const id = queue.shift()!;
    if (visited.has(id)) continue;
    visited.add(id);
    const node = byId.get(id);
    if (node === undefined) continue;
    switch (node.type.type) {
      case 'manual':
        reachable.triggers.manual = true;
        break;
      case 'interval':
        reachable.triggers.interval = true;
        break;
      case 'github_issue_label':
        reachable.triggers.issue = true;
        break;
      case 'github_pull_request_label':
        reachable.triggers.pullRequest = true;
        reachable.pullRequest = true;
        break;
      case 'github_action':
        // `open_pr` is the only GitHub action that mutates the context
        // (issue #1357 slice 2). Any other action still consumes
        // existing context but contributes nothing new upstream.
        if (node.type.action === 'open_pr') reachable.pullRequest = true;
        break;
      case 'spawn_agent_node':
        // Temporal-paradox guard: `node.<id>.output` is only
        // reachable for DOWNSTREAM nodes, not for the spawn itself.
        // A node cannot read its own terminal output before it has
        // executed.
        if (id !== nodeId) spawnOutputs.add(id);
        break;
      case 'deterministic_verification':
        reachable.gates.verification = true;
        break;
      case 'retry_limit':
        reachable.gates.retry = true;
        break;
      case 'llm_turn_classifier':
        reachable.gates.llmClassifier = true;
        break;
      case 'collaborator_check':
        reachable.gates.collaborator = true;
        break;
      default:
        // inject_pty / notify / set_node_status / joins contribute
        // no new namespaces — they're consumers, not producers.
        break;
    }
    for (const upstreamId of incoming.get(id) ?? []) {
      if (!visited.has(upstreamId)) queue.push(upstreamId);
    }
  }

  reachable.nodeOutputIds = [...spawnOutputs].sort();
  return reachable;
}

/** Upstream spawn-agent ids for a node. A pure shortcut for the
 *  InspectorPanel's `target_node_id` picker on InjectPty / SetNodeStatus
 *  — both effects target an agent, so the picker is just this list.
 *  Sorted by id for stable dropdown ordering. */
export function upstreamSpawnTargets(
  nodeId: string,
  graph: Pick<CircuitGraph, 'nodes' | 'edges'>
): string[] {
  return getReachableContext(nodeId, graph).nodeOutputIds;
}

/** Single source of truth for "is this `{{ path }}` chip live in the
 *  selected branch?". Both the MustacheTextarea popup and the
 *  inspector's context-reference drawer consume this — any change to
 *  the rules MUST land here.
 *
 *  Reachability contract:
 *    - `circuit.*`         → always live (trigger wrapper sets them)
 *    - `node.id`           → always live (with_node is called per step)
 *    - `node.<id>.output`  → live iff `id ∈ reachable.nodeOutputIds`
 *    - `issue.*`           → live iff a github_issue_label trigger is upstream
 *    - `pr.*`              → live iff a PR-label trigger OR `open_pr` action is upstream
 *    - `verification.*`    → live iff a `deterministic_verification` gate is upstream
 *    - `retry.*`           → live iff a `retry_limit` gate is upstream
 *
 *  Pass `reachable: undefined` to treat every chip as live (the
 *  ad-hoc reuse case where no node id is in scope). */
export function isReachablePath(
  path: string,
  reachable: ReachableContext | undefined
): boolean {
  if (reachable === undefined) return true;
  const ns = groupForPath(path);
  switch (ns) {
    case 'circuit':
      return true;
    case 'source':
      return reachable.triggers.manual;
    case 'node':
      // `node.id` is always live (with_node is called per step).
      // Spawn-output chips (`node.<id>.output`) are routed to the
      // `spawn_output` namespace by `groupForPath` and handled
      // below — the `node` case only needs to recognise the one
      // `node.id` chip and treat every other `node.<garbage>` as
      // unreachable (issue #1359 round-3 review: the previous
      // duplicate `nodeOutputIds.includes` check was dead code
      // because the dispatch never reaches here for `.output` paths).
      if (path === 'node.id') return true;
      return false;
    case 'issue':
      return reachable.triggers.issue;
    case 'pr':
      return reachable.pullRequest;
    case 'autopilot':
      return true;
    case 'verification':
      return reachable.gates.verification;
    case 'retry':
      return reachable.gates.retry;
    case 'spawn_output':
      if (path.endsWith('.output')) {
        const id = path.slice('node.'.length, -'.output'.length);
        return reachable.nodeOutputIds.includes(id);
      }
      return false;
    default:
      return false;
  }
}

/** A static example value rendered in the inspector reference drawer so
 *  the user can see the *shape* of the resolved string. The runtime
 *  `CircuitContext` resolves to a real value at execution time; this is
 *  purely an authoring hint. */
export function sampleValueForPath(path: string): string {
  switch (path) {
    case 'source.agent_id': return '42';
    case 'source.name': return 'Fix parser';
    case 'source.path': return '/repo/.claude/worktrees/fix-parser';
    case 'source.base_ref': return 'origin/main';
    case 'source.output': return 'Implementation complete; relevant checks passed.';
    case 'circuit.id':
      return '7';
    case 'circuit.name':
      return 'nightly-sweep';
    case 'circuit.mesh_id':
      return '42';
    case 'circuit.run_id':
      return '173';
    case 'node.id':
      return 'spawn_1';
    case 'issue.number':
      return '1208';
    case 'issue.title':
      return 'React to the world';
    case 'issue.body':
      return 'the body of the issue';
    case 'issue.author':
      return 'octocat';
    case 'issue.url':
      return 'https://github.com/alondero/buildmesh/issues/1208';
    case 'issue.labels':
      return 'bug, ready-for-agent';
    case 'issue.prefill':
      return 'Please work on GitHub issue #1208 — React to the world';
    case 'pr.number':
      return '1213';
    case 'pr.title':
      return 'walking skeleton';
    case 'pr.body':
      return 'PR description';
    case 'pr.author':
      return 'octocat';
    case 'pr.url':
      return 'https://github.com/alondero/buildmesh/pull/1213';
    case 'pr.head_ref':
      return 'feat/circuits';
    case 'pr.labels':
      return 'buildmesh:run';
    case 'autopilot.finish_prompt':
      return '<rendered finish prompt>';
    case 'autopilot.wrapup_correction':
      return '<rendered wrap-up correction prompt>';
    case 'verification.outcome':
      return 'green';
    case 'verification.command':
      return 'cargo test';
    case 'retry.attempt':
      return '2';
    case 'retry.max_retries':
      return '3';
    default:
      // Spawn-output chips (`node.<id>.output`) have no static example;
      // the drawer renders a contextual "<terminal text>" placeholder.
      // Truly unknown paths (defensive — every MUSTACHE_PATHS entry has
      // its own case above) return the empty string rather than the
      // generic placeholder, which would be misleading for any
      // future namespace that doesn't happen to be spawn_output-shaped.
      if (path.endsWith('.output')) return '<terminal text>';
      return '';
  }
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
    case 'review_verdict':
      return ['completed', 'blocked', 'working'];
    case 'await_agent_turn':
      return ['completed', 'blocked'];
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
 * vocabulary shared by node cards, the run-history drawer and the Probe's
 * run cards. The two "waiting" tokens are listed explicitly rather than left
 * to the default: they are the statuses a reader is most likely to look for,
 * and an implicit fall-through reads as an oversight (the per-tab copy this
 * replaced named them outright).
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
    // Queued work is deliberately subordinate — it is distinguished by its
    // label and reason line, never by colour alone (WCAG 1.4.1).
    case 'pending':
    case 'pending_slot':
      return 'text-text-muted';
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

/** Epoch-ms for one ledger timestamp, or `NaN`.
 *
 *  SQLite writes `CURRENT_TIMESTAMP` as UTC but WITHOUT a zone designator
 *  ("2026-08-22 10:05:00"). V8's `Date.parse` happily accepts that shape
 *  and reads it as *local* time, so a naive parse is silently wrong by the
 *  host's UTC offset. That cancels out when you subtract two ledger
 *  timestamps (both skew the same way) — which is why `stepDurationMs`
 *  never noticed — but NOT when you compare one against `Date.now()`, as
 *  `runDurationMs` must for a still-running run. So force the zone on any
 *  zoneless timestamp, and leave an explicitly-zoned ISO-8601 string to
 *  `Date.parse` (appending "Z" to it would yield "…ZZ" → NaN). */
const ZONELESS_TIMESTAMP = /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(\.\d+)?$/;

function ledgerTimestampMs(s: string): number {
  if (ZONELESS_TIMESTAMP.test(s)) return Date.parse(s.replace(' ', 'T') + 'Z');
  return Date.parse(s);
}

/** Wall-clock step duration from the ledger timestamps, ms or null. */
export function stepDurationMs(step: Pick<StepLike, 'started_at' | 'completed_at'>): number | null {
  if (step.started_at === null || step.completed_at === null) return null;
  const start = ledgerTimestampMs(step.started_at);
  const end = ledgerTimestampMs(step.completed_at);
  if (Number.isNaN(start) || Number.isNaN(end)) return null;
  return end - start;
}

/**
 * Wall-clock run duration. The run row carries no `started_at` /
 * `finished_at` pair — `created_at` is when the trigger minted it and
 * `updated_at` is stamped on every state transition, so for a terminal
 * run the gap between them IS the run's lifetime. A still-moving run
 * measures against `now` instead, since `updated_at` only tells you when
 * it last changed, not how long it has been going.
 */
export function runDurationMs(
  run: { state: string; created_at: string; updated_at: string },
  now: Date
): number | null {
  const start = ledgerTimestampMs(run.created_at);
  if (Number.isNaN(start)) return null;
  const end = isTerminalRunState(run.state) ? ledgerTimestampMs(run.updated_at) : now.getTime();
  if (Number.isNaN(end)) return null;
  return Math.max(0, end - start);
}

/**
 * Human duration for the run ledger. `RunHistoryDrawer` and the Probe's
 * run cards share it so a 20-minute run doesn't read as "1200.0s".
 * Tiers: sub-second → "0.4s", under a minute → "12.4s", under an hour →
 * "4m 12s", beyond → "1h 6m". Minutes/hours drop the fractional part —
 * a tenth of a second is noise at that scale.
 *
 * Two traps this shape avoids:
 *
 * - **The tier is chosen in the unit it prints.** Comparing raw `ms`
 *   against 60_000 while printing `toFixed(1)` lets the two disagree:
 *   59_999ms is under the threshold but *renders* as "60.0s", so the
 *   sequence read "…59.8s, 60.0s, 1m 0s". Rounding to deciseconds first
 *   and testing that makes the straddle unrepresentable.
 * - **`Math.max(0, NaN)` is NaN**, not 0 — the clamp does not sanitise.
 *   Without the finite guard a corrupt timestamp reaching this function
 *   falls through every comparison and formats as "NaNh NaNm". The
 *   duration helpers above return `null` rather than NaN, so this is
 *   defence for future callers, not a live path.
 */
export function formatDurationMs(ms: number): string {
  if (!Number.isFinite(ms)) return '0.0s';
  const safe = Math.max(0, ms);
  // Deciseconds: the smallest unit this function ever prints.
  const tenths = Math.round(safe / 100);
  if (tenths < 600) return `${(tenths / 10).toFixed(1)}s`;
  const totalSeconds = Math.round(safe / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  if (minutes < 60) return `${minutes}m ${totalSeconds % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

// ---------------------------------------------------------------------------
// Run diagnostics — the plain-English reading of a run's ledger (#1468).
//
// The Probe's Circuits tab used to compress a whole run into
// `trigger:completed -> implementer:running -> ...` inside a truncated
// span, so the one fact a stuck run needs to tell you (WHY it is waiting)
// was the first thing clipped. These helpers turn the raw ledger
// vocabulary into the labels the card renders. They are pure so the
// wording is unit-testable without mounting the Probe.
// ---------------------------------------------------------------------------

/** Run states the worker never moves out of (`stepper::RunState`). */
export function isTerminalRunState(state: string): boolean {
  return state === 'completed' || state === 'failed';
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
  edges: Array<{ source: string; target: string; data?: { condition?: EdgeCondition } }>,
  blueprint: CircuitGraph['blueprint'] = null
): CircuitGraph {
  return {
    version: CIRCUIT_GRAPH_VERSION,
    blueprint: blueprint ?? null,
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
