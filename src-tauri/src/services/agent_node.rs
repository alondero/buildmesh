//! Agent Node service — creation, deletion, and lifecycle orchestration

use crate::agent::provider::parse_spawn_option_id;
use crate::db;
use crate::env;
use crate::git::worktree::{self, WorktreeCloseSafety};
use crate::models::{AgentNode, PendingWorktreeRemoval, SessionStatus};
use crate::preferences::resolve_harness_provider;

/// Error type for agent node service operations
#[derive(Debug)]
pub enum AgentNodeError {
    Db(rusqlite::Error),
    /// Node's `SessionStatus` was not eligible for the requested
    /// operation (e.g. `regenerate` rejects `Spawning` / `Pending` /
    /// `Archived` / `Suspended`). The wrapped string is the user-
    /// facing reason.
    Status(String),
    /// A backend orchestrator call (`spawn_agent_inner` today, but
    /// the variant is intentionally generic so future downstream
    /// calls share the path) failed. The wrapped string is the
    /// underlying error message verbatim, so the frontend's toast
    /// surfaces the same text the Tauri command returns.
    Backend(String),
}

impl std::fmt::Display for AgentNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentNodeError::Db(e) => write!(f, "{}", e),
            AgentNodeError::Status(e) => write!(f, "{}", e),
            AgentNodeError::Backend(e) => write!(f, "{}", e),
        }
    }
}

impl From<rusqlite::Error> for AgentNodeError {
    fn from(e: rusqlite::Error) -> Self {
        AgentNodeError::Db(e)
    }
}

/// Create a new agent node with auto-generated name, environment detection,
/// and provider resolution.
///
/// Pass `None` for `source_pr` / `source_pr_pinned_sha` /
/// `head_repo_owner` / `head_repo_clone_url` on non-PR spawns (issue-spawn,
/// handover, hand-spawn). Fork PRs call [`create_with_source_pr_fork`]
/// directly with the fork fields populated (issue #443). `source_pr_pinned_sha`
/// (issue #444) is the exact-pinning handle — `None` skips the drift check.
///
/// `source_pr` and `source_pr_pinned_sha` are **structurally** rejected here
/// (asserted at function entry) so a future caller that mistakenly passes
/// `Some(_)` for either — an importer, a migration script — fails loudly
/// at the boundary rather than silently writing a
/// `source_pr` onto a node that didn't actually come from a PR. The PR-spawn
/// entry point is [`create_with_source_pr_fork`], which is the only function
/// that should ever pass `Some(_)` for these fields. Pinning test lives in
/// `services::agent_node::tests` (issue #448).
#[allow(clippy::too_many_arguments)]
pub fn create(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    assert!(
        source_pr.is_none(),
        "services::agent_node::create refuses source_pr=Some(_); \
         the non-PR wrappers must persist source_pr=None so spawn_agent_inner's \
         'is this a PR-spawned node?' branch (node.source_pr.is_some()) stays a \
         reliable signal. Use create_with_source_pr_fork for PR spawns."
    );
    assert!(
        source_pr_pinned_sha.is_none(),
        "services::agent_node::create refuses source_pr_pinned_sha=Some(_); \
         use create_with_source_pr_fork for PR spawns that need SHA pinning."
    );
    // Single insert path; fork fields are None for the non-fork entry point.
    create_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree_override,
        name_override,
        None,
        None,
    )
}

/// Like [`create`], but also records the fork's owner login and clone URL
/// when the PR is from a fork (issue #443). `spawn_agent_inner` reads
/// these to register `fork-<owner>` as a remote and fetch the head ref.
/// For same-repo PRs, pass `None` for both fork fields and call [`create`]
/// instead — the spawn path then takes the #420 `origin/<head_ref>` branch.
#[allow(clippy::too_many_arguments)]
pub fn create_with_source_pr_fork(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    let mesh = db::get_mesh_by_id(mesh_id)?;
    let use_worktree = use_worktree_override.unwrap_or(mesh.use_worktree);

    // Caller may supply a pre-derived name (e.g. `slugify_issue_title` for the
    // GitHub-issue spawn path). Validation/fallback is the caller's job — by
    // the time we get here, the name is assumed to be a valid `SLUG_REGEX`
    // match, which is also a safe directory name.
    let session_name = match name_override {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => crate::session_naming::on_spawn(),
    };
    tracing::debug!(
        "agent_node::create: mesh_id={}, name={}, path={}, branch={}, provider={:?}, use_worktree={}, source_pr={:?}, source_pr_pinned_sha={:?}, head_repo_owner={:?}",
        mesh_id, session_name, path, branch, provider, use_worktree, source_pr, source_pr_pinned_sha, head_repo_owner
    );

    let worktree_db_name = if use_worktree {
        Some(session_name.as_str())
    } else {
        None
    };

    let resolved = env::resolve_agent_path(path, worktree_db_name);
    let env_type = resolved.env_type;
    // Store the harness/profile id verbatim (issue #535) — no premature parse
    // to the legacy `Provider` enum, which would flatten an unknown profile id
    // to Anthropic. Resolution happens at the spawn seam. An absent provider
    // defaults to "anthropic", matching the prior `Provider::Anthropic` default.
    let provider_id = provider.unwrap_or("anthropic");

    let node = db::create_agent_node(
        mesh_id,
        &session_name,
        path,
        branch,
        env_type,
        provider_id,
        worktree_db_name,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree,
        head_repo_owner,
        head_repo_clone_url,
    )?;

    Ok(node)
}

/// Like `create`, but sets the initial status to [`SessionStatus::Pending`]
/// so the frontend can distinguish "node row exists, stage-2 not yet
/// started" from "node row exists, agent is idle and ready to re-spawn".
///
/// This is the fast stage-1 of the two-stage issue-spawn flow. The caller
/// is expected to invoke `start_node_background` to do the slow work
/// (git fetch, worktree create, PTY spawn) and update the status to
/// `Running` on success or `Error` on failure. The source-pr / fork-repo
/// fields follow the same contract as [`create`]: `source_pr` and
/// `source_pr_pinned_sha` must be `None` here; the PR-spawn entry point
/// is [`create_pending_with_source_pr_fork`] (issue #448).
#[allow(clippy::too_many_arguments)]
pub fn create_pending(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    assert!(
        source_pr.is_none(),
        "services::agent_node::create_pending refuses source_pr=Some(_); \
         use create_pending_with_source_pr_fork for PR spawns."
    );
    assert!(
        source_pr_pinned_sha.is_none(),
        "services::agent_node::create_pending refuses source_pr_pinned_sha=Some(_); \
         use create_pending_with_source_pr_fork for PR spawns that need SHA pinning."
    );
    // Single insert path; fork fields are None for the non-fork entry point.
    create_pending_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        name_override,
        None,
        None,
    )
}

/// Like [`create_pending`], but also records the fork's owner login and
/// clone URL when the PR is from a fork (issue #443) and the PR's head
/// commit SHA for exact-pinning (issue #444). `commands::agent::create_pr_node`
/// calls this directly with the fork fields populated for fork PRs and
/// `None, None` for same-repo PRs.
#[allow(clippy::too_many_arguments)]
pub fn create_pending_with_source_pr_fork(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    let mut node = create_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        None,
        name_override,
        head_repo_owner,
        head_repo_clone_url,
    )?;
    // Two writes (insert + status update) is one extra ~1ms SQLite round
    // trip. Acceptable: this function is on the fast path, and the second
    // write is the whole point — without it the new node would look
    // identical to a freshly-closed, ready-to-resume idle node.
    // Routes through SessionLifecycle (issue #132). `DbOnlySink` because
    // this function has no `AppHandle` and `Pending` doesn't emit any
    // events anyway.
    crate::agent::session_lifecycle::on_created(
        &crate::agent::session_lifecycle::DbOnlySink,
        node.id,
    )
    .map_err(AgentNodeError::Backend)?;
    node.status = SessionStatus::Pending;
    Ok(node)
}

/// Return whether closing the node can remove its worktree without risking work.
pub fn get_worktree_close_safety(session_id: i64) -> Result<WorktreeCloseSafety, AgentNodeError> {
    let node = db::get_agent_node_by_id(session_id)?;
    let Some(worktree_path) = env::node_worktree_path(&node).map(|r| r.host_path) else {
        return Ok(WorktreeCloseSafety {
            worktree_path: None,
            has_uncommitted: false,
            has_unpushed: false,
            is_detached: false,
        });
    };

    Ok(worktree::close_safety(&worktree_path))
}

/// Close an agent node (Phase 1 — fast and authoritative).
///
/// This kills the agent process tree and removes the node from the database
/// immediately, but deliberately does **not** delete the worktree directory:
/// that recursive delete is slow (a `node_modules`-heavy tree is tens of
/// thousands of files) and retry-prone on Windows, so blocking the close on it
/// is what makes the UI feel frozen. Instead, when a worktree should be removed
/// we record it in the durable `pending_worktree_removals` queue — atomically
/// with deleting the row — and let `process_pending_removals` reclaim the disk
/// in the background or on next launch. The kill stays synchronous here: it's
/// fast, and it's the one step we must finish before parting with the node so we
/// never leave a live agent running or fighting the eventual removal (#239).
pub fn delete(session_id: i64, remove_worktree: bool) -> Result<(), AgentNodeError> {
    let node = db::get_agent_node_by_id(session_id)?;

    let removal_path = if remove_worktree {
        crate::agent::process::PROCESS_REGISTRY.kill_session(session_id);
        crate::agent::process::PROCESS_REGISTRY.remove(&session_id);
        env::node_worktree_path(&node).map(|r| r.host_path)
    } else {
        None
    };

    crate::session_naming::cleanup(session_id);
    // Drop the auto-clear guard too: an awaiting-input node being deleted
    // without first being disarmed would otherwise leak its ARMED entry
    // forever (issue #1263). `disarm` is a no-op on an unarmed node, so
    // sibling-positioning with the other per-node cleanups is safe.
    crate::attention_autoclear::disarm(session_id);
    // Drop autopilot state too. The ledger delete is explicit as a
    // defensive belt: the table declares ON DELETE CASCADE and the
    // bundled SQLite build (rusqlite 0.32 / SQLite 3.46.0) has FK
    // enforcement on by default, so the parent agent_nodes DELETE
    // below already cascades — but a future system-libsqlite link
    // would have FK off by default and need this explicit DELETE to
    // avoid an orphan. Removing the row also frees the issue for a
    // poller retry if it is still open + labelled.
    crate::autopilot::evaluator::unregister(session_id);
    db::delete_autopilot_run(session_id)?;

    let removal = removal_path.as_deref().map(|p| (p, node.name.as_str()));
    db::delete_agent_node_enqueueing_removal(session_id, removal)?;
    // The raw-output subscription is node-scoped, so process exit/restart
    // deliberately preserves it. Once the row deletion commits, this is the
    // authoritative backend cleanup seam (idempotent with frontend disposal).
    crate::agent::output::unregister(session_id);
    Ok(())
}

/// Serializes drains. Each drain processes the *whole* queue, so two running at
/// once (e.g. closing two nodes in quick succession, or a close overlapping the
/// startup reconcile) would both try to remove the same just-enqueued worktree
/// and race on its `.removing` staging directory. Holding this for the drain's
/// duration means a second drain waits, then re-lists and finds the first
/// already dequeued — no wasted work, no spurious cleanup-failed warnings.
static DRAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Drain the pending worktree-removal queue: remove each queued worktree's
/// directory, dequeuing only the ones that succeed. Removals that still fail
/// (e.g. a handle is somehow held) stay queued for the next drain or next
/// launch, and are returned so the caller can warn the user. Safe to run from
/// the close path and from startup reconcile — `remove_one_worktree` is
/// idempotent (a missing directory counts as already removed).
pub fn process_pending_removals() -> Vec<(PendingWorktreeRemoval, String)> {
    let _guard = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let pending = match db::list_pending_worktree_removals() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("could not read pending worktree removals: {}", e);
            return Vec::new();
        }
    };

    process_removals(
        pending,
        crate::git::worktree::remove_one_worktree_and_branch,
        db::delete_pending_worktree_removal,
        // Issue #653 deleter-side guard: skip paths whose `warm_worktrees`
        // row is currently `claimed` (a live spawn has just adopted the
        // directory). The skip deques the pending removal — claim supersedes
        // tombstone intent, and the live agent's worktree must survive. When
        // the spawn's `forget_after_spawn` later drops the row, the path is
        // no longer claimed and any *new* pending removal (e.g. a subsequent
        // close of the same node) flows through normally.
        db::warm_pool_claims_path,
    )
}

/// Pure orchestration for the drain: for each removal, attempt the directory
/// remove; on success dequeue it; on failure keep it queued and collect it.
/// Dependencies are injected so the "dequeue only on success" invariant — the
/// thing that guarantees failed cleanups get retried rather than lost — is
/// testable without the global DB or a real filesystem.
///
/// `is_claimed` is the issue #653 deleter-side guard: when it returns `true`
/// for a removal's path, the drain skips the `remove` call (the directory
/// belongs to a live spawn's worktree — deleting it would destroy the
/// agent's work) AND dequeues the pending entry (claim supersedes tombstone
/// intent). It does NOT append to `failures` — the path is alive, not
/// broken; no user warning is warranted.
fn process_removals(
    pending: Vec<PendingWorktreeRemoval>,
    remove: impl Fn(&str) -> Result<(), String>,
    dequeue: impl Fn(&str) -> db::SqlResult<()>,
    is_claimed: impl Fn(&str) -> db::SqlResult<bool>,
) -> Vec<(PendingWorktreeRemoval, String)> {
    let mut failures = Vec::new();
    for removal in pending {
        match is_claimed(&removal.worktree_path) {
            Ok(true) => {
                // Issue #653 — the warm pool has this path as a live spawn's
                // worktree. Dequeue the tombstone (the spawn's intent
                // supersedes the close's intent) but do NOT touch the
                // directory. The spawn's `forget_after_spawn` will drop the
                // `claimed` row when the agent is up; any subsequent close
                // that enqueues a fresh tombstone at this path will be
                // handled normally because the row is then gone.
                tracing::info!(
                    "warm_pool: skipping pending removal for {} — path is claimed by a warm-pool spawn",
                    removal.worktree_path
                );
                if let Err(e) = dequeue(&removal.worktree_path) {
                    tracing::error!(
                        "warm_pool: failed to dequeue superseded tombstone for claimed path {}: {}",
                        removal.worktree_path,
                        e
                    );
                }
            }
            Ok(false) => match remove(&removal.worktree_path) {
                Ok(()) => {
                    if let Err(e) = dequeue(&removal.worktree_path) {
                        tracing::error!(
                            "removed worktree {} but failed to dequeue it: {}",
                            removal.worktree_path, e
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "worktree removal for {} failed, will retry: {}",
                        removal.node_name, e
                    );
                    failures.push((removal, e));
                }
            },
            Err(e) => {
                // The guard query failed — fail closed (don't remove the
                // directory) but DON'T drop the tombstone either, so the
                // next drain retries. This is the conservative choice: a
                // transient DB error must not cascade into "agent's worktree
                // silently deleted because we couldn't read the pool table".
                //
                // Deliberately NOT pushing to `failures`: the caller
                // (`commands::agent_node::drain_pending_removals`) emits a
                // `worktree-cleanup-failed` Tauri event per failure, which
                // renders a "worktree cleanup failed" toast to the user —
                // but no worktree cleanup was attempted here, only a pool-
                // claim read. A transient SQLite error during the guard
                // check must NOT surface as user-facing "cleanup failed"
                // noise; the next drain retries silently.
                tracing::warn!(
                    "warm_pool: is_claimed check failed for {} ({}); leaving tombstone queued for retry",
                    removal.worktree_path,
                    e
                );
            }
        }
    }
    failures
}

/// Validate that a node's status is eligible for regeneration.
///
/// Allowed: `Idle`, `AwaitingInput`, `Error`, `Running`, `Suspended`,
/// `Completed` — the node is in a state we can either kill-and-replace
/// or, in the `Suspended` case, just respawn (no live process to kill).
/// The orchestrator (see `regenerate` step 3) branches on `Suspended` to
/// skip the unconditional `kill_agent` call.
/// Rejected: `Spawning` / `Pending` (another spawn is in flight) and
/// `Archived` (the node is closed-but-historical).
///
/// Extracted from the #775 spec (step 2) so the orchestrator and the
/// tests share one source of truth. `Suspended` was added so the
/// Regenerate picker can also act as a "Resume" entry for Suspended
/// nodes — those have no live process to kill (crash-recovery was
/// killed on app exit; autopilot-gate never spawned). The kill-skip
/// branch in `regenerate` prevents `kill_agent_blocking`'s
/// unconditional `on_idle` tail (commands/agent.rs:1172) from
/// clobbering Suspended→Idle before the new spawn lands.
///
/// `Completed` was added alongside (the second issue surfaced after
/// the autopilot wrap-up flow matured — issue #485 left autopilot
/// nodes stuck in a "viewable but un-interactable" state from the
/// Regenerate picker's perspective). The agent PTY is still alive at
/// that point (autopilot doesn't kill the process when it writes the
/// wrap-up complete), so the orchestrator's normal kill+respawn path
/// applies — `should_skip_kill_for_regenerate(Completed)` stays
/// `false`. `decide_resume` reuses the captured `cli_session_id`, so a
/// same-harness Regenerate picks up exactly where the autopilot node
/// left off.
pub fn validate_status_eligible(status: SessionStatus) -> Result<(), String> {
    match status {
        SessionStatus::Idle
        | SessionStatus::AwaitingInput
        | SessionStatus::Error
        | SessionStatus::Running
        | SessionStatus::Suspended
        | SessionStatus::Completed => Ok(()),
        _ => Err(format!(
            "regenerate unavailable: node is in {} state (must be idle, awaiting_input, error, running, suspended, or completed)",
            status.to_db_str()
        )),
    }
}

/// Whether the [`regenerate`] orchestrator should skip its pre-spawn
/// `kill_agent` call. True when the node has no live process to
/// terminate; `kill_agent_blocking`'s tail calls
/// `session_lifecycle::on_idle` which unconditionally writes `Idle`
/// (commands/agent.rs:1172), and for a Suspended node that write
/// would clobber Suspended→Idle BEFORE the new spawn lands.
///
/// Pure — extracted from `regenerate` so the rule is unit-testable
/// without Tauri or a real DB. Keep the helper narrow: only `Suspended`
/// qualifies today. Future "skip" states (e.g. an `Archived` revival)
/// would be added here with a corresponding test.
pub fn should_skip_kill_for_regenerate(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Suspended)
}

/// Decide whether to pass the existing `cli_session_id` to the new
/// agent's spawn as a `--resume` arg, or start a fresh session.
///
/// Continue iff ALL of:
///   * the new provider's adapter has `supports_resume() == true`
///     (the executor accepts the resume flag);
///   * the new provider shares the same Agent Harness as the old —
///     the existing `cli_session_id` is bound to the old harness's
///     session format (Claude Code UUIDs are not Codex session ids,
///     so a cross-harness resume would land on a session the new
///     harness can't read);
///   * the node actually has a `cli_session_id` to resume from.
///
/// Pure — extracted from `regenerate` so the rule is unit-testable
/// without Tauri or a real DB. The harness comparison uses the
/// leading `harness_id` part of the composite id
/// (`<harness>:<provider_id>`, issue #575): `claude:minimax` and
/// `claude:openrouter` share the `claude` harness, so a swap between
/// them continues the session.
pub fn decide_resume(
    old_provider: &str,
    new_provider: &str,
    cli_session_id: Option<&str>,
) -> Option<String> {
    let (old_harness, _) = parse_spawn_option_id(old_provider);
    let (new_harness, _) = parse_spawn_option_id(new_provider);
    let same_harness = old_harness == new_harness;
    let new_provider_enum = resolve_harness_provider(new_provider);
    if same_harness && new_provider_enum.adapter().supports_resume() {
        cli_session_id.map(String::from)
    } else {
        None
    }
}

/// Sync helper for step 1 of the regenerate orchestrator: load the
/// node, validate its status, and return the old provider + whether
/// the kill step should be skipped (true only for `Suspended`).
///
/// Pure — no async, no thread-pool orchestration. The command boundary
/// (`commands::agent_node::regenerate_agent_node`) is the single place
/// that wraps this in `crate::commands::run_blocking` (issue #1380
/// review round-2 feedback 1).
///
/// # Architecture
///
/// The previous async `regenerate` orchestrator that called the three
/// sync helpers DIRECTLY on the Tokio runtime was a phantom offload —
/// the helpers are pure sync, but the orchestrator's `await` never
/// crossed a thread-pool boundary. Round-2 review caught it. The
/// orchestrator is now inline in the command, where each helper
/// call IS wrapped in `run_blocking`.
pub fn regenerate_load_blocking(
    node_id: i64,
) -> Result<(String, bool), AgentNodeError> {
    let node = db::get_agent_node_by_id(node_id)?;
    validate_status_eligible(node.status).map_err(AgentNodeError::Status)?;
    Ok((
        node.provider.clone(),
        should_skip_kill_for_regenerate(node.status),
    ))
}

/// Sync helper for steps 4–6 of the regenerate orchestrator: write the
/// new `provider` column, reload the row, and return whether the new
/// spawn should resume the existing session (same-harness with a
/// captured `cli_session_id`) or start fresh.
///
/// Takes `&str` references — the caller owns the strings and the
/// helper does NOT need to capture them into a `'static` closure.
/// `decide_resume` owns the cross-harness rejection rule.
pub fn regenerate_apply_blocking(
    node_id: i64,
    old_provider: &str,
    new_provider: &str,
) -> Result<bool, AgentNodeError> {
    db::set_agent_node_provider(node_id, new_provider)?;
    let node = db::get_agent_node_by_id(node_id)?;
    Ok(decide_resume(old_provider, new_provider, node.cli_session_id.as_deref()).is_some())
}

/// Sync helper for step 8 of [`regenerate`]: reload the node row
/// after the spawn lands so the command returns the post-spawn
/// state. The spawn pipeline may have updated `cli_session_id`,
/// `status_changed_at`, etc.
pub fn regenerate_reload_blocking(node_id: i64) -> Result<AgentNode, AgentNodeError> {
    db::get_agent_node_by_id(node_id).map_err(AgentNodeError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let tmp =
                std::env::temp_dir().join(format!("buildmesh_agent_node_test_{}_{}", pid, id));
            Self(tmp)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sig() -> git2::Signature<'static> {
        git2::Signature::now("test", "test@example.com").unwrap()
    }

    fn init_repo(path: &Path) -> git2::Repository {
        fs::create_dir_all(path).unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(path, &opts).unwrap();

        fs::write(path.join("file.txt"), "initial").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let s = sig();
        repo.commit(Some("HEAD"), &s, &s, "initial commit", &tree, &[])
            .unwrap();
        drop(tree);

        repo
    }

    fn add_worktree(root_repo: &git2::Repository, root: &TempDir, name: &str) -> PathBuf {
        let head = root_repo.head().unwrap().peel_to_commit().unwrap();
        let branch = root_repo.branch(name, &head, false).unwrap();
        let reference = branch.into_reference();
        let worktree_path = root.path().join(".claude").join("worktrees").join(name);
        fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
        root_repo
            .worktree(
                name,
                &worktree_path,
                Some(git2::WorktreeAddOptions::new().reference(Some(&reference))),
            )
            .unwrap();
        worktree_path
    }

    #[test]
    fn drain_removes_real_worktree_and_dequeues_only_on_success() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let good = add_worktree(&root_repo, &root, "wt-good");

        let pending = vec![
            PendingWorktreeRemoval {
                worktree_path: good.to_string_lossy().to_string(),
                node_name: "wt-good".to_string(),
            },
            PendingWorktreeRemoval {
                worktree_path: "/nonexistent/wt-bad".to_string(),
                node_name: "wt-bad".to_string(),
            },
        ];

        let dequeued = std::cell::RefCell::new(Vec::<String>::new());
        let failures = process_removals(
            pending,
            |path| {
                // A missing parent repo can't be opened → genuine failure.
                if path.starts_with("/nonexistent") {
                    return Err("not a removable worktree".to_string());
                }
                crate::git::worktree::remove_one_worktree(path)
            },
            |path| {
                dequeued.borrow_mut().push(path.to_string());
                Ok(())
            },
            // Issue #653: this test predates the deleter-side guard and its
            // fixture doesn't touch `warm_worktrees`, so the guard is a
            // pure no-op. `_| Ok(false)` keeps the original behaviour intact.
            |_path| Ok(false),
        );

        assert!(!good.exists(), "the real worktree directory must be removed");
        assert_eq!(
            dequeued.into_inner(),
            vec![good.to_string_lossy().to_string()],
            "only the successful removal is dequeued"
        );
        assert_eq!(failures.len(), 1, "the failed removal is returned for warning");
        assert_eq!(failures[0].0.node_name, "wt-bad");
    }

    /// Issue #653 deleter-side guard: a pending-removal entry whose path is
    /// currently `claimed` in `warm_worktrees` (a live spawn has just adopted
    /// the directory) must be SKIPPED — `remove` is not called (the directory
    /// belongs to a live agent) and the tombstone IS dequeued (claim
    /// supersedes the close's intent, so the entry is dead). It must NOT be
    /// reported as a failure (no user-facing warning for a healthy state).
    ///
    /// Without this guard, a concurrent close + spawn on the same path would
    /// race `process_pending_removals` to delete the directory out from under
    /// the spawning agent — issue #653's exact symptom (Error node + stranded
    /// `claimed` row).
    #[test]
    fn process_removals_skips_claimed_path_and_dequeues() {
        let pending = vec![
            // Live spawn's worktree: warm pool row is `claimed`. The drain
            // must skip — no `remove`, dequeue + no failure.
            PendingWorktreeRemoval {
                worktree_path: "/repo/m/.claude/worktrees/wt-claimed".to_string(),
                node_name: "wt-claimed".to_string(),
            },
            // Normal cold removal: not claimed. The drain proceeds.
            PendingWorktreeRemoval {
                worktree_path: "/repo/m/.claude/worktrees/wt-cold".to_string(),
                node_name: "wt-cold".to_string(),
            },
        ];

        let removed = std::cell::RefCell::new(Vec::<String>::new());
        let dequeued = std::cell::RefCell::new(Vec::<String>::new());
        let failures = process_removals(
            pending,
            |path| {
                removed.borrow_mut().push(path.to_string());
                Ok(())
            },
            |path| {
                dequeued.borrow_mut().push(path.to_string());
                Ok(())
            },
            // Warm pool says `claimed` for the first path only.
            |path| Ok(path.ends_with("wt-claimed")),
        );

        assert_eq!(
            removed.into_inner(),
            vec!["/repo/m/.claude/worktrees/wt-cold".to_string()],
            "only the non-claimed path is touched by the removal driver — the live spawn's directory must survive"
        );
        // Both paths are dequeued: the skip branch consumes the tombstone
        // (claim supersedes intent), and the successful removal consumes its
        // own tombstone. The shared log is what proves the order-agnostic
        // "both paths leave the queue" invariant.
        let dequeued = dequeued.into_inner();
        assert!(
            dequeued.contains(&"/repo/m/.claude/worktrees/wt-claimed".to_string()),
            "the superseded tombstone must be dequeued (claim supersedes close intent)"
        );
        assert!(
            dequeued.contains(&"/repo/m/.claude/worktrees/wt-cold".to_string()),
            "the normal removal's tombstone must be dequeued"
        );
        assert_eq!(dequeued.len(), 2, "every pending entry must exit the queue");
        assert!(
            failures.is_empty(),
            "a `claimed` skip is not a failure — no user warning warranted (got: {failures:?})"
        );
    }

    /// A transient `is_claimed` error (e.g. SQLite lock contention mid-drain)
    /// must fail CLOSED: don't remove the directory (could be a live spawn)
    /// and don't drop the tombstone — the next drain retries. The caller
    /// MUST NOT see this as a worktree-cleanup failure: only `remove()`
    /// errors reach `failures`, so a guard-query error stays silent on the
    /// user side (no spurious "worktree cleanup failed" toast) and the
    /// tombstone survives for the next drain to retry.
    #[test]
    fn process_removals_fails_closed_when_is_claimed_errors() {
        let pending = vec![PendingWorktreeRemoval {
            worktree_path: "/repo/m/.claude/worktrees/wt-uncertain".to_string(),
            node_name: "wt-uncertain".to_string(),
        }];

        let removed = std::cell::RefCell::new(Vec::<String>::new());
        let dequeued = std::cell::RefCell::new(Vec::<String>::new());
        let failures = process_removals(
            pending,
            |path| {
                removed.borrow_mut().push(path.to_string());
                Ok(())
            },
            |path| {
                dequeued.borrow_mut().push(path.to_string());
                Ok(())
            },
            |_path| Err(rusqlite::Error::QueryReturnedNoRows), // guard query failed
        );

        assert!(
            removed.borrow().is_empty(),
            "must NOT touch the directory when the guard check errors — could be a live spawn"
        );
        assert!(
            dequeued.borrow().is_empty(),
            "must NOT dequeue when the guard check errors — leave the tombstone for the next drain"
        );
        assert!(
            failures.is_empty(),
            "a guard-query error must NOT surface as a worktree-cleanup failure \
             (would emit a misleading user toast); the next drain retries silently"
        );
    }

    // -------------------------------------------------------------------
    // Issue #448 — pin the `source_pr = None` invariant on the issue-spawn
    // and hand-spawn paths. `services::agent_node::create` and
    // `create_pending` are the only entry points non-PR spawns go through
    // (issue-spawn, handover, generic mobile spawn, Tauri hand-spawn);
    // `spawn_agent_inner` uses `node.source_pr.is_some()` to decide whether
    // to take the worktree-adoption path. A future caller — an importer,
    // a migration script — could silently write a
    // `source_pr` onto a node that didn't actually come from a PR, and the
    // user would see a confusing "could not fetch PR head ref" warning on
    // a node spawned from a regular issue.
    //
    // The wrappers now refuse `Some(_)` for `source_pr` and
    // `source_pr_pinned_sha` at function entry (PR-spawn fields). The PR-spawn
    // entry points (`create_with_source_pr_fork` /
    // `create_pending_with_source_pr_fork`) remain the only functions that
    // can write a non-`None` `source_pr` — those tests live with the PR-spawn
    // callers in `commands::agent_tests`.
    //
    // Two complementary tests cover the invariant:
    //
    //   * Positive (happy-path) tests exercise `create` / `create_pending`
    //     with `source_pr = None` and assert the persisted row reads back
    //     `source_pr = None`. These need a real DB row, so they call
    //     `ensure_db_init()` which lazily inits the global DB.
    //
    //   * Negative `#[should_panic]` tests call the wrappers with
    //     `source_pr = Some(_)`. The assertion fires before any DB call, so
    //     these don't need a DB at all — they catch a regression at the
    //     wrapper boundary with zero infrastructure.
    // -------------------------------------------------------------------

    use std::sync::Once;

    /// Lazily initialise the global DB exactly once per test process. The
    /// underlying `db::init` uses a process-global `OnceCell`, so calling
    /// it twice is an error; this guard makes the second-and-later call a
    /// no-op even if another test in the binary (e.g. `db::mesh_tests`)
    /// already won the race. The temp path is process-unique so a sibling
    /// test running with `--test-threads=1` and this one never collide on
    /// the SQLite file.
    fn ensure_db_init() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let temp_path = std::env::temp_dir().join(format!(
                "buildmesh_agent_node_invariant_{}.db",
                std::process::id()
            ));
            // `db::init` returns `Err` if another test already set the
            // global DB (e.g. `db::mesh_tests` running first). That's fine
            // for our tests — they only read the global DB and create their
            // own mesh rows with a unique path.
            let _ = crate::db::init(&temp_path);
        });
    }

    /// Create a fresh mesh in the global DB at a unique per-test path and
    /// return its id. Each call uses a monotonic counter so parallel tests
    /// can't collide on the `meshes.path` UNIQUE constraint.
    fn fresh_mesh() -> i64 {
        ensure_db_init();
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let path = format!("/tmp/buildmesh_invariant_test_{}", id);
        crate::db::create_mesh(&format!("invariant-{}", id), &path)
            .expect("fresh_mesh: create_mesh should succeed")
            .id
    }

    #[test]
    fn create_returns_node_with_source_pr_none() {
        // The wrapper contract: passing `source_pr = None` for an
        // issue-spawn / hand-spawn call persists `source_pr = None`.
        let mesh_id = fresh_mesh();

        let node = create(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create with source_pr=None must succeed");

        assert_eq!(
            node.source_pr, None,
            "issue-spawn wrapper must persist source_pr=None"
        );
        assert_eq!(
            node.source_pr_pinned_sha, None,
            "issue-spawn wrapper must persist source_pr_pinned_sha=None"
        );
        assert_eq!(
            node.source_issue, None,
            "hand-spawn wrapper must persist source_issue=None when not supplied"
        );
    }

    #[test]
    fn create_pending_returns_node_with_source_pr_none_and_pending_status() {
        // `create_pending` is the fast stage-1 of the two-stage issue-spawn
        // flow — it must also persist `source_pr = None` so stage-2's
        // `node.source_pr.is_some()` branch doesn't accidentally fire.
        let mesh_id = fresh_mesh();

        let node = create_pending(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
        )
        .expect("create_pending with source_pr=None must succeed");

        assert_eq!(
            node.source_pr, None,
            "create_pending must persist source_pr=None"
        );
        assert_eq!(
            node.source_pr_pinned_sha, None,
            "create_pending must persist source_pr_pinned_sha=None"
        );
        assert_eq!(
            node.status,
            SessionStatus::Pending,
            "create_pending must leave the row in Pending so the frontend \
             can distinguish stage-1-done from idle-and-ready-to-resume"
        );
    }

    #[test]
    fn create_with_source_issue_set_keeps_source_pr_independent() {
        // Optional step 4 from the issue: when `source_issue = Some(N)`,
        // `source_pr` must still come back as `None` — the two source
        // fields are independent columns on the row.
        let mesh_id = fresh_mesh();

        let node = create(
            mesh_id,
            "/tmp/buildmesh_invariant_test",
            "main",
            Some("anthropic"),
            Some(123),
            None,
            None,
            None,
            Some("gh123-test-slug"),
        )
        .expect("create with source_issue=Some(N), source_pr=None must succeed");

        assert_eq!(
            node.source_issue,
            Some(123),
            "source_issue must round-trip independently of source_pr"
        );
        assert_eq!(
            node.source_pr, None,
            "source_pr must remain None even when source_issue is set"
        );
        assert_eq!(node.source_pr_pinned_sha, None);
    }

    #[test]
    #[should_panic(expected = "source_pr=Some(_)")]
    fn create_rejects_source_pr_some() {
        // A future caller — importer, migration script — tries to set
        // `source_pr` on a node that didn't actually come
        // from a PR. The wrapper must refuse at the boundary rather than
        // silently persist it. The assertion fires before any DB call, so
        // we don't even need `ensure_db_init()` here — a missing DB is the
        // strongest possible failure signal for this regression.
        let _ = create(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            Some(42),
            None,
            None,
            None,
        );
    }

    #[test]
    #[should_panic(expected = "source_pr=Some(_)")]
    fn create_pending_rejects_source_pr_some() {
        let _ = create_pending(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            Some(42),
            None,
            None,
        );
    }

    #[test]
    #[should_panic(expected = "source_pr_pinned_sha=Some(_)")]
    fn create_rejects_source_pr_pinned_sha_some() {
        // Same boundary for the SHA-pinning field (#444) — a caller that
        // tries to pin the head SHA on a non-PR spawn gets the same
        // refused-at-entry treatment.
        let _ = create(
            /* mesh_id */ 0,
            "/tmp/never-read",
            "main",
            None,
            None,
            None,
            Some("deadbeef"),
            None,
            None,
        );
    }

    // -----------------------------------------------------------------------
    // Regenerate (issue #775) — validation + resume-decision surface.
    //
    // The three pure helpers (`validate_status_eligible`,
    // `should_skip_kill_for_regenerate`, `decide_resume`) own the new
    // logic. The async `regenerate` orchestrator is exercised end-to-end
    // via the Playwright e2e suite and the manual smoke test, so the
    // unit tests here stay focused on the rules the orchestrator
    // composes.
    // -----------------------------------------------------------------------

    #[test]
    fn validate_status_eligible_allows_running_idle_awaiting_error_suspended_completed() {
        // The six states the regenerate orchestrator accepts:
        //   * Running / Idle / AwaitingInput / Error — "process is or
        //     was running", so kill + respawn makes sense.
        //   * Suspended — no live process to kill; the orchestrator
        //     branches on `should_skip_kill_for_regenerate` to avoid
        //     the unconditional `on_idle` tail of `kill_agent`
        //     (commands/agent.rs:1172) clobbering Suspended→Idle
        //     before the new spawn lands. The frontend gates the
        //     user-driven Resume affordance on `cli_session_id` non-
        //     empty to keep the autopilot-gate case (Suspended +
        //     NULL session id) from surfacing a confusing toast.
        //   * Completed — autopilot-finished wrap-up (#485). The agent
        //     PTY is still alive at the moment of `complete_autopilot_run`
        //     (knowledge claim, not unit-tested: see
        //     autopilot::pipeline::complete_autopilot_run — the function
        //     never calls `kill_agent`), so we route through the
        //     kill+respawn path. `decide_resume` reuses the captured
        //     `cli_session_id` so a same-harness Regenerate picks up
        //     exactly where autopilot left off — which is the "I want
        //     to keep interacting with this session" affordance that
        //     #774 ticket 04 framed.
        for status in [
            SessionStatus::Running,
            SessionStatus::Idle,
            SessionStatus::AwaitingInput,
            SessionStatus::Error,
            SessionStatus::Suspended,
            SessionStatus::Completed,
        ] {
            assert!(
                validate_status_eligible(status).is_ok(),
                "expected {status:?} to be eligible, got: {:?}",
                validate_status_eligible(status),
            );
        }
    }

    #[test]
    fn validate_status_eligible_rejects_spawning_pending_archived() {
        // The three "no process to swap" states — reject at the
        // boundary. `Suspended` moved to the allowed list (see the
        // matching positive test above) when user-driven recovery for
        // Suspended nodes shipped.
        for status in [
            SessionStatus::Spawning,
            SessionStatus::Pending,
            SessionStatus::Archived,
        ] {
            assert!(
                validate_status_eligible(status).is_err(),
                "expected {status:?} to be rejected",
            );
        }
    }

    #[test]
    fn validate_status_eligible_message_includes_status() {
        // The error message must name the rejected state so the frontend
        // toast (or a dev grepping `buildmesh.log`) can tell *why* the
        // regenerate was refused without a stack trace.
        let err = validate_status_eligible(SessionStatus::Spawning).unwrap_err();
        assert!(
            err.contains("spawning"),
            "error must mention the rejected status, got: {err}",
        );
    }

    #[test]
    fn should_skip_kill_for_regenerate_only_true_for_suspended() {
        // The kill-skip branch is the load-bearing pairing with
        // `validate_status_eligible` accepting Suspended — without it,
        // the unconditional `on_idle` tail of `kill_agent_blocking`
        // would clobber Suspended→Idle before the new spawn lands.
        assert!(
            should_skip_kill_for_regenerate(SessionStatus::Suspended),
            "Suspended must skip the kill (no live process to terminate)"
        );
        for status in [
            SessionStatus::Idle,
            SessionStatus::AwaitingInput,
            SessionStatus::Error,
            SessionStatus::Running,
            // Completed — autopilot wrap-up flipped the status, but the
            // agent PTY is still alive (Claude Code sits at the prompt
            // after `gh pr create`). Regenerate must kill it so
            // `spawn_agent_inner`'s `is_agent_already_running` short-
            // circuit doesn't return early at spawn.rs:743.
            SessionStatus::Completed,
        ] {
            assert!(
                !should_skip_kill_for_regenerate(status),
                "{status:?} must NOT skip the kill (live or recently-live process)",
            );
        }
        // Sanity: Spawning / Pending / Archived never reach the
        // orchestrator's step 3 (validate_status_eligible rejects
        // them), so the skip rule is intentionally undefined for them.
        // The helper still returns a deterministic value — pinned here
        // so a future refactor doesn't accidentally expand the skip
        // set without a test witness.
        assert!(!should_skip_kill_for_regenerate(SessionStatus::Spawning));
        assert!(!should_skip_kill_for_regenerate(SessionStatus::Pending));
        assert!(!should_skip_kill_for_regenerate(SessionStatus::Archived));
    }

    #[test]
    fn decide_resume_continues_same_harness_with_session_id() {
        // Same harness, supports resume, has session id → continue.
        assert_eq!(
            decide_resume("claude:minimax", "claude:openrouter", Some("uuid-abc")),
            Some("uuid-abc".to_string()),
        );
        // Native → proxied within the same harness is also a continue.
        assert_eq!(
            decide_resume("claude", "claude:minimax", Some("uuid-abc")),
            Some("uuid-abc".to_string()),
        );
    }

    #[test]
    fn decide_resume_starts_fresh_when_no_session_id() {
        // Same harness, supports resume, but no captured session id
        // (e.g. the agent was killed before the reader caught one).
        assert_eq!(
            decide_resume("claude:minimax", "claude:openrouter", None),
            None,
        );
    }

    #[test]
    fn decide_resume_starts_fresh_on_cross_harness() {
        // The load-bearing case: a Claude → Codex swap with a Claude
        // session id must NOT resume — the UUID isn't a valid Codex
        // session id. Without this, the new agent would crash on
        // startup with "Conversation not found" and the user would
        // lose the work.
        assert_eq!(
            decide_resume("claude:minimax", "codex", Some("uuid-abc")),
            None,
            "cross-harness swaps must not pass the old session id to the new harness",
        );
        // And the reverse: Codex → Claude with a Codex id also fresh.
        assert_eq!(
            decide_resume("codex", "claude:minimax", Some("codex-session-xyz")),
            None,
        );
    }

    #[test]
    fn decide_resume_starts_fresh_when_new_provider_lacks_resume() {
        // Terminal has no session identity. Even a same-harness
        // Terminal → Terminal swap is fresh.
        assert_eq!(
            decide_resume("terminal", "terminal", Some("oc-session")),
            None,
            "a same-harness swap to a non-resumable adapter must start fresh",
        );
    }

    #[test]
    fn decide_resume_continues_opencode_with_captured_ses_id() {
        assert_eq!(
            decide_resume(
                "opencode",
                "opencode",
                Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
            ),
            Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()),
            "OpenCode now supports --session resume; a captured ses_ id must continue",
        );
    }

    #[test]
    fn set_agent_node_provider_persists_and_round_trips() {
        // Issue #775 — the Regenerate command mutates `agent_nodes.provider`
        // to swap the Model Provider on respawn. Verify the column write
        // persists and reads back the new value, and that a re-write is
        // idempotent (the function comment claims it as a property).
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_provider_test",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node should succeed");
        assert_eq!(node.provider, "anthropic");

        db::set_agent_node_provider(node.id, "claude:minimax")
            .expect("set_agent_node_provider should succeed");
        let reloaded = db::get_agent_node_by_id(node.id)
            .expect("get_agent_node_by_id should succeed");
        assert_eq!(
            reloaded.provider, "claude:minimax",
            "provider column must round-trip through the UPDATE",
        );

        // Idempotent — a no-op write should still succeed.
        db::set_agent_node_provider(node.id, "claude:minimax")
            .expect("idempotent write should succeed");
    }

    // PR #1388 review feedback 2 — the service-layer `regenerate`
    // used to orchestrate three interleaved `spawn_blocking` hops and
    // a chain of pre-clones (`new_provider_for_update`,
    // `old_provider_for_update`) to satisfy the `'static` borrow on
    // each closure. The refactor splits the work into three single-
    // purpose sync helpers that the command boundary wraps once each,
    // threading owned data via return tuples instead of captured clones.

    #[test]
    fn regenerate_load_blocking_returns_old_provider_and_skip_kill() {
        // Idle node: provider is captured verbatim, kill is NOT skipped.
        let mesh_id = fresh_mesh();
        let idle_node = create(
            mesh_id,
            "/tmp/buildmesh_regen_load_idle",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed idle node");
        let (old_provider, skip_kill) =
            regenerate_load_blocking(idle_node.id).expect("load should succeed");
        assert_eq!(old_provider, "anthropic");
        assert!(
            !skip_kill,
            "Idle has no live process to skip, but the skip rule is about Suspended only",
        );

        // Suspended node: skip_kill must be true (this is the whole
        // point of the helper — `kill_agent_blocking`'s `on_idle`
        // tail would clobber Suspended→Idle if we killed).
        let suspended_node = create(
            mesh_id,
            "/tmp/buildmesh_regen_load_suspended",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed suspended node");
        db::update_agent_node_status(suspended_node.id, SessionStatus::Suspended)
            .expect("set suspended status");
        let (suspended_provider, suspended_skip_kill) =
            regenerate_load_blocking(suspended_node.id).expect("load should succeed");
        assert_eq!(suspended_provider, "anthropic");
        assert!(suspended_skip_kill, "Suspended must skip the kill");
    }

    #[test]
    fn regenerate_load_blocking_rejects_spawning_status() {
        // The status guard from `validate_status_eligible` must surface
        // as an error so the command boundary can map it to the user's
        // "regenerate unavailable: node is in X state" toast.
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_regen_load_spawning",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node");
        db::update_agent_node_status(node.id, SessionStatus::Spawning)
            .expect("set spawning status");
        let err = regenerate_load_blocking(node.id).unwrap_err();
        match err {
            AgentNodeError::Status(msg) => assert!(
                msg.contains("spawning"),
                "error must name the rejected status, got: {msg}"
            ),
            other => panic!("expected AgentNodeError::Status, got {other:?}"),
        }
    }

    #[test]
    fn regenerate_apply_blocking_writes_provider_and_decides_resume() {
        // Same-harness provider swap with a captured cli_session_id:
        // apply_blocking must write the new provider AND report
        // `resume = true` (because `decide_resume` continues the
        // session for same-harness swaps).
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_regen_apply",
            "main",
            Some("claude:anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node");
        db::update_cli_session_id(node.id, "sess-abc")
            .expect("set cli_session_id");

        let resume = regenerate_apply_blocking(node.id, "claude:anthropic", "claude:minimax")
            .expect("apply should succeed");
        assert!(resume, "same-harness swap with a session id must resume");

        let reloaded = db::get_agent_node_by_id(node.id).expect("reload");
        assert_eq!(
            reloaded.provider, "claude:minimax",
            "apply_blocking must persist the new provider before returning"
        );
        assert_eq!(
            reloaded.cli_session_id.as_deref(),
            Some("sess-abc"),
            "apply_blocking must NOT clear cli_session_id — the spawner owns that policy",
        );
    }

    #[test]
    fn regenerate_apply_blocking_starts_fresh_on_cross_harness() {
        // Claude → Codex: the captured Claude session id is not a valid
        // Codex id, so the apply step must report `resume = false`.
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_regen_apply_cross",
            "main",
            Some("claude:anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node");
        db::update_cli_session_id(node.id, "claude-uuid")
            .expect("set cli_session_id");

        let resume = regenerate_apply_blocking(node.id, "claude:anthropic", "codex")
            .expect("apply should succeed");
        assert!(
            !resume,
            "cross-harness swap must report resume=false so spawn picks Fresh intent"
        );
    }

    #[test]
    fn regenerate_reload_blocking_returns_fresh_node() {
        // The reload helper is the final hop in the orchestrator — it
        // must return the current row so the command can hand it back
        // to the frontend store.
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            "/tmp/buildmesh_regen_reload",
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node");
        let reloaded = regenerate_reload_blocking(node.id).expect("reload should succeed");
        assert_eq!(reloaded.id, node.id);
        assert_eq!(reloaded.provider, "anthropic");
    }
}
