//! Tests for the preferences snapshot — the one read a derivation consumes
//! (issue #2532).

use super::*;
use crate::preferences::tests::with_temp_dir;

/// The resolver accessors read through the snapshot, so the values they
/// return and the snapshot's lists are the same values from the same read.
///
/// This is the shape issue #2532 asks for: the accessor is not a second
/// independent load that could land on a different read than the
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
            super::super::pairings::provider_pairings().as_slice(),
            snapshot.pairings(),
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

/// The snapshot carries the revision and the source identity it was read at,
/// so a caller can key a memo on them without sampling the store separately
/// (and racing the read).
///
/// The revision belongs to the store, so two stores are independent: the
/// snapshot's revision is the one *that* store published, and a parallel
/// worker's write to another store cannot move it.
#[test]
fn the_snapshot_carries_the_revision_and_source_it_was_read_at() {
    crate::preferences::tests::with_store(|store| {
        let before = store.revision();
        let snapshot = load_snapshot_from(store).expect("preferences read");
        let after = store.revision();
        assert!(
            snapshot.revision >= before && snapshot.revision <= after,
            "the snapshot must carry a revision from its own read: {before} <= {} <= {after}",
            snapshot.revision
        );
        assert_eq!(
            snapshot.source,
            *store.source(),
            "the snapshot must carry the identity of the store it read from"
        );
    });
}
