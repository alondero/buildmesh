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
mod tests {
    use super::*;

    /// Drain the shared broadcast until `predicate` matches, or the deadline
    /// expires.
    ///
    /// The event channel is a process-wide `OnceLock` (see [`channel`]), so a
    /// concurrently running test that calls [`emit`] fans its event into this
    /// receiver too. Asserting on the *first* message therefore asserts on
    /// arrival order across the whole test binary, which is the flake in issue
    /// #2561. Correlating on the payload instead keeps the assertion just as
    /// strict — the intended event must still arrive and its fields are still
    /// checked — without depending on what else the process emitted.
    async fn recv_matching(
        rx: &mut broadcast::Receiver<EventMsg>,
        what: &str,
        mut predicate: impl FnMut(&EventMsg) -> bool,
    ) -> EventMsg {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(message) => {
                        if predicate(&message) {
                            break message;
                        }
                    }
                    // A slow receiver loses history rather than the
                    // connection (capacity 256). Keep draining: the correlated
                    // event may still be ahead of us.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        panic!("event channel closed while waiting for {what}")
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
    }

    /// The `SessionExited` payload both lifecycle delivery tests assert on.
    /// `session_id` is a parameter so a test can correlate on an id no other
    /// test emits.
    fn lifecycle_payload(
        session_id: i64,
    ) -> crate::agent::session_lifecycle::LifecycleChangedPayload {
        crate::agent::session_lifecycle::LifecycleChangedPayload {
            session_id,
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
        }
    }

    #[tokio::test]
    async fn desktop_circuit_update_reaches_the_phone_socket_channel() {
        let mut rx = subscribe();
        forward_circuit_update(r#"{"run_id":987654321,"state":"running"}"#);
        let message = recv_matching(&mut rx, "the circuit-run-updated event", |message| {
            matches!(
                message,
                EventMsg::CircuitRunUpdated {
                    run_id: Some(987654321)
                }
            )
        })
        .await;
        assert_eq!(
            serde_json::to_value(message).unwrap(),
            serde_json::json!({"type":"circuit-run-updated","run_id":987654321})
        );
    }

    #[tokio::test]
    async fn subscribe_then_emit_delivers_message() {
        let mut rx = subscribe();
        emit(EventMsg::AttentionCleared { session_id: 7 });
        let got = recv_matching(&mut rx, "the attention-cleared event", |message| {
            matches!(message, EventMsg::AttentionCleared { session_id: 7 })
        })
        .await;
        match got {
            EventMsg::AttentionCleared { session_id } => assert_eq!(session_id, 7),
            _ => unreachable!("recv_matching only returns a matching message"),
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
        emit(EventMsg::LifecycleChanged(Box::new(lifecycle_payload(7))));
        let got = recv_matching(
            &mut rx,
            "the lifecycle event",
            |message| matches!(message, EventMsg::LifecycleChanged(p) if p.session_id == 7),
        )
        .await;
        match got {
            EventMsg::LifecycleChanged(p) => {
                assert_eq!(p.session_id, 7);
                assert_eq!(
                    p.kind,
                    crate::agent::session_lifecycle::LifecycleKind::SessionExited
                );
            }
            _ => unreachable!("recv_matching only returns a matching message"),
        }
    }

    /// Issue #2561 regression: unrelated traffic already sitting in the shared
    /// channel must not decide the outcome.
    ///
    /// This is the deterministic form of the flake. The old assertion read the
    /// first message off the receiver, so a decoy delivered ahead of the
    /// lifecycle event failed it with `wrong variant`. The correlated read must
    /// skip every decoy *and* still fail if the real payload never arrives —
    /// the assertion is not relaxed into "any event will do".
    ///
    /// The predicate matches on `session_id` as well as variant: a sibling test
    /// emitting its own `LifecycleChanged` into the same process-wide channel
    /// is just another decoy, and matching the variant alone let that sibling's
    /// event satisfy this test (observed while writing this fix).
    #[tokio::test]
    async fn lifecycle_changed_is_correlated_past_unrelated_traffic() {
        let mut rx = subscribe();
        emit(EventMsg::AttentionCleared { session_id: 2561 });
        emit(EventMsg::CircuitRunUpdated { run_id: Some(2561) });
        emit(EventMsg::TerminalInputStalled {
            session_id: 2561,
            queued_bytes: 2561,
            queued_messages: 2561,
        });
        emit(EventMsg::LifecycleChanged(Box::new(lifecycle_payload(
            2561,
        ))));

        let got = recv_matching(
            &mut rx,
            "the lifecycle event for session 2561",
            |message| matches!(message, EventMsg::LifecycleChanged(p) if p.session_id == 2561),
        )
        .await;
        match got {
            EventMsg::LifecycleChanged(p) => {
                assert_eq!(p.session_id, 2561);
                assert_eq!(
                    p.kind,
                    crate::agent::session_lifecycle::LifecycleKind::SessionExited
                );
                assert_eq!(p.cleared_session_id.as_deref(), Some("stale-session-id"));
            }
            _ => unreachable!("recv_matching only returns a matching message"),
        }
    }

    /// Issue #2561 regression: the same correlation holds when the decoys are
    /// emitted concurrently rather than sequentially, which is how the flake
    /// actually presented in the full suite.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lifecycle_changed_survives_concurrent_emitters() {
        let mut rx = subscribe();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(5));
        let mut emitters = Vec::new();
        for _ in 0..4 {
            let barrier = std::sync::Arc::clone(&barrier);
            emitters.push(tokio::spawn(async move {
                barrier.wait().await;
                for _ in 0..32 {
                    emit(EventMsg::AttentionCleared { session_id: 2562 });
                    tokio::task::yield_now().await;
                }
            }));
        }
        barrier.wait().await;
        emit(EventMsg::LifecycleChanged(Box::new(lifecycle_payload(
            2562,
        ))));
        for emitter in emitters {
            emitter.await.unwrap();
        }

        let got = recv_matching(
            &mut rx,
            "the lifecycle event for session 2562",
            |message| matches!(message, EventMsg::LifecycleChanged(p) if p.session_id == 2562),
        )
        .await;
        match got {
            EventMsg::LifecycleChanged(p) => {
                assert_eq!(p.session_id, 2562);
                assert_eq!(
                    p.kind,
                    crate::agent::session_lifecycle::LifecycleKind::SessionExited
                );
            }
            _ => unreachable!("recv_matching only returns a matching message"),
        }
    }
}
