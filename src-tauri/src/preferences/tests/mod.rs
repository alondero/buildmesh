//! Shared test fixtures for the `preferences` submodules.
//!
//! Tests in this module share `APP_DATA_DIR` and `CACHE` global state, so
//! they must run serially. The shared [`TEST_LOCK`] gates everything;
//! [`lock_test_state`] is the seam other test files use to take it.
//!
//! Splitting the monolithic inline `tests` mod into per-feature files keeps
//! each file small, but the fixtures need to live somewhere they can be
//! imported — this file is that somewhere.

use super::storage::{init_for_tests, reset_for_tests};
use std::path::PathBuf;
use std::sync::Mutex as TestMutex;

// Per-feature test files. Each tests a single concern; the cross-module
// fixtures live here.
mod accounts;
mod catalog_tests;
mod compatibility_tests;
mod default_provider;
mod harness_tests;
mod migrations_tests;
mod pairings;
mod storage_tests;

/// Tests in this module share `APP_DATA_DIR` and `CACHE` global state, so
/// they must run serially. Other modules reach this via
/// [`super::storage::test_state_guard`].
pub(crate) static TEST_LOCK: TestMutex<()> = TestMutex::new(());
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
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = test_dir();
    std::fs::create_dir_all(&tmp).unwrap();

    init_for_tests(tmp.clone());

    f(&tmp);

    reset_for_tests();
    let _ = std::fs::remove_dir_all(&tmp);
    tmp
}

pub(crate) fn lock_test_state() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}