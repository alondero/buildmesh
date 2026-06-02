import { create } from 'zustand';
import type { WorktreeCloseAction, WorktreeCloseSafety } from '../lib/worktreeClose';

interface WorktreeClosePrompt {
  nodeName: string;
  safety: WorktreeCloseSafety;
  resolve: (action: WorktreeCloseAction) => void;
}

interface WorktreeClosePromptState {
  pending: WorktreeClosePrompt | null;
  request: (nodeName: string, safety: WorktreeCloseSafety) => Promise<WorktreeCloseAction>;
  choose: (action: WorktreeCloseAction) => void;
}

export const useWorktreeClosePromptStore = create<WorktreeClosePromptState>((set, get) => ({
  pending: null,

  request: (nodeName, safety) => new Promise<WorktreeCloseAction>((resolve) => {
    set({ pending: { nodeName, safety, resolve } });
  }),

  choose: (action) => {
    const pending = get().pending;
    if (!pending) return;
    set({ pending: null });
    pending.resolve(action);
  },
}));

export function requestWorktreeCloseAction(
  nodeName: string,
  safety: WorktreeCloseSafety,
): Promise<WorktreeCloseAction> {
  return useWorktreeClosePromptStore.getState().request(nodeName, safety);
}
