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
#[derive(serde::Deserialize, Default)]
struct HookPayload {
    session_id: Option<String>,
    hook_event_name: Option<String>,
    transcript_path: Option<String>,
    /// Notification hooks carry the human-readable notification text, e.g.
    /// "Claude needs your permission to use Bash".
    message: Option<String>,
}

/// Extract the provider-owned UUID from a structured hook callback. An
/// arbitrary string must never enter `cli_session_id`: resume treats that
/// column as an executable CLI argument. Codex and Claude both use UUIDs.
fn hook_session_id(body: &[u8]) -> Option<String> {
    let id = serde_json::from_slice::<HookPayload>(body).ok()?.session_id?;
    uuid::Uuid::parse_str(&id).ok().map(|parsed| parsed.to_string())
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
/// 2. A permission-prompt Notification (Claude Code) or a `PermissionRequest`
///    event (Codex's dedicated hook for tool approval, issue #884) → `Mark`
///    always. The agent is blocked on a tool-approval decision — that needs
///    the user even while background tasks run.
/// 3. Anything else (Stop, idle Notification) with launched-but-unfinished
///    background tasks in the transcript → `SuppressPendingBackground`.
/// 4. No transcript path, unreadable transcript, or no pending tasks → `Mark`.
///    "Unknown" must never read as "no attention needed".
fn decide(body: &[u8], count_pending: impl FnOnce(&Path) -> Option<usize>) -> Decision {
    let Ok(payload) = serde_json::from_slice::<HookPayload>(body) else {
        return Decision::Mark;
    };
    if payload.hook_event_name.as_deref() == Some("PermissionRequest") {
        return Decision::Mark;
    }
    if payload.hook_event_name.as_deref() == Some("Notification")
        && payload
            .message
            .as_deref()
            .is_some_and(|m| m.to_ascii_lowercase().contains("permission"))
    {
        return Decision::Mark;
    }
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
}
