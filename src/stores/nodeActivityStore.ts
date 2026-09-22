import { create } from 'zustand';
import { useAgentNodeStore } from './agentNodeStore';
import { activityRootId, resolveNodeGroups, type NodeGroups } from '../lib/nodeActivities';

const GROUPS_KEY = 'buildmesh.node-groups';
function loadGroups(): NodeGroups {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(GROUPS_KEY) ?? '[]');
    const seen = new Set<number>();
    if (!Array.isArray(value)) return [];
    return value.filter((ids): ids is number[] => Array.isArray(ids)
      && ids.length > 1 && ids.every(id => Number.isSafeInteger(id) && id > 0))
      .map(ids => ids.filter(id => { if (seen.has(id)) return false; seen.add(id); return true; }))
      .filter(ids => ids.length > 1);
  } catch { return []; }
}

function saveGroups(groups: NodeGroups) {
  try { localStorage.setItem(GROUPS_KEY, JSON.stringify(groups)); } catch { /* View state still works without storage. */ }
}

export type UtilityMode = 'build' | 'run' | 'terminal';
type Selection = { nodeId: number; utility: boolean };

/** View state survives card remounts; process lifetime belongs to the terminal registries. */
export const useNodeActivityStore = create<{
  selections: Record<number, Selection>;
  utilities: Record<number, UtilityMode>;
  groups: NodeGroups;
  groupNodes: (sourceId: number, targetId: number) => void;
  ungroupNode: (nodeId: number) => void;
  /** Canonical UI navigation transition: focus an agent and its activity. */
  activateNode: (nodeId: number, utility?: boolean, utilityMode?: UtilityMode) => void;
  select: (rootId: number, nodeId: number, utility?: boolean) => void;
  openUtility: (rootId: number, nodeId: number, mode: UtilityMode) => void;
  closeUtility: (rootId: number, nodeId: number) => void;
  prune: (validNodeIds: ReadonlySet<number>) => void;
}>((set, get) => ({
  selections: {},
  utilities: {},
  groups: loadGroups(),
  groupNodes: (sourceId, targetId) => {
    const { nodesById, circuitOwnerships } = useAgentNodeStore.getState();
    const source = nodesById[sourceId];
    const target = nodesById[targetId];
    if (!source || !target || source.status === 'archived' || target.status === 'archived'
      || source.mesh_id !== target.mesh_id) return;
    set(s => {
      const currentGroups = resolveNodeGroups(s.groups, nodesById, circuitOwnerships);
      const sourceRoot = activityRootId(sourceId, nodesById, circuitOwnerships, currentGroups);
      const targetRoot = activityRootId(targetId, nodesById, circuitOwnerships, currentGroups);
      if (sourceRoot === targetRoot) return s;
      const sourceGroup = currentGroups.find(ids => ids.includes(sourceRoot));
      const targetGroup = currentGroups.find(ids => ids.includes(targetRoot));
      const groups = [...currentGroups.filter(ids => ids !== sourceGroup && ids !== targetGroup),
        [...(targetGroup ?? [targetRoot]), ...(sourceGroup ?? [sourceRoot])]];
      saveGroups(groups);
      return { groups, selections: { ...s.selections, [targetRoot]: { nodeId: sourceId, utility: false } } };
    });
    get().activateNode(sourceId);
  },
  ungroupNode: (nodeId) => {
    const { nodesById, circuitOwnerships } = useAgentNodeStore.getState();
    const root = activityRootId(nodeId, nodesById, circuitOwnerships);
    set(s => {
      const groups = resolveNodeGroups(s.groups, nodesById, circuitOwnerships).map(ids => ids.filter(id => id !== root))
        .filter(ids => ids.length > 1);
      saveGroups(groups);
      return { groups };
    });
    get().activateNode(nodeId);
  },
  activateNode: (nodeId, utility = false, utilityMode) => {
    const agentState = useAgentNodeStore.getState();
    const rootId = activityRootId(nodeId, agentState.nodesById, agentState.circuitOwnerships, get().groups);
    agentState.setActiveNode(nodeId);
    if (utility && utilityMode) {
      set(s => ({
        utilities: { ...s.utilities, [nodeId]: utilityMode },
        selections: { ...s.selections, [rootId]: { nodeId, utility: true } },
      }));
      return;
    }
    set(s => ({
      selections: { ...s.selections, [rootId]: { nodeId, utility } },
    }));
  },
  select: (rootId, nodeId, utility = false) => set(s => ({
    selections: { ...s.selections, [rootId]: { nodeId, utility } },
  })),
  openUtility: (rootId, nodeId, mode) => set(s => ({
    utilities: { ...s.utilities, [nodeId]: mode },
    selections: { ...s.selections, [rootId]: { nodeId, utility: true } },
  })),
  closeUtility: (rootId, nodeId) => set(s => {
    const utilities = { ...s.utilities };
    delete utilities[nodeId];
    const selected = s.selections[rootId];
    return { utilities, selections: selected?.nodeId === nodeId && selected.utility
      ? { ...s.selections, [rootId]: { nodeId, utility: false } }
      : s.selections };
  }),
  prune: (validNodeIds) => set(s => {
    const selections = Object.fromEntries(
      Object.entries(s.selections).filter(([rootId, selection]) =>
        validNodeIds.has(Number(rootId)) && validNodeIds.has(selection.nodeId),
      ),
    );
    const utilities = Object.fromEntries(
      Object.entries(s.utilities).filter(([nodeId]) => validNodeIds.has(Number(nodeId))),
    );
    if (Object.keys(selections).length === Object.keys(s.selections).length
      && Object.keys(utilities).length === Object.keys(s.utilities).length) return s;
    return { selections, utilities };
  }),
}));
