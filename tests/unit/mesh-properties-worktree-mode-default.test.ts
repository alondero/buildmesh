import { describe, it, expect } from 'vitest';
import { DEFAULT_WORKTREE_MODE } from '../../src/lib/worktreeMode';

describe('worktree mode default (cross-language pin)', () => {
  // Pin the frontend default. The Rust-side constant
  // `DEFAULT_WORKTREE_MODE` in `src-tauri/src/agent/spawn.rs` is pinned by
  // its own unit test; this test catches a flip of the TS constant. The two
  // sides are coupled by convention — a cross-language drift would need a
  // manual check. See [[feedback_cross-language-default-coupling]].
  it('uses branched as the default worktree mode', () => {
    expect(DEFAULT_WORKTREE_MODE).toBe('branched');
  });
});
