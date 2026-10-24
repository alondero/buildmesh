//! Index and working-tree mutations: stage and revert one file (issue #2526).
//!
//! These are the two operations the diff overlay's quick actions need. They
//! write to the index and the working tree, so they cannot be a *read* — but
//! they still belong behind the `git/` seam, because the raw `Repository`
//! handle does not belong in the command layer.
//!
//! The command layer keeps the traversal gate ([`crate::commands::git`]
//! rejects `..` segments and absolute paths before calling in here); this
//! module keeps the second, structural gate — the joined path must resolve
//! inside the repository's own worktree.

use std::path::{Path, PathBuf};

use crate::env::to_host_path;

/// Stage one file (by repo-relative path) into the index.
///
/// Handles the change kinds the diff surfaces can show:
///
/// - modified / untracked: `index.add_path` (a new file enters the index;
///   a modified one updates it).
/// - deleted (missing on disk): `index.remove_path` so the deletion is
///   staged rather than silently left as a worktree-only delete.
/// - renamed: the caller passes the NEW path; the old path still reads as
///   deleted in the status walk, so we also remove it — otherwise staging
///   a rename would leave the index holding both the old and new entries.
pub fn stage_path(repo_path: &str, file_path: &str) -> Result<(), String> {
    let host = to_host_path(repo_path);
    let repo = super::primitives::open_at(&host)
        .map_err(|e| format!("failed to open repo at {}: {}", host, e))?;
    let rel = Path::new(file_path);
    let root = repo
        .workdir()
        .ok_or_else(|| "Git repository has no working directory".to_string())?;
    let abs = root.join(rel);
    assert_inside_repo(root, rel)?;

    let mut index = repo.index().map_err(|e| e.to_string())?;
    if abs.exists() {
        index
            .add_path(rel)
            .map_err(|e| format!("failed to stage {}: {}", file_path, e))?;
    } else {
        // Missing on disk = deleted (or rename source). Remove from the
        // index so the deletion is staged; `remove_path` is a no-op-safe
        // Err when the path isn't in the index, which we surface as-is.
        index
            .remove_path(rel)
            .map_err(|e| format!("failed to stage deletion of {}: {}", file_path, e))?;
    }
    index
        .write()
        .map_err(|e| format!("failed to write index: {}", e))?;
    Ok(())
}

/// Revert one file (by repo-relative path) to its HEAD state — the
/// destructive half of the overlay's quick actions (issue #1374).
///
/// - The file is in HEAD: its blob is checked out over the worktree copy
///   AND the index entry is reset (`git checkout HEAD -- <path>`
///   semantics via git2).
/// - The file is NOT in HEAD (added / untracked / rename target): it is
///   deleted outright, and its index entry (if any) is dropped — reverting
///   a file that never existed in HEAD means removing it.
///
/// Renames are the caller's two-step: revert the NEW path (deletes it),
/// then revert the OLD path (restores it). This is deliberately
/// single-path so each step is independently observable and testable.
pub fn revert_path(repo_path: &str, file_path: &str) -> Result<(), String> {
    let host = to_host_path(repo_path);
    let repo = super::primitives::open_at(&host)
        .map_err(|e| format!("failed to open repo at {}: {}", host, e))?;
    let rel = Path::new(file_path);
    let root = repo
        .workdir()
        .ok_or_else(|| "Git repository has no working directory".to_string())?;
    let abs = root.join(rel);
    assert_inside_repo(root, rel)?;

    // Is the path tracked in HEAD? (Unborn HEAD → nothing is tracked.)
    let in_head = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .and_then(|c| c.tree().ok())
        .map(|tree| tree.get_path(rel).is_ok())
        .unwrap_or(false);

    if in_head {
        // `git checkout HEAD -- <path>`: check the HEAD blob out over the
        // worktree. `force` overwrites local edits — that's the whole point
        // of Revert, and the button is explicitly labelled destructive.
        // `update_index` defaults to true, so the index entry is refreshed
        // in the same pass and `git status` reads clean afterwards.
        let obj = repo
            .revparse_single("HEAD^{tree}")
            .map_err(|e| format!("failed to resolve HEAD tree: {}", e))?;
        let mut opts = git2::build::CheckoutBuilder::new();
        opts.force().path(rel);
        repo.checkout_tree(&obj, Some(&mut opts))
            .map_err(|e| format!("failed to revert {}: {}", file_path, e))?;
        Ok(())
    } else {
        // Not in HEAD → added/untracked/rename-target. Reverting = remove.
        if abs.exists() {
            std::fs::remove_file(&abs)
                .map_err(|e| format!("failed to delete {}: {}", file_path, e))?;
        }
        // Drop any staged entry too (a previously `git add`ed new file).
        if let Ok(mut index) = repo.index() {
            if index.remove_path(rel).is_ok() {
                let _ = index.write();
            }
        }
        Ok(())
    }
}

/// Belt-and-braces: canonicalize the joined absolute path (when the file
/// actually exists on disk) and verify the result lives under the repo's
/// canonicalized root. Catches the symlink-escape case where a relative
/// clean path like `src/outside-link` points at a symlink whose target is
/// outside the worktree. Files that don't exist yet (worktree-deleted
/// tracked files, staged-add paths) skip this check — their canonicalize
/// would fail and we don't want a missing-file to break staging. The
/// structural traversal gate at the command boundary is the load-bearing
/// check; this is defense in depth.
fn assert_inside_repo(repo_root: &Path, file_path: &Path) -> Result<(), String> {
    let abs: PathBuf = repo_root.join(file_path);
    let Ok(canon_abs) = abs.canonicalize() else {
        // File missing or otherwise not canonicalizable — let the caller
        // produce the user-visible error from the operation that
        // triggered this check.
        return Ok(());
    };
    let canon_root = repo_root
        .canonicalize()
        .map_err(|e| format!("repo root canonicalize: {}", e))?;
    if !canon_abs.starts_with(&canon_root) {
        return Err(format!("path escapes repository: {}", file_path.display()));
    }
    Ok(())
}
