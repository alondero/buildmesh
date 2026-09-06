import type { AgentNode } from '../types/generated/AgentNode';
import type { CircuitAgentOwnership } from '../types/generated/CircuitAgentOwnership';

export type NodeOwnerships = Record<number, CircuitAgentOwnership>;
export type NodeIndex = Readonly<Record<number, AgentNode | undefined>>;

export function indexAgentNodes(nodes: readonly AgentNode[]): Record<number, AgentNode> {
  const index: Record<number, AgentNode> = {};
  for (const node of nodes) index[node.id] = node;
  return index;
}

/** Presentation only: missing/archived parents and invalid links leave an agent accessible. */
export function activityRootId(nodeId: number, nodesById: NodeIndex, ownerships: NodeOwnerships): number {
  const seen = new Set<number>();
  let current = nodesById[nodeId];
  while (current) {
    if (seen.has(current.id)) return nodeId;
    seen.add(current.id);
    const parentId = ownerships[current.id]?.parent_node_id;
    const parent = parentId == null ? undefined : nodesById[parentId];
    if (!parent || parent.mesh_id !== current.mesh_id || parent.status === 'archived') return current.id;
    current = parent;
  }
  return nodeId;
}

/** Scope/filter individual agents first, then collapse matches to their containing card. */
export function groupActivityNodes(visible: readonly AgentNode[], nodesById: NodeIndex, ownerships: NodeOwnerships): AgentNode[] {
  const seen = new Set<number>();
  return visible.flatMap(node => {
    const rootId = activityRootId(node.id, nodesById, ownerships);
    if (seen.has(rootId)) return [];
    seen.add(rootId);
    return [nodesById[rootId] ?? node];
  });
}

/** Build card membership once per view update, rather than scanning every
 * node from every card selector. */
export function activityMemberIds(nodes: readonly AgentNode[], ownerships: NodeOwnerships): Record<number, number[]> {
  const index = indexAgentNodes(nodes);
  const groups: Record<number, number[]> = {};
  for (const node of nodes) {
    if (node.status === 'archived') continue;
    const rootId = activityRootId(node.id, index, ownerships);
    (groups[rootId] ??= []).push(node.id);
  }
  return groups;
}

export type ActivityStatusTone = 'warning' | 'error' | 'active' | 'idle';
export type ActivityStatus = { label: string; tone: ActivityStatusTone };

export function activityStatus(root: AgentNode, members: readonly AgentNode[]): ActivityStatus {
  if (members.some(n => n.status === 'error')) return { label: 'Needs attention', tone: 'error' };
  if (members.some(n => n.status === 'awaiting_input')) return { label: 'Needs input', tone: 'warning' };
  const implementing = root.status === 'running';
  const reviewing = members.some(n => n.id !== root.id && n.status === 'running');
  if (implementing && reviewing) return { label: 'Implementing + reviewing', tone: 'active' };
  if (reviewing) return { label: 'Reviewing', tone: 'active' };
  if (implementing) return { label: 'Implementing', tone: 'active' };
  if (members.some(n => n.status === 'pending' || n.status === 'spawning')) return { label: 'Starting', tone: 'active' };
  return { label: 'Waiting', tone: 'idle' };
}
