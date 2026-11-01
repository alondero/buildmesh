//! Owned-work tests for `circuit::observation::WorkEvidence`: how a harness's
//! background-work registry (Claude Code's Stop hook `background_tasks`) ends
//! the work it reported. Sibling-test layout per
//! `docs/development/rust-conventions.md`.
//!
//! Run with: cd src-tauri && cargo test circuit::observation::owned_work_tests
use super::*;

fn identity() -> ObservationIdentity {
    ObservationIdentity {
        run_id: 1,
        step_id: "work".into(),
        attempt: 2,
        agent_node_id: 9,
        session_incarnation: Some("spawn-2".into()),
        session_id: Some("session".into()),
        turn_id: Some("turn".into()),
        report_revision: None,
    }
}

fn event(fact: ObservedWorkFact, sequence: i64) -> CircuitObservation {
    CircuitObservation {
        identity: identity(),
        source: "claude_native_hook".into(),
        source_id: Some(sequence.to_string()),
        observed_at_ms: sequence,
        authoritative: true,
        fact,
    }
}

fn snapshot(active_work: &[&str], sequence: i64) -> CircuitObservation {
    event(
        ObservedWorkFact::OwnershipSnapshot {
            active_work: active_work.iter().map(|id| (*id).to_owned()).collect(),
        },
        sequence,
    )
}

#[test]
fn a_registry_that_stops_listing_its_work_has_ended_it() {
    let mut state = WorkEvidence::default();
    state.observe(&identity(), &snapshot(&["task:verify", "task:tests"], 1));
    state.observe(&identity(), &snapshot(&["task:tests"], 2));
    assert_eq!(state.children.get("task:verify"), Some(&true));
    assert_eq!(state.children.get("task:tests"), Some(&false));

    // Claude Code opens a turn of its own (`<task-notification>`) when
    // background work finishes. Buildmesh sent no prompt for it, so the turn is
    // reduced-confidence, but it is the same session's registry.
    let mut later = snapshot(&["task:later"], 3);
    later.identity.turn_id = Some("task-notification".into());
    later.authoritative = false;
    // `native_hooks::normalize` expects the hook's own turn.
    assert_eq!(
        state.observe(&later.identity.clone(), &later),
        ObservationDisposition::ReducedConfidence
    );
    assert_eq!(state.children.get("task:tests"), Some(&true));
    assert!(
        !state.children.contains_key("task:later"),
        "reduced-confidence evidence may retire owned work, never add it"
    );
    assert!(state.children.values().all(|terminal| *terminal));
}

#[test]
fn a_registry_from_another_session_retires_nothing() {
    let mut state = WorkEvidence::default();
    state.observe(&identity(), &snapshot(&["task:verify"], 1));
    let mut foreign = snapshot(&[], 2);
    foreign.identity.session_id = Some("other-session".into());
    foreign.authoritative = false;
    state.observe(&identity(), &foreign);
    assert_eq!(state.children.get("task:verify"), Some(&false));
}

#[test]
fn a_registry_does_not_end_work_it_never_listed() {
    let mut state = WorkEvidence::default();
    state.observe(
        &identity(),
        &event(
            ObservedWorkFact::OwnedStarted {
                work_id: "task:subagent".into(),
            },
            1,
        ),
    );
    state.observe(&identity(), &snapshot(&["task:verify"], 2));
    state.observe(&identity(), &snapshot(&[], 3));
    assert_eq!(state.children.get("task:verify"), Some(&true));
    assert_eq!(
        state.children.get("task:subagent"),
        Some(&false),
        "a started child ends only with its own termination receipt"
    );
}
