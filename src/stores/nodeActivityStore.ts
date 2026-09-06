import { create } from 'zustand';

export type UtilityMode = 'build' | 'run' | 'terminal';
type Selection = { nodeId: number; utility: boolean };

/** View state survives card remounts; process lifetime belongs to the terminal registries. */
export const useNodeActivityStore = create<{
  selections: Record<number, Selection>;
  utilities: Record<number, UtilityMode>;
  select: (rootId: number, nodeId: number, utility?: boolean) => void;
  openUtility: (rootId: number, nodeId: number, mode: UtilityMode) => void;
  closeUtility: (rootId: number, nodeId: number) => void;
}>((set) => ({
  selections: {},
  utilities: {},
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
    return { utilities, selections: { ...s.selections, [rootId]: { nodeId, utility: false } } };
  }),
}));
