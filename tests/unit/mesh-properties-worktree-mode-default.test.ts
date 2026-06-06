import { describe, it, expect } from 'vitest';
import {
  DEFAULT_WORKTREE_MODE,
} from '../../src/components/MeshPropertiesPanel/MeshPropertiesPanel';

describe('MeshPropertiesPanel worktree mode default', () => {
  // Pin the frontend default. The Rust-side constant
  // `DEFAULT_WORKTREE_MODE` in `src-tauri/src/agent/spawn.rs` is pinned by
  // its own unit test; this test catches a flip of the TS constant. The two
  // sides are coupled by convention (matching the `(default)` radio label
  // in `WORKTREE_MODE_OPTIONS` and the example in `mesh.toml.example`) — a
  // cross-language drift would need a manual check.
  it('uses branched as the default worktree mode', () => {
    expect(DEFAULT_WORKTREE_MODE).toBe('branched');
  });
});
