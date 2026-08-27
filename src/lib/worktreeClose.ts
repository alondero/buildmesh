// Re-export the generated wire type so the rest of the app keeps importing
// from `./worktreeClose` (tauri.ts, agentNodeStore, worktreeClosePromptStore).
// The Rust source lives at src-tauri/src/git/worktree/mod.rs; the generated
// TS lives at src/types/generated/WorktreeCloseSafety.ts. Issue #1248 — the
// previous hand-written interface here drifted from the Rust struct.
import type { WorktreeCloseSafety } from '../types/generated/WorktreeCloseSafety';
export type { WorktreeCloseSafety };

export type WorktreeCloseAction = 'remove' | 'keep' | 'cancel';

export function hasWorktreeCloseRisk(safety: WorktreeCloseSafety): boolean {
  return safety.has_uncommitted || safety.has_unpushed;
}
