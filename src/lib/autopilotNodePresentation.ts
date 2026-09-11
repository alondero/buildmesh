import type { AgentNode } from '../types/generated/AgentNode';
import type { AutopilotRunState } from '../types/generated/AutopilotRunStateKind';
import type { CircuitAgentOwnership } from '../types/generated/CircuitAgentOwnership';
import type { SemanticTurnPayload } from '../types/generated/SemanticTurnPayload';

export type AutopilotIndicatorPhase = 'active' | 'waiting' | 'done';
export type AutopilotIndicatorTone = 'automation' | 'warning' | 'success' | 'error';

export interface AutopilotNodePresentation {
  phase: AutopilotIndicatorPhase;
  tone: AutopilotIndicatorTone;
  label: string;
  detail: string;
}

export interface AutopilotRunDetails {
  label: string;
  title: string;
}

const ACTIVE_LEGACY_STATES = new Set<AutopilotRunState>(['implementing', 'finishing', 'suffix_pending']);
const DONE_LEGACY_STATES = new Set<AutopilotRunState>(['completed', 'merged']);
const LIVE_CIRCUIT_STATES = new Set(['pending', 'running', 'paused']);

const LEGACY_RUN_DETAILS: Record<AutopilotRunState, AutopilotRunDetails> = {
  implementing: { label: 'autopilot', title: 'Autopilot: agent is implementing the task' },
  finishing: { label: 'autopilot · wrap-up', title: 'Autopilot: wrap-up in progress (verify, commit, push, PR)' },
  suffix_pending: {
    label: 'autopilot · suffix',
    title: 'Autopilot: wrap-up verified, running the optional suffix prompt turn before this iteration completes',
  },
  completed: {
    label: 'autopilot · complete',
    title: 'Autopilot: wrap-up verified, hands off — the PR is ready for review; the node closes when it merges',
  },
  merged: { label: 'autopilot · merged', title: 'Autopilot: PR merged' },
  failed: { label: 'autopilot ✗', title: 'Autopilot: wrap-up failed after 3 attempts — needs a human' },
};

export function getAutopilotRunDetails(state: AutopilotRunState): AutopilotRunDetails {
  return LEGACY_RUN_DETAILS[state];
}

const ACTIVE: AutopilotNodePresentation = {
  phase: 'active',
  tone: 'automation',
  label: 'Autopilot active',
  detail: 'Autopilot is driving this Agent Node.',
};

const WAITING: AutopilotNodePresentation = {
  phase: 'waiting',
  tone: 'warning',
  label: 'Autopilot waiting',
  detail: 'Autopilot still owns this Agent Node but is not currently driving it.',
};

const DONE: AutopilotNodePresentation = {
  phase: 'done',
  tone: 'success',
  label: 'Autopilot done',
  detail: 'Autopilot finished; this Agent Node remains available for review.',
};

const NEEDS_ATTENTION: AutopilotNodePresentation = {
  phase: 'waiting',
  tone: 'error',
  label: 'Autopilot needs attention',
  detail: 'Autopilot reported a failure or an unknown state and needs attention.',
};

const WAITING_NODE_DETAILS: Partial<Record<AgentNode['status'], string | ((node: AgentNode) => string)>> = {
  ready: 'Autopilot is between turns after this Agent Node yielded cleanly.',
  idle: 'Autopilot is waiting to start the next turn.',
  awaiting_input: 'Autopilot is waiting for input from you.',
  pending: 'Autopilot is waiting for this Agent Node to start.',
  spawning: 'Autopilot is waiting for this Agent Node to start.',
  suspended: (node) => node.cli_session_id
    ? 'Autopilot is waiting for this Agent Node to recover its existing session.'
    : 'Autopilot is waiting for this Agent Node to recover before starting a new session.',
  completed: 'Autopilot ownership is reconciling; the run is still live while this Agent Node reports completed.',
};

const CIRCUIT_WAITING_DETAILS: Record<'pending' | 'paused', string> = {
  pending: 'Autopilot is queued for the next Circuit step.',
  paused: 'Autopilot is paused by the Circuit.',
};

function waitingForNode(node: AgentNode, circuitState?: string): AutopilotNodePresentation {
  if (node.status === 'error') return NEEDS_ATTENTION;
  if (circuitState === 'pending' || circuitState === 'paused') {
    return { ...WAITING, detail: CIRCUIT_WAITING_DETAILS[circuitState] };
  }
  const detail = WAITING_NODE_DETAILS[node.status];
  return detail
    ? { ...WAITING, detail: typeof detail === 'function' ? detail(node) : detail }
    : WAITING;
}

function mapCircuitOwnership(node: AgentNode, ownership: CircuitAgentOwnership): AutopilotNodePresentation | null {
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

function mapLegacyState(node: AgentNode, state: AutopilotRunState): AutopilotNodePresentation | null {
  if (node.status === 'archived') return null;
  if (DONE_LEGACY_STATES.has(state)) return DONE;
  if (state === 'failed') return NEEDS_ATTENTION;
  if (ACTIVE_LEGACY_STATES.has(state)) {
    if (node.status === 'running' || node.status === 'spawning') return ACTIVE;
    return waitingForNode(node);
  }
  return NEEDS_ATTENTION;
}

/**
 * Resolve the compact ownership indicator. Circuit ownership intentionally
 * wins over the legacy autopilot run state: a node can be visible in both
 * ledgers during migration, but the circuit is the newer source of truth.
 */
export function getAutopilotNodePresentation(
  node: AgentNode,
  autopilotState?: AutopilotRunState,
  circuitOwnership?: CircuitAgentOwnership,
): AutopilotNodePresentation | null {
  if (circuitOwnership) return mapCircuitOwnership(node, circuitOwnership);
  if (autopilotState) return mapLegacyState(node, autopilotState);
  return null;
}

/** Historical terminal rows stay in the ownership ledger for inspection, but
 * only live ownership suppresses the suspended-node recovery warning. */
export function hasActiveAutopilotOwnership(
  autopilotState?: AutopilotRunState,
  circuitOwnership?: CircuitAgentOwnership,
): boolean {
  if (circuitOwnership) return LIVE_CIRCUIT_STATES.has(circuitOwnership.state);
  return autopilotState ? ACTIVE_LEGACY_STATES.has(autopilotState) : false;
}

// ---------------------------------------------------------------------------
// Card-level outcome (the header chip)
// ---------------------------------------------------------------------------
//
// `getAutopilotNodePresentation` above answers the per-node question "is
// automation driving, waiting, or finished?" for the Pilot-light indicator.
// A node card can hold several members, so the header needs a card-level
// answer to "who on this card needs a human, and what do they need?".
//
// This resolver aggregates EVERY attention-demanding member (`error` or
// `awaiting_input`) into one chip, so `revealAttention` can cycle through all
// of them — the reachability guarantee the pre-refactor count pill had
// (round-3 review). It does not drop a waiting session because a sibling
// failed.
//
// One member is chosen as the *primary*: the focused session when it demands
// attention, otherwise the highest-priority one (failures before waits). The
// primary drives the chip's label, tone, detail, and Autopilot-attribution —
// so switching tabs updates what the chip describes instead of pinning the
// copy to whichever member happened to be listed first.
//
// Non-actionable states earn no chip. An active run is carried by the Pilot
// light. A finished run is already told four ways — the green lifecycle dot,
// the Pilot-light check, the PR pill, and the `autopilot-pr-created` toast —
// so a third green chip with a no-op click was pure noise (round-1 review).
// An unpiloted, healthy card renders none.

export type AutopilotOutcomeKind = 'failed' | 'needs_input';

export interface AutopilotOutcome {
  kind: AutopilotOutcomeKind;
  /** Pilot-light shape to draw; mirrors `AutopilotNodePresentation.phase`. */
  phase: AutopilotIndicatorPhase;
  tone: AutopilotIndicatorTone;
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

export interface AutopilotOutcomeSources {
  autopilotStates: Readonly<Record<number, AutopilotRunState>>;
  circuitOwnerships: Readonly<Record<number, CircuitAgentOwnership>>;
  semanticTurns: Readonly<Record<number, SemanticTurnPayload>>;
}

/**
 * Whether the presentation shows Autopilot as *currently responsible* for the
 * node. This must not be inferred from raw ledger presence: both the circuit
 * ledger and the legacy runs ledger retain terminal rows for inspection
 * (`completed`, `failed`, `cancelled`). A cancelled circuit withdrew
 * automation (`mapCircuitOwnership` returns `null`), and a completed run is
 * finished — so a later failure or prompt belongs to the agent, not to
 * Autopilot, and must read "Agent failed" / "This agent is waiting" rather
 * than claiming Autopilot needs a human.
 */
function isAutopilotResponsible(presentation: AutopilotNodePresentation | null): boolean {
  return presentation != null && presentation.phase !== 'done';
}

interface OutcomeCandidate {
  id: number;
  kind: AutopilotOutcomeKind;
  responsible: boolean;
}

function outcomeCandidateFor(node: AgentNode, sources: AutopilotOutcomeSources): OutcomeCandidate | null {
  const presentation = getAutopilotNodePresentation(
    node,
    sources.autopilotStates[node.id],
    sources.circuitOwnerships[node.id],
  );
  if (node.status === 'error' || presentation?.tone === 'error') {
    return { id: node.id, kind: 'failed', responsible: isAutopilotResponsible(presentation) };
  }
  if (node.status === 'awaiting_input') {
    return { id: node.id, kind: 'needs_input', responsible: isAutopilotResponsible(presentation) };
  }
  return null;
}

/** Keep detail copy sentence-shaped however `semanticTurns` phrases it. */
function asSentence(text: string): string {
  return /[.!?]$/.test(text) ? text : `${text}.`;
}

/// The chip's copy must be free of action directives: the accessible name
/// appends its own ("Show next session"), and a directive here produced the
/// double em-dash the round-3 review caught.
function buildOutcome(primary: OutcomeCandidate, nodeIds: number[], sources: AutopilotOutcomeSources): AutopilotOutcome {
  const { id: nodeId, kind, responsible } = primary;
  if (kind === 'failed') {
    return {
      kind, phase: 'waiting', tone: 'error', nodeId, nodeIds,
      label: responsible ? 'Autopilot failed' : 'Agent failed',
      detail: responsible ? 'Autopilot failed and needs a human.' : 'This agent is in an error state.',
    };
  }
  const what = sources.semanticTurns[nodeId]?.description?.trim();
  return {
    kind, phase: 'waiting', tone: 'warning', label: 'Needs input', nodeId, nodeIds,
    detail: what
      ? asSentence(`${responsible ? 'Autopilot' : 'This agent'} is waiting for you: ${what}`)
      : `${responsible ? 'Autopilot' : 'This agent'} is waiting for your input.`,
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
export function resolveAutopilotOutcome(
  members: readonly AgentNode[],
  sources: AutopilotOutcomeSources,
  focusedNodeId?: number,
): AutopilotOutcome | null {
  const candidates = members
    .map(member => outcomeCandidateFor(member, sources))
    .filter((candidate): candidate is OutcomeCandidate => candidate !== null);
  if (candidates.length === 0) return null;

  const ordered = [
    ...candidates.filter(candidate => candidate.kind === 'failed'),
    ...candidates.filter(candidate => candidate.kind === 'needs_input'),
  ];
  const primary = ordered.find(candidate => candidate.id === focusedNodeId) ?? ordered[0];
  return buildOutcome(primary, ordered.map(candidate => candidate.id), sources);
}
