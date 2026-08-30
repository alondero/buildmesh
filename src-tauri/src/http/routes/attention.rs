//! `POST /api/attention/{session_id}` — webhook for a Node Turn (see CONTEXT.md
//! and `crate::node_turn`). Both Claude Code hooks point here — the Stop hook
//! (turn finished) and the catch-all Notification hook (idle or permission
//! prompt) — as do Codex's Stop and PermissionRequest hooks (issue #884).
//! The hook command forwards the hook's stdin JSON as the POST body
//! (issue #878), which is what lets this handler tell a genuine yield from a
//! turn that only ended because the harness is waiting on background tasks and
//! will re-invoke itself — those must NOT mark the node as awaiting input.
//!
//! No token required: the hook is configured locally and runs over localhost.
//! Because it is unauthenticated, the handler verifies the client peer address
//! is loopback (issue #496 / ADR-0012) — an external machine cannot spoof
//! attention events even if it can reach the port.

use std::net::SocketAddr;
use std::path::Path;

use crate::http::MaybeTls;

use crate::http::request;

/// Hook payloads are small (a handful of metadata fields); 64 KB is far above
/// any real payload while still bounding what a local process can make us
/// buffer.
const MAX_HOOK_BODY: usize = 64 * 1024;

/// The fields of Claude Code's hook stdin JSON this route cares about. Unknown
/// fields are ignored; every field is optional so an empty or legacy body
/// (`{}` or nothing at all) degrades to the pre-#878 behaviour of always
/// marking attention.
///
/// Grok Code's HTTP hook (`~/.grok/docs/user-guide/10-hooks.md`, issue
/// #1282) POSTs the same envelope shape but with camelCase top-level
/// keys (`sessionId`, `hookEventName`) — and adds a separate
/// `notificationType` field on `Notification` events (`idle_prompt`,
/// `permission_prompt`, `task_complete`, …). The `#[serde(alias)]`
/// attributes accept either casing so the same parser handles Claude
/// and Grok payloads.
///
/// The fields of agent harness hook stdin JSON this route cares about. Unknown
/// fields are ignored; every field is optional so an empty or legacy body
/// (`{}` or nothing at all) degrades to the pre-#878 behaviour of always
/// marking attention.
///
/// Grok Code's HTTP hook (`~/.grok/docs/user-guide/10-hooks.md`, issue
/// #1282) POSTs the same envelope shape but with camelCase top-level
/// keys (`sessionId`, `hookEventName`) — and adds a separate
/// `notificationType` field on `Notification` events (`idle_prompt`,
/// `permission_prompt`, `task_complete`, …). The `#[serde(alias)]`
/// attributes accept either casing so the same parser handles Claude
/// and Grok payloads.
///
/// Antigravity (agy, issue #1285, #1367) sends the same payload shape in
/// camelCase too, but uses different key names than Grok:
/// `conversationId` (not `sessionId`), `transcriptPath`, `fullyIdle`,
/// `terminationReason`, `workspacePaths`, `artifactDirectoryPath`,
/// `modelName`, `executionNum`, `error`. All aliases accept both camelCase
/// and snake_case forms; a payload that mixes casings also parses.
///
/// Note: fields like `execution_num`, `workspace_paths`, `artifact_directory_path`,
/// `model_name`, and `error` are parsed for telemetry, diagnostics, and forward
/// compatibility with future AGY revisions, but are not decision inputs in `decide()`.
#[derive(serde::Deserialize, Default, Debug, Clone, PartialEq, Eq)]
struct HookPayload {
    #[serde(alias = "sessionId", alias = "conversationId")]
    session_id: Option<String>,
    #[serde(alias = "hookEventName", alias = "hook_event_name")]
    hook_event_name: Option<String>,
    #[serde(alias = "transcriptPath", alias = "transcript_path")]
    transcript_path: Option<String>,
    /// Grok's structured notification type on `Notification` events
    /// (`permission_prompt`, `idle_prompt`, `task_complete`, …). The
    /// Grok docs note the matcher tests this field — we use it as a
    /// belt-and-braces parallel to Claude's `message`-substring check.
    /// Accepts both the wire camelCase (`notificationType`) and the
    /// grok-agent-sdk snake_case (`notification_type`).
    #[serde(alias = "notificationType", alias = "notification_type")]
    notification_type: Option<String>,
    /// Notification hooks carry the human-readable notification text, e.g.
    /// "Claude needs your permission to use Bash".
    message: Option<String>,
    /// AGY signals "the turn truly settled" with `fullyIdle: true` and
    /// "the harness is still busy on background work" with `fullyIdle:
    /// false` (issue #1285, #1367). The latter is the false-yield analogue of
    /// Claude Code's background-task detection (issue #878): we
    /// publish the turn (naming / autopilot still fire) but suppress
    /// the attention marking. Missing / unknown defaults to "idle"
    /// so a future harness that omits the field keeps working.
    #[serde(alias = "fullyIdle", alias = "fully_idle", default)]
    fully_idle: Option<bool>,
    /// AGY's `terminationReason` (e.g. `"model_stop"`,
    /// `"tool_execution_limit_reached"`, `"error"`, `"max_steps_exceeded"`).
    /// Opaque to Buildmesh today — recorded for future debugging but not a
    /// decision input in `decide()`.
    #[serde(alias = "terminationReason", alias = "termination_reason", default)]
    termination_reason: Option<String>,
    /// Present if the hook execution encountered an error.
    #[serde(default)]
    error: Option<String>,
    /// AGY execution step index or invocation count.
    #[serde(alias = "executionNum", alias = "execution_num", default)]
    execution_num: Option<i64>,
    /// AGY workspace paths.
    #[serde(alias = "workspacePaths", alias = "workspace_paths", default)]
    workspace_paths: Option<Vec<String>>,
    /// AGY artifact directory path.
    #[serde(alias = "artifactDirectoryPath", alias = "artifact_directory_path", default)]
    artifact_directory_path: Option<String>,
    /// AGY model name.
    #[serde(alias = "modelName", alias = "model_name", default)]
    model_name: Option<String>,
}

/// Extract the provider-owned UUID from a structured hook callback. An
/// arbitrary string must never enter `cli_session_id`: resume treats that
/// column as an executable CLI argument. Codex, Claude, and AGY all use
/// UUIDs; the alias on `HookPayload::session_id` makes
/// `conversationId` (AGY) parse through the same code path. Validation
/// is delegated to [`request::parse_cli_session_id`] so this route and
/// `import_and_resume` share one boundary check (issue #1237).
fn hook_session_id(body: &[u8]) -> Option<String> {
    let id = serde_json::from_slice::<HookPayload>(body).ok()?.session_id?;
    request::parse_cli_session_id(&id)
}

/// What to do with an incoming attention webhook.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Publish the Node Turn with attention marking — the user is needed.
    Mark,
    /// Publish the Node Turn without attention marking: the turn ended only
    /// because background tasks are still running and the harness will
    /// re-invoke itself when they finish (issue #878).
    SuppressPendingBackground,
}

/// Classify a hook POST body. `count_pending` is the transcript scan
/// (`transcript_reader::count_pending_background_tasks`), injected so tests
/// don't need real transcript files.
///
/// The rules, in order:
/// 1. Unparseable/absent body → `Mark` (pre-#878 behaviour; old hook configs
///    that post no body keep working until the next spawn migrates them).
/// 2. A permission-prompt Notification (Claude Code), a `PermissionRequest`
///    event (Codex's dedicated hook for tool approval, issue #884), a
///    Grok `Notification` with `notificationType == "permission_prompt"`
///    (issue #1282), or an AGY `PreToolUse` event (the harness's pre-tool
///    approval hook, issue #1285) → `Mark` always. The agent is blocked
///    on a tool-approval decision — that needs the user even while
///    background tasks run.
/// 3. AGY's `Stop` with `fullyIdle: false` (or explicit `fullyIdle: false`,
///    issue #1285, #1367) → suppress. The harness signalled the turn ended
///    but the agent is still busy on background work. Same false-yield
///    semantic as Claude Code's transcript-scan path (rule 4), but signalled
///    directly by the harness because AGY has no background task scan.
/// 4. Anything else (Stop, idle Notification) with launched-but-unfinished
///    background tasks in the transcript → `SuppressPendingBackground`.
/// 5. No transcript path, unreadable transcript, or no pending tasks → `Mark`.
///    "Unknown" must never read as "no attention needed".
fn decide(body: &[u8], count_pending: impl FnOnce(&Path) -> Option<usize>) -> Decision {
    let Ok(payload) = serde_json::from_slice::<HookPayload>(body) else {
        return Decision::Mark;
    };
    if matches!(
        payload.hook_event_name.as_deref(),
        Some("PermissionRequest") | Some("PreToolUse")
    ) {
        return Decision::Mark;
    }
    if payload.hook_event_name.as_deref() == Some("Notification") {
        // Claude Code: human-readable message contains "permission".
        if payload
            .message
            .as_deref()
            .is_some_and(|m| m.to_ascii_lowercase().contains("permission"))
        {
            return Decision::Mark;
        }
        // Grok Code (issue #1282): structured notificationType =
        // "permission_prompt". A matcher on the wire might catch it
        // before us, but the runner POSTs the envelope unconditionally
        // for every matched hook entry — we still see the callback.
        if payload.notification_type.as_deref() == Some("permission_prompt") {
            return Decision::Mark;
        }
    }
    // AGY `Stop` with `fullyIdle: false` is a direct false-yield signal
    // from the harness — short-circuit before the transcript scan so an
    // AGY node gets correct suppression.
    //
    // AGY-specific: in AGY Stop payloads, `hook_event_name` is either
    // explicitly "Stop" or omitted from stdin JSON. When `fullyIdle: false`
    // arrives on a Stop event or an AGY payload (with session_id / conversationId
    // or terminationReason), suppress attention without scanning transcripts.
    if (payload.hook_event_name.as_deref() == Some("Stop")
        || (payload.hook_event_name.is_none()
            && (payload.termination_reason.is_some() || payload.session_id.is_some())))
        && payload.fully_idle == Some(false)
    {
        return Decision::SuppressPendingBackground;
    }
    // A Stop with fullyIdle: true or absent falls through to the transcript-scan
    // path so any future transcript reader hooks in normally.
    let Some(transcript_path) = payload.transcript_path.filter(|p| !p.is_empty()) else {
        return Decision::Mark;
    };
    // A WSL-side agent reports a Linux transcript path; convert before the
    // Windows-side read (the module rule: never hand a Linux path to a
    // Windows API).
    let host_path = crate::env::to_host_path(&transcript_path);
    match count_pending(Path::new(&host_path)) {
        Some(n) if n > 0 => Decision::SuppressPendingBackground,
        _ => Decision::Mark,
    }
}

pub async fn handle_post(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    path_without_query: &str,
    peer: SocketAddr,
    content_length: usize,
) {
    // Loopback-only: the Claude Code hook always posts from 127.0.0.1/::1. A
    // non-loopback peer is an external spoof attempt — refuse before doing work.
    if !peer.ip().is_loopback() {
        let _ = request::write_status_only(lines, "403 Forbidden").await;
        return;
    }

    let session_id: Option<i64> = path_without_query
        .strip_prefix("/api/attention/")
        .and_then(|s| s.parse().ok());

    let Some(session_id) = session_id else {
        let _ = request::write_status_only(lines, "400 Bad Request").await;
        return;
    };

    let Some(body) =
        request::read_body_or_send_error(lines, content_length, MAX_HOOK_BODY).await
    else {
        return;
    };

    let Some(app) = crate::http::app_handle() else {
        let _ = request::write_status_only(lines, "503 Service Unavailable").await;
        return;
    };

    // Codex self-assigns its thread id. PTY capture remains the earliest
    // source, while the hook payload is the exact structured fallback after
    // the first turn. The conditional DB update prevents a delayed callback
    // from an old process overwriting the active session (issue #1089).
    if let Some(cli_session_id) = hook_session_id(&body) {
        match crate::db::set_cli_session_id_if_missing(session_id, &cli_session_id) {
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

    // AGY surfaces its `terminationReason` (e.g. `"model_stop"`,
    // `"tool_execution_limit_reached"`) so a future debugging session can
    // distinguish "the model finished its turn" from "the harness
    // aborted the turn" — log at debug so it's there when needed without
    // polluting the happy path (issue #1285, #1367).
    let payload_parsed = serde_json::from_slice::<HookPayload>(&body);
    match payload_parsed {
        Ok(ref payload) => {
            if let Some(ref reason) = payload.termination_reason.as_deref().filter(|r| !r.is_empty()) {
                tracing::debug!(
                    "attention webhook for node {} reported terminationReason={}",
                    session_id,
                    reason
                );
            }
            if let Some(ref err) = payload.error.as_deref().filter(|e| !e.is_empty()) {
                tracing::debug!(
                    "attention webhook for node {} reported error={}",
                    session_id,
                    err
                );
            }
        }
        Err(_) => {
            tracing::warn!(
                "attention webhook for node {} received unparseable or empty body (low-confidence degraded fallback to mark attention)",
                session_id
            );
        }
    }

    match decide(&body, crate::services::transcript_reader::count_pending_background_tasks) {
        Decision::Mark => {
            crate::node_turn::publish(session_id, app);
            crate::http::events::emit(crate::http::events::EventMsg::AttentionNeeded {
                session_id,
            });
        }
        Decision::SuppressPendingBackground => {
            tracing::info!(
                "attention webhook for node {}: background tasks still pending — \
                 turn published without attention marking (issue #878)",
                session_id
            );
            crate::node_turn::publish_without_attention(session_id, app);
        }
    }

    let _ = request::write_status_only(lines, "200 OK").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative Stop-hook stdin payload.
    fn stop_body(transcript_path: &str) -> Vec<u8> {
        serde_json::json!({
            "session_id": "abc-123",
            "transcript_path": transcript_path,
            "cwd": "F:\\src\\repo",
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn empty_or_garbage_body_marks() {
        // Pre-#878 hooks post no body at all; a broken payload must degrade to
        // the old always-mark behaviour, never to silence.
        assert_eq!(decide(b"", |_| Some(5)), Decision::Mark);
        assert_eq!(decide(b"not json", |_| Some(5)), Decision::Mark);
    }

    #[test]
    fn structured_hook_exposes_canonical_session_uuid() {
        let body = serde_json::json!({
            "session_id": "C1234567-89AB-CDEF-0123-456789ABCDEF",
            "hook_event_name": "Stop",
        })
        .to_string();
        assert_eq!(
            hook_session_id(body.as_bytes()).as_deref(),
            Some("c1234567-89ab-cdef-0123-456789abcdef")
        );
    }

    #[test]
    fn missing_malformed_or_non_uuid_session_id_is_ignored() {
        assert_eq!(hook_session_id(b"{}"), None);
        assert_eq!(hook_session_id(b"not json"), None);
        assert_eq!(hook_session_id(br#"{"session_id":"most-recent"}"#), None);
    }

    #[test]
    fn stop_with_pending_background_tasks_suppresses() {
        let body = stop_body("/tmp/session.jsonl");
        assert_eq!(
            decide(&body, |_| Some(2)),
            Decision::SuppressPendingBackground
        );
    }

    #[test]
    fn stop_with_no_pending_tasks_marks() {
        let body = stop_body("/tmp/session.jsonl");
        assert_eq!(decide(&body, |_| Some(0)), Decision::Mark);
    }

    #[test]
    fn unreadable_transcript_marks() {
        // "Unknown" is not "no attention needed" — a missing/unreadable
        // transcript falls back to marking.
        let body = stop_body("/tmp/session.jsonl");
        assert_eq!(decide(&body, |_| None), Decision::Mark);
    }

    #[test]
    fn missing_transcript_path_marks() {
        let body = serde_json::json!({"hook_event_name": "Stop"}).to_string().into_bytes();
        assert_eq!(decide(&body, |_| Some(3)), Decision::Mark);
    }

    #[test]
    fn permission_prompt_notification_marks_even_with_pending_tasks() {
        // A tool-approval question blocks the whole turn — background work
        // running in parallel doesn't make the user less needed.
        let body = serde_json::json!({
            "hook_event_name": "Notification",
            "transcript_path": "/tmp/session.jsonl",
            "message": "Claude needs your permission to use Bash",
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(2)), Decision::Mark);
    }

    #[test]
    fn codex_permission_request_marks_even_with_pending_tasks() {
        // Codex raises a dedicated PermissionRequest hook event when a tool
        // needs approval (issue #884) — the user is needed, same as a Claude
        // permission Notification, regardless of background work.
        let body = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "transcript_path": "/tmp/session.jsonl",
            "tool_name": "Bash",
            "message": "Codex needs your permission to run Bash",
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(2)), Decision::Mark);
    }

    #[test]
    fn idle_notification_with_pending_tasks_suppresses() {
        // The 60s idle notification fires while the agent sits at its input
        // box waiting for a long background build — same false yield as Stop.
        let body = serde_json::json!({
            "hook_event_name": "Notification",
            "transcript_path": "/tmp/session.jsonl",
            "message": "Claude is waiting for your input",
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            decide(&body, |_| Some(1)),
            Decision::SuppressPendingBackground
        );
    }

    // -------------------------------------------------------------------
    // Grok Code (issue #1282) — camelCase wire, no transcript_path,
    // Notification carries structured `notificationType`.
    // -------------------------------------------------------------------

    /// Grok's permission prompt: the matcher fires `Notification` with
    /// `notificationType = "permission_prompt"`. We mark the node
    /// regardless of any background-task count (defensive — Grok has
    /// no transcript reader yet, so the closure is unused, but
    /// keeping the signature uniform guards against a future
    /// transcript reader silently swallowing the permission yield).
    #[test]
    fn grok_notification_with_permission_type_marks() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "cwd": "/Users/you/project",
            "workspaceRoot": "/Users/you/project",
            "notificationType": "permission_prompt",
            "message": "Grok needs your permission to run Bash",
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(2)), Decision::Mark);
    }

    /// Grok's idle prompt (`notificationType = "idle_prompt"`) carries
    /// no transcript path, so it falls through to Mark — the right
    /// outcome for a Node Turn signal we have no transcript scan to
    /// disambiguate.
    #[test]
    fn grok_idle_notification_marks_without_transcript() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "notificationType": "idle_prompt",
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(2)), Decision::Mark);
    }

    /// Grok's `Stop` event is a Node Turn — Mark. The runner treats
    /// the route's empty 200 OK as "allow the stop" (we never return
    /// a `decision: "block"` JSON), so the agent doesn't loop on the
    /// gate, but the node still flips to awaiting_input.
    #[test]
    fn grok_stop_event_marks() {
        let body = serde_json::json!({
            "hookEventName": "stop",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "stopHookActive": false,
            "lastAssistantMessage": "Done.",
            "reason": "end_turn",
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(0)), Decision::Mark);
    }

    /// The payload parser accepts Grok's camelCase `sessionId` and
    /// canonicalises it to the same UUID string Claude payloads do.
    /// `hook_session_id` is the only consumer of the field on the
    /// session-id side of the route — both casings must round-trip.
    #[test]
    fn hook_payload_reads_camel_case_session_id() {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550E8400-E29B-41D4-A716-446655440000",
            "notificationType": "idle_prompt",
        })
        .to_string();
        assert_eq!(
            hook_session_id(body.as_bytes()).as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    /// Grok wires `notificationType` as a separate field, distinct
    /// from Claude's message-substring convention — both styles must
    /// classify as a permission yield so a hook that emits either
    /// shape gets marked.
    #[test]
    fn grok_notification_type_via_snake_case_alias_also_marks() {
        // The grok-agent-sdk converts camelCase top-level keys to
        // snake_case — accept both so the same parser handles both
        // delivery surfaces.
        let body = serde_json::json!({
            "hook_event_name": "Notification",
            "session_id": "550e8400-e29b-41d4-a716-446655440000",
            "notification_type": "permission_prompt",
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(2)), Decision::Mark);
    }

    // -------------------------------------------------------------------
    // Antigravity (agy) payload shape — issue #1285.
    //
    // AGY sends camelCase fields (`conversationId`, `transcriptPath`,
    // `fullyIdle`, `terminationReason`) and two event kinds (`Stop` and
    // `PreToolUse`). The route's `HookPayload` accepts both via serde
    // aliases; `decide` short-circuits `PreToolUse` to always-mark and
    // `Stop` with `fullyIdle: false` to always-suppress.
    // -------------------------------------------------------------------

    /// AGY's `Stop` with `fullyIdle: false` is a direct false-yield signal
    /// from the harness — the turn ended but background work is still
    /// running. Short-circuits before the transcript scan so an AGY node
    /// (which has no transcript reader) gets correct suppression. Even
    /// when `count_pending` reports zero tasks (the harness said so), the
    /// harness's own signal wins — AGY's view is authoritative for AGY.
    #[test]
    fn agy_stop_with_fully_idle_false_suppresses() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": false,
            "terminationReason": "model_stop",
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            decide(&body, |_| Some(0)),
            Decision::SuppressPendingBackground
        );
    }

    /// AGY's `Stop` with `fullyIdle: true` is a genuine yield — falls
    /// through to the transcript-scan path. No pending tasks → Mark.
    #[test]
    fn agy_stop_with_fully_idle_true_marks() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": true,
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(0)), Decision::Mark);
    }

    /// `fullyIdle: true` with pending tasks still suppresses — the
    /// transcript scan is consulted, not skipped, when the harness says
    /// the turn actually settled.
    #[test]
    fn agy_stop_with_fully_idle_true_and_pending_tasks_suppresses() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": true,
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            decide(&body, |_| Some(2)),
            Decision::SuppressPendingBackground
        );
    }

    /// An older AGY payload that omits `fullyIdle` entirely (or any
    /// future harness that doesn't set it) falls through to the
    /// transcript-scan path — `Stop` with no transcript path → Mark,
    /// matching the pre-#1285 safe default. The new field is additive,
    /// not breaking.
    #[test]
    fn agy_stop_without_fully_idle_uses_transcript_scan() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(0)), Decision::Mark);

        let missing_transcript = serde_json::json!({
            "conversationId": "abc-123",
            "hook_event_name": "Stop",
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            decide(&missing_transcript, |_| Some(3)),
            Decision::Mark
        );
    }

    /// AGY's `PreToolUse` fires before a tool call — analogous to Codex's
    /// `PermissionRequest`. The agent is at a tool-approval decision, so
    /// the user is needed regardless of background work. Always marks.
    #[test]
    fn agy_pre_tool_use_marks_even_with_pending_tasks() {
        let body = serde_json::json!({
            "conversationId": "abc-123",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "PreToolUse",
            "toolCall": {"name": "run_command", "args": {"cmd": "ls"}},
            "stepIdx": 5,
        })
        .to_string()
        .into_bytes();
        assert_eq!(decide(&body, |_| Some(5)), Decision::Mark);
    }

    /// `hook_session_id` extracts the AGY UUID from `conversationId` via
    /// the alias, just like Claude Code's `session_id`. Lower-cased so
    /// the value matches what the orchestrator's spawn pipeline writes
    /// into `agent_nodes.cli_session_id`.
    #[test]
    fn hook_session_id_reads_agy_conversation_id() {
        let body = serde_json::json!({
            "conversationId": "C1234567-89AB-CDEF-0123-456789ABCDEF",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": true,
        })
        .to_string();
        assert_eq!(
            hook_session_id(body.as_bytes()).as_deref(),
            Some("c1234567-89ab-cdef-0123-456789abcdef")
        );
    }

    /// A payload that mixes snake_case (Claude Code shape) and
    /// camelCase (AGY shape) for different fields still parses — the
    /// alias is per-field, not per-payload. Both Grok's `sessionId` and
    /// AGY's `conversationId` flow through the same `session_id` field
    /// via stacked aliases.
    #[test]
    fn hook_payload_tolerates_mixed_case_field_names() {
        let body = serde_json::json!({
            "session_id": "C1234567-89AB-CDEF-0123-456789ABCDEF",
            "transcriptPath": "/tmp/session.jsonl",
            "hook_event_name": "Stop",
            "fullyIdle": true,
        })
        .to_string();
        assert_eq!(
            hook_session_id(body.as_bytes()).as_deref(),
            Some("c1234567-89ab-cdef-0123-456789abcdef")
        );
    }

    /// Issue #1367: Fixture for a complete AGY Stop payload emitted by current releases (1.0.0-1.1.22+).
    /// Covers all standard metadata fields, camelCase keys, and verifies exact parsing.
    #[test]
    fn agy_full_release_fixture_parses_all_fields() {
        let json_body = serde_json::json!({
            "conversationId": "550e8400-e29b-41d4-a716-446655440000",
            "executionNum": 4,
            "terminationReason": "model_stop",
            "error": "",
            "fullyIdle": true,
            "workspacePaths": ["/Users/dev/project"],
            "transcriptPath": "/Users/dev/project/.gemini/antigravity/transcript.jsonl",
            "artifactDirectoryPath": "/Users/dev/project/.gemini/antigravity/artifacts",
            "modelName": "gemini-3.7-flash"
        });
        let body = json_body.to_string().into_bytes();

        let parsed = serde_json::from_slice::<HookPayload>(&body).expect("must parse full AGY fixture");
        assert_eq!(parsed.session_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(parsed.execution_num, Some(4));
        assert_eq!(parsed.termination_reason.as_deref(), Some("model_stop"));
        assert!(parsed.error.as_deref().is_some_and(|e| e.is_empty()) || parsed.error.is_none());
        assert_eq!(parsed.fully_idle, Some(true));
        assert_eq!(parsed.workspace_paths.as_deref(), Some(&["/Users/dev/project".to_string()][..]));
        assert_eq!(parsed.model_name.as_deref(), Some("gemini-3.7-flash"));

        assert_eq!(hook_session_id(&body).as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(decide(&body, |_| Some(0)), Decision::Mark);
    }

    /// Issue #1367: Background yield fixture (`fullyIdle: false`) with no hook_event_name
    /// (the natural AGY Stop hook stdin payload). Must suppress attention.
    #[test]
    fn agy_background_yield_without_hook_event_name_suppresses() {
        let json_body = serde_json::json!({
            "conversationId": "550e8400-e29b-41d4-a716-446655440000",
            "executionNum": 2,
            "terminationReason": "model_stop",
            "fullyIdle": false,
            "workspacePaths": ["/work/proj"],
            "transcriptPath": "/work/proj/transcript.jsonl"
        });
        let body = json_body.to_string().into_bytes();
        assert_eq!(decide(&body, |_| Some(0)), Decision::SuppressPendingBackground);
    }

    /// Issue #1367: Various termination reasons emitted by AGY releases.
    #[test]
    fn agy_termination_reasons_classification() {
        for reason in [
            "model_stop",
            "tool_execution_limit_reached",
            "max_steps_exceeded",
            "user_interrupt",
            "error",
        ] {
            let body_idle = serde_json::json!({
                "conversationId": "550e8400-e29b-41d4-a716-446655440000",
                "terminationReason": reason,
                "fullyIdle": true,
            })
            .to_string()
            .into_bytes();
            assert_eq!(
                decide(&body_idle, |_| Some(0)),
                Decision::Mark,
                "reason={reason} with fullyIdle=true must Mark"
            );

            let body_busy = serde_json::json!({
                "conversationId": "550e8400-e29b-41d4-a716-446655440000",
                "terminationReason": reason,
                "fullyIdle": false,
            })
            .to_string()
            .into_bytes();
            assert_eq!(
                decide(&body_busy, |_| Some(0)),
                Decision::SuppressPendingBackground,
                "reason={reason} with fullyIdle=false must Suppress"
            );
        }
    }

    /// Issue #1367: Malformed or unexpected JSON payloads degrade safely to Mark
    /// so the user is alerted that attention may be required.
    #[test]
    fn agy_malformed_payload_degrades_to_mark() {
        let malformed = b"{not: valid, json";
        assert_eq!(decide(malformed, |_| Some(0)), Decision::Mark);

        let empty_obj = b"{}";
        assert_eq!(decide(empty_obj, |_| Some(0)), Decision::Mark);
    }
}
