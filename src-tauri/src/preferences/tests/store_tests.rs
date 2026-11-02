//! Tests for the preferences store — the owner of one profile's state
//! (issue #1482).
//!
//! Every test here drives a [`PreferencesStore`] it constructs. The handle is
//! passed in, so a test isolates itself by *owning* a store rather than by
//! swapping an ambient global.
//!
//! The concurrency tests use barriers and channels, not wall-clock sleeps: a
//! sleep cannot establish that two operations actually overlapped, so it
//! cannot establish that the store serialises them correctly. A barrier
//! inside the critical section establishes the overlap directly, and the
//! bounded wait turns "the two locks are shared" into a failure rather than
//! a hang.

use super::super::model::AppPreferences;
use super::super::store::PreferencesStore;
use crate::preferences::PairingVerification;
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::Duration;

/// A store over a fresh temporary directory.
fn store() -> (tempfile::TempDir, PreferencesStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path().to_path_buf());
    (dir, store)
}

/// A store over a directory that is not one — every path under it is
/// unreadable, so a write fails at the I/O layer.
///
/// The file is a named temporary, so it is removed when the handle is
/// dropped: the store's "directory" is a file, and a plain path would leak
/// one file per run.
fn store_over_a_file() -> (tempfile::NamedTempFile, PreferencesStore) {
    let file = tempfile::NamedTempFile::new().unwrap();
    let store = PreferencesStore::new(file.path().to_path_buf());
    (file, store)
}

#[test]
fn a_store_owns_one_immutable_profile_directory() {
    let (dir, store) = store();
    let clone = store.clone();

    assert_eq!(store.profile_dir(), dir.path());
    assert_eq!(clone.profile_dir(), dir.path());
    assert_eq!(
        store.source(),
        clone.source(),
        "a cloned handle addresses the same source"
    );
    assert_eq!(
        store.preferences_path(),
        dir.path().join("preferences.json")
    );
}

#[test]
fn two_stores_over_the_same_directory_are_still_two_sources() {
    let dir = tempfile::tempdir().unwrap();
    let first = PreferencesStore::new(dir.path().to_path_buf());
    let second = PreferencesStore::new(dir.path().to_path_buf());

    assert_ne!(
        first.source(),
        second.source(),
        "each store owns its own cache, revision and locks, so each is its own \
         source — a revision from one is not comparable with the other's"
    );
}

#[test]
fn the_revision_advances_only_after_a_durable_write() {
    let (_dir, store) = store();
    assert_eq!(store.revision(), 0, "a fresh store has published nothing");

    store.save(AppPreferences::default()).unwrap();
    assert_eq!(store.revision(), 1);

    store
        .update(|prefs| prefs.default_provider = Some("claude".into()))
        .unwrap();
    assert_eq!(store.revision(), 2);
}

#[test]
fn a_refused_write_leaves_the_previous_value_and_revision_authoritative() {
    // The store's directory is a *file*, so the write is refused at the I/O
    // layer. The point is that the refusal is total: the value that was
    // already published stays authoritative, and the revision does not move —
    // a revision that advanced on a failed write would let a derived cache
    // believe the failed value was persisted.
    let (_file, store) = store_over_a_file();
    store
        .save(AppPreferences {
            default_provider: Some("claude".into()),
            ..Default::default()
        })
        .expect_err("a store over a file cannot write");
    assert_eq!(store.revision(), 0);
}

#[test]
fn a_refused_write_publishes_no_pairing_verification() {
    // A pairing verification that never reached the disk must not be visible
    // in memory either: launch preflight would read it as proof that the
    // pairing was checked.
    let (_file, store) = store_over_a_file();
    let before = store.revision();

    let error = store
        .update(|prefs| {
            prefs.pairing_verifications.push(PairingVerification {
                harness_id: "claude".into(),
                provider_id: "minimax".into(),
                pairing_signature: "sig".into(),
                endpoint: "https://example.invalid".into(),
                model_id: "model".into(),
                auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
                runtime: "native-windows".into(),
                executable: "codex.cmd".into(),
                codex_version: "0.0.0".into(),
                capability_result: crate::agent::provider::compatibility::CompatibilityDecision {
                    compatible: true,
                    reason: None,
                },
                status: crate::preferences::PairingVerificationStatus::Verified,
                verified_at: None,
                reason: None,
            });
        })
        .expect_err("a store over a file cannot write");

    assert!(
        !error.contains("PREFERENCES_CORRUPT"),
        "an unreadable path is not a corrupt file, got {error:?}"
    );
    assert_eq!(
        store.revision(),
        before,
        "a failed write must not advance the revision"
    );
    assert!(
        store
            .load()
            .map(|prefs| prefs.pairing_verifications.is_empty())
            .unwrap_or(true),
        "a failed write must not publish a verification record"
    );
}

#[test]
fn two_threads_updating_distinct_fields_both_persist() {
    // The store serialises competing read-modify-write operations: the
    // second thread's candidate is built on the first thread's published
    // value, so neither update is lost. The barrier establishes that both
    // threads start together — a sleep could not establish that the two
    // updates actually overlapped.
    let (_dir, store) = store();
    let barrier = Arc::new(Barrier::new(2));

    let first = store.clone();
    let first_barrier = barrier.clone();
    let one = std::thread::spawn(move || {
        first_barrier.wait();
        first
            .update(|prefs| prefs.default_provider = Some("claude".into()))
            .unwrap();
    });

    let second = store.clone();
    let second_barrier = barrier.clone();
    let two = std::thread::spawn(move || {
        second_barrier.wait();
        second
            .update(|prefs| prefs.reviewer_provider = Some("codex".into()))
            .unwrap();
    });

    one.join().unwrap();
    two.join().unwrap();

    let prefs = store.load().unwrap();
    assert_eq!(
        prefs.default_provider.as_deref(),
        Some("claude"),
        "the first thread's update must survive the second"
    );
    assert_eq!(
        prefs.reviewer_provider.as_deref(),
        Some("codex"),
        "the second thread's update must survive the first"
    );
    assert_eq!(
        store.revision(),
        2,
        "each durable write advances the revision exactly once"
    );
}

#[test]
fn unrelated_stores_do_not_serialize_against_each_other() {
    // Each store owns its own state lock, so two stores can be inside their
    // critical sections at the same time. The rendezvous sits *inside* the
    // mutator: the mutator runs while the state lock is held, so both threads
    // reaching it at once is the proof that the lock is per store. If the two
    // stores shared it, the second thread could never reach the rendezvous and
    // the first would wait out its bounded timeout — a failure, not a hang.
    //
    // This pins the *state* lock. The write lock is per store by construction
    // — it lives in `StoreInner`, next to the cache, so a clone shares it and
    // a second store cannot — and it is not observed here: the durable write
    // has no test hook, and adding one would put a `cfg(test)` seam back into
    // the store, which is the fork issue #1482 removes.
    let (_first_dir, first) = store();
    let (_second_dir, second) = store();
    let (first_inside_tx, first_inside_rx) = mpsc::channel::<()>();
    let (second_inside_tx, second_inside_rx) = mpsc::channel::<()>();

    let first_handle = first.clone();
    let one = std::thread::spawn(move || {
        first_handle
            .update(|_| {
                first_inside_tx.send(()).expect("signal the other thread");
                second_inside_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("two unrelated stores must not share a lock");
            })
            .unwrap();
    });

    let second_handle = second.clone();
    let two = std::thread::spawn(move || {
        second_handle
            .update(|_| {
                second_inside_tx.send(()).expect("signal the other thread");
                first_inside_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("two unrelated stores must not share a lock");
            })
            .unwrap();
    });

    one.join().unwrap();
    two.join().unwrap();
}

#[test]
fn a_cloned_handle_reads_the_same_profile_from_a_spawned_thread() {
    // The handle is a pointer to the store, not a copy of it: the worker
    // shares the parent's cache, revision and locks. Before the store existed
    // this was the gap the issue names — a spawned thread consulted a
    // thread-local preferences source and read an uninitialized one.
    let (_dir, store) = store();
    store
        .save(AppPreferences {
            default_provider: Some("claude".into()),
            ..Default::default()
        })
        .unwrap();

    let worker = store.clone();
    let seen = std::thread::spawn(move || worker.load().unwrap().default_provider)
        .join()
        .unwrap();
    assert_eq!(
        seen.as_deref(),
        Some("claude"),
        "a cloned handle must read the profile the parent wrote"
    );

    // And a write from the worker is visible to the parent.
    let worker = store.clone();
    std::thread::spawn(move || {
        worker
            .update(|prefs| prefs.reviewer_provider = Some("codex".into()))
            .unwrap();
    })
    .join()
    .unwrap();
    assert_eq!(
        store.load().unwrap().reviewer_provider.as_deref(),
        Some("codex"),
        "the parent must see the worker's write"
    );
    assert_eq!(
        store.revision(),
        2,
        "the worker's write advanced the revision"
    );
}

#[test]
fn two_stores_with_equal_revision_numbers_stay_independent() {
    // Revisions are per store, so two stores can sit at the same revision
    // while holding different preferences. Neither may serve the other's
    // value, and neither write may disturb the other.
    let (_first_dir, first) = store();
    let (_second_dir, second) = store();

    first
        .save(AppPreferences {
            default_provider: Some("claude".into()),
            ..Default::default()
        })
        .unwrap();
    second
        .save(AppPreferences {
            default_provider: Some("codex".into()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        first.revision(),
        second.revision(),
        "both stores are at revision 1 — the numbers are not comparable"
    );
    assert_eq!(
        first.load().unwrap().default_provider.as_deref(),
        Some("claude")
    );
    assert_eq!(
        second.load().unwrap().default_provider.as_deref(),
        Some("codex")
    );
}

#[test]
fn the_snapshot_pairs_its_value_with_its_own_revision() {
    // The value and the revision are sampled together under the store's own
    // lock, so a reader can never pair a value with another revision.
    let (_dir, store) = store();
    store
        .save(AppPreferences {
            default_provider: Some("claude".into()),
            ..Default::default()
        })
        .unwrap();

    let (value, revision) = store.load_with_revision().unwrap();
    assert_eq!(value.default_provider.as_deref(), Some("claude"));
    assert_eq!(revision, store.revision());
    assert_eq!(revision, 1);
}

#[test]
fn a_corrupt_file_is_served_as_defaults_but_never_published() {
    let (_dir, store) = store();
    std::fs::write(store.preferences_path(), b"{not valid json").unwrap();

    let prefs = store.load().unwrap();
    assert_eq!(
        prefs,
        AppPreferences::default(),
        "a corrupt file still serves defaults to a read-only caller"
    );
    assert_eq!(
        std::fs::read_to_string(store.preferences_path()).unwrap(),
        "{not valid json",
        "a read must not touch the corrupt file"
    );
    assert!(
        store
            .update(|prefs| prefs.default_provider = Some("claude".into()))
            .is_err(),
        "an ordinary write must be refused over a corrupt file"
    );
    assert_eq!(
        store.revision(),
        0,
        "a refused write must not advance the revision"
    );
}

#[test]
fn a_store_and_its_clone_share_one_cache() {
    let (_dir, store) = store();
    let clone = store.clone();

    store
        .update(|prefs| prefs.default_provider = Some("claude".into()))
        .unwrap();

    assert_eq!(
        clone.load().unwrap().default_provider.as_deref(),
        Some("claude"),
        "a clone shares the cache, not a copy of it"
    );
    assert_eq!(clone.revision(), store.revision());
}
