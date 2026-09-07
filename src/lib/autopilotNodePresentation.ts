import type { AgentNode } from '../types/generated/AgentNode';
import type { AutopilotRunState } from '../types/generated/AutopilotRunStateKind';
import type { CircuitAgentOwnership } from '../types/generated/CircuitAgentOwnership';

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
