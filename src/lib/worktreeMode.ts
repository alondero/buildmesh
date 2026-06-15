// Cross-language sentinel for the Rust `DEFAULT_WORKTREE_MODE` in
// `src-tauri/src/agent/spawn.rs` (paired-constants pattern — see
// [[feedback_cross-language-default-coupling]]). The Rust constant is the
// source of truth at runtime (used when a mesh config leaves the field
// unset). The TS side currently has no UI consumer — `MeshPropertiesTab`
// (the only Mesh Properties surface) intentionally excludes the worktree
// mode radio, and `WorktreeManagerTab` doesn't expose one either — so this
// file is read by the cross-language pin test only. If a future PR
// re-exposes the worktree mode selector in the UI, import this constant
// for the form's default and update both tests.
export const DEFAULT_WORKTREE_MODE = 'branched';
