//! Git branch & worktree pruning — enumeration and batch deletion.
//!
//! All git reads go through git2 against the live filesystem (never cached),
//! so the view always reflects on-disk reality. The `is_active` flag is the
//! one piece that crosses into application state: a worktree is "active" when
//! a non-archived agent node points at its path.
//!
//! The enumeration and the branch deletion live in [`crate::git::branches`];
//! this layer supplies the application state (which nodes are live, which
//! paths are warm-pool entries) and turns the result into a wire type.

use crate::db;
use crate::env::{active_node_branches, to_host_path};
use crate::git::branches::{
    collect_prune_info, delete_branches_in_repo, path_is_active, reject_blocked,
};
use crate::models::GitRepoPruneInfo;

/// Discover all local branches, worktrees, and remote-tracking branches for
/// the repo(s) in a mesh. MVP: the mesh path is treated as a single repo.
// Offloaded via `run_blocking` (issue #762 convention): the enumeration walks
// every local branch, worktree, and remote-tracking ref with libgit2 — and the
// squash-merge detection can compute up to `SQUASH_SCAN_CAP` patch-ids — which
// on a large repo parks a Tauri async worker for the whole walk. The Worktree
// Manager tab re-fetches this on every `git-changed` event, so at agent-edit
// rates an inline body starves the pool (the #761/#762 freeze class).
#[tauri::command]
pub async fn get_git_prune_info(mesh_id: i64) -> Result<Vec<GitRepoPruneInfo>, String> {
    crate::commands::run_blocking("get_git_prune_info", move || {
        get_git_prune_info_blocking(mesh_id)
    })
    .await
}

/// Sync core for [`get_git_prune_info`] — plain fn so the command can offload
/// it to the blocking pool.
fn get_git_prune_info_blocking(mesh_id: i64) -> Result<Vec<GitRepoPruneInfo>, String> {
    let mesh =
        db::get_mesh_by_id(mesh_id).map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Sibling Meshes share ownership within a common Git repository. Branch
    // names in unrelated repositories remain independent (#661).
    let nodes = crate::git::project::repository_nodes(
        &mesh.path,
        db::list_agent_nodes().map_err(|e| format!("failed to list agent nodes: {e}"))?,
    )?;
    let active_paths = crate::git::project::active_paths(&nodes);
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
/// Active branch protection includes sibling Meshes sharing the common Git
/// directory; branch names in unrelated repositories remain independent.
// Offloaded via `run_blocking`: branch deletion rewrites refs on disk with
// libgit2 — filesystem work that must not run inline on a Tauri async worker.
#[tauri::command]
pub async fn delete_branches(
    mesh_id: i64,
    worktree_path: String,
    branch_names: Vec<String>,
) -> Result<(), String> {
    crate::commands::run_blocking("delete_branches", move || {
        let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
        let nodes = crate::git::project::repository_nodes(
            &mesh.path,
            db::list_agent_nodes().map_err(|e| format!("failed to list agent nodes: {e}"))?,
        )?;
        let active_branches = active_node_branches(&nodes);
        delete_branches_in_repo(
            &to_host_path(&worktree_path),
            &branch_names,
            &active_branches,
        )
    })
    .await
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
// Offloaded via `run_blocking`: each removal is a recursive directory delete
// (potentially gigabytes of node_modules/target) plus a libgit2 prune — on a
// Tauri async worker that parks the pool for the whole delete, which on
// Windows can run tens of seconds.
#[tauri::command]
pub async fn delete_worktrees(worktree_paths: Vec<String>) -> Result<(), String> {
    crate::commands::run_blocking("delete_worktrees", move || {
        remove_worktrees(&worktree_paths)
    })
    .await
}

fn remove_worktrees(worktree_paths: &[String]) -> Result<(), String> {
    // Fail closed if ownership cannot be read: a sibling Mesh's live checkout
    // is just as protected as one visible in the current Mesh.
    let nodes =
        db::list_agent_nodes().map_err(|e| format!("cannot read worktree ownership: {e}"))?;
    let active_paths = crate::git::project::active_paths(&nodes);
    let owned: Vec<&String> = worktree_paths
        .iter()
        .filter(|path| path_is_active(path, &active_paths))
        .collect();
    reject_blocked(
        &owned,
        "cannot delete worktrees held by an active agent node",
    )?;
    // Partition: pool paths are rejected up-front with a single combined
    // error so the user sees the full blocked set in one toast, not
    // multiple per-path errors mixed with successful deletes. Mirror of
    // the active-branch partition in `delete_branches_in_repo` — both
    // route their blocked subset through `reject_blocked` so the
    // combined-error contract lives in one place.
    //
    // DB error handling: `is_warm_pool_path`'s `Err` is treated as
    // "not blocked" so a transient DB read failure doesn't silently
    // swallow a legitimate prune request. The user can still retry.
    let (pool_paths, deletable): (Vec<&String>, Vec<&String>) = worktree_paths
        .iter()
        .partition(|path| matches!(db::is_warm_pool_path(path), Ok(true)));
    reject_blocked(
        &pool_paths,
        "cannot delete pre-spawn pool entries (managed automatically)",
    )?;

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
///
/// Returns the trimmed stderr from `git fetch --prune` on success so the
/// frontend can show what happened (e.g. `"From <url>\n - [deleted]
/// origin/feature-x"` when refs were dropped, or an empty/`"From <url>"`
/// line when nothing changed). Issue #657.
///
/// Delegates to [`locked_prune_remote_tracking`] so the fetch acquires the
/// per-Mesh sync lock and serialises against the auto-sync (`#652`),
/// manual `git_sync` (`#680`), PR-spawn head fetch (`#698`), and any
/// concurrent prune on the same worktree. Keyed on `&worktree_path` (the
/// worktree's own path), matching the prune UI's input — the same key a
/// second concurrent `prune_remote_tracking` call on the SAME worktree
/// would use, so the two can't collide on `.git/FETCH_HEAD`.
// Offloaded via `run_blocking`: the core is a network `git fetch --prune`
// (no client-side timeout) that additionally *waits on the blocking per-Mesh
// sync lock* — inline on a Tauri async worker either of those parks the
// worker for the duration (the overnight-freeze class, #761/#762).
#[tauri::command]
pub async fn prune_remote_tracking(worktree_path: String) -> Result<String, String> {
    crate::commands::run_blocking("prune_remote_tracking", move || {
        locked_prune_remote_tracking(&worktree_path)
    })
    .await
}

/// Run `git fetch --prune` inside `crate::git::sync_lock::with_mesh_sync_lock`
/// so the worktree prune can't race against itself (issue #709 — the
/// fourth and final per-Mesh git shell-out to land under the lock).
///
/// `sync_lock` discovers the supplied path to its common Git directory,
/// sharing coordination across linked worktrees and sibling Meshes.
fn locked_prune_remote_tracking(worktree_path: &str) -> Result<String, String> {
    let host_path = to_host_path(worktree_path);
    crate::git::sync_lock::with_mesh_sync_lock(worktree_path, || {
        // `run_command_with_timeout` (not a bare `.output()`) — this is a
        // network-facing fetch under the sync lock; a half-open connection
        // would otherwise wedge the blocking-pool thread AND the lock
        // forever (the issue #762 class). Same 5-minute manual-path cap as
        // the manual `git_sync`'s fetch.
        let mut prune_builder = crate::process_util::git_command();
        prune_builder
            .args(["fetch", "--prune"])
            .current_dir(&host_path);
        let output = crate::process_util::run_command_with_timeout(
            prune_builder,
            "git fetch --prune",
            std::time::Duration::from_secs(300),
        )?;

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if output.status.success() {
            Ok(stderr)
        } else {
            Err(stderr)
        }
    })
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod prune_tests;
