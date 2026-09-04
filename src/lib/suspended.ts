// Shared helpers for the user-driven recovery affordances on Suspended
// agent nodes (sidebar `NodeItem` + main-panel `GridNodeHeader`). The
// `Suspended` status has three origins (crash recovery, app-exit
// graceful shutdown, autopilot-gate approval) that share a badge but
// mean different things; the autopilot-gate case has no captured
// `cli_session_id` because the agent never ran. Surfacing a Resume
// button on autopilot-gate rows would land on the backend's
// "cannot resume node X: no CLI session ID is stored" error
// (spawn.rs:1095-1098) as a confusing toast — so the visibility gate
// is `cli_session_id` non-empty, with the data column as the
// disambiguator. Single source of truth so the sidebar and the
// header can never disagree.

import type { AgentNode } from '../stores/agentNodeStore';

/**
 * True when a Suspended node has a captured `cli_session_id` and is
 * therefore recoverable via the user-driven Resume affordance. Startup
 * discovery also includes nodes without IDs so it can attempt recovery.
 */
export function canResumeSuspendedNode(node: Pick<AgentNode, 'status' | 'cli_session_id'>): boolean {
  return (
    node.status === 'suspended' &&
    typeof node.cli_session_id === 'string' &&
    node.cli_session_id.length > 0
  );
}
export function hasLostConversation(
  node: Pick<AgentNode, 'status' | 'cli_session_id'>,
  isAutopilot: boolean,
): boolean {
  return node.status === 'suspended' && !canResumeSuspendedNode(node) && !isAutopilot;
}
