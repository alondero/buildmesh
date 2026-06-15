//! Git branch & worktree pruning — enumeration and batch deletion.
//!
//! All git reads go through git2 against the live filesystem (never cached),
//! so the view always reflects on-disk reality. The `is_active` flag is the
//! one piece that crosses into application state: a worktree is "active" when
//! a non-archived agent node points at its path.

use git2::{BranchType, Oid, Repository};

use crate::db;
use crate::env::to_host_path;
use crate::git::primitives;
use crate::models::{BranchInfo, GitRepoPruneInfo, WorktreeInfo};

/// Discover all local branches, worktrees, and remote-tracking branches for
/// the repo(s) in a mesh. MVP: the mesh path is treated as a single repo.
#[tauri::command]
pub async fn get_git_prune_info(mesh_id: i64) -> Result<Vec<GitRepoPruneInfo>, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Cross-reference worktree paths against non-archived agent nodes.
    let active_paths: Vec<String> = db::list_agent_nodes()
        .map_err(|e| format!("failed to list agent nodes: {}", e))?
        .into_iter()
        .map(|n| n.path)
        .collect();

    let repo_path = to_host_path(&mesh.path);
    let info = collect_prune_info(&repo_path, &active_paths)?;
    Ok(vec![info])
}

/// Force-delete local branches by name from the repo at `worktree_path`.
/// Batches across all names, continuing past individual failures and
/// reporting the combined set of errors at the end.
#[tauri::command]
pub async fn delete_branches(
    worktree_path: String,
    branch_names: Vec<String>,
) -> Result<(), String> {
    delete_branches_in_repo(&to_host_path(&worktree_path), &branch_names)
}

fn delete_branches_in_repo(repo_path: &str, branch_names: &[String]) -> Result<(), String> {
    let repo = Repository::open(repo_path).map_err(|e| e.to_string())?;

    let mut errors: Vec<String> = Vec::new();
    for name in branch_names {
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

/// Remove worktrees by path. Each path is opened as a worktree of its parent
/// repo and pruned, deleting both the admin entry and the working directory.
/// The main worktree cannot be removed this way and surfaces as an error.
#[tauri::command]
pub async fn delete_worktrees(worktree_paths: Vec<String>) -> Result<(), String> {
    remove_worktrees(&worktree_paths)
}

fn remove_worktrees(worktree_paths: &[String]) -> Result<(), String> {
    let mut errors: Vec<String> = Vec::new();
    for path in worktree_paths {
        if let Err(e) = crate::git::worktree::remove_one_worktree(path) {
            errors.push(format!("{}: {}", path, e));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Prune stale remote-tracking refs by fetching with `--prune`.
#[tauri::command]
pub async fn prune_remote_tracking(worktree_path: String) -> Result<(), String> {
    let host_path = to_host_path(&worktree_path);
    let output = crate::process_util::command_no_window("git")
        .args(["fetch", "--prune"])
        .current_dir(&host_path)
        .output()
        .map_err(|e| format!("failed to run git fetch --prune: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

// ── Internals (DB-free, unit-testable against real temp repos) ──────────────

/// Resolve the tip commit of the repo's `main` (or `master`) branch, if any.
fn main_branch_oid(repo: &Repository) -> Option<git2::Oid> {
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
    if main_oid == branch_oid || repo.graph_descendant_of(main_oid, branch_oid).unwrap_or(false) {
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
    let from_tree = repo.find_commit(from).and_then(|c| c.tree()).map_err(|_| ())?;
    let to_tree = repo.find_commit(to).and_then(|c| c.tree()).map_err(|_| ())?;
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

/// Pure enumeration: given a repo path and the set of active node paths,
/// build the prune info. No DB access — the caller supplies `active_paths`.
fn collect_prune_info(
    repo_path: &str,
    active_paths: &[String],
) -> Result<GitRepoPruneInfo, String> {
    let repo = Repository::open(repo_path).map_err(|e| e.to_string())?;

    let main_oid = main_branch_oid(&repo);
    let head_dirty = primitives::is_dirty(&repo).unwrap_or(false);

    // ── Local branches ──────────────────────────────────────────────────
    let mut local_branches: Vec<BranchInfo> = Vec::new();
    let mut local_names: Vec<String> = Vec::new();

    let branches = repo
        .branches(Some(BranchType::Local))
        .map_err(|e| e.to_string())?;
    for entry in branches {
        let (branch, _) = entry.map_err(|e| e.to_string())?;
        let name = match branch.name().map_err(|e| e.to_string())? {
            Some(n) => n.to_string(),
            None => continue, // non-UTF8 branch name
        };
        local_names.push(name.clone());

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
            if let Some(upstream_ref) = upstream_buf.as_str() {
                match repo.find_reference(upstream_ref) {
                    Ok(up_ref) => {
                        if let (Some(local), Some(up)) =
                            (branch_oid, up_ref.target())
                        {
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

        local_branches.push(BranchInfo {
            name,
            is_head,
            is_merged_into_main,
            is_orphan,
            has_uncommitted: is_head && head_dirty,
            last_commit_date,
            ahead,
            behind,
        });
    }

    // ── Worktrees (main + linked) ───────────────────────────────────────
    let mut worktrees: Vec<WorktreeInfo> = Vec::new();

    if let Some(workdir) = repo.workdir() {
        let path = workdir.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
        let branch = primitives::head_branch_name(&repo);
        worktrees.push(WorktreeInfo {
            is_active: path_is_active(&path, active_paths),
            is_stale: branch_is_stale(&branch, &local_names),
            branch,
            path,
        });
    }

    if let Ok(names) = repo.worktrees() {
        for wt_name in names.iter().flatten() {
            let wt = match repo.find_worktree(wt_name) {
                Ok(w) => w,
                Err(_) => continue,
            };
            let path = wt.path().to_string_lossy().trim_end_matches(['/', '\\']).to_string();
            let branch = Repository::open(wt.path())
                .ok()
                .and_then(|r| primitives::head_branch_name(&r));
            worktrees.push(WorktreeInfo {
                is_active: path_is_active(&path, active_paths),
                is_stale: branch_is_stale(&branch, &local_names),
                branch,
                path,
            });
        }
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

fn path_is_active(path: &str, active_paths: &[String]) -> bool {
    let norm = primitives::normalize_for_compare(path);
    active_paths.iter().any(|p| primitives::normalize_for_compare(p) == norm)
}

/// A worktree is stale when it points at a branch that no longer exists
/// locally. Detached worktrees (no branch) are never stale.
fn branch_is_stale(branch: &Option<String>, local_names: &[String]) -> bool {
    matches!(branch, Some(b) if !local_names.iter().any(|n| n == b))
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod prune_tests;
