/**
 * The "this mesh's folder is gone" signal (issue #2602).
 *
 * A mesh whose folder no longer exists fails the Git reads it is asked
 * for. The frontend used to keep asking: a `git-changed` event for a
 * sibling worktree under a shared container, a window focus, a re-mount of
 * the sidebar each produced a fresh failing IPC call — about 140
 * failures per mesh in five hours, each one a backend round-trip and a pair
 * of log lines.
 *
 * The backend prefixes the error with {@link MESH_FOLDER_MISSING} when — and
 * only when — the folder it was asked about is not there. Everything else
 * keeps failing loudly, because everything else might fix itself.
 *
 * Which clients stop on it
 * ------------------------
 * The three clients the issue names — the mesh health, the Git summary and
 * the mesh GitHub URL — treat the marker as a reason to stop asking on their
 * own initiative. The other mesh-level reads are exempt, and not by
 * accident: `get_mesh_git_static` answers "is this a repository?" with
 * *data* (`is_git_repo: false`), not with an error, and the file-list read
 * behind `get_mesh_git_status` is gated on that check, so it is not asked at
 * all for a folder that is gone.
 */

/**
 * The prefix a command's error carries when the folder it was asked about
 * no longer exists.
 *
 * The Rust half is `MESH_FOLDER_MISSING` in
 * `src-tauri/src/services/mesh_folder.rs`. Both halves are pinned by a test
 * so a rename cannot silently break the other side.
 */
export const MESH_FOLDER_MISSING = 'MESH_FOLDER_MISSING:';

/**
 * Whether `error` means the folder it was asked about is gone.
 *
 * Accepts anything the cache layer might hand over: an `Error`, a raw IPC
 * string, or `null`.
 */
export function isMeshFolderMissing(error: unknown): boolean {
  if (error === null || error === undefined) return false;
  const message = error instanceof Error ? error.message : String(error);
  return message.includes(MESH_FOLDER_MISSING);
}
