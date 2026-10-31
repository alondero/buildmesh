import type { CircuitAgentBlockedPayload } from '../types/generated/CircuitAgentBlockedPayload';
import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useMeshStore } from '../stores/meshStore';
import { addToast, useToastStore } from '../stores/toastStore';
import { isAgentInput, needsAttention } from './notifications';
import { nodeInputRequest } from './status';

export function reportCircuitInput({ node_id: nodeId, issue }: CircuitAgentBlockedPayload): void {
  const node = useAgentNodeStore.getState().nodesById[nodeId];
  const agentName = node?.name?.trim() || 'Circuit agent';
  const meshName = node ? useMeshStore.getState().meshesById.get(node.mesh_id)?.name ?? '' : '';
  const subject = meshName ? `${agentName} in ${meshName}` : agentName;
  const issueLabel = issue > 0 ? ` (issue #${issue})` : '';
  const request = node && nodeInputRequest(node);
  addToast('Needs input', `${subject} needs your input${issueLabel}. ${request?.message || 'Open the agent to see the request and respond.'}`,
    'warning', { kind: 'agent-input', nodeId, agentName, meshName });
}

/** Start after the boot snapshot succeeds. A Circuit intervention can name a
 * Ready agent (for example, a missing result file). Retain it across restart;
 * later status transitions can establish renewed work or an ended input wait. */
export function watchAgentInputNotifications(): () => void {
  const reconcile = (initial: boolean, previous = useAgentNodeStore.getState()) => {
    const { nodesById } = useAgentNodeStore.getState();
    for (const notice of useToastStore.getState().notifications) {
      const action = notice.action;
      if (!isAgentInput(action) || !needsAttention(notice)) continue;
      const node = nodesById[action.nodeId];
      if (node?.status === 'awaiting_input') continue;
      const unavailable = !node || node.status === 'archived';
      const previousNode = previous.nodesById[action.nodeId];
      const leftInput = previousNode?.status === 'awaiting_input';
      const resumed = previousNode && previousNode.status !== node?.status
        && (node?.status === 'running' || node?.status === 'spawning');
      if (initial) {
        if (!unavailable) continue;
      } else if (!previousNode || (!unavailable && !leftInput && !resumed)) continue;
      useToastStore.getState().resolveAgentInputNotification(action.nodeId,
        unavailable ? 'This agent is no longer available.'
          : leftInput ? 'The agent is no longer waiting for input.' : 'The agent has resumed work.');
    }
  };
  reconcile(true);
  return useAgentNodeStore.subscribe((_state, previous) => reconcile(false, previous));
}
