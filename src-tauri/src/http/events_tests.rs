//! Tests for the browser-facing event broadcast.
//!
//! The channel is process-global — one per process, shared with the mobile
//! `/ws/events` endpoint — so a subscriber here also sees the events a
//! sibling test publishes. A test that emits its own event and asserts on
//! the *next* event received therefore races its siblings: issue #2561 saw
//! `lifecycle_changed_delivers_through_broadcast` fail with `wrong variant`
//! in the full Rust suite, while an isolated rerun passed.
//!
//! The rule these tests follow: identify an event by its own content rather
//! than by its arrival position, and keep the payload assertions that prove
//! the intended event was delivered. An unrelated event is skipped, not
//! accepted in place of the intended one, and a missing event still fails
//! the test.

use tokio::sync::broadcast;

use super::{emit, forward_circuit_update, subscribe, EventMsg};

/// How long a test waits for the event it published before giving up.
///
/// The event is already in the channel when `emit` returns, so this only
/// bounds a genuine failure — the event did not arrive.
const RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Node ids this module publishes under.
///
/// Distinct per test, so this module's tests match only their own event on
/// the shared broadcast.
const ATTENTION_SESSION_ID: i64 = 7;
const LIFECYCLE_SESSION_ID: i64 = 8;
const CONCURRENT_ATTENTION_SESSION_ID: i64 = 9;
const CONCURRENT_LIFECYCLE_SESSION_ID: i64 = 10;

/// Await the next event on the process-global broadcast that `wanted`
/// recognises, skipping the ones other tests publish in the meantime.
///
/// Returns the matching event, so the caller still asserts on the payload it
/// cares about. This is the difference from the assertion issue #2561 caught:
/// an unrelated event used to be the answer, and the test failed with
/// `wrong variant` when a sibling test published one first.
async fn recv_matching(
    rx: &mut broadcast::Receiver<EventMsg>,
    what: &str,
    wanted: impl Fn(&EventMsg) -> bool,
) -> EventMsg {
    tokio::time::timeout(RECV_TIMEOUT, async {
        loop {
            match rx.recv().await {
                Ok(event) if wanted(&event) => return event,
                // Another test's event: keep looking for ours.
                Ok(_) => continue,
                // A burst of other events overtook this receiver: the cursor
                // jumped forward, so keep looking for ours — it may still be
                // in the buffer.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    panic!("the event broadcast closed before {what} arrived")
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// The lifecycle payload the delivery tests publish: a node exiting, with
/// the stale session id the transition invalidated.
fn lifecycle_payload(session_id: i64) -> crate::agent::session_lifecycle::LifecycleChangedPayload {
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
    let message = recv_matching(
        &mut rx,
        "the circuit-run-updated event for run 987654321",
        |event| {
            matches!(
                event,
                EventMsg::CircuitRunUpdated {
                    run_id: Some(987654321)
                }
            )
        },
    )
    .await;
    assert_eq!(
        serde_json::to_value(message).unwrap(),
        serde_json::json!({"type":"circuit-run-updated","run_id":987654321})
    );
}

#[tokio::test]
async fn subscribe_then_emit_delivers_message() {
    let mut rx = subscribe();
    emit(EventMsg::AttentionCleared {
        session_id: ATTENTION_SESSION_ID,
    });
    let got = recv_matching(
        &mut rx,
        &format!("the attention-cleared event for session {ATTENTION_SESSION_ID}"),
        |event| matches!(event, EventMsg::AttentionCleared { session_id } if *session_id == ATTENTION_SESSION_ID),
    )
    .await;
    let EventMsg::AttentionCleared { session_id } = got else {
        panic!("the correlated event must be an attention-cleared event");
    };
    assert_eq!(session_id, ATTENTION_SESSION_ID);
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
    let payload = lifecycle_payload(LIFECYCLE_SESSION_ID);
    emit(EventMsg::LifecycleChanged(Box::new(payload)));
    let got = recv_matching(
        &mut rx,
        &format!("the agent-lifecycle event for session {LIFECYCLE_SESSION_ID}"),
        |event| {
            matches!(
                event,
                EventMsg::LifecycleChanged(payload)
                    if payload.session_id == LIFECYCLE_SESSION_ID
            )
        },
    )
    .await;
    let EventMsg::LifecycleChanged(got) = got else {
        panic!("the correlated event must be a lifecycle event");
    };
    assert_eq!(got.session_id, LIFECYCLE_SESSION_ID);
    assert_eq!(
        got.kind,
        crate::agent::session_lifecycle::LifecycleKind::SessionExited
    );
    assert_eq!(got.cleared_session_id.as_deref(), Some("stale-session-id"));
}

/// Issue #2561: the lifecycle event must still be received — and checked in
/// full — when another emitter publishes an unrelated event first.
///
/// A concurrent emitter stands in for a sibling test in this binary: a
/// subscriber on the process-global broadcast sees the events it publishes.
/// The handshake makes the order deterministic rather than a timing
/// accident: the unrelated event is on the channel before this test
/// publishes its own, so the first event this receiver sees is not the
/// lifecycle one. The old assertion — that the next event received *is* the
/// lifecycle one — failed here with `wrong variant`.
#[tokio::test]
async fn lifecycle_event_is_checked_after_a_concurrent_unrelated_event() {
    let mut rx = subscribe();

    let (published, arrived) = tokio::sync::oneshot::channel();
    let emitter = tokio::spawn(async move {
        emit(EventMsg::AttentionCleared {
            session_id: CONCURRENT_ATTENTION_SESSION_ID,
        });
        let _ = published.send(());
    });
    arrived
        .await
        .expect("the concurrent emitter published its event");
    emitter.await.expect("the concurrent emitter finished");

    let payload = lifecycle_payload(CONCURRENT_LIFECYCLE_SESSION_ID);
    emit(EventMsg::LifecycleChanged(Box::new(payload)));

    // The concurrent emitter's event is on the channel before ours, so the
    // first event received is not ours — this module's lifecycle events
    // carry their own session id, so the race the old assertion lost is
    // exercised here deterministically. A sibling test's event may sit in
    // front of both; that one is not ours either.
    let first = rx
        .recv()
        .await
        .expect("an event was published on the broadcast");
    assert!(
        !matches!(
            &first,
            EventMsg::LifecycleChanged(payload)
                if payload.session_id == CONCURRENT_LIFECYCLE_SESSION_ID
        ),
        "the concurrent emitter's event must be received before ours"
    );

    // Ours still arrives behind it, and it is still checked.
    let got = recv_matching(
        &mut rx,
        &format!("the agent-lifecycle event for session {CONCURRENT_LIFECYCLE_SESSION_ID}"),
        |event| {
            matches!(
                event,
                EventMsg::LifecycleChanged(payload)
                    if payload.session_id == CONCURRENT_LIFECYCLE_SESSION_ID
            )
        },
    )
    .await;
    let EventMsg::LifecycleChanged(got) = got else {
        panic!("the correlated event must be a lifecycle event");
    };
    assert_eq!(got.session_id, CONCURRENT_LIFECYCLE_SESSION_ID);
    assert_eq!(
        got.kind,
        crate::agent::session_lifecycle::LifecycleKind::SessionExited
    );
    assert_eq!(got.cleared_session_id.as_deref(), Some("stale-session-id"));
}
