export const GIT_CHANGED = 'git-changed';

/**
 * Event name emitted by the backend after any worktree-directory setting
 * changes (issue #1519) — `update_mesh_worktree_directory` (payload
 * `mesh_id`) and `set_app_worktree_directory` (payload `null`, every
 * inheriting mesh may have moved). Symmetric constant in
 * `src-tauri/src/commands/mesh_properties.rs` as
 * `WORKTREE_DIR_CHANGED_EVENT`.
 *
 * Declared by the module that OWNS the stream —
 * `src/lib/streams/worktreeDirectoryStream.ts` (issue #2531) — so the
 * constant and its subscriber live in the same file. Re-exported here
 * for the callers that still import it from this module.
 */
export { WORKTREE_DIR_CHANGED_EVENT } from './streams/worktreeDirectoryStream';
