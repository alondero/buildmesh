//! Git branch & worktree pruning — enumeration and batch deletion.
//!
//! All git reads go through git2 against the live filesystem (never cached),
//! so the view always reflects on-disk reality. The `is_active` flag is the
//! one piece that crosses into application state: a worktree is "active" when
//! a non-archived agent node points at its path.

use git2::{BranchType, Oid, Repository};

use crate::db;
use crate::env::{active_node_branches, active_node_paths, to_host_path};
use crate::git::primitives;
use crate::models::{BranchInfo, GitRepoPruneInfo, WorktreeInfo};

/// Discover all local branches, worktrees, and remote-tracking branches for
/// the repo(s) in a mesh. MVP: the mesh path is treated as a single repo.
#[tauri::command]
pub async fn get_git_prune_info(mesh_id: i64) -> Result<Vec<GitRepoPruneInfo>, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Cross-reference worktree paths AND branch names against THIS mesh's
    // non-archived agent nodes. Mesh-scoping matters more for branches
    // than for worktrees: worktree paths are filesystem-unique on the
    // host, so a global path-set happens to work; branch names are NOT
    // — `feature-a` in `/repo1` would collide with `feature-a` in
    // `/repo2` if a Mesh-A agent was on its own `feature-a`, falsely
    // flagging Mesh-B's branch as `is_active` in the prune UI. The
    // mesh-scoped query prevents that. `active_node_branches` further
    // drops any `Archived` nodes defensively (the SQL query already
    // filters them, but the helper enforces the contract for any future
    // caller that passes unfiltered input).
    let nodes = db::list_agent_nodes_by_mesh(mesh_id)
        .map_err(|e| format!("failed to list agent nodes: {}", e))?;
    let active_paths = active_node_paths(&nodes);
    let active_branches = active_node_branches(&nodes);

    let repo_path = to_host_path(&mesh.path);
    // Pool-path lookup is supplied as a closure so the pure enumeration
    // function stays DB-free and unit-testable against temp repos. Tests
    // pass `|_| false` (the `no_pool_paths` helper in `prune_tests.rs`)
    // so they don't need the global DB initialized — production here
    // wraps `db::is_warm_pool_path` for the real pool-entry classification.
    let is_pool_path = |p: &str| db::is_warm_pool_path(p).unwrap_or(false);
    let info = collect_prune_info(&repo_path, &active_paths, &active_branches, &is_pool_path)?;
    Ok(vec![info])
}

/// Force-delete local branches by name from the repo at `worktree_path`.
/// Batches across all names, continuing past individual failures and
/// reporting the combined set of errors at the end.
///
/// `mesh_id` is used to scope the "is this branch active" guard: the
/// mesh's non-archived agent nodes feed `active_branches`, and any
/// requested branch whose name matches an active node's `branch` is
/// rejected before we touch git. Mesh-scoping prevents cross-mesh
/// false positives — a `feature-a` in `/repo2` is not blocked by a
/// node in `/repo1` that happens to be on its own `feature-a`. The UI
/// already disables the row's checkbox as the primary defence; this
/// is belt-and-braces.
#[tauri::command]
pub async fn delete_branches(
    mesh_id: i64,
    worktree_path: String,
    branch_names: Vec<String>,
) -> Result<(), String> {
    let nodes = db::list_agent_nodes_by_mesh(mesh_id)
        .map_err(|e| format!("failed to list agent nodes: {}", e))?;
    let active_branches = active_node_branches(&nodes);
    delete_branches_in_repo(
        &to_host_path(&worktree_path),
        &branch_names,
        &active_branches,
    )
}

fn delete_branches_in_repo(
    repo_path: &str,
    branch_names: &[String],
    active_branches: &[String],
) -> Result<(), String> {
    let repo = Repository::open(repo_path).map_err(|e| e.to_string())?;

    // Partition: active branches are rejected up-front with a single combined
    // error so the user sees the full blocked set in one toast, not multiple
    // per-branch errors mixed with successful deletes. Mirrors the
    // pool-path rejection in `remove_worktrees` above.
    //
    // Note on the "continue past failures" contract: the original function
    // accumulated per-branch git2 errors and still attempted the rest of
    // the batch. The active-branch carve-out intentionally departs from
    // that — a request containing any active branch is rejected atomically
    // (matching `remove_worktrees` for pool paths), because silently
    // deleting the deletable siblings would leave the user with a partial
    // state they may not want. The frontend disables active-branch rows so
    // this only fires for stale-UI / direct-API paths.
    let mut active_blocked: Vec<&String> = Vec::new();
    let mut deletable: Vec<&String> = Vec::new();
    for name in branch_names {
        if active_branches.iter().any(|b| b == name) {
            active_blocked.push(name);
        } else {
            deletable.push(name);
        }
    }
    if !active_blocked.is_empty() {
        return Err(format!(
            "cannot delete branches held by an active agent node: {}",
            active_blocked
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

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

/// Remove worktrees by path. Each path is opened as a worktree of its parent
/// repo and pruned, deleting both the admin entry and the working directory.
/// The main worktree cannot be removed this way and surfaces as an error.
///
/// Pool entries are rejected up-front: a pre-spawn worktree's directory
/// is owned by the `services::warm_pool` background worker, which will
/// refill it on the next reconcile (or as soon as the user enables the
/// pool again). Allowing the Worktree Manager tab to delete a pool
/// entry would either (a) succeed in deleting the dir, leaving the
/// worker to detect a missing dir on its next `prewarm_one` and
/// re-cut — wasteful and confusing — or (b) leave a stale row the
/// worker doesn't GC until the next reconcile. Either way, the user
/// loses the warm-path latency win. The Worktree Manager UI also
/// disables the row's checkbox as the primary defence; this backend
/// check is defence-in-depth.
#[tauri::command]
pub async fn delete_worktrees(worktree_paths: Vec<String>) -> Result<(), String> {
    remove_worktrees(&worktree_paths)
}

fn remove_worktrees(worktree_paths: &[String]) -> Result<(), String> {
    // Partition: pool paths are rejected up-front with a single combined
    // error so the user sees the full blocked set in one toast, not
    // multiple per-path errors mixed with successful deletes.
    let mut pool_paths: Vec<&String> = Vec::new();
    let mut deletable: Vec<&String> = Vec::new();
    for path in worktree_paths {
        match db::is_warm_pool_path(path) {
            Ok(true) => pool_paths.push(path),
            Ok(false) => deletable.push(path),
            // DB error: be conservative and treat as deletable — we don't
            // want a transient DB read failure to silently swallow a
            // legitimate prune request. The user can still retry.
            Err(_) => deletable.push(path),
        }
    }
    if !pool_paths.is_empty() {
        return Err(format!(
            "cannot delete pre-spawn pool entries (managed automatically): {}",
            pool_paths
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut errors: Vec<String> = Vec::new();
    for path in deletable {
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

/// Pure enumeration: given a repo path, the set of active node paths and
/// branches, and an `is_pool_path` predicate, build the prune info. No DB
/// access — the caller supplies everything. The three siblings
/// (`active_paths` for worktrees, `active_branches` for branches,
/// `is_pool_path` for pre-spawn pool entries) feed the row flags; lifting
/// the pool check out of this function removes a latent test-isolation
/// race that surfaced when new branch-active tests started exercising
/// the worktree enumeration path.
fn collect_prune_info(
    repo_path: &str,
    active_paths: &[String],
    active_branches: &[String],
    is_pool_path: &dyn Fn(&str) -> bool,
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

        // Cross-reference against the active-branch set: a branch is
        // "active" iff its name matches any non-archived agent node's
        // `branch` field. Mirrors the worktree `is_active` derivation
        // (path matching) — same intent, different identity key.
        let is_active = active_branches.iter().any(|b| b == &name);

        local_branches.push(BranchInfo {
            name,
            is_head,
            is_merged_into_main,
            is_orphan,
            is_active,
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
                is_pool: is_pool_path(&path),
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
