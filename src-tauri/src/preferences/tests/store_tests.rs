//! Explicit-handle tests for [`PreferencesStore`] (issue #1482).
//!
//! These suites construct a store over a temporary directory and drive it
//! directly, the way production will once the compatibility free functions are
//! gone. They assert the store-level invariants — one source, one cache, one
//! revision — including the multithreaded cases that the old thread-local
//! `cfg(test)` cache could not exercise at all.

use super::super::model::{AppPreferences, ProviderAccount};
use super::super::store::PreferencesStore;
use super::test_dir;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// A store over a fresh, uniquely named temporary directory.
fn temp_store() -> (PreferencesStore, PathBuf) {
    let dir = test_dir();
    std::fs::create_dir_all(&dir).unwrap();
    (PreferencesStore::new(dir.clone()), dir)
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// A non-builtin account carrying a plaintext key, for the credential-profile
/// tests. The key is moved to the credential store on save.
fn keyed_account(id: &str, api_key: &str) -> ProviderAccount {
    ProviderAccount {
        id: id.to_string(),
        name: id.to_string(),
        enabled: true,
        billing_mode: Default::default(),
        claude_compatible: true,
        api_key: Some(api_key.to_string()),
    }
}

/// Two threads mutating distinct fields through the **same handle** must both
/// land. The cache mutex serialises the read-modify-write, so the second
/// mutator sees the first one's value; a lost update would drop one field.
#[test]
fn two_threads_updating_distinct_fields_on_one_handle_both_persist() {
    let (store, dir) = temp_store();
    let barrier = Arc::new(std::sync::Barrier::new(2));

    let provider_writer = {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            store
                .update(|prefs| prefs.default_provider = Some("claude".to_string()))
                .expect("provider write");
        })
    };
    let reviewer_writer = {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            store
                .update(|prefs| prefs.reviewer_provider = Some("codex".to_string()))
                .expect("reviewer write");
        })
    };
    provider_writer.join().unwrap();
    reviewer_writer.join().unwrap();

    let saved = store.load().expect("load after concurrent writes");
    assert_eq!(saved.default_provider.as_deref(), Some("claude"));
    assert_eq!(saved.reviewer_provider.as_deref(), Some("codex"));

    // Not just the cache: a fresh store over the same directory sees both.
    let reopened = PreferencesStore::new(dir.clone());
    let persisted = reopened.load().expect("reload from disk");
    assert_eq!(persisted.default_provider.as_deref(), Some("claude"));
    assert_eq!(persisted.reviewer_provider.as_deref(), Some("codex"));

    cleanup(&dir);
}

/// Distinct stores must not share write coordination. Each mutator announces
/// itself and waits for the other's signal, so the test fails (on a timeout,
/// not a hang) if one store's update blocks the other's.
#[test]
fn distinct_stores_do_not_serialise_each_other() {
    let (first, first_dir) = temp_store();
    let (second, second_dir) = temp_store();

    let (first_in_mutator, first_seen) = mpsc::channel::<()>();
    let (second_in_mutator, second_seen) = mpsc::channel::<()>();
    let first_observed = Arc::new(AtomicBool::new(false));
    let second_observed = Arc::new(AtomicBool::new(false));

    let first_thread = {
        let store = first.clone();
        let observed = Arc::clone(&first_observed);
        let (mine, theirs) = (first_in_mutator, second_seen);
        std::thread::spawn(move || {
            store
                .update(|prefs| {
                    mine.send(()).ok();
                    let overlapped = theirs.recv_timeout(Duration::from_secs(5)).is_ok();
                    observed.store(overlapped, Ordering::Relaxed);
                    prefs.default_provider = Some("first".to_string());
                })
                .expect("first update");
        })
    };
    let second_thread = {
        let store = second.clone();
        let observed = Arc::clone(&second_observed);
        let (mine, theirs) = (second_in_mutator, first_seen);
        std::thread::spawn(move || {
            store
                .update(|prefs| {
                    mine.send(()).ok();
                    let overlapped = theirs.recv_timeout(Duration::from_secs(5)).is_ok();
                    observed.store(overlapped, Ordering::Relaxed);
                    prefs.default_provider = Some("second".to_string());
                })
                .expect("second update");
        })
    };
    first_thread.join().unwrap();
    second_thread.join().unwrap();

    assert!(
        first_observed.load(Ordering::Relaxed) && second_observed.load(Ordering::Relaxed),
        "both mutators must run at the same time; unrelated stores shared a lock"
    );
    assert_eq!(
        first.load().unwrap().default_provider.as_deref(),
        Some("first")
    );
    assert_eq!(
        second.load().unwrap().default_provider.as_deref(),
        Some("second")
    );
    assert_ne!(first.source(), second.source());

    cleanup(&first_dir);
    cleanup(&second_dir);
}

/// A cloned handle moved into a spawned OS thread addresses the same profile,
/// not an uninitialized per-thread source. This is the case the old
/// thread-local `APP_DATA_DIR` could not satisfy.
#[test]
fn a_cloned_handle_reads_the_same_profile_from_a_spawned_thread() {
    let (store, dir) = temp_store();
    store
        .save(AppPreferences {
            default_provider: Some("claude".to_string()),
            ..Default::default()
        })
        .expect("save");

    let source = store.source();
    let revision = store.revision();
    let clone = store.clone();
    let (seen_source, seen_revision, seen_provider) = std::thread::spawn(move || {
        let snapshot = clone.snapshot().expect("read from worker");
        (
            clone.source(),
            snapshot.revision,
            snapshot.preferences.default_provider,
        )
    })
    .join()
    .unwrap();

    assert_eq!(
        seen_source, source,
        "the clone must keep its source identity"
    );
    assert_eq!(seen_revision, revision, "the clone must share the revision");
    assert_eq!(seen_provider.as_deref(), Some("claude"));

    cleanup(&dir);
}

/// A profile's snapshot pairs its value with the revision it was published
/// at, and both writers advance the revision only after the durable write.
#[test]
fn a_snapshot_pairs_its_value_with_its_revision() {
    let (store, dir) = temp_store();

    let initial = store.snapshot().expect("cold snapshot");
    let after_update = store
        .update(|prefs| prefs.default_provider = Some("claude".to_string()))
        .expect("update");
    assert!(
        after_update.revision > initial.revision,
        "a durable publish must advance the revision ({} -> {})",
        initial.revision,
        after_update.revision
    );
    assert_eq!(after_update.source, initial.source);

    let reread = store.snapshot().expect("warm snapshot");
    assert_eq!(reread.revision, after_update.revision);
    assert_eq!(reread.preferences, after_update.preferences);

    cleanup(&dir);
}

/// A failed durable replacement leaves the previous value and revision
/// authoritative: nothing is published in memory that was not persisted.
#[test]
fn a_failed_write_does_not_publish_a_new_value_or_revision() {
    // A *file* where the profile directory should be makes every write fail.
    let dir = test_dir();
    std::fs::write(&dir, "not a directory").unwrap();
    let store = PreferencesStore::new(dir.clone());

    let before = store.snapshot().expect("a read-only fallback is allowed");
    let error = store
        .update(|prefs| prefs.default_provider = Some("must-not-leak".to_string()))
        .expect_err("the write must fail");
    assert!(
        !error.contains("PREFERENCES_CORRUPT"),
        "an unreadable file is not a corrupt one, got {error:?}"
    );

    let after = store.snapshot().expect("cache still serves the old value");
    assert_eq!(after.revision, before.revision, "no publish, no revision");
    assert_eq!(after.preferences, before.preferences);

    std::fs::remove_file(&dir).unwrap();
}

/// A corrupt file keeps read-only defaults available, refuses an ordinary
/// write, and leaves the bytes exactly as they were.
#[test]
fn corrupt_bytes_keep_reads_working_and_refuse_writes() {
    let (store, dir) = temp_store();
    let original = "{not valid json";
    std::fs::write(dir.join("preferences.json"), original).unwrap();

    let before = store.snapshot().expect("read falls back to defaults");
    assert_eq!(before.preferences.default_provider, None);
    assert!(before.preferences.spawn_configurations.is_empty());

    let error = store
        .update(|prefs| prefs.default_provider = Some("must-not-land".to_string()))
        .expect_err("a corrupt file must refuse ordinary writes");
    assert!(error.contains("PREFERENCES_CORRUPT"), "got {error:?}");

    assert_eq!(store.revision(), before.revision);
    assert_eq!(
        std::fs::read_to_string(dir.join("preferences.json")).unwrap(),
        original,
        "the corrupt bytes must be preserved for recovery"
    );

    cleanup(&dir);
}

/// Each store backs its own profile: values and credential-store entries do
/// not cross even when both stores are alive at once.
#[test]
fn distinct_stores_keep_values_and_credentials_separate() {
    let (first, first_dir) = temp_store();
    let (second, second_dir) = temp_store();

    first
        .save(AppPreferences {
            provider_accounts: vec![keyed_account("custom", "sk-first")],
            ..Default::default()
        })
        .expect("first save");
    second
        .save(AppPreferences {
            provider_accounts: vec![keyed_account("custom", "sk-second")],
            ..Default::default()
        })
        .expect("second save");

    // Fresh handles force a cold read through the credential seam, keyed on
    // each store's own profile directory.
    let first_reopened = PreferencesStore::new(first_dir.clone());
    let second_reopened = PreferencesStore::new(second_dir.clone());
    let first_key = first_reopened
        .load()
        .unwrap()
        .provider_accounts
        .into_iter()
        .find(|account| account.id == "custom")
        .and_then(|account| account.api_key);
    let second_key = second_reopened
        .load()
        .unwrap()
        .provider_accounts
        .into_iter()
        .find(|account| account.id == "custom")
        .and_then(|account| account.api_key);

    assert_eq!(first_key.as_deref(), Some("sk-first"));
    assert_eq!(second_key.as_deref(), Some("sk-second"));

    cleanup(&first_dir);
    cleanup(&second_dir);
}

/// Revisions come from one process-wide sequence, so two live stores never
/// report the same number even when their values are identical. That is what
/// makes the `(source, revision)` pair a derived cache keys on a total
/// identity: there is no equal-revision pair for two profiles to collide on.
#[test]
fn distinct_stores_never_share_a_revision() {
    let (first, first_dir) = temp_store();
    let (second, second_dir) = temp_store();
    assert_ne!(
        first.identity(),
        second.identity(),
        "two stores must not share a source/revision pair"
    );

    let identical = AppPreferences {
        default_provider: Some("claude".to_string()),
        ..Default::default()
    };
    let first_snapshot = first.save(identical.clone()).expect("first save");
    let second_snapshot = second.save(identical).expect("second save");
    assert_eq!(first_snapshot.preferences, second_snapshot.preferences);
    assert_ne!(
        first_snapshot.revision, second_snapshot.revision,
        "equal values must still carry distinct revisions"
    );

    cleanup(&first_dir);
    cleanup(&second_dir);
}

/// Reset is an explicit recovery action: it replaces the value, advances the
/// revision, and leaves the store serving the new value.
#[test]
fn reset_republishes_defaults_and_advances_the_revision() {
    let (store, dir) = temp_store();
    store
        .save(AppPreferences {
            default_provider: Some("claude".to_string()),
            ..Default::default()
        })
        .expect("save");
    let before = store.snapshot().expect("snapshot before reset");

    let outcome = store.reset().expect("reset");
    assert_eq!(outcome.preferences.default_provider, None);

    let after = store.snapshot().expect("snapshot after reset");
    assert_eq!(after.preferences.default_provider, None);
    assert!(after.revision > before.revision);

    cleanup(&dir);
}
