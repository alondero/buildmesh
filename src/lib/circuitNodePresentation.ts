import type { AgentNode } from '../types/generated/AgentNode';
import type { CircuitAgentOwnership } from '../types/generated/CircuitAgentOwnership';
import type { SemanticTurnPayload } from '../types/generated/SemanticTurnPayload';

export type CircuitIndicatorPhase = 'active' | 'waiting' | 'done';
export type CircuitIndicatorTone = 'automation' | 'warning' | 'success' | 'error';

export interface CircuitNodePresentation {
  phase: CircuitIndicatorPhase;
  tone: CircuitIndicatorTone;
  label: string;
  detail: string;
}

const LIVE_CIRCUIT_STATES = new Set(['pending', 'running', 'paused']);

const ACTIVE: CircuitNodePresentation = {
  phase: 'active',
  tone: 'automation',
  label: 'Circuit active',
  detail: 'Circuit is driving this Agent Node.',
};

const WAITING: CircuitNodePresentation = {
  phase: 'waiting',
  tone: 'warning',
  label: 'Circuit waiting',
  detail: 'Circuit still owns this Agent Node but is not currently driving it.',
};

const DONE: CircuitNodePresentation = {
  phase: 'done',
  tone: 'success',
  label: 'Circuit done',
  detail: 'Circuit finished; this Agent Node remains available for review.',
};

const NEEDS_ATTENTION: CircuitNodePresentation = {
  phase: 'waiting',
  tone: 'error',
  label: 'Circuit needs attention',
  detail: 'Circuit reported a failure or an unknown state and needs attention.',
};

const WAITING_NODE_DETAILS: Partial<Record<AgentNode['status'], string | ((node: AgentNode) => string)>> = {
  ready: 'Circuit is between turns after this Agent Node yielded cleanly.',
  idle: 'Circuit is waiting to start the next turn.',
  awaiting_input: 'Circuit is waiting for input from you.',
  pending: 'Circuit is waiting for this Agent Node to start.',
  spawning: 'Circuit is waiting for this Agent Node to start.',
  suspended: (node) => node.cli_session_id
    ? 'Circuit is waiting for this Agent Node to recover its existing session.'
    : 'Circuit is waiting for this Agent Node to recover before starting a new session.',
  completed: 'Circuit ownership is reconciling; the run is still live while this Agent Node reports completed.',
};

const CIRCUIT_WAITING_DETAILS: Record<'pending' | 'paused', string> = {
  pending: 'Circuit is queued for the next step.',
  paused: 'Circuit is paused.',
};

function waitingForNode(node: AgentNode, circuitState?: string): CircuitNodePresentation {
  if (node.status === 'error' || node.status === 'lost') return NEEDS_ATTENTION;
  if (circuitState === 'pending' || circuitState === 'paused') {
    return { ...WAITING, detail: CIRCUIT_WAITING_DETAILS[circuitState] };
  }
  const detail = WAITING_NODE_DETAILS[node.status];
  return detail
    ? { ...WAITING, detail: typeof detail === 'function' ? detail(node) : detail }
    : WAITING;
}

function mapCircuitOwnership(node: AgentNode, ownership: CircuitAgentOwnership): CircuitNodePresentation | null {
  if (node.status === 'archived' || ownership.state === 'cancelled') return null;
  if (ownership.state === 'completed') return DONE;
  if (ownership.state === 'failed') return NEEDS_ATTENTION;
  if (LIVE_CIRCUIT_STATES.has(ownership.state)) {
    // A live run can be paused or queued even when the node lifecycle still
    // says running. Only a running run driving a running/spawning node is
    // compactly represented as active.
    if (ownership.state === 'running' && (node.status === 'running' || node.status === 'spawning')) {
      return ACTIVE;
    }
    return waitingForNode(node, ownership.state);
  }
  return NEEDS_ATTENTION;
}

export function getCircuitNodePresentation(
  node: AgentNode,
  circuitOwnership?: CircuitAgentOwnership,
): CircuitNodePresentation | null {
  if (circuitOwnership) return mapCircuitOwnership(node, circuitOwnership);
  return null;
}

/** Historical terminal rows stay in the ownership ledger for inspection, but
 * only live ownership suppresses the suspended-node recovery warning. */
export function hasActiveCircuitOwnership(
  circuitOwnership?: CircuitAgentOwnership,
): boolean {
  if (circuitOwnership) return LIVE_CIRCUIT_STATES.has(circuitOwnership.state);
  return false;
}

// ---------------------------------------------------------------------------
// Card-level outcome (the header chip)
// ---------------------------------------------------------------------------
//
// `getCircuitNodePresentation` above answers the per-node question "is
// automation driving, waiting, or finished?" for the glyph's Circuit ring.
// A node card can hold several members, so the header needs a card-level
// answer to "who on this card needs a human, and what do they need?".
//
// This resolver aggregates EVERY attention-demanding member (`error` or
// `awaiting_input`) into one chip, so `revealAttention` can cycle through all
// of them — the reachability guarantee the pre-refactor count pill had
// (round-3 review). It does not drop a waiting session because a sibling
// failed, and it does not drop a background member's failure because the
// focused member happens to look healthy.
//
// One member is chosen as the *primary*: the focused session when it demands
// attention, otherwise the highest-priority one (failures before waits). The
// primary drives the chip's label, tone, detail, and Circuit attribution —
// so switching tabs updates what the chip describes instead of pinning the
// copy to whichever member happened to be listed first.
//
// The only failure suppressed is a solo card's own: with a single member the
// title bar glyph's dashed red ring already shows that member's failure, so the
// chip would repeat it and its `revealAttention` click would land on the same
// session. See `isSoloFocusedFailure`.
//
// An active run is carried by the glyph's comet. A finished run is already told
// by the green status circle, the closed green ring, and the PR pill, so a
// third green chip with a no-op click would be redundant. An unpiloted,
// healthy card renders none.

export type CircuitOutcomeKind = 'failed' | 'needs_input';

export interface CircuitOutcome {
  kind: CircuitOutcomeKind;
  /** Pilot-light shape to draw; mirrors `CircuitNodePresentation.phase`. */
  phase: CircuitIndicatorPhase;
  tone: CircuitIndicatorTone;
  /** Short chip label. */
  label: string;
  /** Factual sentence describing what the primary session needs. */
  detail: string;
  /** Every attention-demanding member, failures first, for chip cycling. */
  nodeIds: number[];
  /** The member the label/detail describe — the focused session when it needs
   * attention, otherwise the highest-priority one. */
  nodeId: number;
}

export interface CircuitOutcomeSources {
  circuitOwnerships: Readonly<Record<number, CircuitAgentOwnership>>;
  semanticTurns: Readonly<Record<number, SemanticTurnPayload>>;
}

/**
 * Whether the presentation shows Circuit as *currently responsible* for the
 * node. This must not be inferred from raw ledger presence: the Circuit
 * ledger retains terminal rows for inspection
 * (`completed`, `failed`, `cancelled`). A cancelled circuit withdrew
 * automation (`mapCircuitOwnership` returns `null`), and a completed run is
 * finished — so a later failure or prompt belongs to the agent, not to
 * Circuit, and must read "Agent failed" / "This agent is waiting" rather
 * than claiming Circuit needs a human.
 */
function isCircuitResponsible(presentation: CircuitNodePresentation | null): boolean {
  return presentation != null && presentation.phase !== 'done';
}

interface OutcomeCandidate {
  id: number;
  kind: CircuitOutcomeKind;
  responsible: boolean;
}

function outcomeCandidateFor(node: AgentNode, sources: CircuitOutcomeSources): OutcomeCandidate | null {
  const presentation = getCircuitNodePresentation(
    node,
    sources.circuitOwnerships[node.id],
  );
  if (node.status === 'error' || presentation?.tone === 'error') {
    return { id: node.id, kind: 'failed', responsible: isCircuitResponsible(presentation) };
  }
  if (node.status === 'awaiting_input') {
    return { id: node.id, kind: 'needs_input', responsible: isCircuitResponsible(presentation) };
  }
  return null;
}

/**
 * A solo card whose focused member failed under Circuit: the title bar's one
 * glyph already shows a dashed red ring for that member, and `revealAttention`
 * could only cycle back to it. The chip adds no reachability here, so it is the
 * single case this resolver suppresses. A multi-member card keeps the chip even
 * when the focused member failed, because its siblings are otherwise hidden.
 */
function isSoloFocusedFailure(
  members: readonly AgentNode[],
  primary: OutcomeCandidate,
  focusedNodeId?: number,
): boolean {
  return members.length === 1
    && primary.kind === 'failed'
    && primary.responsible
    && primary.id === focusedNodeId;
}

/** Keep detail copy sentence-shaped however `semanticTurns` phrases it. */
function asSentence(text: string): string {
  return /[.!?]$/.test(text) ? text : `${text}.`;
}

/// The chip's copy must be free of action directives: the accessible name
/// appends its own ("Show next session"), and a directive here produced the
/// double em-dash the round-3 review caught.
function buildOutcome(primary: OutcomeCandidate, nodeIds: number[], sources: CircuitOutcomeSources): CircuitOutcome {
  const { id: nodeId, kind, responsible } = primary;
  if (kind === 'failed') {
    return {
      kind, phase: 'waiting', tone: 'error', nodeId, nodeIds,
      label: responsible ? 'Circuit failed' : 'Agent failed',
      detail: responsible ? 'Circuit failed and needs a human.' : 'This agent is in an error state.',
    };
  }
  const what = sources.semanticTurns[nodeId]?.description?.trim();
  return {
    kind, phase: 'waiting', tone: 'warning', label: 'Needs input', nodeId, nodeIds,
    detail: what
      ? asSentence(`${responsible ? 'Circuit' : 'This agent'} is waiting for you: ${what}`)
      : `${responsible ? 'Circuit' : 'This agent'} is waiting for your input.`,
  };
}

/**
 * Resolve the card-level attention chip over all members.
 *
 * `focusedNodeId` is the member the user is currently on. When it is one of
 * the attention-demanding members it becomes the primary (so the chip
 * describes the session in front of the user); otherwise the primary is the
 * highest-priority attention member. Either way `nodeIds` lists every
 * attention-demanding member, failures first, for `revealAttention` to cycle.
 */
export function resolveCircuitOutcome(
  members: readonly AgentNode[],
  sources: CircuitOutcomeSources,
  focusedNodeId?: number,
): CircuitOutcome | null {
  const candidates = members
    .map(member => outcomeCandidateFor(member, sources))
    .filter((candidate): candidate is OutcomeCandidate => candidate !== null);
  if (candidates.length === 0) return null;

  const ordered = [
    ...candidates.filter(candidate => candidate.kind === 'failed'),
    ...candidates.filter(candidate => candidate.kind === 'needs_input'),
  ];
  const primary = ordered.find(candidate => candidate.id === focusedNodeId) ?? ordered[0];
  if (isSoloFocusedFailure(members, primary, focusedNodeId)) return null;
  return buildOutcome(primary, ordered.map(candidate => candidate.id), sources);
}
