//! Git operations via git2 crate

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use git2::{DiffOptions, Patch, Repository, StatusOptions};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use tauri::command;
use tauri::Emitter;
use ts_rs::TS;

use crate::db;
use crate::env::{active_node_paths, to_host_path};
use crate::git::{health, primitives};
use crate::models::MeshHealth;

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
///
/// Thin async wrapper that offloads the libgit2 work onto the blocking pool
/// (issue #762 — `#[command(async)]` would park a Tauri tokio worker; WSL UNC
/// paths with a paused VM can make `open_from_host_path` stall). The sync
/// core [`get_git_branch_status_blocking`] stays directly callable so the
/// mobile `/git/branch` HTTP route can `.await` it through here.
#[command]
pub async fn get_git_branch_status(path: String) -> Result<Option<GitBranchStatus>, String> {
    crate::commands::run_blocking("get_git_branch_status", move || {
        get_git_branch_status_blocking(path)
    })
    .await
}

/// Sync core for [`get_git_branch_status`]. See its doc for the offload
/// rationale.
pub(crate) fn get_git_branch_status_blocking(
    path: String,
) -> Result<Option<GitBranchStatus>, String> {
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
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload rationale.
#[command]
pub async fn get_git_status(path: String) -> Result<Vec<GitStatus>, String> {
    crate::commands::run_blocking("get_git_status", move || get_git_status_blocking(path)).await
}

/// Sync core for [`get_git_status`].
pub(crate) fn get_git_status_blocking(path: String) -> Result<Vec<GitStatus>, String> {
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

/// Get aggregate git change summary for a directory.
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload rationale.
#[command]
pub async fn get_git_summary(path: String) -> Result<GitSummary, String> {
    crate::commands::run_blocking("get_git_summary", move || get_git_summary_blocking(path)).await
}

/// Sync core for [`get_git_summary`].
pub(crate) fn get_git_summary_blocking(path: String) -> Result<GitSummary, String> {
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

/// Read the default branch name from an already-open repository.
///
/// Extracted from [`get_default_branch`] in #431 so call sites that
/// already have a `Repository` handle open (e.g. `get_mesh_git_static`'s
/// trio, `commands::ai_context`'s portability-PR helper) can reuse it
/// without paying for a second `Repository::open` + refdb lookup. Callers
/// with only a path still go through [`get_default_branch`].
pub(crate) fn default_branch_from_repo(repo: &Repository) -> String {
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(target) = reference.symbolic_target() {
            if let Some(branch) = target.strip_prefix("refs/remotes/origin/") {
                return branch.to_string();
            }
        }
    }
    "main".to_string()
}

/// Get the default branch name for the remote named "origin".
/// Reads the local symbolic ref (populated by clone/fetch) to avoid a network round-trip.
/// Falls back to "main" if no remote is configured or HEAD ref is missing.
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload rationale.
/// **Preserves the pre-#762 contract** of returning `String` directly (with
/// `"main"` as the fallback), so `spawn.rs` / `agent.rs` callers continue to
/// treat the return value as infallible. The `Result`-flavoured sync core
/// is `pub(crate)` for callers that want the distinction (and for tests).
#[command]
pub async fn get_default_branch(path: String) -> String {
    crate::commands::run_blocking("get_default_branch", move || {
        get_default_branch_blocking(path)
    })
    .await
    .unwrap_or_else(|_| "main".to_string())
}

/// Sync core for [`get_default_branch`]. Returns `Ok("main")` on any open
/// failure so the async wrapper can preserve its infallible contract; the
/// `Err` variant is reserved for genuine worker-pool join errors.
pub(crate) fn get_default_branch_blocking(path: String) -> Result<String, String> {
    let repo = match Repository::open(&path) {
        Ok(r) => r,
        Err(_) => return Ok("main".to_string()),
    };
    Ok(default_branch_from_repo(&repo))
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

/// Fetch from the current branch's configured upstream remote and
/// attempt a fast-forward pull. Returns a structured result with
/// feedback about what happened.
///
/// This is now a thin wrapper over the shared `do_sync` helper
/// (issue #274). The wrapper's job is purely the `git_sync`-specific
/// bookkeeping: resolve the remote from the current branch's
/// upstream (so the test
/// `git_sync_fetches_current_branch_upstream_when_remote_is_not_origin`
/// keeps passing — a repo whose upstream is named `main` rather
/// than `origin` must still be synced against that upstream), and
/// translate the shared `SyncOutcome` back to the `GitSyncResult`
/// shape the frontend expects (the wire type is unchanged).
///
/// Note: the shared helper always runs the fetch (a fetch never
/// touches the working tree) and only skips the fast-forward pull
/// when the tree is dirty (returning `FetchedButDirty` → "Fetched N
/// new commits — fast-forward skipped: working tree has uncommitted
/// changes"). A manual Sync click on a dirty mesh therefore still
/// freshens the remote-tracking refs that worktree nodes are cut from.
///
/// **Fetch scope trade-off:** the pre-#274 inline code did
/// `git fetch` with NO arguments, which git resolves to "fetch the
/// current branch's tracking ref only" (~300 ms on a 250-branch
/// remote). The new code passes `branch = None` to `do_sync`, which
/// runs `git fetch <remote>` and negotiates **every** remote-tracking
/// ref (7–36 s on the same repo, per the project's spawn-latency
/// memory note). The spawn-time `fetch_origin` keeps the narrow
/// fetch because cold-spawn latency is on the critical path; the
/// manual `git_sync` user click accepts the slower fetch in exchange
/// for a Sync that catches branches the user may have switched to.
/// A future change could resolve the current branch and pass it
/// here to recover the pre-#274 latency if user feedback warrants.
#[command]
pub async fn git_sync(path: String) -> Result<GitSyncResult, String> {
    // `do_sync` shells out to `git fetch`/`git pull` (a network round-trip
    // with no client-side timeout) behind a blocking per-Mesh mutex — that
    // must run on the blocking pool, not a Tauri async worker, or a stalled
    // fetch parks the worker (see the overnight-freeze investigation and
    // [`crate::commands::run_blocking`]).
    crate::commands::run_blocking("git_sync", move || git_sync_blocking(path)).await
}

/// Sync core for [`git_sync`]. Plain fn so the async command can offload it to
/// the blocking pool; also keeps the git_tests call shape (`.await` on the
/// command) intact.
fn git_sync_blocking(path: String) -> Result<GitSyncResult, String> {
    let host_path = to_host_path(&path);

    // Resolve the remote the same way `git fetch` (no args) used to:
    // the current branch's configured upstream, falling back to
    // `origin`. We open the repo here so we can query its branch
    // config; `do_sync` re-opens (microsecond cost) to keep its
    // signature self-contained and the call shape uniform with
    // `fetch_origin`.
    let repo = Repository::open(&host_path)
        .map_err(|e| format!("failed to open repo at {}: {}", host_path, e))?;
    let remote = crate::git::sync::current_branch_upstream_remote(&repo)
        .unwrap_or_else(|| "origin".to_string());

    // Delegate to the shared helper. Pass `None` for the branch —
    // `git_sync` is a manual user click, not a spawn-time path, so
    // the all-refs fetch is fine here (and matches the behaviour of
    // `git fetch` with no arguments that the pre-#274 code used).
    //
    // Issue #680 — wrap in the per-Mesh sync lock so a manual Sync
    // click can't race against concurrent spawn-time `fetch_origin`
    // (or against a second manual Sync) for the same Mesh. Without
    // this lock, two `git fetch` shell-outs collide on
    // `.git/FETCH_HEAD` / `refs/heads/<branch>.lock`. Keying on `&path`
    // (the DB-stored canonical form, NOT `&host_path`) matches the
    // spawn-time caller in `agent::spawn`, which keys on `node.path` —
    // both call sites share one lock entry per Mesh row.
    //
    // Issue #709 — the wrap is consolidated into
    // `git::sync::locked_do_sync` (issue #680's helper) so the lock-
    // acquisition shape is identical to the spawn-time
    // `locked_fetch_origin`, the PR-spawn's `locked_fetch_pr_head`,
    // and the prune's `locked_prune_remote_tracking`. The wrap body
    // used to live inline here.
    let outcome =
        crate::git::sync::locked_do_sync(&path, &host_path, &remote, None);

    // Ref-freshness (issue #613 AC3): a manual Sync that pulled new commits
    // moved the mesh's base ref, so its warm pool entries are now stale.
    // Kick a background freshness pass (serialized behind the pool fill lock)
    // to `git reset --hard` them onto the new commit. Fired only when new
    // commits actually arrived; `UpToDate` / skipped means nothing moved.
    // `SyncOutcome::advanced_ref()` is the single-sourced predicate that
    // matches the spawn-time `FetchOutcome::advanced_ref()` — both kept in
    // lockstep on `git::sync::SyncOutcome` / `git::sync::FetchOutcome`
    // (issue #634).
    // The `is_initialized` guard keeps this hermetic under `cargo test`:
    // `db::get()` panics on an uninitialized global DB, and a filtered test
    // run may execute `git_sync` before any DB-initializing test has run.
    // In production the DB is always initialized at startup.
    if outcome.advanced_ref() && crate::db::is_initialized() {
        if let Ok(mesh) = crate::db::get_mesh_by_path(&path) {
            let mesh_id = mesh.id;
            std::thread::spawn(move || {
                crate::services::warm_pool::on_fetch_completed(mesh_id);
            });
        }
    }

    Ok(sync_outcome_to_git_sync_result(outcome))
}

/// Map a [`crate::git::sync::SyncOutcome`] to the [`GitSyncResult`]
/// shape the frontend expects from the `git_sync` Tauri command. The
/// four wire fields are unchanged; the messages match the prior
/// `format!` strings where the variant maps cleanly.
fn sync_outcome_to_git_sync_result(
    outcome: crate::git::sync::SyncOutcome,
) -> GitSyncResult {
    use crate::git::sync::SyncOutcome;
    match outcome {
        SyncOutcome::FetchedButDirty { new_commits } => GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits,
            message: format!(
                "Fetched {} new commit{}; fast-forward skipped: working tree has \
                 uncommitted changes",
                new_commits,
                plural_s(new_commits)
            ),
        },
        SyncOutcome::SkippedNoRemote => GitSyncResult {
            fetched: false,
            pulled: false,
            new_commits: 0,
            message: "No remote configured".to_string(),
        },
        SyncOutcome::UpToDate => GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits: 0,
            message: "Already up to date".to_string(),
        },
        SyncOutcome::Synced { new_commits } => GitSyncResult {
            fetched: true,
            pulled: true,
            new_commits,
            message: format!(
                "Pulled {} new commit{}",
                new_commits,
                plural_s(new_commits)
            ),
        },
        SyncOutcome::FetchedButDiverged { new_commits, reason } => GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits,
            message: format!(
                "Fetched {} new commit{} but fast-forward failed: {}",
                new_commits,
                plural_s(new_commits),
                reason
            ),
        },
        SyncOutcome::PullTimedOut { new_commits, reason } => GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits,
            // Distinct wording from `FetchedButDiverged` so the user can
            // tell a network hang apart from a real history divergence
            // (issue #762 review). The 5-min `MANUAL_FETCH_TIMEOUT` cap is what
            // surfaced this; pre-#762 the pull path could only fail via
            // spawn-error or non-zero-exit.
            message: format!(
                "Fetched {} new commit{} but pull timed out: {}",
                new_commits,
                plural_s(new_commits),
                reason
            ),
        },
        SyncOutcome::FetchFailed { reason } => GitSyncResult {
            fetched: false,
            pulled: false,
            new_commits: 0,
            message: format!("Fetch failed: {}", reason),
        },
        SyncOutcome::RepoUnusable { reason } => GitSyncResult {
            fetched: false,
            pulled: false,
            new_commits: 0,
            message: format!("Repository unusable: {}", reason),
        },
    }
}

/// `""` for `n == 1`, `"s"` otherwise. Used four times in
/// `sync_outcome_to_git_sync_result` for the "N new commit[s]" wording.
fn plural_s(n: u32) -> &'static str {
    if n == 1 { "" } else { "s" }
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
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload
/// rationale. The libgit2 status walk + holder lookup can take hundreds
/// of ms on a large repo and must not park a Tauri tokio worker.
#[command]
pub async fn get_mesh_health(mesh_id: i64) -> Result<MeshHealth, String> {
    crate::commands::run_blocking("get_mesh_health", move || {
        get_mesh_health_blocking(mesh_id)
    })
    .await
}

/// Sync core for [`get_mesh_health`].
pub(crate) fn get_mesh_health_blocking(mesh_id: i64) -> Result<MeshHealth, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let host_path = to_host_path(&mesh.path);
    let repo = Repository::open(&host_path)
        .map_err(|e| format!("failed to open repo at {}: {}", host_path, e))?;

    let mut health = health::compute_mesh_health(&repo, &mesh.base_ref)?;

    // Refine the holder with the active paths for THIS mesh's nodes so
    // `is_active` reflects whether an agent node points at the worktree.
    // Scoped to `mesh_id` — the holder can only live inside this repo, so
    // nodes from other meshes are dead weight here.
    if let Some(local_base) = health.local_base_branch.clone() {
        let nodes = db::list_agent_nodes_by_mesh(mesh_id)
            .map_err(|e| format!("failed to list agent nodes: {}", e))?;
        let active_paths = active_node_paths(&nodes);
        health.base_branch_holder =
            health::find_base_branch_holder(&repo, &local_base, &active_paths);
    }

    Ok(health)
}

/// Restore the Mesh root to its Base Ref. Guarded by the same
/// `restore_to_base_impl` rules: refuses if the root is dirty, has
/// unpushed commits, or the Base Ref's branch is held by a worktree.
///
/// On success, emits a `git-changed` event for the mesh path so the
/// sidebar `!` badge clears and the file explorer refreshes.
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload
/// rationale. The mesh row is resolved ONCE up front and the derived
/// `host_path` + `local_base` are passed into the blocking core, so the
/// core doesn't pay for a second `db::get_mesh_by_id` (or `to_host_path`
/// call). The emit payload after `run_blocking` reuses the same values.
///
/// Issue #1232 hardening: an adversarial `base_ref` (e.g. `-b`,
/// `--upload-pack=evil`) is rejected by [`health::parse_local_branch`]
/// with `None`; we surface that as a descriptive `invalid base ref`
/// error here instead of silently handing it to git.
#[command]
pub async fn restore_mesh_to_base(
    mesh_id: i64,
    app: tauri::AppHandle,
) -> Result<RestoreResult, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let host_path = to_host_path(&mesh.path);
    let local_base = health::parse_local_branch(&mesh.base_ref).ok_or_else(|| {
        if mesh.base_ref.starts_with('-') {
            format!(
                "invalid base ref '{}': must not start with '-'",
                mesh.base_ref
            )
        } else {
            format!("invalid base ref '{}'", mesh.base_ref)
        }
    })?;

    let host_path_for_emit = host_path.clone();
    let result =
        crate::commands::run_blocking("restore_mesh_to_base", move || {
            restore_mesh_to_base_blocking(host_path, local_base)
        })
        .await?;
    // Emit only on success — a failing restore never refreshes the panel.
    let _ = app.emit(
        "git-changed",
        serde_json::json!({
            "path": host_path_for_emit,
            "internal_path": host_path_for_emit,
        }),
    );
    Ok(result)
}

/// Sync core for [`restore_mesh_to_base`]. Takes the already-resolved
/// `host_path` + `local_base` so the wrapper's DB lookup is the only one
/// paid per invocation (issue #762 review).
pub(crate) fn restore_mesh_to_base_blocking(
    host_path: String,
    local_base: String,
) -> Result<RestoreResult, String> {
    let repo = Repository::open(&host_path)
        .map_err(|e| format!("failed to open repo at {}: {}", host_path, e))?;

    let moved = health::restore_to_base_impl(&repo, &local_base)?;
    let message = if moved {
        format!("checked out {}", local_base)
    } else {
        format!("already on {}", local_base)
    };

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
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload
/// rationale. The mesh row is resolved ONCE up front and the derived
/// `mesh_host_path` + `local_base` are passed into the blocking core so
/// the core doesn't pay for a second `db::get_mesh_by_id`. The freed
/// worktree's `host_path` is the command's `worktree_path` arg (host-
/// mapped) — the core doesn't need to compute it.
#[command]
pub async fn free_base_branch(
    mesh_id: i64,
    worktree_path: String,
    app: tauri::AppHandle,
) -> Result<FreeResult, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let mesh_host_path = to_host_path(&mesh.path);
    let local_base = health::parse_local_branch(&mesh.base_ref)
        .ok_or_else(|| format!("base_ref '{}' is not a valid branch", mesh.base_ref))?;
    let host_wt_path = to_host_path(&worktree_path);

    let result = crate::commands::run_blocking("free_base_branch", move || {
        free_base_branch_blocking(host_wt_path, local_base)
    })
    .await?;

    // Notify the frontend so the panel refreshes. The freed worktree's
    // `path` is what `get_git_prune_info` and the worktree list use to
    // identify the worktree.
    let _ = app.emit(
        "git-changed",
        serde_json::json!({
            "path": mesh_host_path,
            "internal_path": to_host_path(&worktree_path),
        }),
    );

    Ok(result)
}

/// Sync core for [`free_base_branch`]. Takes the already-resolved
/// `host_wt_path` + `local_base` so the wrapper's DB lookup is the only
/// one paid per invocation (issue #762 review).
pub(crate) fn free_base_branch_blocking(
    host_wt_path: String,
    local_base: String,
) -> Result<FreeResult, String> {
    let detached_at_sha = health::free_base_branch_impl(&host_wt_path, &local_base)?;

    Ok(FreeResult { detached_at_sha })
}

// ── Static mesh-snapshot for the git-status panel (issue #348) ──────────────

/// Static "is this mesh a git repo + is the user GitHub-authenticated + what
/// is the default branch" snapshot for `useMeshGitStatus`. The trio was
/// previously three independent Tauri commands fetched in parallel from the
/// hook; folding them into one IPC cuts the round-trip count, removes the
/// possibility of partial-state races in the UI, and makes future static
/// fields (e.g. `git_user_email` for blame rendering) a one-line struct
/// change instead of a new IPC.
///
/// `default_branch` is the repo's local symbolic ref fallback
/// (`refs/remotes/origin/HEAD` → "main") — same logic as
/// [`get_default_branch`]. The trio is composed inline so the snapshot
/// never crosses the IPC boundary partially.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "MeshGitStatic.ts")]
pub struct MeshGitStatic {
    pub is_git_repo: bool,
    pub is_gh_authenticated: bool,
    pub default_branch: String,
}

/// Compute the static trio in one IPC. `check_gh_auth` is composed
/// (delegated to the existing `commands::github::check_gh_auth`) so the
/// mobile `/git/auth` HTTP route and `MeshPropertiesTab.tsx` keep using
/// the same source of truth — the snapshot just bundles it with the
/// repo-only fields the panel needs.
///
/// The gh-auth call is wrapped in a process-global TTL'd cache (see
/// [`check_gh_auth_cached`]) so a user with N meshes pays one gh
/// round-trip per TTL window instead of N. The cache lives HERE, not in
/// `commands::github::check_gh_auth` itself, because that command is also
/// called by the mobile `/git/auth` HTTP route and by
/// `MeshPropertiesTab.tsx` on a user-triggered re-check — both want a
/// fresh value, not a 30s-stale snapshot.
///
/// Offloaded to the blocking pool via `run_blocking`: the
/// `check_gh_auth` delegation does a blocking HTTPS GET to
/// `https://api.github.com/user` (bounded by the client request
/// timeout), and this command fires on every sidebar mesh mount —
/// running it on a Tauri async worker would park that worker for the
/// call's duration and, across several simultaneous mounts, starve the
/// pool (see *Command Threading* in `docs/knowledge-primer.md`). The
/// sync core `get_mesh_git_static_blocking` stays directly callable
/// (the cache-behaviour tests exercise it without a runtime).
///
/// Never returns an error: a non-repo path yields `is_git_repo = false`
/// with the auth check still attempted (a non-git directory can still
/// have `gh` configured) and `default_branch = "main"`. That matches
/// the hook's prior short-circuit (`if (!repoOk) return;`) and keeps the
/// UI contract identical.
#[command]
pub async fn get_mesh_git_static(mesh_path: String) -> Result<MeshGitStatic, String> {
    crate::commands::run_blocking("get_mesh_git_static", move || {
        get_mesh_git_static_blocking(mesh_path)
    })
    .await
}

/// Sync core for [`get_mesh_git_static`] — see its doc for the offload
/// rationale.
pub(crate) fn get_mesh_git_static_blocking(mesh_path: String) -> Result<MeshGitStatic, String> {
    // Open once and reuse the handle for both `is_git_repo` and
    // `default_branch` (#431 — eliminates the pre-refactor double-open).
    let repo = git2::Repository::open(&mesh_path).ok();
    let is_git_repo = repo.is_some();
    let is_gh_authenticated = check_gh_auth_cached();
    let default_branch = match &repo {
        Some(r) => default_branch_from_repo(r),
        None => "main".to_string(),
    };

    Ok(MeshGitStatic {
        is_git_repo,
        is_gh_authenticated,
        default_branch,
    })
}

// ── gh-auth cache for get_mesh_git_static (issue #432) ──────────────────────

/// How long a cached `check_gh_auth` result is reused across mesh mounts.
/// Short enough that a token change (`gh auth login` / `gh auth logout` /
/// token expiry) shows up promptly; long enough that a user iterating
/// through the sidebar's N meshes within ~30s pays 1 round-trip, not N.
const GH_AUTH_CACHE_TTL: Duration = Duration::from_secs(30);

/// Process-global cache: `OnceCell<Mutex<(Instant, bool)>>`. The cell is
/// initialised once with the Mutex; the Mutex interior holds the
/// (last-checked-at, result) pair and is mutated on every read.
static GH_AUTH_CACHE: OnceCell<Mutex<(Instant, bool)>> = OnceCell::new();

/// Counts cache misses (i.e. the number of times the underlying
/// `check_gh_auth()` was actually invoked). Exposed via
/// [`__gh_auth_cache_misses`] for TDD — the test asserts that N snapshot
/// calls within the TTL window produce exactly 1 miss. `Relaxed` is
/// sufficient: the counter has no happens-before relationship to any
/// other state (matches the convention in `http/mod.rs::SNAPSHOT_COUNTER`).
static GH_AUTH_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

/// Cached wrapper around `commands::github::check_gh_auth()`. Returns the
/// cached value when the TTL hasn't expired; otherwise re-invokes the
/// underlying command and refreshes the cache.
///
/// The cache is process-global, so a user mounting N meshes within
/// `GH_AUTH_CACHE_TTL` triggers exactly one `check_gh_auth` round-trip
/// (the first mesh's call). Subsequent calls share that result. After
/// the TTL elapses, the next call re-checks.
///
/// The cell is initialised with an already-expired timestamp so the
/// first call in the process is always a miss — matching the issue's
/// intent ("the first-mesh cost is unchanged; only subsequent meshes
/// within the TTL window benefit"). Without this, the cell's birth
/// timestamp would make the very first call a no-op miss that returns
/// the default `false`, which would silently lie about auth state on a
/// fresh launch.
fn check_gh_auth_cached() -> bool {
    let cell = GH_AUTH_CACHE.get_or_init(|| {
        let expired = Instant::now() - GH_AUTH_CACHE_TTL - Duration::from_secs(1);
        Mutex::new((expired, false))
    });
    let now = Instant::now();
    let mut guard = cell.lock().expect("gh-auth cache mutex poisoned");
    if now.duration_since(guard.0) >= GH_AUTH_CACHE_TTL {
        guard.1 = crate::commands::github::check_gh_auth_blocking();
        // Stamp post-call, not at entry. The HTTPS GET has a 30 s request
        // timeout, the same magnitude as the cache TTL — a near-timeout
        // call would otherwise leave `guard.0` already past the TTL before
        // the result is even returned, and the very next reader triggers a
        // second miss (issue #782).
        guard.0 = Instant::now();
        GH_AUTH_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
    guard.1
}

/// Test seam: number of times the underlying `check_gh_auth()` was
/// actually invoked. Used by `commands::git_tests` to assert the cache
/// hit rate; not part of the public surface.
#[cfg(test)]
pub(crate) fn __gh_auth_cache_misses() -> u64 {
    GH_AUTH_CACHE_MISSES.load(Ordering::Relaxed)
}

/// Test seam: reset the miss counter to 0 and backdate the cached
/// timestamp so the next read is a miss. Used to put the test in a
/// known "first call" state regardless of test ordering.
#[cfg(test)]
pub(crate) fn __reset_gh_auth_cache_for_tests() {
    GH_AUTH_CACHE_MISSES.store(0, Ordering::Relaxed);
    if let Some(cell) = GH_AUTH_CACHE.get() {
        let mut guard = cell.lock().expect("gh-auth cache mutex poisoned");
        // Backdate well past the TTL so the next `check_gh_auth_cached`
        // call sees an expired entry and refreshes.
        guard.0 = Instant::now() - GH_AUTH_CACHE_TTL - Duration::from_secs(1);
    }
}
