import type { AgentNode } from '../types/generated/AgentNode';
import type { CircuitAgentOwnership } from '../types/generated/CircuitAgentOwnership';

export type NodeOwnerships = Record<number, CircuitAgentOwnership>;
export type NodeIndex = Readonly<Record<number, AgentNode | undefined>>;
export type NodeGroups = readonly (readonly number[])[];

export function indexAgentNodes(nodes: readonly AgentNode[]): Record<number, AgentNode> {
  const index: Record<number, AgentNode> = {};
  for (const node of nodes) index[node.id] = node;
  return index;
}

/** Presentation only: missing/archived parents and invalid links leave an agent accessible. */
export function activityRootId(nodeId: number, nodesById: NodeIndex, ownerships: NodeOwnerships, groups: NodeGroups = []): number {
  const rootId = circuitRootId(nodeId, nodesById, ownerships);
  return resolveNodeGroups(groups, nodesById, ownerships).find(ids => ids.includes(rootId))?.[0] ?? rootId;
}

/** Ownership can arrive after persisted view state. Resolve sources and merge
 * overlapping groups without changing their insertion order. */
export function resolveNodeGroups(groups: NodeGroups, nodesById: NodeIndex, ownerships: NodeOwnerships): number[][] {
  let resolved: number[][] = [];
  for (const group of groups) {
    const byMesh = new Map<number, number[]>();
    for (const id of group) {
      const root = nodesById[circuitRootId(id, nodesById, ownerships)];
      if (!root || root.status === 'archived') continue;
      const ids = byMesh.get(root.mesh_id) ?? [];
      if (!ids.includes(root.id)) ids.push(root.id);
      byMesh.set(root.mesh_id, ids);
    }
    for (const ids of byMesh.values()) {
      const overlaps = resolved.filter(existing => existing.some(id => ids.includes(id)));
      const merged = [...new Set([...overlaps.flat(), ...ids])];
      resolved = resolved.filter(existing => !overlaps.includes(existing));
      resolved.push(merged);
    }
  }
  return resolved.filter(ids => ids.length > 1);
}

function circuitRootId(nodeId: number, nodesById: NodeIndex, ownerships: NodeOwnerships): number {
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
export function groupActivityNodes(visible: readonly AgentNode[], nodesById: NodeIndex, ownerships: NodeOwnerships, groups: NodeGroups = []): AgentNode[] {
  const seen = new Set<number>();
  const resolved = resolveNodeGroups(groups, nodesById, ownerships);
  const cards = visible.flatMap(node => {
    const source = circuitRootId(node.id, nodesById, ownerships);
    const rootId = resolved.find(ids => ids.includes(source))?.[0] ?? source;
    if (seen.has(rootId)) return [];
    seen.add(rootId);
    return [nodesById[rootId] ?? node];
  });
  // Existing callers supply the canonical custom order. Manual groups use the
  // representative's persisted position, rather than an earlier hidden member.
  return orderActivityCards(cards, resolved);
}

function orderActivityCards(cards: AgentNode[], groups: readonly (readonly number[])[]): AgentNode[] {
  if (groups.length === 0) return cards;
  return [...cards].sort((a, b) => a.mesh_id - b.mesh_id || a.position - b.position || a.id - b.id);
}

/** Build card membership once per view update, rather than scanning every
 * node from every card selector. */
export function activityMemberIds(nodes: readonly AgentNode[], ownerships: NodeOwnerships, manualGroups: NodeGroups = []): Record<number, number[]> {
  const index = indexAgentNodes(nodes);
  const groups: Record<number, number[]> = {};
  const resolved = resolveNodeGroups(manualGroups, index, ownerships);
  for (const node of nodes) {
    if (node.status === 'archived') continue;
    const source = circuitRootId(node.id, index, ownerships);
    const rootId = resolved.find(ids => ids.includes(source))?.[0] ?? source;
    (groups[rootId] ??= []).push(node.id);
  }
  for (const [root, ids] of Object.entries(groups)) {
    const order = resolved.find(group => group.includes(Number(root)));
    if (order) ids.sort((a, b) => {
      const aSource = circuitRootId(a, index, ownerships);
      const bSource = circuitRootId(b, index, ownerships);
      return order.indexOf(aSource) - order.indexOf(bSource)
        || Number(b === bSource) - Number(a === aSource) || a - b;
    });
  }
  return groups;
}

/** Candidates for a node's "Handover to node" picker, split so the agents that
 *  share the source's Node Activity come first — the implementer↔reviewer
 *  pairing this picker exists for is one Node Activity, whether it was grouped
 *  by hand or born from the review circuit's ownership lineage. */
export interface HandoverTargets {
  sameActivity: AgentNode[];
  others: AgentNode[];
}

/** Same-Mesh agents a node can hand its terminal selection to. `sameActivity`
 *  are the ones resolving to the source's activity root (see
 *  [`activityRootId`]); both halves are ordered by the canonical `(position, id)`
 *  the grid uses, so the picker is stable across fetches. */
export function handoverTargets(
  sourceId: number,
  nodesById: NodeIndex,
  ownerships: NodeOwnerships,
  groups: NodeGroups = [],
): HandoverTargets {
  const source = nodesById[sourceId];
  if (!source) return { sameActivity: [], others: [] };
  const rootId = activityRootId(sourceId, nodesById, ownerships, groups);
  const sameActivity: AgentNode[] = [];
  const others: AgentNode[] = [];
  for (const node of Object.values(nodesById)) {
    if (!node || node.id === sourceId || node.mesh_id !== source.mesh_id || node.status === 'archived') continue;
    const bucket = activityRootId(node.id, nodesById, ownerships, groups) === rootId ? sameActivity : others;
    bucket.push(node);
  }
  const byPosition = (a: AgentNode, b: AgentNode) => a.position - b.position || a.id - b.id;
  return { sameActivity: sameActivity.sort(byPosition), others: others.sort(byPosition) };
}

export type ActivityStatusTone = 'warning' | 'error' | 'active' | 'idle';
export type ActivityStatus = { label: string; tone: ActivityStatusTone };

export function activityStatus(root: AgentNode, members: readonly AgentNode[], grouped = false): ActivityStatus {
  if (members.some(n => n.status === 'error' || n.status === 'lost')) return { label: 'Needs attention', tone: 'error' };
  if (members.some(n => n.status === 'awaiting_input')) return { label: 'Needs input', tone: 'warning' };
  if (grouped && members.some(n => n.status === 'running')) return { label: 'Running', tone: 'active' };
  const implementing = root.status === 'running';
  const reviewing = members.some(n => n.id !== root.id && n.status === 'running');
  if (implementing && reviewing) return { label: 'Implementing + reviewing', tone: 'active' };
  if (reviewing) return { label: 'Reviewing', tone: 'active' };
  if (implementing) return { label: 'Implementing', tone: 'active' };
  if (members.some(n => n.status === 'pending' || n.status === 'spawning')) return { label: 'Starting', tone: 'active' };
  return { label: 'Waiting', tone: 'idle' };
}
