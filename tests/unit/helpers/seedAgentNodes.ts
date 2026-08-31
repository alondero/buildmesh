/**
 * Test helper — seed the normalized `nodesById` + `nodeIds` slices of
 * `useAgentNodeStore` from a flat `AgentNode[]`, mirroring the production
 * store shape (issue #1384).
 *
 * Use this anywhere a test previously called
 * `useAgentNodeStore.setState({ agentNodes: [...] })`. The helper takes
 * care of the array→record split and preserves the original order in
 * `nodeIds` (canonical `(mesh_id, position)`). `activeNodeId` is
 * optional — tests that care about the active node pass it explicitly;
 * tests that don't, leave it null.
 *
 * Example:
 *
 *   seedAgentNodes([nodeA, nodeB], nodeB.id);
 *
 * replaces:
 *
 *   useAgentNodeStore.setState({
 *     agentNodes: [nodeA, nodeB],
 *     activeNodeId: nodeB.id,
 *   });
 */
import type { AgentNode } from '../../../src/types/generated/AgentNode';
import { useAgentNodeStore } from '../../../src/stores/agentNodeStore';

export function seedAgentNodes(
  nodes: AgentNode[],
  activeNodeId: number | null = null,
): void {
  const nodesById: Record<number, AgentNode> = {};
  const nodeIds: number[] = [];
  for (const n of nodes) {
    nodesById[n.id] = n;
    nodeIds.push(n.id);
  }
  useAgentNodeStore.setState({ nodesById, nodeIds, activeNodeId });
}
