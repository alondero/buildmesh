//! Shared test fixtures for the `preferences` submodules.
//!
//! Issue #1482: the preferences state belongs to a [`PreferencesStore`] — one
//! handle per profile directory, owning the cached value, the revision and
//! the locks. A test that owns its state constructs a store over a temporary
//! directory and passes the handle in; it never mutates an ambient global to
//! achieve isolation.
//!
//! There is deliberately no "install a store for this thread" fixture: issue
//! #1482 forbids a test-only ambient override, and a thread-local slot would
//! not be inherited by a worker thread anyway. A test that needs a worker to
//! see the same preferences clones the handle and passes it in.

use super::store::PreferencesStore;
use std::path::PathBuf;

// Per-feature test files. Each tests a single concern; the cross-module
// fixtures live here.
//
// A module whose tests moved next to it keeps them in a sibling `*_tests.rs`
// (issue #2532): `resolver::accounts`, `resolver::catalog`, `resolver::harness`
// and `resolver::pairings` own theirs, so a scoped check that selects
// `preferences::resolver::<module>::` finds the tests that cover it.
mod compatibility_tests;
mod confirm_quit_tests;
mod default_provider_tests;
mod migrations_tests;
mod pairing_compat_tests;
mod recovery_tests;
mod secrets_tests;
mod storage_tests;
mod store_tests;

static TEST_DIR_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn test_dir() -> PathBuf {
    let id = TEST_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("buildmesh-prefs-test-{}-{id}", std::process::id()))
}

/// Run `f` with a store over a fresh temporary directory.
///
/// The store is a real [`PreferencesStore`] — the same implementation
/// production uses — so the code under test runs against production's
/// synchronization. Only the *instance* is test-scoped, and the handle is
/// handed to the test explicitly.
pub(crate) fn with_store<F: FnOnce(&PreferencesStore)>(f: F) -> PathBuf {
    let tmp = test_dir();
    std::fs::create_dir_all(&tmp).unwrap();
    let store = PreferencesStore::new(tmp.clone());
    // The credential-store double is per-thread; a fresh vault keeps the
    // secrets a test seeds invisible to its siblings.
    super::secrets::test_support::reset();
    f(&store);
    super::secrets::test_support::reset();
    let _ = std::fs::remove_dir_all(&tmp);
    tmp
}
