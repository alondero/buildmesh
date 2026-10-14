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

    // Cross-reference worktree paths AND branch names against THIS mesh's
    // non-archived agent nodes (issue #661 — mesh-scoped symmetry).
    // Branch names are not filesystem-unique like paths, so a `feature-a`
    // in `/repo1` would collide with a `feature-a` in `/repo2` if a
    // Mesh-A agent was on its own `feature-a`, falsely flagging Mesh-B's
    // branch as `is_active` in the prune UI. Worktree paths happen to be
    // host-unique on disk, but we still feed them through the same mesh-
    // scoped query — the boundary is "this mesh's nodes" full stop, so
    // a future nested-repo setup can't accidentally surface a sibling
    // mesh's worktree as active. `active_node_branches` further drops any
    // `Archived` nodes defensively (the SQL query already filters them,
    // but the helper enforces the contract for any future caller that
    // passes unfiltered input).
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
// Offloaded via `run_blocking`: branch deletion rewrites refs on disk with
// libgit2 — filesystem work that must not run inline on a Tauri async worker.
#[tauri::command]
pub async fn delete_branches(
    mesh_id: i64,
    worktree_path: String,
    branch_names: Vec<String>,
) -> Result<(), String> {
    crate::commands::run_blocking("delete_branches", move || {
        let nodes = db::list_agent_nodes_by_mesh(mesh_id)
            .map_err(|e| format!("failed to list agent nodes: {}", e))?;
        let active_branches = active_node_branches(&nodes);
        delete_branches_in_repo(
            &to_host_path(&worktree_path),
            &branch_names,
            &active_branches,
        )
    })
    .await
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
    // pool-path rejection in `remove_worktrees` below.
    //
    // Note on the "continue past failures" contract: the original function
    // accumulated per-branch git2 errors and still attempted the rest of
    // the batch. The active-branch carve-out intentionally departs from
    // that — a request containing any active branch is rejected atomically
    // (matching `remove_worktrees` for pool paths), because silently
    // deleting the deletable siblings would leave the user with a partial
    // state they may not want. The frontend disables active-branch rows so
    // this only fires for stale-UI / direct-API paths.
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
fn reject_blocked<T: AsRef<str>>(blocked: &[&T], header: &str) -> Result<(), String> {
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

/// Run `git fetch --prune` inside `services::sync_lock::with_mesh_sync_lock`
/// so the worktree prune can't race against itself (issue #709 — the
/// fourth and final per-Mesh git shell-out to land under the lock).
///
/// **Lock key is the worktree's own path** (`worktree_path`), NOT the
/// parent mesh's `agent_nodes.path`. Rationale:
///
/// The contention scope for `git fetch --prune` from a worktree is the
/// worktree's remote-tracking refs and the shared parent
/// `.git/FETCH_HEAD`. Two concurrent prunes on different worktrees of
/// the same mesh share the parent's `.git` and CAN race on `FETCH_HEAD`;
/// the parent-mesh key would serialise those, but the per-worktree key
/// only serialises prunes on the SAME worktree. The issue's author made
/// this trade-off deliberately: the prune UI fires once per worktree
/// (the user picks the worktree from a list), so same-worktree
/// collisions are the realistic case. Cross-worktree collisions on the
/// parent's `FET_HEAD` are guarded by the much narrower window that
/// `git fetch --prune` opens (single refspec negotiation) versus the
/// all-refs auto-sync fetch.
///
/// All four lock-wrap sites now use the same shape — `locked_*` helper +
/// `with_mesh_sync_lock(key, || work())` — so adding a fifth site is
/// copy-paste trivial rather than a new pattern.
///
/// The closure captures `worktree_path` by reference; the host-path
/// conversion (`to_host_path`) happens once outside the closure so the
/// closure body is the bare shell-out, mirroring `fetch_single_ref` /
/// `locked_fetch_pr_head`'s closure shape.
fn locked_prune_remote_tracking(worktree_path: &str) -> Result<String, String> {
    let host_path = to_host_path(worktree_path);
    crate::services::sync_lock::with_mesh_sync_lock(worktree_path, || {
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
fn collect_prune_info(
    repo_path: &str,
    active_paths: &[String],
    active_branches: &[String],
    is_pool_path: &dyn Fn(&str) -> bool,
) -> Result<GitRepoPruneInfo, String> {
    let repo = Repository::open(repo_path).map_err(|e| e.to_string())?;

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
    let mut branch_to_wt_path: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

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
        for wt_name in names.iter().flatten() {
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
            if let Some(upstream_ref) = upstream_buf.as_str() {
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

fn path_is_active(path: &str, active_paths: &[String]) -> bool {
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

#[cfg(test)]
#[path = "prune_tests.rs"]
mod prune_tests;
