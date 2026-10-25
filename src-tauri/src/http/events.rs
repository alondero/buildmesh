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
    /// Invalidate the phone's run snapshot, including person-gated steps.
    #[serde(rename = "circuit-run-updated")]
    CircuitRunUpdated {
        /// `None` invalidates all runs after a bulk attention dismissal.
        #[ts(as = "Option<i32>")]
        run_id: Option<i64>,
    },
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

/// All producers already notify the desktop; bridge that bus once instead of
/// maintaining a second list of worker and command transition call sites.
pub(crate) fn forward_circuit_update(payload: &str) {
    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(payload) {
        if let Some(run_id) = payload.get("run_id").and_then(serde_json::Value::as_i64) {
            emit(EventMsg::CircuitRunUpdated {
                run_id: Some(run_id),
            });
        }
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
