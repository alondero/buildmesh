//! Git operations via git2 crate

use std::collections::HashMap;

use git2::{DiffOptions, Patch, Repository, StatusOptions};
use serde::{Deserialize, Serialize};
use tauri::command;
use tauri::Emitter;
use ts_rs::TS;

use crate::db;
use crate::env::to_host_path;
use crate::git::{health, primitives};
use crate::models::MeshHealth;
use crate::process_util::command_no_window;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitSummary.ts")]
/// Generated to src/types/generated/GitSummary.ts (issue #404). `usize`
/// counters carry `#[ts(as = "i32")]` so they emit `number`.
pub struct GitSummary {
    #[ts(as = "i32")]
    pub total: usize,
    #[ts(as = "i32")]
    pub added: usize,
    #[ts(as = "i32")]
    pub modified: usize,
    #[ts(as = "i32")]
    pub deleted: usize,
}

/// One changed file in a git status listing. Generated to
/// src/types/generated/GitStatus.ts (issue #359); the same struct backs the
/// desktop `get_git_status` Tauri command and the mobile `/git/status` HTTP
/// route. `usize` counts carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitStatus.ts")]
pub struct GitStatus {
    pub path: String,
    pub status: String, // "modified" | "added" | "deleted" | "renamed" | "untracked"
    #[ts(as = "i32")]
    pub additions: usize,
    #[ts(as = "i32")]
    pub deletions: usize,
}

/// Build a map of `relative path -> (additions, deletions)` covering every
/// uncommitted change (HEAD → index + working tree), so each status entry can
/// be annotated with its line-level diff stats.
///
/// Untracked files require `show_untracked_content` so their new lines are
/// counted as additions; without it git2 emits an empty patch for them.
fn line_stats_by_path(repo: &Repository) -> HashMap<String, (usize, usize)> {
    let mut diff_opts = DiffOptions::new();
    diff_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);

    // No HEAD yet (repo with no commits) → diff against an empty tree.
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());

    let mut stats: HashMap<String, (usize, usize)> = HashMap::new();

    let diff = match repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut diff_opts)) {
        Ok(diff) => diff,
        Err(_) => return stats,
    };

    let num_deltas = diff.deltas().len();
    for idx in 0..num_deltas {
        // Binary files / errors yield no patch — leave them at (0, 0).
        let patch = match Patch::from_diff(&diff, idx) {
            Ok(Some(p)) => p,
            _ => continue,
        };

        let (_context, additions, deletions) = match patch.line_stats() {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Key on both old and new path so deleted (old) and added (new) files
        // are both found by the status loop's relative path.
        let delta = patch.delta();
        for file in [delta.new_file().path(), delta.old_file().path()].into_iter().flatten() {
            stats.insert(file.to_string_lossy().to_string(), (additions, deletions));
        }
    }

    stats
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitBranchStatus.ts")]
/// Generated to src/types/generated/GitBranchStatus.ts (issue #404). `u32`
/// counters carry `#[ts(as = "i32")]` so they emit `number`.
pub struct GitBranchStatus {
    pub name: String,
    #[ts(as = "i32")]
    pub ahead: u32,
    #[ts(as = "i32")]
    pub behind: u32,
    /// Abbreviated HEAD OID (7 chars by default, matches `git rev-parse --short HEAD`).
    /// Empty string when HEAD is unborn. Useful for showing a stable identifier on
    /// detached-HEAD worktrees (e.g. after `free_base_branch` recovery detaches a
    /// branched worktree — see ADR 0006) where `name == "HEAD"` would otherwise be
    /// uninformative.
    pub short_sha: String,
}

/// Report the current branch and how far ahead/behind its upstream it is.
///
/// Returns `None` when `path` is not a git repository or HEAD is unborn (no
/// commits yet). A detached HEAD reports as `name = "HEAD"` with no upstream,
/// but `short_sha` is still populated so the UI can render e.g.
/// `detached @ a064f55`. Ahead/behind are `0` when no upstream is configured.
///
/// Uses git2's `graph_ahead_behind` rather than `git rev-list HEAD..@{u}`: the
/// brace syntax is silently mangled by `Command::args` on Windows
/// (see commands/prune.rs for the same pattern).
// `(async)` runs the command on a worker thread: git2 work (repo open, status
// walk, diffs) can take hundreds of ms on large repos, and a bare `#[command]`
// would execute it on the main thread, stalling the UI and every other IPC call.
#[command(async)]
pub fn get_git_branch_status(path: String) -> Result<Option<GitBranchStatus>, String> {
    let repo = match primitives::open_from_host_path(&path) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return Ok(None), // unborn HEAD (no commits yet)
    };

    // shorthand() yields the branch name, "HEAD" when detached, and None only
    // for a non-UTF8 ref name (which we can't represent, so treat as no branch).
    let name = match head.shorthand() {
        Some(n) => n.to_string(),
        None => return Ok(None),
    };

    let local_oid = head.target();

    // For unborn HEAD the OID is None; we leave short_sha empty rather than fabricate.
    let short_sha = local_oid
        .map(|oid| primitives::short_sha(&repo, oid))
        .unwrap_or_default();

    let (mut ahead, mut behind) = (0u32, 0u32);
    if let (Some(local), Some(up)) =
        (local_oid, primitives::upstream_oid_for_branch(&repo, &name))
    {
        if let Ok((a, b)) = primitives::ahead_behind(&repo, local, up) {
            ahead = a;
            behind = b;
        }
    }

    Ok(Some(GitBranchStatus {
        name,
        ahead,
        behind,
        short_sha,
    }))
}

/// Get git status for a directory — returns list of changed files with per-file
/// line additions/deletions for all uncommitted changes.
#[command(async)]
pub fn get_git_status(path: String) -> Result<Vec<GitStatus>, String> {
    let repo = primitives::open_from_host_path(&path).map_err(|e| e.to_string())?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut opts))
        .map_err(|e| e.to_string())?;

    let line_stats = line_stats_by_path(&repo);

    let mut changed_files: Vec<GitStatus> = Vec::new();

    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }

        let status_flag = entry.status();
        let status_str = if status_flag.is_index_new() || status_flag.is_wt_new() {
            "added"
        } else if status_flag.is_index_modified() || status_flag.is_wt_modified() {
            "modified"
        } else if status_flag.is_index_deleted() || status_flag.is_wt_deleted() {
            "deleted"
        } else if status_flag.is_index_renamed() || status_flag.is_wt_renamed() {
            "renamed"
        } else if status_flag.is_ignored() {
            continue;
        } else {
            "untracked"
        };

        let (additions, deletions) = line_stats.get(&path).copied().unwrap_or((0, 0));

        changed_files.push(GitStatus {
            path,
            status: status_str.to_string(),
            additions,
            deletions,
        });
    }

    Ok(changed_files)
}

/// Get aggregate git change summary for a directory
#[command(async)]
pub fn get_git_summary(path: String) -> Result<GitSummary, String> {
    let repo = primitives::open_from_host_path(&path).map_err(|e| e.to_string())?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut opts))
        .map_err(|e| e.to_string())?;

    let mut total = 0usize;
    let mut added = 0usize;
    let mut modified = 0usize;
    let mut deleted = 0usize;

    for entry in statuses.iter() {
        let status_flag = entry.status();
        if status_flag.is_ignored() {
            continue;
        }

        total += 1;
        if status_flag.is_index_new() || status_flag.is_wt_new() {
            added += 1;
        } else if status_flag.is_index_modified() || status_flag.is_wt_modified() {
            modified += 1;
        } else if status_flag.is_index_deleted() || status_flag.is_wt_deleted() {
            deleted += 1;
        }
        // renamed and untracked don't affect the count display but contribute to total
    }

    Ok(GitSummary {
        total,
        added,
        modified,
        deleted,
    })
}

/// Check whether a path is a valid git repository
#[command(async)]
pub fn check_is_git_repo(path: String) -> bool {
    git2::Repository::open(&path).is_ok()
}

/// Get the default branch name for the remote named "origin".
/// Reads the local symbolic ref (populated by clone/fetch) to avoid a network round-trip.
/// Falls back to "main" if no remote is configured or HEAD ref is missing.
#[command(async)]
pub fn get_default_branch(path: String) -> String {
    let repo = match Repository::open(&path) {
        Ok(r) => r,
        Err(_) => return "main".to_string(),
    };

    // Try the local symbolic ref first (no network needed)
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(target) = reference.symbolic_target() {
            if let Some(branch) = target.strip_prefix("refs/remotes/origin/") {
                return branch.to_string();
            }
        }
    }

    "main".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitSyncResult.ts")]
/// Generated to src/types/generated/GitSyncResult.ts (issue #404). `u32`
/// counter carries `#[ts(as = "i32")]` so it emits `number`.
pub struct GitSyncResult {
    pub fetched: bool,
    pub pulled: bool,
    #[ts(as = "i32")]
    pub new_commits: u32,
    pub message: String,
}

/// Fetch from the current branch's default remote and attempt a fast-forward pull.
/// Returns a structured result with feedback about what happened.
#[command]
pub async fn git_sync(path: String) -> Result<GitSyncResult, String> {
    let host_path = to_host_path(&path);

    // Step 1: git fetch
    let fetch_output = command_no_window("git")
        .args(["fetch"])
        .current_dir(&host_path)
        .output()
        .map_err(|e| format!("Failed to run git fetch: {}", e))?;

    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        return Ok(GitSyncResult {
            fetched: false,
            pulled: false,
            new_commits: 0,
            message: format!("Fetch failed: {}", stderr.trim()),
        });
    }

    // Step 2: Count how many commits we're behind, via the shared
    // `git::sync::commits_behind_upstream` (git2's `graph_ahead_behind`, which
    // avoids the Windows `@{u}` brace-stripping bug).
    let behind_count: u32 = Repository::open(&host_path)
        .map_err(|e| e.to_string())
        .and_then(|repo| crate::git::sync::commits_behind_upstream(&repo))
        .unwrap_or_else(|e| {
            tracing::warn!("git_sync: failed to count commits behind upstream: {}", e);
            0
        });

    if behind_count == 0 {
        return Ok(GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits: 0,
            message: "Already up to date".to_string(),
        });
    }

    // Step 3: git pull --ff-only --no-rebase. The explicit --no-rebase defeats a
    // user's global pull.rebase=true, which would otherwise turn this into a
    // rebase and write conflict markers on diverged history (same policy as the
    // auto-sync fetch_origin — see [[buildmesh-pull-rebase-default]]).
    let pull_output = command_no_window("git")
        .args(["pull", "--ff-only", "--no-rebase"])
        .current_dir(&host_path)
        .output()
        .map_err(|e| format!("Failed to run git pull: {}", e))?;

    if !pull_output.status.success() {
        let stderr = String::from_utf8_lossy(&pull_output.stderr);
        return Ok(GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits: behind_count,
            message: format!(
                "Fetched {} new commit{} but fast-forward failed: {}",
                behind_count,
                if behind_count == 1 { "" } else { "s" },
                stderr.trim()
            ),
        });
    }

    Ok(GitSyncResult {
        fetched: true,
        pulled: true,
        new_commits: behind_count,
        message: format!(
            "Pulled {} new commit{}",
            behind_count,
            if behind_count == 1 { "" } else { "s" }
        ),
    })
}

// ── Mesh health detection (issue #231) ──────────────────────────────────────

/// Result of a successful `restore_mesh_to_base` invocation. `restored` is
/// `true` only when HEAD actually moved; an already-on-base call returns
/// `restored = false` with a "no-op" message so the UI can distinguish
/// the two outcomes for telemetry / toast wording.
///
/// Generated to src/types/generated/RestoreResult.ts (issue #404).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "RestoreResult.ts")]
pub struct RestoreResult {
    pub restored: bool,
    pub message: String,
}

/// Result of a successful `free_base_branch` invocation. `detached_at_sha`
/// is the 7-char short OID the worktree was detached at, useful for the
/// "freed at a064f55" toast.
///
/// Generated to src/types/generated/FreeResult.ts (issue #404).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "FreeResult.ts")]
pub struct FreeResult {
    pub detached_at_sha: String,
}

// ── Tauri commands (issue #231) ─────────────────────────────────────────────

/// Compute a `MeshHealth` snapshot for the given mesh. The snapshot is
/// what powers the sidebar `!` badge and the health block in
/// `BranchesWorktreesSection`. The active-paths list is sourced from
/// the live agent-nodes table so the holder's `is_active` reflects
/// whether a real agent is using the worktree.
#[command]
pub async fn get_mesh_health(mesh_id: i64) -> Result<MeshHealth, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let host_path = to_host_path(&mesh.path);
    let repo = Repository::open(&host_path)
        .map_err(|e| format!("failed to open repo at {}: {}", host_path, e))?;

    let mut health = health::compute_mesh_health(&repo, &mesh.base_ref)?;

    // Refine the holder: compute the holder with live active-paths so
    // `is_active` reflects whether an agent node points at the worktree.
    if let Some(local_base) = health.local_base_branch.clone() {
        let active_paths: Vec<String> = db::list_agent_nodes()
            .map_err(|e| format!("failed to list agent nodes: {}", e))?
            .into_iter()
            .map(|n| n.path)
            .collect();
        health.base_branch_holder =
            health::find_base_branch_holder(&repo, &local_base, &active_paths);
    }

    Ok(health)
}

/// Restore the Mesh root to its base branch. Guarded by the same
/// `restore_to_base_impl` rules: refuses if the root is dirty, has
/// unpushed commits, or the base branch is held by a worktree.
///
/// On success, emits a `git-changed` event for the mesh path so the
/// sidebar `!` badge clears and the file explorer refreshes.
#[command]
pub async fn restore_mesh_to_base(
    mesh_id: i64,
    app: tauri::AppHandle,
) -> Result<RestoreResult, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let host_path = to_host_path(&mesh.path);
    let repo = Repository::open(&host_path)
        .map_err(|e| format!("failed to open repo at {}: {}", host_path, e))?;
    let local_base = health::parse_local_branch(&mesh.base_ref)
        .ok_or_else(|| format!("base_ref '{}' is not a valid branch", mesh.base_ref))?;

    let moved = health::restore_to_base_impl(&repo, &local_base)?;
    let message = if moved {
        format!("checked out {}", local_base)
    } else {
        format!("already on {}", local_base)
    };

    // Notify the frontend so the badge clears and the panel refreshes.
    let _ = app.emit(
        "git-changed",
        serde_json::json!({ "path": host_path, "internal_path": host_path }),
    );

    Ok(RestoreResult { restored: moved, message })
}

/// Detach the holding worktree's HEAD at its current commit, releasing
/// the branch lock so the root can `git checkout <base>` next. Idempotent
/// and non-destructive — the worktree's working tree, index, and HEAD
/// commit are all preserved; only the symbolic `HEAD → refs/heads/<base>`
/// link is replaced with a detached HEAD at the same OID.
///
/// On success, emits a `git-changed` event for the freed worktree's path
/// so any open panel re-fetches.
#[command]
pub async fn free_base_branch(
    mesh_id: i64,
    worktree_path: String,
    app: tauri::AppHandle,
) -> Result<FreeResult, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let local_base = health::parse_local_branch(&mesh.base_ref)
        .ok_or_else(|| format!("base_ref '{}' is not a valid branch", mesh.base_ref))?;

    let host_wt_path = to_host_path(&worktree_path);
    let detached_at_sha = health::free_base_branch_impl(&host_wt_path, &local_base)?;

    // Notify the frontend so the panel refreshes. The freed worktree's
    // `path` is what `get_git_prune_info` and the worktree list use to
    // identify the worktree.
    let _ = app.emit(
        "git-changed",
        serde_json::json!({
            "path": to_host_path(&mesh.path),
            "internal_path": host_wt_path,
        }),
    );

    Ok(FreeResult { detached_at_sha })
}

