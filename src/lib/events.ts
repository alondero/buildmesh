export const GIT_CHANGED = 'git-changed';

/**
 * Event name emitted by the backend after any worktree-directory setting
 * changes (issue #1519) — `update_mesh_worktree_directory` (payload
 * `mesh_id`) and `set_app_worktree_directory` (payload `null`, every
 * inheriting mesh may have moved). Symmetric constant in
 * `src-tauri/src/commands/mesh_properties.rs` as
 * `WORKTREE_DIR_CHANGED_EVENT`.
 */
export const WORKTREE_DIR_CHANGED_EVENT = 'worktree-directory-changed';
