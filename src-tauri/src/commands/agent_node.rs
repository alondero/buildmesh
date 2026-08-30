//! Agent Node management commands

use crate::db;
use crate::models::AgentNode;
use crate::services;
use crate::git::worktree::WorktreeCloseSafety;
use serde::Serialize;
use tauri::{command, Emitter};
use ts_rs::TS;

/// Payload of the `worktree-cleanup-failed` Tauri event. Emitted by
/// [`crate::services::agent_node::process_pending_removals`] when the
/// background-drained worktree delete (issue #613 deferred removal) fails —
/// the row stays in `pending_worktree_removals` and the user is told via a
/// toast that it'll be retried on next launch.
///
/// Generated to `src/types/generated/WorktreeCleanupFailedPayload.ts`; the
/// TS half is imported by `src/App.tsx`. Three fields because the toast
/// surfaces the node name (the user-facing identity) and the worktree path
/// (the on-disk artifact) and the error reason (so support can copy/paste).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "WorktreeCleanupFailedPayload.ts")]
pub struct WorktreeCleanupFailedPayload {
    pub node_name: String,
    pub worktree_path: String,
    pub error: String,
}

/// Create a new agent node
#[command]
pub async fn create_agent_node(
    mesh_id: i64,
    _name: String,
    path: String,
    branch: String,
    provider: Option<String>,
    use_worktree: Option<bool>,
) -> Result<AgentNode, String> {
    // Tauri command surface has no PR-spawn plumbing; PR flows go via
    // `commands::pr::create_pr_node`. If we ever expose PR spawn here,
    // this is the call site to grow.
    // Offload: create locks SQLite, touches the filesystem, and may run
    // git worktree operations (issue #1380).
    crate::commands::run_blocking("create_agent_node", move || {
        services::agent_node::create(
            mesh_id,
            &path,
            &branch,
            provider.as_deref(),
            None, // source_issue
            None, // source_pr — non-PR spawn (issue #450)
            None, // source_pr_pinned_sha — non-PR spawn (issue #444)
            use_worktree,
            None, // name_override — Tauri surface doesn't accept one
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
pub fn drain_pending_removals(app: tauri::AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        for (removal, error) in services::agent_node::process_pending_removals() {
            let _ = app.emit(
                "worktree-cleanup-failed",
                WorktreeCleanupFailedPayload {
                    node_name: removal.node_name,
                    worktree_path: removal.worktree_path,
                    error,
                },
            );
        }
    });
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
pub async fn set_node_pinned(
    node_id: i64,
    pinned: bool,
) -> Result<AgentNode, String> {
    crate::commands::run_blocking("set_node_pinned", move || {
        let updated = db::set_agent_node_pinned(node_id, pinned)
            .map_err(|e| e.to_string())?;
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
    let (old_provider, skip_kill) = crate::commands::run_blocking(
        "regenerate_agent_node_load",
        move || regenerate_load_blocking(node_id).map_err(|e| e.to_string()),
    )
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
    let resume = crate::commands::run_blocking(
        "regenerate_agent_node_apply",
        move || {
            regenerate_apply_blocking(node_id, &old_provider, &new_provider_for_apply)
                .map_err(|e| e.to_string())
        },
    )
    .await?;

    let intent = if resume {
        SpawnIntent::Resume {
            cause: ResumeCause::Explicit,
        }
    } else {
        SpawnIntent::Fresh
    };

    spawn_with_intent(
        &app,
        SpawnRequest::new(node_id, intent, TerminalSize::default()),
    )
    .await
    .map_err(|e| e.to_string())?;

    // 7. Final reload off-thread — returns the post-spawn row state
    // (the spawn pipeline may have updated `cli_session_id` /
    // `status_changed_at`).
    Ok(crate::commands::run_blocking(
        "regenerate_agent_node_reload",
        move || regenerate_reload_blocking(node_id).map_err(|e| e.to_string()),
    )
    .await?)
}

#[cfg(test)]
mod tests {
    use super::validate_rename_name;

    #[test]
    fn validate_accepts_trimmed_name() {
        assert_eq!(validate_rename_name("Fix OAuth callback").unwrap(), "Fix OAuth callback");
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
