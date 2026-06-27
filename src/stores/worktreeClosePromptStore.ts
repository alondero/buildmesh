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
    // A back-to-back close (× on A, then × on B before dismissing A's
    // dialog) would otherwise orphan A's resolver — deleteAgentNode awaits
    // a promise that never settles, leaving the row stuck dimmed
    // (issue #644). Settle any prior pending as 'cancel': that's the same
    // path deleteAgentNode takes on manual dismiss.
    get().choose('cancel');
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
