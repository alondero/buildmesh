//! Browser-facing event broadcast channel.
//!
//! Tauri's event bus only reaches the desktop webview. To push real-time
//! updates to mobile clients (attention-needed, attention-cleared, future
//! status-changed) we maintain a single `tokio::sync::broadcast` channel
//! per process and a WebSocket endpoint that subscribes to it.
//!
//! Pattern mirrors the per-node PTY broadcast in `ws.rs` — generalised
//! to one shared channel.

use std::sync::OnceLock;
use tokio::sync::broadcast;

#[derive(Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "EventMsg.ts")]
#[serde(tag = "type")]
pub enum EventMsg {
    #[serde(rename = "attention-cleared")]
    AttentionCleared {
        #[ts(as = "i32")]
        session_id: i64,
    },
    /// The node's PTY input queue refused an inbound mobile keystroke
    /// (issue #1530) because the agent is not draining its input.
    ///
    /// The WS input direction is fire-and-forget — there is no inbound
    /// channel to acknowledge a write on — so without this event a phone that
    /// typed into a wedged agent would simply lose the keystroke, which is the
    /// exact failure #1530 exists to close. The client shows a stalled-input
    /// state and knows to re-send. `queued_bytes`/`queued_messages` let it
    /// explain *why* the write stalled rather than just that it did.
    #[serde(rename = "terminal-input-stalled")]
    TerminalInputStalled {
        #[ts(as = "i32")]
        session_id: i64,
        /// Bytes buffered in the node's PTY input queue at the moment of the
        /// refusal. A `u32`, not a `u64`: the queue is capped at 1 MiB, so this
        /// can never exceed a `u32`, and `u64` would generate a TypeScript
        /// `bigint` that `JSON.parse` can never actually produce on the wire.
        /// The client gets a plain number it can use in a tooltip.
        queued_bytes: u32,
        #[ts(as = "i32")]
        queued_messages: i32,
    },
    /// Normalized lifecycle event (issue #1364) — the same wire shape the
    /// desktop receives as the `agent-lifecycle` Tauri event, so both clients
    /// patch the affected node identically. Boxed: the full envelope dwarfs
    /// the 8-byte `attention-cleared` variant (clippy::large_enum_variant)
    /// and the wire shape is unchanged.
    #[serde(rename = "agent-lifecycle")]
    LifecycleChanged(Box<crate::agent::session_lifecycle::LifecycleChangedPayload>),
}

static EVENTS_CHANNEL: OnceLock<broadcast::Sender<EventMsg>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<EventMsg> {
    EVENTS_CHANNEL.get_or_init(|| {
        // Capacity 256: a slow mobile client falling behind just loses
        // history (the receiver returns Lagged and reconnects); current
        // state is reloaded via /api/nodes on reconnect.
        let (tx, _) = broadcast::channel(256);
        tx
    })
}

pub fn subscribe() -> broadcast::Receiver<EventMsg> {
    channel().subscribe()
}

#[cfg(test)]
pub(crate) fn receiver_count() -> usize {
    channel().receiver_count()
}

/// Fire-and-forget — if no receivers, the send returns Err which we ignore.
pub fn emit(msg: EventMsg) {
    let _ = channel().send(msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribe_then_emit_delivers_message() {
        let mut rx = subscribe();
        emit(EventMsg::AttentionCleared { session_id: 7 });
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        match got {
            EventMsg::AttentionCleared { session_id } => assert_eq!(session_id, 7),
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn event_serialises_as_tagged_json() {
        let json = serde_json::to_string(&EventMsg::AttentionCleared { session_id: 42 }).unwrap();
        assert!(json.contains(r#""type":"attention-cleared""#));
        assert!(json.contains(r#""session_id":42"#));
    }

    #[tokio::test]
    async fn lifecycle_changed_serialises_as_agent_lifecycle() {
        let payload = crate::agent::session_lifecycle::LifecycleChangedPayload {
            session_id: 7,
            provider: Some("anthropic".into()),
            kind: crate::agent::session_lifecycle::LifecycleKind::TurnCompleted,
            status: crate::models::SessionStatus::Ready,
            message: Some("turn finished".into()),
            provider_event: Some("Stop".into()),
            provider_session_id: Some("550e8400-e29b-41d4-a716-446655440000".into()),
            completion_reason: Some("end_turn".into()),
            transcript_path: Some("/tmp/session.jsonl".into()),
            timestamp: "2026-08-31T00:00:00+00:00".into(),
            signal_health: crate::agent::session_lifecycle::SignalHealth::Ok,
            semantic_turn: None,
            request: None,
            cleared_session_id: None,
        };
        let json = serde_json::to_string(&EventMsg::LifecycleChanged(Box::new(payload))).unwrap();
        assert!(json.contains(r#""type":"agent-lifecycle""#));
        assert!(json.contains(r#""kind":"turn_completed""#));
        assert!(json.contains(r#""status":"ready""#));
        assert!(json.contains(r#""completion_reason":"end_turn""#));
    }

    #[tokio::test]
    async fn lifecycle_changed_delivers_through_broadcast() {
        let mut rx = subscribe();
        let payload = crate::agent::session_lifecycle::LifecycleChangedPayload {
            session_id: 7,
            provider: None,
            kind: crate::agent::session_lifecycle::LifecycleKind::SessionExited,
            status: crate::models::SessionStatus::Idle,
            message: None,
            provider_event: None,
            provider_session_id: None,
            completion_reason: None,
            transcript_path: None,
            timestamp: "2026-08-31T00:00:00+00:00".into(),
            signal_health: crate::agent::session_lifecycle::SignalHealth::Ok,
            semantic_turn: None,
            request: None,
            cleared_session_id: Some("stale-session-id".into()),
        };
        emit(EventMsg::LifecycleChanged(Box::new(payload)));
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        match got {
            EventMsg::LifecycleChanged(p) => {
                assert_eq!(p.session_id, 7);
                assert_eq!(
                    p.kind,
                    crate::agent::session_lifecycle::LifecycleKind::SessionExited
                );
            }
            _ => panic!("wrong variant"),
        }
    }
}
