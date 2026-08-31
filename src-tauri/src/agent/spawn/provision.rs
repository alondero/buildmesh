use super::{MeshSyncOutcome, MeshSyncWarningPayload};
use tauri::Emitter;

/// Default `worktree_mode` when the mesh config leaves it unset. Pinned by
/// the unit test in this module (`default_worktree_mode_is_branched`).
///
/// This was previously paired with a TS sentinel at `src/lib/worktreeMode.ts`,
/// deleted in #411 once the TS side lost its only consumer (a self-referential
/// test). If a future UI re-exposes a worktree-mode selector, re-introduce
/// the TS constant alongside it and re-couple by doc comment + paired test
/// (see [[feedback_cross-language-default-coupling]]). See
/// `docs/knowledge-primer.md` (Worktree Support) for the branched-vs-detached
/// rationale.
pub const DEFAULT_WORKTREE_MODE: &str = "branched";

pub(super) static SPAWNS_IN_FLIGHT: once_cell::sync::Lazy<
    parking_lot::Mutex<std::collections::HashSet<i64>>,
> = once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(std::collections::HashSet::new()));

/// RAII claim on a session id in [`SPAWNS_IN_FLIGHT`]. Dropping releases
/// the claim on every exit path, including a cancelled async task.
pub(crate) struct SpawnInFlightClaim {
    session_id: i64,
}

impl SpawnInFlightClaim {
    /// `Some(claim)` if no spawn is in flight for this session, `None` if
    /// one already is (the caller should short-circuit as a duplicate).
    pub(crate) fn try_claim(session_id: i64) -> Option<Self> {
        // parking_lot::Mutex::lock is non-poisoning and a strict upgrade
        // over std here: no unwrap on contention, and `try_lock` lets
        // Drop fall back gracefully if the runtime is mid-shutdown.
        let mut guard = SPAWNS_IN_FLIGHT.lock();
        if guard.insert(session_id) {
            Some(Self { session_id })
        } else {
            None
        }
    }
}

impl Drop for SpawnInFlightClaim {
    fn drop(&mut self) {
        // `lock` (blocking) is correct here: the guard's lifetime is the
        // whole spawn function, so contention is at most a few µs of
        // contended HashSet::remove — never long enough to starve a
        // tokio worker. `try_lock` would silently leak the claim on
        // contention, which is the opposite of what the bug requires.
        SPAWNS_IN_FLIGHT.lock().remove(&self.session_id);
    }
}

/// Resolve the `base_ref` string that `git::sync::fetch_origin` will use for
/// the spawn-time auto-sync. The chain (each tier only runs if the previous
/// one yields nothing useful):
///
/// 1. The mesh's `base_ref` column from the `meshes` DB row — explicit
///    user intent wins, even on a repo whose default branch disagrees.
///    **The COALESCE default `'origin/main'` is treated as "no config"**:
///    a fresh mesh whose `base_ref` column was never explicitly set reads
///    as `'origin/main'` from the DB (see `db::MESH_COLUMNS`), and a
///    user who never touched the field is functionally identical to a
///    user who has no config. Detecting both via the same path is what
///    closes the master-trunk regression. **There is no `mesh.toml`
///    file**: the value lives on the `meshes` SQLite row (and is
///    mirrored to `.claude/settings.json` at the mesh root for Claude
///    Code, see `commands::mesh_properties`).
/// 2. The repo's actual default branch read from
///    `refs/remotes/origin/HEAD` (populated by `git clone` / `git fetch`)
///    — closes the master-trunk regression where a repo whose default
///    branch is `master` was always fetched as `origin/main`.
/// 3. The literal `"origin/main"` as a last resort. Used only for a
///    non-repo / unconfigured path so the spawn path never blocks.
///
/// Extracted from `spawn_agent_inner` so the regression test in
/// `mod tests` can call it directly without standing up the full async /
/// PTY / DB machinery — the call site is a single expression.
///
/// `pub(crate)` so the background mesh sync (`services::pool_worker`)
/// resolves its fetch target through the exact same 3-tier chain the spawn
/// uses — a worker that fetched a literal `origin/main` on a repo whose
/// default branch is `master` would fail every pass and never satisfy the
/// spawn-time freshness TTL.
pub(crate) fn resolve_base_ref_for_spawn(mesh_path: &str, config_base_ref: Option<&str>) -> String {
    const COALESCE_DEFAULT: &str = "origin/main";
    let user_set = config_base_ref.filter(|b| b.trim() != COALESCE_DEFAULT);
    if let Some(b) = user_set {
        let trimmed = b.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // No explicit config (or the COALESCE sentinel) — read the repo's
    // actual default branch from `refs/remotes/origin/HEAD` (populated by
    // `git clone` / `git fetch`). `get_default_branch` falls back to
    // "main" if the repo can't be opened or the symbolic ref is missing,
    // so a non-repo / unconfigured mesh path still resolves to
    // "origin/main" — preserving pre-fix behaviour and never blocking the
    // spawn.
    //
    // Called from the synchronous spawn path — use the sync core directly
    // (issue #762). The blocking pool offload that the async wrapper
    // provides is irrelevant for this single repo-open + symbolic-ref read;
    // the small wall-clock cost is well within the spawn budget and the
    // outer spawn task already runs on a blocking-pool thread.
    let branch = crate::commands::git::get_default_branch_blocking(mesh_path.to_string())
        .unwrap_or_else(|_| "main".to_string());
    format!("origin/{}", branch)
}

/// Run the two provider-owned launch prerequisites in order while preserving
/// independent failures. Hook provisioning must still run when trust setup
/// reports an error, and a provider without attention hooks should not invoke
/// its hook seam. The caller supplies this synchronous body to the blocking
/// pool because adapters may inspect or mutate files and invoke WSL.
pub(super) fn run_provider_provisioning<Trust, Hooks>(
    ensure_trusted: Trust,
    provision_hooks: Hooks,
    needs_attention_hook: bool,
) -> (Result<(), String>, Result<(), String>)
where
    Trust: FnOnce() -> Result<(), String>,
    Hooks: FnOnce() -> Result<(), String>,
{
    let trust = ensure_trusted();
    let hooks = if needs_attention_hook {
        provision_hooks()
    } else {
        Ok(())
    };
    (trust, hooks)
}

/// Map an `crate::git::sync::fetch_origin` outcome to either a silent `tracing` log
/// or a `mesh-sync-warning` Tauri event. The frontend's `App.tsx`
/// listens for the event and shows a non-fatal warning toast.
///
/// Per issue #213:
/// - `FetchedButDirty`, `SkippedNoRemote`, `UpToDate`, `Synced` are silent.
/// - `FetchedButDiverged`, `FetchFailed`, `RepoUnusable` emit a
///   warning so the user knows the spawn fell back to local HEAD.
///
/// Spawn proceeds either way; the event is purely informational.
pub(super) fn emit_sync_outcome_event(
    app: &tauri::AppHandle,
    session_id: i64,
    mesh_path: &str,
    outcome: Result<crate::git::sync::FetchOutcome, crate::git::sync::FetchError>,
) {
    let payload = match outcome {
        Ok(crate::git::sync::FetchOutcome::FetchedButDirty { new_commits }) => {
            // Silent, like Synced/UpToDate: the fetch reached the remote and
            // advanced the tracking refs the worktree is cut from — the new
            // node IS fresh. Only the parent checkout's fast-forward was
            // skipped, and the user already knows their own tree is dirty.
            tracing::info!(
                "spawn_agent_inner: auto-sync fetched {} commit(s) but skipped the pull \
                 (parent dirty) for session {}",
                new_commits,
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::SkippedNoRemote) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync skipped (no origin) for session {}",
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::UpToDate) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync up-to-date for session {}",
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::Synced { new_commits }) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync pulled {} commit(s) for session {}",
                new_commits,
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::FetchedButDiverged {
            new_commits,
            reason,
        }) => {
            // Diverged is informational, not an error — the fetch
            // succeeded, the new commits are visible locally, we just
            // can't auto-apply them without a real merge. The user
            // should know so they can decide whether to `git pull`
            // themselves or rebase.
            let message = format!(
                "Fetched {} new commit(s) from origin, but local history has diverged ({}). Spawning from local HEAD — pull manually to sync.",
                new_commits, reason
            );
            tracing::warn!("spawn_agent_inner: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::Diverged,
                new_commits: Some(new_commits),
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
        Err(crate::git::sync::FetchError::RepoUnusable(reason)) => {
            let message = format!(
                "Couldn't auto-sync the mesh — repository is unusable: {}. Spawning from local HEAD instead.",
                reason
            );
            tracing::warn!("spawn_agent_inner: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::RepoUnusable,
                new_commits: None,
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
        Err(crate::git::sync::FetchError::FetchFailed(reason)) => {
            // The most common case: network down. We don't try to
            // distinguish "no network" from "auth failure" — both look
            // the same to `git fetch`. The user knows whether they
            // have connectivity; we just tell them we couldn't sync.
            let message = if reason.is_empty() {
                "Couldn't auto-sync the mesh (fetch failed). Spawning from local HEAD instead."
                    .to_string()
            } else {
                format!(
                    "Couldn't auto-sync the mesh ({}). Spawning from local HEAD instead.",
                    reason
                )
            };
            tracing::warn!("spawn_agent_inner: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::FetchFailed,
                new_commits: None,
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
    };
    if let Some(payload) = payload {
        let _ = app.emit("mesh-sync-warning", payload);
    }
}

// The worktree-provision helpers — `fetch_single_ref`, `locked_fetch_pr_head`,
// `fork_remote_alias`, `fetch_fork_head`, `read_origin_ref_sha`,
// `upgrade_warm_to_mode`, `adopt_warm_worktree_by_move`,
// `checkout_worktree_to_base`, `run_git_checkout` — live in
// `crate::git::worktree::provision` (ADR 0007 consolidation, issue #677, plus
// #698's `locked_fetch_pr_head` wrapper). The spawn path reaches them through
// the module-level `use` at the top of this file; the call sites inside
// `spawn_agent_inner` use them transparently.
