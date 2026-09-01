//! Shared test fixtures for the `preferences` submodules.
//!
//! Issue #1386: each `cargo test` worker thread owns its own `APP_DATA_DIR`
//! and `CACHE` (per-thread `RefCell` under `cfg(test)`), so concurrent tests
//! never collide on the same static. No `TEST_LOCK` is needed — every test
//! pointing [`with_temp_dir`] at a unique [`test_dir()`] is fully isolated
//! from its siblings, including prefs-touching tests in other modules (e.g.
//! `services::provider_verification`).

use super::storage::{init_for_tests, reset_for_tests};
use std::path::PathBuf;

// Per-feature test files. Each tests a single concern; the cross-module
// fixtures live here.
mod accounts_tests;
mod catalog_tests;
mod compatibility_tests;
mod default_provider_tests;
mod harness_tests;
mod migrations_tests;
mod pairings_tests;
mod pairing_compat_tests;
mod storage_tests;

static TEST_DIR_COUNTER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn test_dir() -> PathBuf {
    let id = TEST_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "buildmesh-prefs-test-{}-{id}",
        std::process::id()
    ))
}

pub(crate) fn with_temp_dir<F: FnOnce(&PathBuf)>(f: F) -> PathBuf {
    // No TEST_LOCK: per-thread `APP_DATA_DIR` (issue #1386). Two parallel
    // tests run on different threads and each write to its own
    // `test_dir()`-derived path, so the thread-local statics stay clean.
    let tmp = test_dir();
    std::fs::create_dir_all(&tmp).unwrap();

    init_for_tests(tmp.clone());

    f(&tmp);

    reset_for_tests();
    let _ = std::fs::remove_dir_all(&tmp);
    tmp
}