//! Tests for `http::events`.
//!
//! Split out of `src-tauri/src/http/events.rs` by issue #2561. The layout
//! conventions this depends on — why the module keeps the name `tests`, and
//! why every `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test http::events::tests

use super::*;

/// Session ids reserved for this module's broadcast tests.
///
/// The event channel is process-wide, so a sibling test emitting a lifecycle
/// event for the same id is indistinguishable from the event under test. These
/// are deliberately far from the small ids (`7`, `42`) the serialisation tests
/// use, and each test owns its own, so a predicate can correlate on identity
/// rather than on arrival order.
const DELIVERY_SESSION: i64 = 2561;
const DECOY_SESSION: i64 = 2562;
const CONCURRENT_SESSION: i64 = 2563;
const ATTENTION_SESSION: i64 = 2564;

/// Drain the shared broadcast until `predicate` matches, or the deadline
/// expires.
///
/// The event channel is a process-wide `OnceLock` (see [`channel`]), so a
/// concurrently running test that calls [`emit`] fans its event into this
/// receiver too. Asserting on the *first* message therefore asserts on arrival
/// order across the whole test binary, which is the flake in issue #2561.
/// Correlating on the payload instead keeps the assertion just as strict — the
/// intended event must still arrive and its fields are still checked — without
/// depending on what else the process emitted.
///
/// The predicate must identify the event completely, not just by variant: a
/// sibling test emitting the same variant for another session, or another
/// lifecycle kind for the same session, is still a decoy. Returning on a
/// partial match would only move the race from the variant assertion to the
/// field assertions after it.
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
                // A slow receiver loses history rather than the connection
                // (capacity 256). Keep draining: the correlated event may
                // still be ahead of us.
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

/// The `SessionExited` payload the lifecycle delivery tests assert on.
///
/// `session_id` is a parameter so each test correlates on an id no other test
/// emits.
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

/// Matches exactly the lifecycle event a test emitted: session *and* kind.
fn is_session_exited_for(message: &EventMsg, session_id: i64) -> bool {
    matches!(
        message,
        EventMsg::LifecycleChanged(p)
            if p.session_id == session_id
                && p.kind == crate::agent::session_lifecycle::LifecycleKind::SessionExited
    )
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
    emit(EventMsg::AttentionCleared {
        session_id: ATTENTION_SESSION,
    });
    let got = recv_matching(&mut rx, "the attention-cleared event", |message| {
        matches!(
            message,
            EventMsg::AttentionCleared { session_id } if *session_id == ATTENTION_SESSION
        )
    })
    .await;
    match got {
        EventMsg::AttentionCleared { session_id } => assert_eq!(session_id, ATTENTION_SESSION),
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
    emit(EventMsg::LifecycleChanged(Box::new(lifecycle_payload(
        DELIVERY_SESSION,
    ))));
    let got = recv_matching(&mut rx, "the lifecycle event", |message| {
        is_session_exited_for(message, DELIVERY_SESSION)
    })
    .await;
    match got {
        EventMsg::LifecycleChanged(p) => {
            assert_eq!(p.session_id, DELIVERY_SESSION);
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
/// first message off the receiver, so a decoy delivered ahead of the lifecycle
/// event failed it with `wrong variant`. The correlated read must skip every
/// decoy *and* still fail if the real payload never arrives — the assertion is
/// not relaxed into "any event will do".
///
/// The decoys include a `LifecycleChanged` for the same session id but a
/// different kind, which is the case a variant-only or id-only predicate would
/// wrongly accept.
#[tokio::test]
async fn lifecycle_changed_is_correlated_past_unrelated_traffic() {
    let mut rx = subscribe();
    emit(EventMsg::AttentionCleared {
        session_id: DECOY_SESSION,
    });
    emit(EventMsg::CircuitRunUpdated {
        run_id: Some(DECOY_SESSION),
    });
    emit(EventMsg::TerminalInputStalled {
        session_id: DECOY_SESSION,
        queued_bytes: DECOY_SESSION as u32,
        queued_messages: DECOY_SESSION as i32,
    });
    // Same session, different kind: an id-only predicate would accept this and
    // then fail the kind assertion below.
    let mut other_kind = lifecycle_payload(DECOY_SESSION);
    other_kind.kind = crate::agent::session_lifecycle::LifecycleKind::TurnCompleted;
    other_kind.status = crate::models::SessionStatus::Running;
    emit(EventMsg::LifecycleChanged(Box::new(other_kind)));
    emit(EventMsg::LifecycleChanged(Box::new(lifecycle_payload(
        DECOY_SESSION,
    ))));

    let got = recv_matching(&mut rx, "the session-exited lifecycle event", |message| {
        is_session_exited_for(message, DECOY_SESSION)
    })
    .await;
    match got {
        EventMsg::LifecycleChanged(p) => {
            assert_eq!(p.session_id, DECOY_SESSION);
            assert_eq!(
                p.kind,
                crate::agent::session_lifecycle::LifecycleKind::SessionExited
            );
            assert_eq!(p.cleared_session_id.as_deref(), Some("stale-session-id"));
        }
        _ => unreachable!("recv_matching only returns a matching message"),
    }
}

/// Issue #2561 regression: correlation holds while decoys are still being
/// emitted, which is how the flake actually presented in the full suite.
///
/// The read runs *concurrently* with the emitters rather than after they have
/// all joined. Draining a quiescent queue would only prove the sequential case
/// above with a longer loop; the race is between `recv` and `send`, so the
/// receiver has to be waiting while the sends are in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lifecycle_changed_survives_concurrent_emitters() {
    let mut rx = subscribe();
    // Four decoy emitters plus the target emitter.
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(5));

    let mut decoys = Vec::new();
    for _ in 0..4 {
        let barrier = std::sync::Arc::clone(&barrier);
        decoys.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..32 {
                emit(EventMsg::AttentionCleared {
                    session_id: CONCURRENT_SESSION,
                });
                tokio::task::yield_now().await;
            }
        }));
    }

    let target = {
        let barrier = std::sync::Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            emit(EventMsg::LifecycleChanged(Box::new(lifecycle_payload(
                CONCURRENT_SESSION,
            ))));
        })
    };

    // Read while every emitter is running, not after they finish.
    let reader = recv_matching(&mut rx, "the session-exited lifecycle event", |message| {
        is_session_exited_for(message, CONCURRENT_SESSION)
    });
    let emitters = async {
        for decoy in decoys {
            decoy.await.unwrap();
        }
        target.await.unwrap();
    };

    let (got, ()) = tokio::join!(reader, emitters);
    match got {
        EventMsg::LifecycleChanged(p) => {
            assert_eq!(p.session_id, CONCURRENT_SESSION);
            assert_eq!(
                p.kind,
                crate::agent::session_lifecycle::LifecycleKind::SessionExited
            );
        }
        _ => unreachable!("recv_matching only returns a matching message"),
    }
}
