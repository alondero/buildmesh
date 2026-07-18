//! `POST /api/attention/{session_id}` — webhook for a Node Turn (see CONTEXT.md
//! and `crate::node_turn`). Both Claude Code hooks point here: the Stop hook
//! (turn finished) and the catch-all Notification hook (idle or permission
//! prompt). The hook command forwards the hook's stdin JSON as the POST body
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
    hook_event_name: Option<String>,
    transcript_path: Option<String>,
    /// Notification hooks carry the human-readable notification text, e.g.
    /// "Claude needs your permission to use Bash".
    message: Option<String>,
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
/// 2. A permission-prompt Notification → `Mark` always. The agent is blocked
///    on a tool-approval decision — that needs the user even while background
///    tasks run.
/// 3. Anything else (Stop, idle Notification) with launched-but-unfinished
///    background tasks in the transcript → `SuppressPendingBackground`.
/// 4. No transcript path, unreadable transcript, or no pending tasks → `Mark`.
///    "Unknown" must never read as "no attention needed".
fn decide(body: &[u8], count_pending: impl FnOnce(&Path) -> Option<usize>) -> Decision {
    let Ok(payload) = serde_json::from_slice::<HookPayload>(body) else {
        return Decision::Mark;
    };
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
