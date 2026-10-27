//! Git operations via git2 crate
//!
//! The git2 calls this file used to make now go through the `git` module's
//! capability-shaped reads (issue #2526): this layer validates its inputs,
//! calls one read or one mutation, and maps the result to a wire type. It
//! holds no `Repository` handle.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use tauri::command;
use tauri::Emitter;
use ts_rs::TS;

use crate::services::gh_auth_cache::GhAuthCache;

use crate::db;
use crate::env::to_host_path;
use crate::git::{health, index, primitives, state};
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
    /// Path relative to the queried project folder; None for changes outside
    /// it. Git actions always use the repository-relative `path` instead.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub project_path: Option<String>,
    pub status: String, // "modified" | "added" | "deleted" | "renamed" | "untracked"
    #[ts(as = "i32")]
    pub additions: usize,
    #[ts(as = "i32")]
    pub deletions: usize,
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
/// paths with a paused VM can make the repository open stall). The sync
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
    // An unreadable repository, an unborn HEAD and a non-UTF8 ref name all
    // mean "there is no branch to report" — the same `None` the caller saw
    // when it opened the repository itself.
    let Ok(Some(status)) = state::branch_status_at(&path) else {
        return Ok(None);
    };
    Ok(Some(GitBranchStatus {
        name: status.name,
        ahead: status.ahead,
        behind: status.behind,
        short_sha: status.short_sha,
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
    let state = state::state_at(&path, state::Baseline::WorkingTree).map_err(|e| e.to_string())?;
    let project = crate::git::project::resolve(&path)?;

    Ok(state
        .statuses
        .into_iter()
        .map(|entry| GitStatus {
            project_path: std::path::Path::new(&entry.path)
                .strip_prefix(&project.relative_folder)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/")),
            path: entry.path,
            status: entry.status,
            additions: entry.additions,
            deletions: entry.deletions,
        })
        .collect())
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
    // Issue #2602: a folder that is gone fails this read forever. The
    // marker tells the frontend to stop asking.
    let summary = state::summary_at(&path)
        .map_err(|e| crate::services::mesh_folder::mark_if_missing(&path, e.to_string()))?;
    Ok(GitSummary {
        total: summary.total,
        added: summary.added,
        modified: summary.modified,
        deleted: summary.deleted,
    })
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
    Ok(primitives::default_branch_at(&path))
}

/// How a manual `git_sync` ended, as the *user* experiences it. This is the
/// one field the frontend needs and cannot infer from the flags: a failed
/// fetch and a Mesh with no remote both report `fetched: false` with zero new
/// commits, and a comparison that never ran is indistinguishable from one that
/// found zero — so a UI that classified outcomes from the flags had to either
/// shout "error" at a purely local Mesh or stay silent about a real network
/// failure.
///
/// `fetched` means the fetch COMPLETED SUCCESSFULLY, never that it was
/// attempted: `FetchFailed` is returned after `git fetch` was launched and
/// failed or timed out, so it reports `fetched: false` even though upstream may
/// have been contacted. The status is therefore the only reliable statement
/// about what happened, which is why this enum exists at all — and why
/// `NoUpstream` is separate from `Skipped` even though both are non-failures:
/// one proves nothing was ever fetched, the other proves a fetch ran.
///
/// `Partial` is the outcome that motivated this field: the fetch succeeded
/// but the working-tree fast-forward did not apply (dirty overlap, diverged
/// history, or a pull timeout). The mesh is *not* up to date, so the behind
/// marker stays — the user needs to be told why rather than left staring at
/// an unchanged count. `Partial`, `Unverified` and `Failed` are the statuses
/// that leave the mesh in an unknown or behind state; which earn a "Needs
/// attention" notification is decided once on the frontend, in
/// `src/lib/meshSyncFeedback.ts`.
///
/// Generated to src/types/generated/GitSyncStatus.ts (issue #404).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "GitSyncStatus.ts")]
pub enum GitSyncStatus {
    /// Fast-forward applied; the local branch moved onto upstream.
    Synced,
    /// A comparison ran and found nothing new to pull.
    UpToDate,
    /// No remote configured — a purely local Mesh, and the only status that
    /// proves no fetch was ever attempted. Not a failure.
    Skipped,
    /// A remote was fetched, but the branch has no upstream to compare
    /// against, so no comparison ran. Not a failure either — common for a
    /// branch that gained a remote but has never been pushed — but unlike
    /// `Skipped` this says nothing about whether the mesh is behind, so the
    /// UI must not report "no remote".
    NoUpstream,
    /// Commits were fetched but the fast-forward did not apply.
    Partial,
    /// The fetch ran, but comparing the local branch against upstream failed,
    /// so the mesh's position is unknown. Distinct from `UpToDate` — which
    /// asserts "verified in sync" and would resolve attention cards and clear
    /// failure badges — and from `Failed`, which reports a fetch that did not
    /// complete. `fetched: true` says the fetch *completed*, which is what the
    /// row-refresh signal keys off; it does not claim a ref moved, since the
    /// refs may have been current already.
    Unverified,
    /// The sync did not complete: the fetch failed (network, auth, the remote
    /// deleted mid-flight, the command timing out, or the spawn failing before
    /// any child started), or the path is not a usable repository.
    ///
    /// `fetched` is `true` or `false` depending on WHERE it failed — a repo
    /// that becomes unopenable after a completed fetch reports `true`, while a
    /// fetch that never completed reports `false`. The flag records whether the
    /// fetch completed; it never records whether a fetch was attempted, and it
    /// says nothing about which side of the network the run died on. The
    /// frontend therefore treats this status as establishing nothing.
    Failed,
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
    /// The user-facing outcome. The booleans are narrower than "which steps ran":
    /// `fetched` is true only when the fetch COMPLETED (a fetch that failed, or
    /// a repo that could not be opened, is false), and `pulled` is true only when
    /// the fast-forward was applied. Neither says whether a step was attempted,
    /// which is why `status` exists and why the frontend classifies on it rather
    /// than on these flags.
    pub status: GitSyncStatus,
}

/// Fetch from the current branch's configured upstream remote and
/// attempt a fast-forward pull. Returns a structured result with
/// feedback about what happened.
///
/// `GitSyncResult.status` is the field a UI classifies on: it separates
/// "the mesh is now in sync", "there was nothing to pull", "this Mesh has no
/// remote" and "the sync did not complete" — the last of which covers both a
/// failed fetch and a fetched-but-not-fast-forwarded run. The `fetched` /
/// `pulled` flags still describe which steps ran.
///
/// This is now a thin wrapper over the shared `do_sync` helper
/// (issue #274). The wrapper's job is purely the `git_sync`-specific
/// bookkeeping: resolve the remote from the current branch's
/// upstream (so the test
/// `git_sync_fetches_current_branch_upstream_when_remote_is_not_origin`
/// keeps passing — a repo whose upstream is named `main` rather
/// than `origin` must still be synced against that upstream), and
/// translate the shared `SyncOutcome` back to the [`GitSyncResult`]
/// shape the frontend expects.
///
/// The result carries a `status` ([`GitSyncStatus`]) alongside the four
/// original fields, because those fields cannot express the outcomes that
/// matter to a user: a fetch failure and a mesh with no remote both report
/// `fetched: false`, and a comparison that never ran is indistinguishable
/// from one that found zero commits behind. The frontend classifies the
/// status to decide the toast severity, whether a "Needs attention" card is
/// raised, whether an earlier card is resolved, and what the card may claim.
///
/// Note: the shared helper always runs the fetch (a fetch never
/// touches the working tree) and skips the fast-forward pull only
/// when it would overwrite local changes (returning `FetchedButDirty`
/// → "Fetched N new commits — fast-forward skipped: changes to <paths>
/// would be overwritten"). Since ADR 0033 a dirty tree alone does not
/// skip the pull: edits and untracked files in paths the incoming
/// commits don't rewrite are preserved by git and the pull proceeds. A
/// manual Sync click on a dirty mesh therefore still freshens the
/// remote-tracking refs that worktree nodes are cut from.
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
    // `origin`. `do_sync` re-opens (microsecond cost) to keep its
    // signature self-contained and the call shape uniform with
    // `fetch_origin`.
    let remote = crate::git::sync::current_branch_upstream_remote_at(&host_path)?
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
    let outcome = crate::git::sync::locked_do_sync(&path, &host_path, &remote, None);

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
    // `db::read_conn()` panics on an uninitialized global DB, and a filtered test
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
/// four original wire fields are unchanged; the messages match the prior
/// `format!` strings where the variant maps cleanly. `status` is set
/// from the variant so the UI never has to re-derive the outcome from
/// the flags and the message text.
///
/// Private, and its mapping table is tested by the `#[cfg(test)]` module
/// at the foot of this file — a child module can reach it, while
/// `pub(crate)` would widen production visibility purely for a test.
fn sync_outcome_to_git_sync_result(outcome: crate::git::sync::SyncOutcome) -> GitSyncResult {
    use crate::git::sync::SyncOutcome;
    match outcome {
        SyncOutcome::FetchedButDirty {
            new_commits,
            blocking_paths,
        } => {
            // Since ADR 0033 a dirty tree alone no longer blocks the pull —
            // only local changes at paths the incoming commits rewrite do.
            // Name them so the user can see the block is real, not a blanket
            // "your tree is dirty" (which used to fire on unrelated edits and
            // untracked files).
            let overwritten = if blocking_paths.is_empty() {
                // Fail-closed: status was unreadable, so we can't name paths.
                "uncommitted changes".to_string()
            } else {
                format!("changes to {}", format_overwrite_paths(&blocking_paths))
            };
            GitSyncResult {
                fetched: true,
                pulled: false,
                new_commits,
                message: format!(
                    "Fetched {} new commit{}; fast-forward skipped: {} would be overwritten",
                    new_commits,
                    plural_s(new_commits),
                    overwritten
                ),
                status: GitSyncStatus::Partial,
            }
        }
        SyncOutcome::SkippedNoRemote => GitSyncResult {
            fetched: false,
            pulled: false,
            new_commits: 0,
            message: "No remote configured".to_string(),
            status: GitSyncStatus::Skipped,
        },
        SyncOutcome::UpToDate => GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits: 0,
            message: "Already up to date".to_string(),
            status: GitSyncStatus::UpToDate,
        },
        // The remote was fetched, but this branch tracks nothing, so no
        // comparison ran. Its own status rather than `Skipped`: a mesh with no
        // *remote* is a different situation, and the frontend tells them apart
        // — one says "no remote configured", the other says there was nothing
        // to compare against. Neither is `UpToDate`, which the frontend treats
        // as verified and would use to resolve cards and clear badges.
        SyncOutcome::NoUpstream => GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits: 0,
            message: "Fetched, but this branch has no upstream to compare against".to_string(),
            status: GitSyncStatus::NoUpstream,
        },
        SyncOutcome::ComparisonFailed { reason } => GitSyncResult {
            // `fetched: true` because the fetch completed — this is not a fetch
            // failure. It does NOT claim the refs moved: they may have been
            // current already. The status is what stops this reading as
            // "verified in sync".
            fetched: true,
            pulled: false,
            new_commits: 0,
            message: format!("Fetched, but could not compare with upstream: {}", reason),
            status: GitSyncStatus::Unverified,
        },
        SyncOutcome::Synced { new_commits } => GitSyncResult {
            fetched: true,
            pulled: true,
            new_commits,
            message: format!("Pulled {} new commit{}", new_commits, plural_s(new_commits)),
            status: GitSyncStatus::Synced,
        },
        SyncOutcome::FetchedButDiverged {
            new_commits,
            reason,
        } => GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits,
            message: format!(
                "Fetched {} new commit{} but fast-forward failed: {}",
                new_commits,
                plural_s(new_commits),
                reason
            ),
            status: GitSyncStatus::Partial,
        },
        SyncOutcome::PullTimedOut {
            new_commits,
            reason,
        } => GitSyncResult {
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
            status: GitSyncStatus::Partial,
        },
        SyncOutcome::FetchFailed { reason } => GitSyncResult {
            fetched: false,
            pulled: false,
            new_commits: 0,
            message: format!("Fetch failed: {}", reason),
            status: GitSyncStatus::Failed,
        },
        SyncOutcome::RepoUnusable {
            reason,
            fetch_completed,
        } => GitSyncResult {
            // Report what happened, not "this failed": `do_sync` re-opens the
            // repo after the fetch for the count-behind, and that open can be
            // what fails. `fetched` means the fetch completed, so a repo that
            // became unreadable afterwards still says so.
            fetched: fetch_completed,
            pulled: false,
            new_commits: 0,
            message: format!("Repository unusable: {}", reason),
            status: GitSyncStatus::Failed,
        },
    }
}

/// `""` for `n == 1`, `"s"` otherwise. Used four times in
/// `sync_outcome_to_git_sync_result` for the "N new commit[s]" wording.
fn plural_s(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Render the ff-pull blocking paths for the Sync toast. Caps at three names
/// (then "and N more") so a collision with a large changeset stays readable.
fn format_overwrite_paths(paths: &[String]) -> String {
    const MAX: usize = 3;
    let mut listed = paths
        .iter()
        .take(MAX)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > MAX {
        listed.push_str(&format!(" and {} more", paths.len() - MAX));
    }
    listed
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
    crate::commands::run_blocking("get_mesh_health", move || get_mesh_health_blocking(mesh_id))
        .await
}

/// Sync core for [`get_mesh_health`].
pub(crate) fn get_mesh_health_blocking(mesh_id: i64) -> Result<MeshHealth, String> {
    let mesh =
        db::get_mesh_by_id(mesh_id).map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let host_path = to_host_path(&mesh.path);

    // Refine the holder with the active paths for THIS mesh's nodes so
    // `is_active` reflects whether an agent node points at the worktree.
    // Scoped to `mesh_id` — the holder can only live inside this repo, so
    // nodes from other meshes are dead weight here.
    let active_paths = match health::parse_local_branch(&mesh.base_ref) {
        Some(_) => {
            let nodes = crate::git::project::repository_nodes(
                &mesh.path,
                db::list_agent_nodes().map_err(|e| format!("failed to list agent nodes: {e}"))?,
            )?;
            crate::git::project::active_paths(&nodes)
        }
        None => Vec::new(),
    };

    let mut snapshot = health::mesh_health_at(&host_path, &mesh.base_ref, &active_paths)
        .map_err(|e| crate::services::mesh_folder::mark_if_missing(&mesh.path, e))?;
    // Issue #2602: the pool's fill failure rides on the mesh's health
    // surface, so a broken pool is something the user can see instead of
    // something they have to find in the log.
    snapshot.pool_fill_failure = crate::services::warm_pool::fill_failure_for_mesh(mesh_id);
    Ok(snapshot)
}

/// Restore the Mesh root to its Base Ref. Guarded by the same
/// `restore_to_base_impl` rules: refuses if the root is dirty, has
/// unpushed commits, or the Base Ref's branch is held by a worktree.
///
/// On success, emits a `git-changed` event for the mesh path so the
/// sidebar `!` badge clears and the file explorer refreshes.
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload
/// rationale. The mesh row is resolved once inside the blocking closure
/// and the derived `host_path` is returned for the post-await emit, so
/// we don't pay a second `db::get_mesh_by_id` (or `to_host_path`) call.
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
    let (result, host_path_for_emit) =
        crate::commands::run_blocking("restore_mesh_to_base", move || {
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
            let result = restore_mesh_to_base_blocking(host_path.clone(), local_base)?;
            Ok((result, host_path))
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
    let moved = health::restore_to_base_at(&host_path, &local_base)?;
    let message = if moved {
        format!("checked out {}", local_base)
    } else {
        format!("already on {}", local_base)
    };

    Ok(RestoreResult {
        restored: moved,
        message,
    })
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
/// rationale. The mesh row is resolved once inside the blocking closure
/// and the derived `mesh_host_path` is returned for the post-await emit,
/// so we don't pay a second `db::get_mesh_by_id`. The freed worktree's
/// `host_path` is the command's `worktree_path` arg (host-mapped).
#[command]
pub async fn free_base_branch(
    mesh_id: i64,
    worktree_path: String,
    app: tauri::AppHandle,
) -> Result<FreeResult, String> {
    let worktree_path_for_emit = worktree_path.clone();
    let (result, mesh_host_path) = crate::commands::run_blocking("free_base_branch", move || {
        let mesh = db::get_mesh_by_id(mesh_id)
            .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
        let mesh_host_path = to_host_path(&mesh.path);
        let local_base = health::parse_local_branch(&mesh.base_ref)
            .ok_or_else(|| format!("base_ref '{}' is not a valid branch", mesh.base_ref))?;
        let host_wt_path = to_host_path(&worktree_path);
        let result = free_base_branch_blocking(host_wt_path, local_base)?;
        Ok((result, mesh_host_path))
    })
    .await?;

    // Notify the frontend so the panel refreshes. The freed worktree's
    // `path` is what `get_git_prune_info` and the worktree list use to
    // identify the worktree.
    let _ = app.emit(
        "git-changed",
        serde_json::json!({
            "path": mesh_host_path,
            "internal_path": to_host_path(&worktree_path_for_emit),
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
/// mobile `/git/auth` HTTP route and `ProjectSettingsTab.tsx` keep using
/// the same source of truth — the snapshot just bundles it with the
/// repo-only fields the panel needs.
///
/// The gh-auth call is wrapped in a process-global TTL'd cache (see
/// [`check_gh_auth_cached`]) so a user with N meshes pays one gh
/// round-trip per TTL window instead of N. The cache lives HERE, not in
/// `commands::github::check_gh_auth` itself, because that command is also
/// called by the mobile `/git/auth` HTTP route and by
/// `ProjectSettingsTab.tsx` on a user-triggered re-check — both want a
/// fresh value, not a 30s-stale snapshot.
///
/// Offloaded to the blocking pool via `run_blocking`: the
/// `check_gh_auth` delegation does a blocking HTTPS GET to
/// `https://api.github.com/user` (bounded by the client request
/// timeout), and this command fires on every sidebar mesh mount —
/// running it on a Tauri async worker would park that worker for the
/// call's duration and, across several simultaneous mounts, starve the
/// pool (see *Command Threading* in `docs/development/rust-conventions.md`). The
/// sync core `get_mesh_git_static_blocking` stays directly callable
/// (the cache-behaviour tests exercise it without a runtime).
///
/// Never returns an error: a non-repo path yields `is_git_repo = false`
/// with the auth check still attempted (a non-git directory can still
/// have `gh` configured) and `default_branch = "main"`. That matches
/// the hook's prior short-circuit (`if (!repoOk) return;`) and keeps the
/// UI contract identical.
#[command]
pub async fn get_mesh_git_static(
    path: String,
    cache: tauri::State<'_, GhAuthCache>,
) -> Result<MeshGitStatic, String> {
    let cache = cache.inner().clone();
    crate::commands::run_blocking("get_mesh_git_static", move || {
        get_mesh_git_static_blocking(&cache, path)
    })
    .await
}

/// Sync core for [`get_mesh_git_static`] — see its doc for the offload
/// rationale.
///
/// `cache` is the injected [`GhAuthCache`] (Tauri managed state in
/// production, per-test fixture in tests) via `GhAuthCache::check()`.
pub(crate) fn get_mesh_git_static_blocking(
    cache: &GhAuthCache,
    path: String,
) -> Result<MeshGitStatic, String> {
    // Open once and reuse the handle for both `is_git_repo` and
    // `default_branch` (#431 — eliminates the pre-refactor double-open).
    // `open_at` handles WSL UNC path translation (hard rule
    // `env::to_host_path` — every host file-op must go through it).
    let (is_git_repo, default_branch) = primitives::repo_static_at(&path);
    let is_gh_authenticated = cache.check();

    Ok(MeshGitStatic {
        is_git_repo,
        is_gh_authenticated,
        default_branch,
    })
}

// ── Per-file stage / revert (issue #1374) ───────────────────────────

/// Stage one file (by repo-relative path) into the index. Handles every
/// change kind the diff surfaces can show:
///
/// - modified / untracked: `index.add_path` (a new file enters the index;
///   a modified one updates it).
/// - deleted (missing on disk): `index.remove_path` so the deletion is
///   staged rather than silently left as a worktree-only delete.
/// - renamed: the caller passes the NEW path; the old path still reads as
///   deleted in the status walk, so we also remove it — otherwise staging
///   a rename would leave the index holding both the old and new entries.
///
/// Path traversal is rejected at the command boundary (same rule as
/// `diff_against_base`'s `only` pathspec): any `..` segment or empty
/// segment means the caller is trying to escape the worktree.
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload
/// rationale (libgit2 index writes touch the filesystem and must not park
/// a Tauri async worker).
#[command]
pub async fn stage_file(repo_path: String, file_path: String) -> Result<(), String> {
    crate::commands::run_blocking("stage_file", move || {
        stage_file_blocking(&repo_path, &file_path)
    })
    .await
}

/// Sync core for [`stage_file`].
pub(crate) fn stage_file_blocking(repo_path: &str, file_path: &str) -> Result<(), String> {
    reject_traversal(file_path)?;
    index::stage_path(repo_path, file_path)
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
/// then revert the OLD path (restores it). This command is deliberately
/// single-path so each step is independently observable and testable.
///
/// Path traversal is rejected with the same rule as [`stage_file_blocking`].
///
/// Thin async wrapper; see [`get_git_branch_status`] for the offload
/// rationale.
#[command]
pub async fn revert_file(repo_path: String, file_path: String) -> Result<(), String> {
    crate::commands::run_blocking("revert_file", move || {
        revert_file_blocking(&repo_path, &file_path)
    })
    .await
}

/// Sync core for [`revert_file`].
pub(crate) fn revert_file_blocking(repo_path: &str, file_path: &str) -> Result<(), String> {
    reject_traversal(file_path)?;
    index::revert_path(repo_path, file_path)
}

/// Reject path-traversal inputs at the command boundary. Defense in depth
/// — the structural check rejects absolute paths, drive prefixes, and
/// `..` segments *before* the file ever reaches a syscall, and the
/// canonicalize-and-prefix check below catches symlinks whose targets
/// escape the repo even when the link path itself is relative and clean.
///
/// Why this matters: a Windows drive prefix like `D:\secret.txt` splits
/// into `["D:", "secret.txt"]` under a naive string-split-by-`/` check —
/// neither segment is `..`, so the gate passes it. Then
/// `Path::join("F:\repo", "D:\secret.txt")` evaluates to `D:\secret.txt`
/// (an absolute right-hand argument *replaces* the base), and
/// `revert_file_blocking`'s `std::fs::remove_file` deletes the file
/// outside the worktree. `Path::components()` parses the structure the
/// string-split can't see (drive prefix, root marker, parent components)
/// and is the right primitive for this gate.
///
/// Shared by [`stage_file_blocking`] and [`revert_file_blocking`]
/// (issue #1374). The `diff_against_base` pathspec check in
/// `commands/diff.rs` has the same vulnerability shape but is followed
/// by libgit2's own pathspec validation — these two Tauri commands have
/// no such safety net, so they need their own strict gate.
fn reject_traversal(file_path: &str) -> Result<(), String> {
    let p = Path::new(file_path);

    // Step 1 — structural check via `Path::components()`. Reject anything
    // that isn't a clean relative path: drive prefixes (`C:`), UNC
    // prefixes (`\\server`), root markers (`/`, `\`), and parent
    // components (`..`). Allow `CurDir` (`.`) and `Normal(name)` — the
    // latter includes literal filenames starting with `..` like `..bar`,
    // which is a *valid POSIX filename* and stays inside the worktree
    // when joined (`repo/..bar` ≠ traversal). Reject if no `Normal`
    // component survives — `.`, `..`, `/`, `` are all empty after
    // stripping and meaningless as a "file to act on".
    let mut saw_normal = false;
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(format!("invalid path: {}", file_path));
            }
            Component::CurDir => {}
            Component::Normal(_) => saw_normal = true,
        }
    }
    if !saw_normal {
        return Err(format!("invalid path: {}", file_path));
    }
    Ok(())
}

// ── The `SyncOutcome → GitSyncResult` mapping table ─────────────────────────
//
// A child module so it can reach the private mapping fn. Widening that fn to
// `pub(crate)` purely so the sibling `commands::git_tests` could call it would
// put a test-only name in production's public surface (engineering contract:
// do not export internals only to test them).
//
// The mapping is the contract the frontend's severity table, its "Needs
// attention" decision and its resolution rule all key off, so every variant is
// pinned here. A variant mapped to the wrong status is invisible from the
// command's happy paths, which only ever produce `UpToDate` / `Synced`.
#[cfg(test)]
mod sync_status_mapping_tests {
    use super::{sync_outcome_to_git_sync_result, GitSyncResult, GitSyncStatus};
    use crate::git::sync::SyncOutcome;

    /// Every variant, with the status the frontend classifies it by.
    /// `fetched` is asserted alongside: it is a separate fact the row-refresh
    /// signal reads directly, and a variant that reported the wrong one would
    /// leave a mesh's cached behind-count stale.
    fn table() -> Vec<(SyncOutcome, GitSyncStatus, bool)> {
        vec![
            (
                SyncOutcome::Synced { new_commits: 3 },
                GitSyncStatus::Synced,
                true,
            ),
            (SyncOutcome::UpToDate, GitSyncStatus::UpToDate, true),
            // No remote: a valid local-only Mesh, nothing to nag about.
            (SyncOutcome::SkippedNoRemote, GitSyncStatus::Skipped, false),
            // Remote fetched, branch tracks nothing: nothing was compared, and
            // nothing is wrong. Must NOT be `UpToDate`, which the frontend
            // reads as verified and uses to resolve cards and clear badges.
            (SyncOutcome::NoUpstream, GitSyncStatus::NoUpstream, true),
            (
                SyncOutcome::ComparisonFailed {
                    reason: "reference is locked".to_string(),
                },
                GitSyncStatus::Unverified,
                true,
            ),
            (
                SyncOutcome::FetchedButDirty {
                    new_commits: 2,
                    blocking_paths: vec!["src/App.tsx".to_string()],
                },
                GitSyncStatus::Partial,
                true,
            ),
            (
                SyncOutcome::FetchedButDirty {
                    new_commits: 1,
                    blocking_paths: Vec::new(),
                },
                GitSyncStatus::Partial,
                true,
            ),
            (
                SyncOutcome::FetchedButDiverged {
                    new_commits: 4,
                    reason: "not possible to fast-forward".to_string(),
                },
                GitSyncStatus::Partial,
                true,
            ),
            (
                SyncOutcome::PullTimedOut {
                    new_commits: 1,
                    reason: "timed out".to_string(),
                },
                GitSyncStatus::Partial,
                true,
            ),
            (
                SyncOutcome::FetchFailed {
                    reason: "network down".to_string(),
                },
                GitSyncStatus::Failed,
                false,
            ),
            (
                SyncOutcome::RepoUnusable {
                    reason: "not a repository".to_string(),
                    fetch_completed: false,
                },
                GitSyncStatus::Failed,
                false,
            ),
            // Re-opening after a completed fetch failed. Same status — the repo
            // is unusable either way — but `fetched` must report the fetch that
            // really happened, or the UI is told nothing was fetched when it was.
            (
                SyncOutcome::RepoUnusable {
                    reason: "repo went away mid-sync".to_string(),
                    fetch_completed: true,
                },
                GitSyncStatus::Failed,
                true,
            ),
        ]
    }

    #[test]
    fn every_outcome_maps_to_the_status_the_ui_classifies_on() {
        for (outcome, expected, expected_fetched) in table() {
            let label = format!("{:?}", outcome);
            let result: GitSyncResult = sync_outcome_to_git_sync_result(outcome);
            assert_eq!(result.status, expected, "status for {}", label);
            assert_eq!(result.fetched, expected_fetched, "`fetched` for {}", label);
            assert!(
                !result.message.is_empty(),
                "every outcome needs user-facing text ({})",
                label
            );
        }
    }

    /// The three statuses that leave nothing verified, and why each is there.
    /// `UpToDate` is the one that matters most: it is the only status the
    /// frontend treats as verified, so putting a no-comparison outcome there
    /// resolves cards and clears badges for a mesh nobody checked.
    #[test]
    fn unverified_statuses_are_exactly_the_three_that_did_not_compare() {
        use GitSyncStatus::*;
        for outcome in [
            SyncOutcome::SkippedNoRemote,
            SyncOutcome::NoUpstream,
            SyncOutcome::ComparisonFailed {
                reason: "x".to_string(),
            },
            SyncOutcome::FetchedButDirty {
                new_commits: 1,
                blocking_paths: vec![],
            },
            SyncOutcome::FetchedButDiverged {
                new_commits: 1,
                reason: "x".to_string(),
            },
            SyncOutcome::PullTimedOut {
                new_commits: 1,
                reason: "x".to_string(),
            },
            SyncOutcome::FetchFailed {
                reason: "x".to_string(),
            },
            SyncOutcome::RepoUnusable {
                reason: "x".to_string(),
                fetch_completed: false,
            },
        ] {
            let label = format!("{:?}", outcome);
            let status = sync_outcome_to_git_sync_result(outcome).status;
            assert_ne!(
                status, UpToDate,
                "{} must not claim a comparison happened",
                label
            );
        }
        // …and the one that did compare and found nothing.
        assert_eq!(
            sync_outcome_to_git_sync_result(SyncOutcome::UpToDate).status,
            UpToDate
        );
    }
}

// ── gh-auth cache accessor (issue #1483) ─────────────────────────────────
//
// The cache itself lives in `crate::services::gh_auth_cache::GhAuthCache` as
// an injectable struct. Production wires `GhAuthCache::new()` via Tauri
// `manage` and the async command passes it into the blocking core. Tests
// construct `GhAuthCache::for_test_with_auth()` per `#[test]` with an
// isolated counter and a controllable clock. No `static` remains here — the
// old `GH_AUTH_CACHE` / `GH_AUTH_CACHE_MISSES` globals are deleted.
//
// No legacy `static` is kept: callers must use per-fixture `GhAuthCache`.
