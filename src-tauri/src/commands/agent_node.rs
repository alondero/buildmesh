//! Agent Node management commands

use crate::db;
use crate::git::worktree::WorktreeCloseSafety;
use crate::models::{AgentNode, PendingWorktreeRemoval};
use crate::services;
use crate::worktree_blockers::BlockingProcess;
use serde::Serialize;
use tauri::{command, Emitter};
use ts_rs::TS;

/// Payload of the `worktree-cleanup-failed` Tauri event. Emitted by
/// [`crate::services::agent_node::process_pending_removals`] when a
/// background-drained worktree delete (issue #613 deferred removal) fails and
/// the blocker is new information — the row stays in `pending_worktree_removals`
/// and the UI opens a dialog offering Copy path / Copy diagnostics / Retry /
/// Keep worktree (issue #2139).
///
/// Carries everything the user needs to act: the node's identity, the full
/// worktree path, which removal step failed and why (the OS error), how many
/// attempts have been made, when the last one ran, and when the next automatic
/// retry may run. The pre-#2139 payload carried only the error string, which is
/// why the toast could hide both the path and the reason.
///
/// Generated to `src/types/generated/WorktreeCleanupFailedPayload.ts`; the
/// TS half is imported by `src/App.tsx`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "WorktreeCleanupFailedPayload.ts")]
pub struct WorktreeCleanupFailedPayload {
    pub node_name: String,
    pub worktree_path: String,
    pub error: String,
    /// Stable identifier of the removal step that failed (`git::worktree`
    /// operation vocabulary), e.g. `rename-worktree-to-staging`.
    pub operation: String,
    /// Failed attempts so far, including this one.
    #[ts(as = "i32")]
    pub attempt_count: i64,
    /// Epoch milliseconds of the attempt that just failed.
    #[ts(as = "i32")]
    pub last_attempt_at: i64,
    /// Epoch milliseconds before which the drain will not retry (the backoff).
    /// A user-initiated retry ignores it.
    #[ts(as = "i32")]
    pub retry_not_before: i64,
}

/// Create a new agent node
#[command]
pub async fn create_agent_node(
    mesh_id: i64,
    _name: String,
    #[allow(unused)] path: String,
    branch: String,
    provider: Option<String>,
    use_worktree: Option<bool>,
    configuration_id: Option<String>,
) -> Result<AgentNode, String> {
    // Tauri command surface has no PR-spawn plumbing; PR flows go via
    // `commands::pr::create_pr_node`. If we ever expose PR spawn here,
    // this is the call site to grow.
    //
    // Offload: create locks SQLite, touches the filesystem, and may run
    // git worktree operations (issue #1380).
    //
    // Issue #1658 step 5 — the mesh-lookup + branch + create dance now
    // flows through `services::agent_node::create_blocking`, the single
    // shared runner used by both this command and `http::routes::nodes::create`.
    // `path` is accepted on the IPC surface (deserialization contract)
    // but unused — the helper derives `mesh.path` itself from the mesh
    // row it loads. `branch_override = Some(&branch)` short-circuits the
    // helper's `get_default_branch_blocking` since the IPC caller
    // already supplied the branch. The legacy
    // `provider.unwrap_or("anthropic")` fallback (issue #538 default)
    // is preserved inside `create_with_source_pr_fork`.
    crate::commands::run_blocking("create_agent_node", move || {
        let configuration = crate::preferences::spawn_configurations::resolve_saved(
            provider.as_deref().unwrap_or("anthropic"),
            configuration_id.as_deref().filter(|s| !s.trim().is_empty()),
        )
        .map_err(|error| error.to_string())?;
        services::agent_node::create_blocking_configured(
            mesh_id,
            provider.as_deref(),
            Some(&branch),
            None, // source_issue
            None, // name_override — Tauri surface doesn't accept one
            use_worktree,
            false, // pending — Idle matches the prior create(...) semantics
            configuration.as_ref(),
        )
        .map_err(|e| {
            tracing::error!("create_agent_node failed: {}", e);
            e.to_string()
        })
    })
    .await
}

/// List all agent nodes
#[command]
pub async fn list_agent_nodes() -> Result<Vec<AgentNode>, String> {
    crate::commands::run_blocking("list_agent_nodes", || {
        db::list_agent_nodes().map_err(|e| e.to_string())
    })
    .await
}

/// Get agent node by ID
#[command]
pub async fn list_agent_history() -> Result<Vec<AgentNode>, String> {
    crate::commands::run_blocking("list_agent_history", || {
        db::list_agent_history().map_err(|error| error.to_string())
    })
    .await
}

/// Reopen archived work without starting a process or replacing its session.
#[command]
pub async fn reopen_agent_node(node_id: i64) -> Result<AgentNode, String> {
    crate::commands::run_blocking("reopen_agent_node", move || {
        db::reopen_agent_node(node_id).map_err(|error| error.to_string())
    })
    .await
}

/// Get agent node by ID
#[command]
pub async fn get_agent_node(node_id: i64) -> Result<AgentNode, String> {
    crate::commands::run_blocking("get_agent_node", move || {
        db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
    })
    .await
}

/// Delete an agent node permanently.
///
/// Returns as soon as the node is killed and removed from the database (Phase 1),
/// so the UI can drop it at once. The slow worktree-directory removal runs in a
/// background task that emits `worktree-cleanup-failed` if it can't finish — the
/// node is already gone either way (#243).
#[command]
pub async fn delete_agent_node(
    node_id: i64,
    remove_worktree: Option<bool>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    crate::commands::run_blocking("delete_agent_node", move || {
        services::agent_node::delete(node_id, remove_worktree.unwrap_or(false))
            .map_err(|e| e.to_string())
    })
    .await?;

    drain_pending_removals(app);
    Ok(())
}

/// Spawn the background worktree-removal drain, emitting `worktree-cleanup-failed`
/// for any removal that couldn't complete. Shared by close and startup reconcile.
///
/// Issue #2139: one event per *unreported* blocker, not per failed attempt. The
/// drain persists each failure (operation, error, attempts, backoff) and only
/// asks for an event when the user hasn't already been told about that exact
/// blocker, so a worktree that stays blocked doesn't raise a toast on every
/// drain. The event now carries the evidence the frontend needs to act.
pub fn drain_pending_removals(app: tauri::AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        for blocked in services::agent_node::process_pending_removals() {
            if !blocked.notify {
                continue;
            }
            let _ = app.emit(
                "worktree-cleanup-failed",
                WorktreeCleanupFailedPayload {
                    node_name: blocked.removal.node_name,
                    worktree_path: blocked.removal.worktree_path,
                    error: blocked.removal.last_error.unwrap_or_default(),
                    operation: blocked.removal.last_operation.unwrap_or_default(),
                    attempt_count: blocked.removal.attempt_count,
                    last_attempt_at: blocked.removal.last_attempt_at,
                    retry_not_before: blocked.removal.retry_not_before,
                },
            );
        }
    });
}

/// Every worktree cleanup that is still blocked, with its persisted evidence.
/// The blocked-cleanup dialog lists these and acts on them, so what the user
/// sees is exactly what the drain knows (issue #2139).
///
/// A failure to read the queue is an error rather than an empty list: the
/// dialog must stay up and say so, not close and imply nothing is blocked
/// (issue #2139 review round 2).
#[command]
pub async fn list_pending_worktree_removals() -> Result<Vec<PendingWorktreeRemoval>, String> {
    crate::commands::run_blocking("list_pending_worktree_removals", || {
        services::agent_node::list_blocked_worktree_cleanups().map_err(|e| {
            tracing::error!("could not read pending worktree removals: {}", e);
            e.to_string()
        })
    })
    .await
}

/// What a user-initiated worktree-cleanup retry actually did. `Removed` is the
/// only outcome that means the worktree was deleted; `Gone`, `Claimed`,
/// `QueueReadFailed` all mean it was not, and the frontend must not tell the
/// user it was (issue #2139 review round 1).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "WorktreeCleanupRetry.ts")]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeCleanupRetry {
    /// The directory and its git bookkeeping are gone; the queue entry is
    /// dequeued.
    Removed,
    /// Still blocked. `record` carries the updated evidence row.
    StillBlocked,
    /// The queue entry is no longer there — already cleaned or dismissed.
    Gone,
    /// The warm pool has adopted the path for a live spawn, so it was left
    /// alone and the tombstone was dequeued.
    Claimed,
    /// The queue row or the claim guard could not be read. Nothing was
    /// attempted; the row is untouched.
    QueueReadFailed,
}

/// The reply to `retry_worktree_cleanup`: what happened, plus the updated row
/// when the cleanup is still blocked.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "WorktreeCleanupRetryResult.ts")]
pub struct WorktreeCleanupRetryResult {
    pub status: WorktreeCleanupRetry,
    /// Present only for `StillBlocked`.
    pub record: Option<PendingWorktreeRemoval>,
}

/// Retry one blocked worktree cleanup immediately, ignoring its backoff.
#[command]
pub async fn retry_worktree_cleanup(
    worktree_path: String,
) -> Result<WorktreeCleanupRetryResult, String> {
    crate::commands::run_blocking("retry_worktree_cleanup", move || {
        let (status, record) =
            match services::agent_node::retry_pending_worktree_removal(&worktree_path) {
                services::agent_node::CleanupRetryOutcome::Removed => {
                    (WorktreeCleanupRetry::Removed, None)
                }
                services::agent_node::CleanupRetryOutcome::StillBlocked(record) => {
                    (WorktreeCleanupRetry::StillBlocked, Some(record))
                }
                services::agent_node::CleanupRetryOutcome::Gone => {
                    (WorktreeCleanupRetry::Gone, None)
                }
                services::agent_node::CleanupRetryOutcome::ClaimedByLiveSpawn => {
                    (WorktreeCleanupRetry::Claimed, None)
                }
                services::agent_node::CleanupRetryOutcome::QueueReadFailed => {
                    (WorktreeCleanupRetry::QueueReadFailed, None)
                }
            };
        Ok(WorktreeCleanupRetryResult { status, record })
    })
    .await
}

/// Diagnose which processes are holding a worktree directory, so a blocked
/// cleanup can name what to close instead of leaving the user to guess.
///
/// Read-only, and deliberately approximate: it reports every process whose
/// executable or working directory lies inside the tree. The removal error
/// itself says which permission failed; this says *who* is likely responsible.
/// Nothing is terminated here (issue #2139).
#[command]
pub async fn diagnose_worktree_cleanup_blockers(
    worktree_path: String,
) -> Result<Vec<BlockingProcess>, String> {
    crate::commands::run_blocking("diagnose_worktree_cleanup_blockers", move || {
        Ok(crate::worktree_blockers::diagnose_blocking_processes(
            &worktree_path,
        ))
    })
    .await
}

/// Explicitly terminate one process that the diagnosis named.
///
/// Never reached automatically: the only caller is a user clicking "End process"
/// on a row the diagnosis produced. The pid is re-checked here, against a fresh
/// diagnosis of the same worktree, before anything is terminated — process IDs
/// are reused on Windows, so the pid a user saw a minute ago may now belong to
/// an unrelated program. Ending that one, and (through `taskkill /T`) its child
/// processes, would be silent data loss. Passing the worktree path rather than
/// the pid alone is what makes that check possible (issue #2139 review round 1).
#[command]
pub async fn release_worktree_cleanup_blocker(
    worktree_path: String,
    pid: u32,
) -> Result<(), String> {
    crate::commands::run_blocking("release_worktree_cleanup_blocker", move || {
        // Re-diagnoses internally and refuses a pid that is not a current
        // blocker of this worktree, so a reused id cannot be terminated.
        crate::worktree_blockers::release_blocker(&worktree_path, pid)
    })
    .await
}

/// The reply to `dismiss_worktree_cleanup`: what the user should be told, and
/// whether the cleanup intent was actually cancelled. `cancelled` is false when
/// the staged copy could not be moved back — the queue row is still there, the
/// drain keeps trying, and the entry must stay in the dialog (issue #2139
/// review round 2).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "WorktreeCleanupDismissalResult.ts")]
pub struct WorktreeCleanupDismissalResult {
    pub message: String,
    pub cancelled: bool,
}

/// "Keep worktree" — cancel the cleanup intent for one path, and report what
/// was actually done to the disk (issue #2139 review round 1): cancelling the
/// intent is safe, but a removal may already have moved the worktree aside, so
/// the reply carries the message the user should be told and whether the queue
/// entry is gone.
#[command]
pub async fn dismiss_worktree_cleanup(
    worktree_path: String,
) -> Result<WorktreeCleanupDismissalResult, String> {
    crate::commands::run_blocking("dismiss_worktree_cleanup", move || {
        let dismissed = services::agent_node::dismiss_pending_worktree_removal(&worktree_path);
        let mut message = dismissed.kind.message(&worktree_path);
        if let Some(error) = dismissed.dequeue_error {
            // The row is still queued, which is what actually happened, so say
            // so rather than implying the cleanup was forgotten.
            message.push_str(&format!(" The cleanup stays queued: {error}."));
        }
        Ok(WorktreeCleanupDismissalResult {
            message,
            cancelled: dismissed.cancelled,
        })
    })
    .await
}

/// Persist new grid positions for a batch of agent nodes (drag-to-reorder).
/// The frontend sends the full new ordering for the affected mesh so the DB
/// stays in sync with its optimistic update. Mirrors `update_mesh_positions`.
#[command]
pub async fn update_agent_node_positions(updates: Vec<(i64, i64)>) -> Result<(), String> {
    crate::commands::run_blocking("update_agent_node_positions", move || {
        db::update_agent_node_positions_batch(&updates).map_err(|e| e.to_string())
    })
    .await
}

/// Check whether the node's worktree can be removed safely on close.
#[command]
pub async fn get_worktree_close_safety(node_id: i64) -> Result<WorktreeCloseSafety, String> {
    // Offload: the safety check runs a full `git status` walk plus an
    // ahead/behind graph walk on the node's worktree (`worktree::close_safety`)
    // — seconds on a large repo, and the frontend awaits it on every node
    // close while showing a spinner. Running it inline parked a Tauri async
    // worker for the duration (the Command Threading anti-pattern).
    crate::commands::run_blocking("get_worktree_close_safety", move || {
        services::agent_node::get_worktree_close_safety(node_id).map_err(|e| e.to_string())
    })
    .await
}

/// Trim and validate a user-supplied rename. Returns the canonical (trimmed)
/// form on success, or an error message on rejection. Pulled out of the
/// `#[command]` body so it can be unit-tested without Tauri.
pub fn validate_rename_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name cannot be empty".to_string());
    }
    if trimmed.chars().count() > 80 {
        return Err("name too long (max 80 chars)".to_string());
    }
    Ok(trimmed.to_string())
}

/// Manually rename an agent node, overriding the auto-LLM renamer.
///
/// The user's name is "sticky": `is_default_name` returns false for it, so
/// `should_trigger_rename` short-circuits on every subsequent turn. We also
/// tear down the in-memory rename state via `session_naming::cleanup` so
/// `SESSION_BUFFERS` doesn't keep growing for a node that no longer needs
/// a rename. Emits the same `node-renamed` event as the LLM path so the
/// frontend store and any other listeners stay in sync.
#[command]
pub async fn rename_agent_node(
    node_id: i64,
    name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let trimmed = validate_rename_name(&name)?;

    // Drop any in-flight rename state FIRST so the LLM's eventual commit
    // hits our race guard (which re-reads the node's name from the DB).
    crate::session_naming::cleanup(node_id);

    let name_for_emit = trimmed.clone();
    crate::commands::run_blocking("rename_agent_node", move || {
        db::update_agent_node_name(node_id, &trimmed).map_err(|e| e.to_string())
    })
    .await?;

    let _ = app.emit(
        "node-renamed",
        crate::session_naming::NodeRenamedPayload {
            node_id,
            name: name_for_emit,
        },
    );
    Ok(())
}

/// Set an agent node's `is_pinned` flag explicitly (wayfinder #982 /
/// ticket #984). Used by the UI affordance (ticket #985) when the user
/// wants a known-good state (e.g. "Pin this node" in a context menu) —
/// distinguishes from `toggle_node_pinned`, which flips whatever the
/// current value is. Returns the post-write `AgentNode` so the frontend
/// store can patch the local entry directly without a follow-up
/// `get_agent_node_by_id` round-trip. Surfaces "node not found" as an
/// error string rather than silently no-op'ing — matches the
/// `set_agent_node_provider` and `update_mesh_layout` zero-rows contract.
#[command]
pub async fn set_node_pinned(node_id: i64, pinned: bool) -> Result<AgentNode, String> {
    crate::commands::run_blocking("set_node_pinned", move || {
        let updated = db::set_agent_node_pinned(node_id, pinned).map_err(|e| e.to_string())?;
        if updated == 0 {
            return Err(format!("set_node_pinned: node {node_id} not found"));
        }
        db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
    })
    .await
}

/// Flip an agent node's `is_pinned` flag and return the new state
/// (wayfinder #982 / ticket #984). The single-action shape the UI's
/// click-to-pin button uses — the user doesn't need to know the current
/// pinned value, just "toggle". Returns the post-write `AgentNode` so the
/// frontend store can patch the local entry; surfaces "node not found" as
/// an error string (same contract as `set_node_pinned`).
#[command]
pub async fn toggle_node_pinned(node_id: i64) -> Result<AgentNode, String> {
    crate::commands::run_blocking("toggle_node_pinned", move || {
        db::toggle_agent_node_pinned(node_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("toggle_node_pinned: node {node_id} not found"))?;
        db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
    })
    .await
}

/// Swap an agent node's Model Provider (issue #774 / #775). The
/// worktree, branch, name, position, and all other state are preserved;
/// the new agent resumes from the existing `cli_session_id` when both
/// providers share the same Agent Harness and the new adapter supports
/// resume, else starts fresh. Cross-harness swaps are allowed (the user
/// may pick any Spawn Option) but always start fresh — the existing
/// `cli_session_id` is bound to the old harness's session format.
///
/// Frontend trigger: right-click context menu on `NodeItem` (see
/// ticket #776). UI flow: confirmation dialog for running nodes
/// (ticket #778). Returns the updated `AgentNode` on success so the
/// caller's store can patch the local entry without a refetch; on
/// failure the caller gets an `Err` (the `provider` column has
/// already been updated at that point — the user can retry, and the
/// local store stays on the old provider). The existing
/// `agent-spawned` event from `spawn_agent_inner` drives PTY /
/// resize sync on the frontend.
///
/// # Architecture (issue #1380 review round-2 feedback 1)
///
/// The three sync helpers (`regenerate_load_blocking`,
/// `regenerate_apply_blocking`, `regenerate_reload_blocking`) and the
/// two unavoidable async hops (`kill_agent`, `spawn_with_intent`) live
/// inline in this command — not behind a service-level async
/// orchestrator — so the offload boundary (each
/// `crate::commands::run_blocking` call) is explicit at the command
/// boundary. The previous `services::agent_node::regenerate` async
/// wrapper called the helpers directly on the Tokio runtime,
/// defeating the whole point of the refactor; round-2 review caught
/// the regression.
#[command]
pub async fn regenerate_agent_node(
    node_id: i64,
    new_provider_id: String,
    app: tauri::AppHandle,
) -> Result<crate::models::AgentNode, String> {
    use crate::agent::spawn::{
        spawn_with_intent, ResumeCause, SpawnIntent, SpawnRequest, TerminalSize,
    };
    use crate::services::agent_node::{
        regenerate_apply_blocking, regenerate_load_blocking, regenerate_reload_blocking,
    };

    // 1–2. Load + validate off-thread. `regenerate_load_blocking` is
    // pure sync; the command boundary wraps it. Returns owned data —
    // no pre-clones needed beyond the `i64` (Copy). The closure maps
    // the inner `AgentNodeError` to a `String` so `run_blocking`'s
    // `Result<T, String>` signature matches; `T` is inferred as the
    // helper's return tuple, so `.await?` unwraps once.
    let (old_provider, skip_kill) =
        crate::commands::run_blocking("regenerate_agent_node_load", move || {
            regenerate_load_blocking(node_id).map_err(|e| e.to_string())
        })
        .await?;

    // 3. Kill the live process ONLY when one is registered. See
    // `services::agent_node::regenerate` (the removed orchestrator)
    // for the full rationale — `should_skip_kill_for_regenerate`
    // keeps the Suspended case safe from `kill_agent_blocking`'s
    // unconditional `on_idle` tail.
    if !skip_kill {
        let _ = crate::agent::process::kill_agent(node_id).await;
    }

    // 4–6. Update provider, reload, decide resume off-thread. The
    // spawn pipeline reads `node.provider` for backend env resolution
    // and preflight (spawn.rs:1399), so the write must land BEFORE
    // `spawn_with_intent`.
    let new_provider_for_apply = new_provider_id;
    let resume = crate::commands::run_blocking("regenerate_agent_node_apply", move || {
        regenerate_apply_blocking(node_id, &old_provider, &new_provider_for_apply)
            .map_err(|e| e.to_string())
    })
    .await?;

    let intent = if resume {
        SpawnIntent::Resume {
            cause: ResumeCause::Explicit,
        }
    } else {
        SpawnIntent::Fresh
    };

    let spawn_request =
        SpawnRequest::new(node_id, intent, TerminalSize::default()).with_lifecycle_lease();
    spawn_with_intent(&app, spawn_request)
        .await
        .map_err(|e| e.to_string())?;

    // 7. Final reload off-thread — returns the post-spawn row state
    // (the spawn pipeline may have updated `cli_session_id` /
    // `status_changed_at`).
    crate::commands::run_blocking("regenerate_agent_node_reload", move || {
        regenerate_reload_blocking(node_id).map_err(|e| e.to_string())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::validate_rename_name;

    #[test]
    fn validate_accepts_trimmed_name() {
        assert_eq!(
            validate_rename_name("Fix OAuth callback").unwrap(),
            "Fix OAuth callback"
        );
    }

    #[test]
    fn validate_strips_surrounding_whitespace() {
        assert_eq!(validate_rename_name("   spaced   ").unwrap(), "spaced");
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(validate_rename_name("").is_err());
        assert!(validate_rename_name("    ").is_err());
        assert!(validate_rename_name("\t\n").is_err());
    }

    #[test]
    fn validate_rejects_over_80_chars() {
        let long = "x".repeat(81);
        let err = validate_rename_name(&long).unwrap_err();
        assert!(err.contains("too long"), "unexpected error: {}", err);
    }

    #[test]
    fn validate_accepts_exactly_80_chars() {
        let eighty = "x".repeat(80);
        assert_eq!(validate_rename_name(&eighty).unwrap().len(), 80);
    }
}
