//! Loopback attention webhook: node/session ownership, HTTP security and one lifecycle fan-out.
//! Harness wire validation and mapping live in `attention::normalizers`.
mod common;
#[cfg(test)]
mod isolation_tests;
mod normalizers;
mod ordering;
#[cfg(test)]
mod tests;

use crate::agent::session_lifecycle::{SemanticTurnKind, SemanticTurnPayload};
use crate::http::{response::Response, router::ParsedRequest, state};
use common::Decision;
use ordering::{accept_hook, apply_hook_after_turn_fence, lifecycle_decision};

/// Bound local hook envelopes before buffering their JSON.
pub(crate) const MAX_HOOK_BODY: usize = 64 * 1024;

/// Pure-function verifier for the attention-route token gate
/// (issue #1366 round-2 + round-3). Extracted from `handle_post`
/// so the comparator semantics can be tested in isolation
/// without spinning up a Tokio listener or a real SQLite handle.
///
/// Issue #1661 step 9: per-harness verification lives on the
/// `TranscriptAdapter::verify_attention_token` seam. Every adapter
/// other than Grok's accepts every callback (matches the pre-#1366
/// behaviour: non-Grok harnesses bind the loopback peer and don't need
/// a minted token); Grok's adapter implements the strict minted-
/// token comparison.
fn verify_attention_token(
    provider: &str,
    query_string: Option<&str>,
    minted: Option<&str>,
) -> bool {
    let adapter = crate::services::transcript_reader::adapter::dispatch(provider)
        .unwrap_or_else(crate::services::transcript_reader::adapter::default_adapter);
    adapter.verify_attention_token(query_string, minted)
}

pub async fn handle_post(req: &ParsedRequest) -> Response {
    // Loopback-only: the Claude Code hook always posts from 127.0.0.1/::1. A
    // non-loopback peer is an external spoof attempt — refuse before doing work.
    if !req.peer.ip().is_loopback() {
        return Response::empty("403 Forbidden");
    }

    // Parse the session id early — the token gate below needs it to
    // look up the session's provider and decide whether to enforce
    // the hook token. Bad path → 400 before any other work.
    let session_id: Option<i64> = req
        .path
        .strip_prefix("/api/attention/")
        .and_then(|s| s.parse().ok());
    if session_id.is_none() && req.path != "/api/attention/mcode" {
        return Response::empty("400 Bad Request");
    }
    let hook_body = req.body.clone();

    // Runtime-scoped token gate (issue #1366, round-2 + round-3 +
    // N1 fixes). The decision is **per-provider**, not per-`?token=`
    // presence:
    //
    //   provider = grok  → require matching ?token=<minted> against the
    //                      runtime-scoped `RUNTIME_HOOK_TOKEN` OnceLock.
    //   provider ∈ {claude, codex, agy, …} → no token check.
    //
    // Looked up by **session id** (the trusted path component). The
    // session row is the canonical record of which harness is
    // calling. The DB lookup is wrapped in `spawn_blocking` so
    // the synchronous SQLite read lock does not stall the Tokio
    // worker (round-3 review point 1). The `(cli_session_id, provider)`
    // pair is captured here and passed into the run_blocking closure
    // below — see the N1 review point about hitting SQLite twice
    // for the same row.
    let (node, provider_owned, hook_generation, raw_payload) =
        tokio::task::spawn_blocking(move || {
            let addressed = session_id.and_then(|id| crate::db::get_agent_node_by_id(id).ok());
            let value = serde_json::from_slice::<serde_json::Value>(&hook_body).ok();
            // Old shared manifests may still name the last-spawned node.
            // Their numeric address cannot establish MiniMax ownership either.
            let node = normalizers::resolve_node(
                addressed,
                value.as_ref(),
                session_id.is_none(),
                crate::services::mcode_session::hook_target,
            );
            let provider = node
                .as_ref()
                .map(|node| normalizers::provider_for(&node.provider))
                .unwrap_or_default();
            let generation = node
                .as_ref()
                .and_then(|node| crate::db::session_started_at_ms(node.id).ok().flatten());
            (node, provider, generation, value)
        })
        .await
        .ok()
        .unwrap_or_default();
    let stored_cli_session_id: String = node
        .as_ref()
        .and_then(|n| n.cli_session_id.clone())
        .unwrap_or_default();
    let provider = provider_owned.as_str();
    if !verify_attention_token(
        provider,
        req.query(),
        crate::agent::runtime_hook_token().as_deref(),
    ) {
        return Response::empty("403 Forbidden");
    }
    let Some(app) = state::app_handle() else {
        return Response::empty("503 Service Unavailable");
    };
    let Some(session_id) = node.as_ref().map(|node| node.id) else {
        return Response::empty("404 Not Found");
    };

    // The path id is untrusted input. Do not create a process-lifetime
    // HookState entry or attempt a lifecycle publish for a node that has
    // already been deleted (or never existed); both would turn a typo/flood
    // of unknown ids into an unbounded global-map leak. Existing nodes proceed
    // through the normal provider/session fences below.
    if node
        .as_ref()
        .is_none_or(|node| node.status == crate::models::SessionStatus::Archived)
    {
        return Response::empty("404 Not Found");
    }

    // Normalize only after the HTTP gates. Transcript reconciliation is file I/O,
    // so it runs on the blocking pool, with no database connection held.
    let normalize_provider = provider_owned.clone();
    let normalizers::Normalized { payload: hook_payload, classified, semantic, session_id: hook_uuid, native_hook } =
        tokio::task::spawn_blocking(move || normalizers::normalize(raw_payload.as_ref(), &normalize_provider,
            crate::services::transcript_reader::adapters::claude_code::count_pending_background_tasks))
        .await.unwrap_or_default();
    let mut detail = classified.detail.clone();
    detail.semantic_turn = semantic
        .filter(|turn| {
            detail.kind != Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested)
                && (classified.decision == Decision::Ready
                    || turn.kind != SemanticTurnKind::TurnFinished)
        })
        .map(|turn| SemanticTurnPayload {
            node_id: session_id,
            kind: turn.kind,
            description: turn.description,
        });

    // Issue #1389 — every step below is blocking SQLite; one `spawn_blocking`
    // hop for the whole sequence. `app` is `&'static AppHandle` (returned by
    // `state::app_handle()`), which is what lets `move ||` capture it.
    // N1 fix: the row we fetched for the token gate above is also the
    // row we'll persist + check ordering-token against — share it via
    // `move ||` capture rather than re-querying SQLite. Use clear
    // shadows so the outer `Option<String>` is gone before the
    // closure constructed (avoids the `move ||` capture error
    // for `Option<String>`, which doesn't implement Copy).
    let stored_cli_session_id_owned = stored_cli_session_id;
    let classified_decision = classified.decision;
    let applied = crate::commands::run_blocking(
        "http_attention_apply",
        move || -> Result<Applied, String> {
            // Hold the per-node owner through acceptance and effects so a
            // new prompt cannot overtake a Stop that passed its turn fence.
            let state_owner = crate::agent::hook_state::for_node(session_id);
            let mut state = state_owner.lock();
            if provider_owned == "mcode"
                && (hook_generation.is_none()
                    || hook_generation
                        != crate::db::session_started_at_ms(session_id).ok().flatten())
            {
                return Ok(Applied::StaleDropped);
            }
            // Codex self-assigns its thread id. PTY capture remains the
            // earliest source; SessionStart is the structured capture at
            // boot, and Stop/PermissionRequest remain the later fallback
            // (issue #1089).
            if let Some(cli_session_id) = hook_uuid.clone() {
                let captured = if provider_owned == "mcode" {
                    crate::db::recover_live_cli_session_id(
                        node.as_ref().expect("resolved hook node"),
                        &cli_session_id,
                        hook_generation.expect("fenced hook generation"),
                    )
                } else {
                    crate::db::set_cli_session_id_if_missing(session_id, &cli_session_id)
                };
                match captured {
                    Ok(true) => tracing::info!(
                        "attention webhook captured session ID {} for node {}",
                        cli_session_id,
                        session_id
                    ),
                    Ok(false) => {}
                    Err(error) => tracing::warn!(
                        "attention webhook could not persist session ID for node {}: {}",
                        session_id,
                        error
                    ),
                }
            }
            // Re-read after conditional capture. A callback racing another
            // identity write must not publish lifecycle for the losing session.
            if provider_owned == "mcode"
                && crate::db::get_agent_node_by_id(session_id)
                    .ok()
                    .and_then(|node| node.cli_session_id)
                    != hook_uuid
            {
                return Ok(Applied::StaleDropped);
            }

            // Issue #1364 §1 — ordering token: a hook whose provider session
            // id is a valid UUID that differs from the node's stored one
            // belongs to a previous process generation. It must never
            // overwrite the newer state — the POST is answered 200 (the
            // harness's fail-open contract) and dropped. N1 fix: this
            // uses `stored_cli_session_id_owned` (captured above; one
            // DB hit, not two).
            if !stored_cli_session_id_owned.is_empty() {
                if let Some(hook) = hook_uuid.as_deref() {
                    if hook != stored_cli_session_id_owned {
                        tracing::info!(
                            "attention webhook for node {}: stale callback from a previous \
                             process (hook session {hook} != active {stored_cli_session_id_owned}) \
                             — dropped (issue #1364 ordering token)",
                            session_id
                        );
                        return Ok(Applied::StaleDropped);
                    }
                }
            }
            detail.provider = Some(provider_owned);

            let codex_permission_pending =
                classified_decision == Decision::CodexToolResult && state.has_permission_requests();
            // A persisted child may outlive its launching foreground turn.
            // Keep the session fence above, but let Circuit ownership validate
            // child termination independently of the foreground attention gate.
            if let Some(hook) = native_hook
                .as_ref()
                .filter(|hook| hook.event == "SubagentStop" && hook.child_id.is_some())
            {
                crate::services::circuit_worker::native_hooks::receive(
                    session_id,
                    hook.clone(),
                    state.matches_turn(hook.turn_id.as_deref()),
                    state.mismatches_turn(hook.turn_id.as_deref()),
                )?;
                if let Some(payload) = hook_payload.as_ref() {
                    let accepted = accept_hook(&mut state, payload, &classified);
                    if accepted.accepted
                        && lifecycle_decision(classified_decision, &state, false, accepted)
                            == Decision::Ready
                    {
                        crate::node_turn::publish_ready(session_id, app, detail);
                    }
                }
                return Ok(Applied::Applied);
            }
            let applied = apply_hook_after_turn_fence(
                session_id,
                &mut state,
                hook_payload.as_ref(),
                &classified,
                native_hook.as_ref(),
                |state, native_hook, accept| {
                    // `PostToolUse` is catch-all in Codex. Only an approval marker
                    // makes it a lifecycle resume; ordinary tool output is
                    // correlation-neutral and must not spam `work_resumed`.
                    let receipt_result = if let Some(hook) = native_hook {
                        let turn_fenced = state.matches_turn(hook.turn_id.as_deref());
                        let explicit_turn_mismatch = state.mismatches_turn(hook.turn_id.as_deref());
                        crate::services::circuit_worker::native_hooks::receive(
                            session_id,
                            hook.clone(),
                            turn_fenced,
                            explicit_turn_mismatch,
                        )
                    } else {
                        Ok(())
                    };

                    // A Kimi task notification can race with the foreground Stop.
                    // While the model is still active it is correlation-only; after
                    // the foreground turn has ended it is the authoritative clean
                    // completion. This prevents an asynchronous background callback
                    // from flipping an active turn to Ready.
                    let decision = lifecycle_decision(
                        classified_decision,
                        state,
                        codex_permission_pending,
                        accept,
                    );

                    if let Some(kind) = decision.lifecycle_kind(&detail) {
                        tracing::info!(session_id, ?kind, "attention webhook publishing lifecycle");
                        crate::node_turn::publish_hook(session_id, app, kind, detail);
                    } else if decision == Decision::Ignore {
                        tracing::debug!(session_id, "lifecycle-neutral hook, session capture only");
                    } else {
                        // An unresolved correlation observation must not panic or
                        // terminate the HTTP request task.
                        tracing::warn!(session_id, ?decision, "unnormalized hook correlation");
                    }
                    receipt_result?;
                    Ok(Applied::Applied)
                },
            )?;
            Ok(applied)
        },
    )
    .await;

    // A successful receipt is durable even when reconciliation has not run.
    // Persistence failure must not be acknowledged as received. Ordinary
    // Agent Node lifecycle publication still runs before returning this error.
    match applied {
        Ok(_) => Response::empty("200 OK"),
        Err(error) => {
            tracing::warn!("attention receipt for node {session_id} failed: {error}");
            Response::empty("503 Service Unavailable")
        }
    }
}

/// Outcome of the single blocking apply pass (issue #1364 review): the
/// webhook was applied, or dropped as a stale callback from a previous
/// process generation.
enum Applied {
    Applied,
    StaleDropped,
}
