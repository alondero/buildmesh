import { focusNode } from './navigation';

/**
 * Canonical "take me to this agent node" transition. Selects the owning mesh,
 * focuses the node's activity selection, and solos the node so it is
 * unambiguously on screen regardless of the grid's current filter, sort, or
 * view mode.
 *
 * Returns `false` when the node is not in the store (deleted or archived
 * since the caller rendered) so the caller can surface a fallback instead of
 * silently doing nothing.
 */
export function focusAgentNode(nodeId: number): boolean {
  return focusNode(nodeId, { view: 'single' });
}
