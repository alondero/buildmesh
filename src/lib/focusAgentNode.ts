import { focusNode } from './navigation';
import { useAgentNodeStore } from '../stores/agentNodeStore';

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

/**
 * Restore a closed agent node through the agent-history reopen command (it
 * comes back suspended) and adopt the returned row into the store.
 */
export async function reopenAndAdoptAgentNode(nodeId: number): Promise<void> {
  const { reopenAgentNode } = await import('./tauri/history');
  const node = await reopenAgentNode(nodeId);
  const store = useAgentNodeStore.getState();
  store.adoptAgentNode(node);
  if (useAgentNodeStore.getState().nodesById[nodeId]?.status === 'archived') {
    store.patchAgentNode(nodeId, { status: node.status });
  }
}

/** Focus the node, reopening it from history first when it was closed. */
export async function openAgentNode(nodeId: number): Promise<void> {
  if (focusAgentNode(nodeId)) return;
  await reopenAndAdoptAgentNode(nodeId);
  if (!focusAgentNode(nodeId)) throw new Error(`Agent node #${nodeId} is no longer available.`);
}
