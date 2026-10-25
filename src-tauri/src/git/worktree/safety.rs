//! Read-only close safety: dirty state and commits retained by other refs.

use super::*;

/// Inspect a worktree to decide whether closing its node can remove it.
///
/// A worktree we can't even open as a repository — its directory is gone or its
/// git metadata is unreadable — has no detectable work to protect and nothing
/// removable, so it reports `worktree_path: None` ("nothing to remove, safe to
/// close") rather than failing. Blocking the close on it would leave the node
/// permanently un-closable from the UI (#239).
pub fn close_safety(path: &str) -> WorktreeCloseSafety {
    let host_path = to_host_path(path);
    let nothing_to_remove = WorktreeCloseSafety {
        worktree_path: None,
        has_uncommitted: false,
        has_unpushed: false,
        is_detached: false,
    };

    let Ok(repo) = Repository::open(&host_path) else {
        return nothing_to_remove;
    };
    let Ok(head) = repo.head() else {
        return nothing_to_remove;
    };

    // Fail closed: an unreadable status means we can't prove the worktree is
    // safe to remove, so treat it as having work to protect.
    let has_uncommitted = primitives::is_dirty(&repo).unwrap_or(true);
    let is_detached = !head.is_branch();
    let has_unpushed = head
        .target()
        .map(|head_oid| head_has_unpushed_or_unmerged_commits(&repo, &head, head_oid, is_detached))
        .unwrap_or(false);

    WorktreeCloseSafety {
        worktree_path: Some(path.to_string()),
        has_uncommitted,
        has_unpushed,
        is_detached,
    }
}

pub(super) fn head_has_unpushed_or_unmerged_commits(
    repo: &Repository,
    head: &Reference,
    head_oid: Oid,
    is_detached: bool,
) -> bool {
    let current_refname = head.name().ok();

    if !is_detached {
        if let Some(refname) = current_refname {
            if let Ok(upstream_buf) = repo.branch_upstream_name(refname) {
                // Each step short-circuits on success only. A missing or
                // unreadable tracking ref is the everyday post-merge state
                // (`git status` shows `[gone]` after a squash-merge + remote
                // auto-delete); the reachability walk below is the better
                // safety net there, so we fall through instead of warning
                // unconditionally (#252).
                if let Ok(upstream_refname) = upstream_buf.as_str() {
                    if let Ok(upstream_ref) = repo.find_reference(upstream_refname) {
                        if let Some(upstream_oid) = upstream_ref.target() {
                            return primitives::ahead_behind(repo, head_oid, upstream_oid)
                                .map(|(ahead, _)| ahead > 0)
                                .unwrap_or(true);
                        }
                    }
                }
            }
        }
    }

    !head_is_reachable_from_another_branch_or_remote(repo, head_oid, current_refname)
}

pub(super) fn head_is_reachable_from_another_branch_or_remote(
    repo: &Repository,
    head_oid: Oid,
    current_refname: Option<&str>,
) -> bool {
    let Ok(references) = repo.references() else {
        return false;
    };

    for reference in references.flatten() {
        let Ok(name) = reference.name() else {
            continue;
        };
        if Some(name) == current_refname || name.ends_with("/HEAD") {
            continue;
        }
        if !name.starts_with("refs/heads/") && !name.starts_with("refs/remotes/") {
            continue;
        }
        let Some(ref_oid) = reference.target() else {
            continue;
        };

        if ref_oid == head_oid || repo.graph_descendant_of(ref_oid, head_oid).unwrap_or(false) {
            return true;
        }
    }

    false
}
