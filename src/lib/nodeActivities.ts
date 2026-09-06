import type { AgentNode } from '../types/generated/AgentNode';
import type { CircuitAgentOwnership } from '../types/generated/CircuitAgentOwnership';

export type NodeOwnerships = Record<number, CircuitAgentOwnership>;

/** Presentation only: missing/archived parents and invalid links leave an agent accessible. */
export function activityRootId(nodeId: number, nodes: readonly AgentNode[], ownerships: NodeOwnerships): number {
  const seen = new Set<number>();
  let current = nodes.find(n => n.id === nodeId);
  while (current) {
    if (seen.has(current.id)) return nodeId;
    seen.add(current.id);
    const parentId = ownerships[current.id]?.parent_node_id;
    const parent = nodes.find(n => n.id === parentId && n.mesh_id === current!.mesh_id && n.status !== 'archived');
    if (!parent) return current.id;
    current = parent;
  }
  return nodeId;
}

/** Scope/filter individual agents first, then collapse matches to their containing card. */
export function groupActivityNodes(visible: readonly AgentNode[], all: readonly AgentNode[], ownerships: NodeOwnerships): AgentNode[] {
  const seen = new Set<number>();
  return visible.flatMap(node => {
    const rootId = activityRootId(node.id, all, ownerships);
    if (seen.has(rootId)) return [];
    seen.add(rootId);
    return [all.find(n => n.id === rootId) ?? node];
  });
}

export function activityStatus(root: AgentNode, members: readonly AgentNode[]): string {
  if (members.some(n => n.status === 'awaiting_input')) return 'Needs input';
  if (members.some(n => n.status === 'error')) return 'Needs attention';
  const implementing = root.status === 'running';
  const reviewing = members.some(n => n.id !== root.id && n.status === 'running');
  if (implementing && reviewing) return 'Implementing + reviewing';
  if (reviewing) return 'Reviewing';
  if (implementing) return 'Implementing';
  if (members.some(n => n.status === 'pending' || n.status === 'spawning')) return 'Starting';
  return 'Waiting';
}
