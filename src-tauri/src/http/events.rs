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

#[derive(Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum EventMsg {
    #[serde(rename = "attention-needed")]
    AttentionNeeded {
        session_id: i64,
        semantic_turn: Option<crate::agent::session_lifecycle::SemanticTurnPayload>,
    },
    #[serde(rename = "attention-cleared")]
    AttentionCleared { session_id: i64 },
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
        emit(EventMsg::AttentionNeeded { session_id: 7, semantic_turn: None });
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        match got {
            EventMsg::AttentionNeeded { session_id, .. } => assert_eq!(session_id, 7),
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn event_serialises_as_tagged_json() {
        let json =
            serde_json::to_string(&EventMsg::AttentionCleared { session_id: 42 }).unwrap();
        assert!(json.contains(r#""type":"attention-cleared""#));
        assert!(json.contains(r#""session_id":42"#));
    }
}
