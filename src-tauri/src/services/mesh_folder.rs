//! The "this mesh's folder is gone" marker (issue #2602).
//!
//! A mesh whose folder no longer exists fails the Git reads it is asked
//! for. The frontend used to keep asking: a `git-changed` event for a
//! sibling worktree, a window focus, a re-mount of the sidebar each
//! produced a fresh failing IPC call, and each one cost a backend
//! round-trip and a pair of log lines. Four meshes with deleted folders
//! produced about 140 failures each in five hours.
//!
//! The marker gives the frontend one unambiguous signal to stop on. The
//! backend prefixes the error with [`MESH_FOLDER_MISSING`] when — and only
//! when — the folder it was asked about is not there, and the frontend's
//! `src/lib/meshFolder.ts` matches on that prefix. Everything else keeps
//! failing loudly, because everything else might fix itself.
//!
//! The prefix is an IPC contract, like the `pool-count-changed` event name:
//! both halves are pinned by a test so a rename cannot silently break the
//! other side.
//!
//! Which commands mark
//! -------------------
//! The three commands the issue names — `get_mesh_health`,
//! `get_git_summary` and `get_github_url_for_mesh` — all fail on a folder
//! that is gone, so all three mark their error.
//!
//! The other mesh-level Git reads are exempt, and not by accident:
//! `get_mesh_git_static` answers "is this a repository?" with *data*
//! (`is_git_repo: false`), not with an error, so there is no failure to
//! mark; and the file-list read behind `get_mesh_git_status` is gated on
//! that static check, so it is not asked at all for a folder that is gone.

/// The prefix a command's error carries when the folder it was asked about
/// no longer exists.
pub(crate) const MESH_FOLDER_MISSING: &str = "MESH_FOLDER_MISSING:";

/// Whether `path` names a folder that is not there.
///
/// The check is deliberately about the folder itself and not about the Git
/// repository inside it: a folder that exists but is not a repository is a
/// different failure with a different fix, and it is reported elsewhere
/// (issue #2348).
pub(crate) fn folder_is_missing(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    !std::path::Path::new(&crate::env::to_host_path(path)).exists()
}

/// Wrap `error` with the marker when `path` is gone.
///
/// Returns the error unchanged when the folder is there — a failure that
/// might fix itself must not be mistaken for one that cannot.
pub(crate) fn mark_if_missing(path: &str, error: String) -> String {
    if folder_is_missing(path) {
        format!("{} {}", MESH_FOLDER_MISSING, error)
    } else {
        error
    }
}

#[cfg(test)]
#[path = "mesh_folder_tests.rs"]
mod tests;
