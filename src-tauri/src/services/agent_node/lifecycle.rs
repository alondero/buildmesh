//! Close/delete and regenerate transitions for an existing Agent Node.

use super::*;
/// Load a node row for the close path, treating an already-removed row as
/// `None` rather than an error. Closing is idempotent: a node deleted out from
/// under the UI — a Circuit step's `CloseAgentNode`, or any other backend-driven
/// delete — must not fault with "Query returned no rows". Without this, the
/// frontend's Phase-1 safety check aborts the close and leaves the ghost row on
/// screen (issue: closing a review node's stale tab).
pub(super) fn find_agent_node_for_close(
    session_id: i64,
) -> Result<Option<AgentNode>, AgentNodeError> {
    match db::get_agent_node_by_id(session_id) {
        Ok(node) => Ok(Some(node)),
        // Legitimate for an already-retired row, but a bogus/negative id lands
        // here too — log so a genuinely bad close is diagnosable rather than
        // silently reported as a clean success.
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            tracing::warn!(
                "close for agent node {session_id}: row already removed, treating as a no-op"
            );
            Ok(None)
        }
        Err(error) => Err(AgentNodeError::Db(error)),
    }
}

/// Return whether closing the node can remove its worktree without risking work.
pub fn get_worktree_close_safety(session_id: i64) -> Result<WorktreeCloseSafety, AgentNodeError> {
    let Some(node) = find_agent_node_for_close(session_id)? else {
        // Already gone — there is no worktree of ours left to protect.
        return Ok(WorktreeCloseSafety {
            worktree_path: None,
            has_uncommitted: false,
            has_unpushed: false,
            is_detached: false,
        });
    };
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
    let node = find_agent_node_for_close(session_id)?;

    let removal_path = if remove_worktree {
        crate::agent::process::PROCESS_REGISTRY.kill_session(session_id);
        node.as_ref()
            .and_then(env::node_worktree_path)
            .map(|r| r.host_path)
    } else {
        None
    };

    crate::session_naming::cleanup(session_id);
    // HookState is keyed by node id and lives for the process lifetime. A
    // deleted node must release its entry even when it had no live PTY (the
    // normal `notify_process_terminated` path is otherwise never reached),
    // or every create/delete cycle permanently grows the global map.
    //
    // Only the process-scoped stores are released here. The PTY fanout channel
    // is retired *after* the row delete commits, further down: a tombstone
    // fences channel creation, so retiring a node whose row survived a failed
    // delete would strand a live node with no way to get a channel back
    // (#2019).
    crate::agent::node_teardown::release(session_id);
    // Drop autopilot state too. The ledger delete is explicit as a
    // defensive belt: the table declares ON DELETE CASCADE and the
    // bundled SQLite build (rusqlite 0.32 / SQLite 3.46.0) has FK
    // enforcement on by default, so the parent agent_nodes DELETE
    // below already cascades — but a future system-libsqlite link
    // would have FK off by default and need this explicit DELETE to
    // avoid an orphan. Removing the row also frees the issue for a
    // poller retry if it is still open + labelled.
    crate::circuit::evaluator::unregister(session_id);
    db::legacy_retirement::delete_history(session_id)?;

    // Only the row delete (and its atomic worktree enqueue) is skipped when the
    // row is already gone — every per-node cleanup above still runs, so a
    // backend-driven delete that won the race leaves no process, naming, or
    // telemetry state behind.
    if let Some(node) = node.as_ref() {
        let removal = removal_path.as_deref().map(|p| (p, node.name.as_str()));
        db::delete_agent_node_enqueueing_removal(session_id, removal)?;
        // The row is gone, so this is the first moment the node is *permanently*
        // deleted and its PTY fanout channel can be retired (issue #2019). After
        // the commit, not before: a tombstone refuses channel creation, so
        // retiring while the row could still survive an error above would leave
        // a live node that can never stream again.
        let app_data_dir = crate::preferences::app_data_dir();
        crate::agent::node_teardown::release_after_delete(session_id, app_data_dir.as_deref());
        // Announce the deletion so a frontend that did not initiate it — the
        // Circuit worker's `CloseAgentNode`, cancelled-run retirement, abort
        // compensation — drops the card and disposes its terminal. A
        // user-initiated close has already removed the row optimistically and
        // disposes on its own success path, so its listener call is a no-op.
        if let Some(app) = crate::http::app_handle() {
            let _ = tauri::Emitter::emit(
                app,
                "node-deleted",
                crate::commands::agent::NodeDeletedPayload {
                    node_id: session_id,
                },
            );
        }
    }
    // The raw-output subscription is node-scoped, so process exit/restart
    // deliberately preserves it. Once the row deletion commits, this is the
    // authoritative backend cleanup seam (idempotent with frontend disposal).
    // Both Agent and Build/Run maps key by node id, so a single helper
    // releases them in lock-step -- reaching into the globals directly would
    // risk half-cleaning a node that exists on both surfaces.
    crate::pty::sink::unregister_node_sinks(session_id);
    Ok(())
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
        | SessionStatus::Completed
        | SessionStatus::Ready => Ok(()),
        _ => Err(format!(
            "regenerate unavailable: node is in {} state (must be idle, awaiting_input, error, running, suspended, completed, or ready)",
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
    // Parse both providers through the typed `SpawnOptionId` once each
    // (issue #1659 item 1) and compare the harness halves. The
    // provider half is irrelevant here — resume lives on the executor
    // keyed by `old_harness == new_harness`. Route the executor lookup
    // through the typed entry point so `new_provider` is parsed exactly
    // once on this path (review #1730 fix: previously
    // `resolve_harness_provider(&str)` re-parsed the id internally).
    let old_id = SpawnOptionId::from(old_provider);
    let new_id = SpawnOptionId::from(new_provider);
    let same_harness = old_id.harness_id() == new_id.harness_id();
    let new_provider_enum = resolve_harness_provider_for(&new_id);
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
pub fn regenerate_load_blocking(node_id: i64) -> Result<(String, bool), AgentNodeError> {
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
    let node = db::get_agent_node_by_id(node_id)?;
    let prefs = crate::preferences::load().map_err(AgentNodeError::Backend)?;
    let mut plan = crate::preferences::launch_configurations::resolve(
        &prefs,
        new_provider,
        &Default::default(),
    )
    .map_err(AgentNodeError::InvalidConfiguration)?;
    let resume = decide_resume(
        old_provider,
        &plan.spawn_option_id,
        node.cli_session_id.as_deref(),
    )
    .is_some();
    // A resumed session lives in the home of the Harness Login that started
    // it, so a native resume keeps that login rather than the new plan's.
    if resume && plan.route.is_none() {
        let previous = db::node_spawn_configuration(node_id, old_provider)
            .map_err(|error| AgentNodeError::Backend(error.to_string()))?
            .and_then(|value| value.resolved)
            .and_then(|plan| plan.harness_login);
        if previous.is_some() {
            plan.harness_login = previous;
        }
    }
    let runtime = plan.harness.runtime.unwrap_or_else(|| {
        crate::env::resolve_raw_path(&crate::env::node_working_path(&node).raw_path).env_type
    });
    db::replace_node_launch(
        node_id,
        &crate::preferences::launch_configurations::snapshot(plan),
        runtime,
    )
    .map_err(|error| AgentNodeError::Backend(error.to_string()))?;
    Ok(resume)
}

/// Sync helper for step 8 of [`regenerate`]: reload the node row
/// after the spawn lands so the command returns the post-spawn
/// state. The spawn pipeline may have updated `cli_session_id`,
/// `status_changed_at`, etc.
pub fn regenerate_reload_blocking(node_id: i64) -> Result<AgentNode, AgentNodeError> {
    db::get_agent_node_by_id(node_id).map_err(AgentNodeError::from)
}
