//! Tests for `commands::preferences`.
//!
//! The owner keeps only the sibling module declaration; the bodies live here
//! so the production file carries no test code (the repo's rust-test-layout
//! gate).

use super::*;
use crate::preferences::PairingVerificationStatus;

/// Two launches in quick succession must not queue two probes against the
/// same endpoint — the second is refused while the first is in flight.
#[test]
fn a_second_probe_for_the_same_route_is_refused_while_the_first_is_in_flight() {
    let key = "test:dedup:harness:provider:Windows";
    assert!(
        claim_verification_slot(key),
        "the first claim wins the slot"
    );
    assert!(
        !claim_verification_slot(key),
        "the second claim is refused while the probe is in flight"
    );
    release_verification_slot(key);
    assert!(
        claim_verification_slot(key),
        "the slot is reusable once the probe has released it"
    );
    release_verification_slot(key);
}

/// A probe that dies must not leave the route permanently un-re-checkable:
/// the next launch would then silently stop verifying it.
#[test]
fn the_in_flight_slot_is_released_when_the_guard_drops() {
    let key = "test:guard:harness:provider:Windows";
    // The production order: claim the slot, then hand the key to the guard
    // that owns it for the probe's lifetime.
    assert!(claim_verification_slot(key));
    {
        let _guard = InFlightVerification(key.to_string());
        assert!(
            !claim_verification_slot(key),
            "the slot is held while the probe runs"
        );
    }
    assert!(
        claim_verification_slot(key),
        "dropping the guard releases the slot"
    );
    release_verification_slot(key);
}

/// Different routes never share a slot: the key covers the harness, the
/// provider and the runtime.
#[test]
fn different_routes_hold_different_slots() {
    let first = "test:route-a:harness:provider:Windows";
    let second = "test:route-b:harness:provider:Windows";
    assert!(claim_verification_slot(first));
    assert!(
        claim_verification_slot(second),
        "a second route is not blocked by the first"
    );
    release_verification_slot(first);
    release_verification_slot(second);
}

/// A background check that saved a fresh record but forgot to raise
/// `provider-list-changed` would leave the stale caveat on screen: the spawn
/// surfaces read the provider list from a cache that this event invalidates.
#[test]
fn a_completed_verification_publishes_the_record_and_the_provider_list() {
    #[derive(Default)]
    struct Recorder {
        events: std::sync::Mutex<Vec<&'static str>>,
    }

    impl PairingVerificationEvents for Recorder {
        fn verification_changed(&self, _record: &PairingVerification) {
            self.events
                .lock()
                .unwrap()
                .push("pairing-verification-changed");
        }

        fn provider_list_changed(&self) {
            self.events.lock().unwrap().push("provider-list-changed");
        }
    }

    let recorder = Recorder::default();
    publish_verification_result(&recorder, &verification_fixture());
    assert_eq!(
        *recorder.events.lock().unwrap(),
        vec!["pairing-verification-changed", "provider-list-changed"],
        "both events are required: the record event updates Settings, the provider-list event is what clears the Spawn Menu's stale warning"
    );
}

/// A completed verification publishes the same pair of events whether the user
/// asked for it or the spawn scheduled it in the background.
#[test]
fn a_failed_verification_still_publishes_both_events() {
    #[derive(Default)]
    struct Recorder {
        events: std::sync::Mutex<Vec<&'static str>>,
    }

    impl PairingVerificationEvents for Recorder {
        fn verification_changed(&self, _record: &PairingVerification) {
            self.events
                .lock()
                .unwrap()
                .push("pairing-verification-changed");
        }

        fn provider_list_changed(&self) {
            self.events.lock().unwrap().push("provider-list-changed");
        }
    }

    let recorder = Recorder::default();
    // A check that failed still writes its record, so the Spawn Menu has to
    // learn about it: the row's caveat changes to "the last verification
    // failed", not to "not verified yet".
    let mut record = verification_fixture();
    record.status = PairingVerificationStatus::Failed;
    record.reason = Some("authentication failed (401)".into());
    publish_verification_result(&recorder, &record);
    assert_eq!(
        *recorder.events.lock().unwrap(),
        vec!["pairing-verification-changed", "provider-list-changed"]
    );
}

fn verification_fixture() -> PairingVerification {
    PairingVerification {
        harness_id: "codex".into(),
        provider_id: "minimax".into(),
        pairing_signature: "signature".into(),
        endpoint: "https://api.minimax.io/v1".into(),
        model_id: "MiniMax-M3".into(),
        auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
        runtime: "native-windows".into(),
        executable: r"C:\npm\codex.cmd".into(),
        codex_version: "0.166.0".into(),
        capability_result: crate::agent::provider::compatibility::CompatibilityDecision {
            compatible: true,
            reason: None,
        },
        status: PairingVerificationStatus::Verified,
        verified_at: Some(chrono::Utc::now()),
        reason: None,
    }
}
