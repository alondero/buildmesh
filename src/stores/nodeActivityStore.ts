import { create } from 'zustand';
import { useAgentNodeStore } from './agentNodeStore';
import { activityRootId } from '../lib/nodeActivities';

export type UtilityMode = 'build' | 'run' | 'terminal';
type Selection = { nodeId: number; utility: boolean };

/** View state survives card remounts; process lifetime belongs to the terminal registries. */
export const useNodeActivityStore = create<{
  selections: Record<number, Selection>;
  utilities: Record<number, UtilityMode>;
  /** Canonical UI navigation transition: focus an agent and its activity. */
  activateNode: (nodeId: number, utility?: boolean, utilityMode?: UtilityMode) => void;
  select: (rootId: number, nodeId: number, utility?: boolean) => void;
  openUtility: (rootId: number, nodeId: number, mode: UtilityMode) => void;
  closeUtility: (rootId: number, nodeId: number) => void;
  prune: (validNodeIds: ReadonlySet<number>) => void;
}>((set) => ({
  selections: {},
  utilities: {},
  activateNode: (nodeId, utility = false, utilityMode) => {
    const agentState = useAgentNodeStore.getState();
    const rootId = activityRootId(nodeId, agentState.nodesById, agentState.circuitOwnerships);
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
