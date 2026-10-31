//! Tests for `commands::preferences`.
//!
//! The owner keeps only the sibling module declaration; the bodies live here
//! so the production file carries no test code (the repo's rust-test-layout
//! gate).

use super::*;

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
