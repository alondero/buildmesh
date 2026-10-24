//! Branch and worktree enumeration, and branch deletion (issue #2526).
//!
//! The Worktree Manager's prune view is a read of the repository: its local
//! branches, its worktrees, its remote-tracking refs, and — for each branch —
//! how far ahead or behind its upstream is and whether its work is already
//! on `main`. That read, and the branch deletion it enables, belong to `git/`
//! so the command layer never holds a `Repository` handle.

use std::collections::HashMap;

use git2::{BranchType, Oid, Repository};

use crate::models::{BranchInfo, GitRepoPruneInfo, WorktreeInfo};

use super::primitives;

#[cfg(test)]
#[path = "branches_tests.rs"]
mod tests;

/// Delete local branches by name from the repo at `repo_path`.
///
/// Active branches are rejected up-front with a single combined error so the
/// user sees the full blocked set in one toast, not multiple per-branch errors
/// mixed with successful deletes. Mirrors the pool-path rejection in the
/// worktree removal path.
///
/// Note on the "continue past failures" contract: the original function
/// accumulated per-branch git2 errors and still attempted the rest of the
/// batch. The active-branch carve-out intentionally departs from that — a
/// request containing any active branch is rejected atomically, because
/// silently deleting the deletable siblings would leave the user with a
/// partial state they may not want.
pub(crate) fn delete_branches_in_repo(
    repo_path: &str,
    branch_names: &[String],
    active_branches: &[String],
) -> Result<(), String> {
    let repo = primitives::open_at(repo_path).map_err(|e| e.to_string())?;

    let (active_blocked, deletable): (Vec<&String>, Vec<&String>) = branch_names
        .iter()
        .partition(|name| active_branches.iter().any(|b| b == *name));
    reject_blocked(
        &active_blocked,
        "cannot delete branches held by an active agent node",
    )?;

    let mut errors: Vec<String> = Vec::new();
    for name in deletable {
        match repo.find_branch(name, BranchType::Local) {
            Ok(mut branch) => {
                if let Err(e) = branch.delete() {
                    errors.push(format!("{}: {}", name, e));
                }
            }
            Err(e) => errors.push(format!("{}: {}", name, e)),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Pure enumeration: given a repo path, the set of active node paths and
/// branches, and an `is_pool_path` predicate, build the prune info. No DB
/// access — the caller supplies everything. The three siblings
/// (`active_paths` for worktrees, `active_branches` for branches,
/// `is_pool_path` for pre-spawn pool entries) feed the row flags; lifting
/// the pool check out of this function removes a latent test-isolation
/// race that surfaced when new branch-active tests started exercising
/// the worktree enumeration path.
///
/// Three passes, in dependency order:
/// 1. Cheap local-branch name list — feeds worktree `is_stale`.
/// 2. Worktrees (main + linked) + a `branch → worktree_path` map so the
///    branch pass below can stamp `checked_out_in_worktree` on every
///    branch that is HEAD of some working tree (live or orphan).
/// 3. Full `BranchInfo` list with worktree annotation.
pub(crate) fn collect_prune_info(
    repo_path: &str,
    active_paths: &[String],
    active_branches: &[String],
    is_pool_path: &dyn Fn(&str) -> bool,
) -> Result<GitRepoPruneInfo, String> {
    let repo = primitives::open_at(repo_path).map_err(|e| e.to_string())?;

    let main_oid = main_branch_oid(&repo);
    let head_dirty = primitives::is_dirty(&repo).unwrap_or(false);

    // ── Pass 1: cheap local-branch name list ────────────────────────────
    let mut local_names: Vec<String> = Vec::new();
    for entry in repo
        .branches(Some(BranchType::Local))
        .map_err(|e| e.to_string())?
    {
        let (branch, _) = entry.map_err(|e| e.to_string())?;
        if let Ok(Some(name)) = branch.name() {
            local_names.push(name.to_string());
        }
    }

    // ── Pass 2: worktrees (main + linked) + branch → worktree-path map ──
    //
    // The map captures every branch checked out as the HEAD of some
    // working tree, regardless of whether an agent node still points at
    // it. A branch's value here drives `BranchInfo.checked_out_in_worktree`
    // below, which is what stops the libgit2 "current HEAD of a linked
    // repository" error in the prune UI: orphan agent worktrees
    // (whose node was deleted/archived but whose directory survives)
    // don't trip `is_active`, but they *do* appear in this map.
    let mut worktrees: Vec<WorktreeInfo> = Vec::new();
    let mut branch_to_wt_path: HashMap<String, String> = HashMap::new();

    if let Some(workdir) = repo.workdir() {
        let path = workdir
            .to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .to_string();
        let branch = primitives::head_branch_name(&repo);
        if let Some(ref b) = branch {
            branch_to_wt_path.insert(b.clone(), path.clone());
        }
        worktrees.push(WorktreeInfo {
            is_active: path_is_active(&path, active_paths),
            is_stale: branch_is_stale(&branch, &local_names),
            // Primary (repo-root) worktree is never a pool entry — pool
            // entries are always linked worktrees under
            // `.claude/worktrees/<slug>`. The predicate comes from the
            // caller (production: `db::is_warm_pool_path`; tests: `|_| false`)
            // so this function stays DB-free.
            is_pool: is_pool_path(&path),
            branch,
            path,
        });
    }

    if let Ok(names) = repo.worktrees() {
        for wt_name in names.iter().filter_map(|name| name.ok().flatten()) {
            let wt = match repo.find_worktree(wt_name) {
                Ok(w) => w,
                Err(_) => continue,
            };
            let path = wt
                .path()
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_string();
            let branch = Repository::open(wt.path())
                .ok()
                .and_then(|r| primitives::head_branch_name(&r));
            if let Some(ref b) = branch {
                branch_to_wt_path.insert(b.clone(), path.clone());
            }
            worktrees.push(WorktreeInfo {
                is_active: path_is_active(&path, active_paths),
                is_stale: branch_is_stale(&branch, &local_names),
                is_pool: is_pool_path(&path),
                branch,
                path,
            });
        }
    }

    // ── Pass 3: local branches with worktree annotation ─────────────────
    let mut local_branches: Vec<BranchInfo> = Vec::new();
    for entry in repo
        .branches(Some(BranchType::Local))
        .map_err(|e| e.to_string())?
    {
        let (branch, _) = entry.map_err(|e| e.to_string())?;
        let name = match branch.name().map_err(|e| e.to_string())? {
            Some(n) => n.to_string(),
            None => continue, // non-UTF8 branch name
        };

        let is_head = branch.is_head();
        let branch_oid = branch.get().peel_to_commit().ok().map(|c| c.id());
        let last_commit_date = branch
            .get()
            .peel_to_commit()
            .ok()
            .and_then(|c| format_commit_time(c.time()));

        // Upstream resolution → ahead/behind + orphan detection.
        let mut ahead = 0u64;
        let mut behind = 0u64;
        let mut is_orphan = false;
        let refname = format!("refs/heads/{}", name);
        if let Ok(upstream_buf) = repo.branch_upstream_name(&refname) {
            // An upstream is configured for this branch.
            if let Ok(upstream_ref) = upstream_buf.as_str() {
                match repo.find_reference(upstream_ref) {
                    Ok(up_ref) => {
                        if let (Some(local), Some(up)) = (branch_oid, up_ref.target()) {
                            if let Ok((a, b)) = primitives::ahead_behind(&repo, local, up) {
                                ahead = a as u64;
                                behind = b as u64;
                            }
                        }
                    }
                    // Configured upstream no longer exists → orphan.
                    Err(_) => is_orphan = true,
                }
            }
        }

        let is_merged_into_main = match (main_oid, branch_oid) {
            (Some(main), Some(b)) => Some(branch_is_merged_into_main(&repo, main, b)),
            _ => None,
        };

        // Cross-reference against the active-branch set: a branch is
        // "active" iff its name matches any non-archived agent node's
        // `branch` field. Mirrors the worktree `is_active` derivation
        // (path matching) — same intent, different identity key.
        let is_active = active_branches.iter().any(|b| b == &name);

        // Worktree HEAD annotation — independent of `is_active`. A branch
        // can be HEAD of an orphan worktree (no live node) or of a live
        // one (also `is_active: true`); the two flags overlap on the
        // live case but each covers a separate scenario on its own.
        let checked_out_in_worktree = branch_to_wt_path.get(&name).cloned();

        local_branches.push(BranchInfo {
            name,
            is_head,
            is_merged_into_main,
            is_orphan,
            is_active,
            checked_out_in_worktree,
            has_uncommitted: is_head && head_dirty,
            last_commit_date,
            ahead,
            behind,
        });
    }

    // ── Remote-tracking branches ────────────────────────────────────────
    let mut remote_tracking_branches: Vec<String> = Vec::new();
    if let Ok(branches) = repo.branches(Some(BranchType::Remote)) {
        for entry in branches.flatten() {
            if let Ok(Some(name)) = entry.0.name() {
                if name.ends_with("/HEAD") {
                    continue;
                }
                remote_tracking_branches.push(name.to_string());
            }
        }
    }

    Ok(GitRepoPruneInfo {
        path: repo_path.to_string(),
        local_branches,
        worktrees,
        remote_tracking_branches,
    })
}

/// Reject a batch request atomically when any of its members are blocked.
///
/// Returns `Err(<header>: <name1>, <name2>, …)` listing every blocked
/// item, or `Ok(())` when the batch is clean. Shared by the branch
/// (`delete_branches_in_repo`) and worktree (`remove_worktrees`) delete
/// paths — both reject any-blocked requests atomically and surface every
/// blocked item in one combined message rather than splitting the error
/// across multiple per-item failures interleaved with successful deletes
/// (issue #661). A stale-UI row or a misrouted direct API call must see
/// every blocked item, not just the first failure.
///
/// Caller responsibility: build `blocked` by filtering the request items
/// with the site-specific predicate first — branch-name membership for
/// `delete_branches`, warm-pool path for `delete_worktrees`. The
/// predicates genuinely differ between the two sites, so this helper is
/// purely the combined-error formatting; the partition itself stays at
/// each call site (typically via `iter().partition(...)`).
pub(crate) fn reject_blocked<T: AsRef<str>>(blocked: &[&T], header: &str) -> Result<(), String> {
    if blocked.is_empty() {
        return Ok(());
    }
    let names = blocked
        .iter()
        .map(|n| n.as_ref())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!("{header}: {names}"))
}

/// Whether a worktree path is one an agent node is using.
pub(crate) fn path_is_active(path: &str, active_paths: &[String]) -> bool {
    let norm = primitives::normalize_for_compare(path);
    active_paths
        .iter()
        .any(|p| primitives::normalize_for_compare(p) == norm)
}

/// A worktree is stale when it points at a branch that no longer exists
/// locally. Detached worktrees (no branch) are never stale.
fn branch_is_stale(branch: &Option<String>, local_names: &[String]) -> bool {
    matches!(branch, Some(b) if !local_names.iter().any(|n| n == b))
}

/// Resolve the tip commit of the repo's `main` (or `master`) branch, if any.
fn main_branch_oid(repo: &Repository) -> Option<Oid> {
    for candidate in ["main", "master"] {
        if let Ok(branch) = repo.find_branch(candidate, BranchType::Local) {
            if let Ok(commit) = branch.get().peel_to_commit() {
                return Some(commit.id());
            }
        }
    }
    None
}

/// Cap on how many commits to scan back from `main` when hunting for the squash
/// commit that integrated a branch. A real integration sits within a handful of
/// commits of the tip; the cap stops a pathological history from making the
/// prune scan crawl.
const SQUASH_SCAN_CAP: usize = 2000;

/// Whether `branch_oid`'s work is already present in `main_oid`.
///
/// Ancestry (`graph_descendant_of`) catches fast-forward and merge-commit
/// integration. But GitHub's default *squash and merge* rewrites the whole
/// branch into a single new commit on main with no ancestry link back — so an
/// ancestry-only check reports squash-merged branches as unmerged, and they
/// pile up locally forever. A squash commit's patch is, by construction, the
/// branch's *cumulative* diff against the merge base, so we compute that one
/// patch-id and look for a commit on main (since the merge base) whose own
/// patch matches. Rebase-and-merge that collapses to an equal patch is caught
/// the same way. False negatives are safe (the branch just isn't recommended);
/// a patch-id match is strong evidence the identical change is already on main.
fn branch_is_merged_into_main(repo: &Repository, main_oid: Oid, branch_oid: Oid) -> bool {
    if main_oid == branch_oid
        || repo
            .graph_descendant_of(main_oid, branch_oid)
            .unwrap_or(false)
    {
        return true;
    }

    let Ok(base) = repo.merge_base(main_oid, branch_oid) else {
        return false;
    };

    let cumulative = match cumulative_patch_id(repo, base, branch_oid) {
        // Identical trees → the branch adds nothing main lacks → nothing to
        // lose, so treat it as merged.
        Ok(None) => return true,
        Ok(Some(pid)) => pid,
        Err(()) => return false,
    };

    main_contains_patch(repo, main_oid, base, cumulative)
}

/// Patch-id of the cumulative diff from `from` to `to`. `Ok(None)` = the trees
/// are identical (empty diff); `Err(())` = it could not be computed.
fn cumulative_patch_id(repo: &Repository, from: Oid, to: Oid) -> Result<Option<Oid>, ()> {
    let from_tree = repo
        .find_commit(from)
        .and_then(|c| c.tree())
        .map_err(|_| ())?;
    let to_tree = repo
        .find_commit(to)
        .and_then(|c| c.tree())
        .map_err(|_| ())?;
    let diff = repo
        .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)
        .map_err(|_| ())?;
    if diff.deltas().len() == 0 {
        return Ok(None);
    }
    diff.patchid(None).map(Some).map_err(|_| ())
}

/// Scan commits added to `main` since `base` for one whose own patch equals
/// `target` — the fingerprint a squash merge of the branch leaves behind.
fn main_contains_patch(repo: &Repository, main_oid: Oid, base: Oid, target: Oid) -> bool {
    let Ok(mut walk) = repo.revwalk() else {
        return false;
    };
    if walk.push(main_oid).is_err() {
        return false;
    }
    let _ = walk.hide(base);

    for oid in walk.flatten().take(SQUASH_SCAN_CAP) {
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        // A squash lands as a single-parent commit; skip merge commits, whose
        // patch-id isn't comparable to a linear branch diff.
        if commit.parent_count() != 1 {
            continue;
        }
        let (Ok(tree), Ok(parent_tree)) = (commit.tree(), commit.parent(0).and_then(|p| p.tree()))
        else {
            continue;
        };
        let Ok(diff) = repo.diff_tree_to_tree(Some(&parent_tree), Some(&tree), None) else {
            continue;
        };
        if diff.patchid(None).map(|pid| pid == target).unwrap_or(false) {
            return true;
        }
    }
    false
}

fn format_commit_time(time: git2::Time) -> Option<String> {
    chrono::DateTime::from_timestamp(time.seconds(), 0).map(|dt| dt.to_rfc3339())
}
