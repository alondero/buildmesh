//! Tests for the preferences snapshot — the one read a derivation consumes
//! (issue #2532).

use super::*;
use crate::preferences::tests::with_temp_dir;

/// The resolver accessors read through the snapshot, so the values they
/// return and the snapshot's lists are the same values from the same read.
///
/// This is the shape issue #2532 asks for: the accessor is not a second
/// independent load that could land on a different generation than the
/// derivation's snapshot.
#[test]
fn the_resolver_accessors_read_through_the_snapshot() {
    with_temp_dir(|_| {
        let snapshot = load_snapshot().expect("preferences read");
        assert_eq!(
            super::super::accounts::provider_accounts(),
            snapshot.accounts,
            "provider_accounts must be the snapshot's account list"
        );
        assert_eq!(
            super::super::harness::harness_profiles(),
            snapshot.harness_profiles,
            "harness_profiles must be the snapshot's profile list"
        );
        assert_eq!(
            super::super::pairings::provider_pairings(),
            snapshot.pairings,
            "provider_pairings must be the snapshot's pairing list"
        );
    });
}

/// The snapshot reads preferences once (issue #2532). The counter is
/// per-thread, like the preferences cache itself, so a parallel test on
/// another worker thread leaves this thread's count alone.
#[test]
fn the_snapshot_reads_preferences_exactly_once() {
    with_temp_dir(|_| {
        let before = crate::preferences::load_count();
        let _snapshot = load_snapshot().expect("preferences read");
        assert_eq!(
            crate::preferences::load_count() - before,
            1,
            "the snapshot must read preferences exactly once"
        );
    });
}

/// The snapshot carries the generation it was read at, so a caller can key a
/// memo on it without sampling the counter separately (and racing the read).
#[test]
fn the_snapshot_carries_the_generation_it_was_read_at() {
    with_temp_dir(|_| {
        let snapshot = load_snapshot().expect("preferences read");
        assert_eq!(
            snapshot.generation,
            crate::preferences::storage::generation(),
            "the snapshot must carry the current preferences generation"
        );
    });
}
