export interface WorktreeCloseSafety {
  worktree_path: string | null;
  has_uncommitted: boolean;
  has_unpushed: boolean;
  is_detached: boolean;
}

export type WorktreeCloseAction = 'remove' | 'keep' | 'cancel';

export function hasWorktreeCloseRisk(safety: WorktreeCloseSafety): boolean {
  return safety.has_uncommitted || safety.has_unpushed;
}
