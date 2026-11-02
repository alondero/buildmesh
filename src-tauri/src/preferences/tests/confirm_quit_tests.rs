//! Confirm-before-quit preference (issue #1501) — default-true contract.
//!
//! Issue #1482: the suite constructs its own
//! [`super::super::store::PreferencesStore`] over a temporary directory (via
//! the shared [`with_store`] fixture) and passes the handle into the code
//! under test. No test swaps a process-wide current store.

use super::super::model::AppPreferences;
use super::with_store;

#[test]
fn confirm_before_quit_defaults_to_true() {
    assert!(AppPreferences::default().confirm_before_quit);
}

#[test]
fn missing_field_in_legacy_json_loads_as_true() {
    with_store(|store| {
        std::fs::write(store.profile_dir().join("preferences.json"), "{}").unwrap();
        assert!(store.load().unwrap().confirm_before_quit);
    });
}

#[test]
fn explicit_false_round_trips() {
    with_store(|store| {
        let prefs = AppPreferences {
            confirm_before_quit: false,
            ..Default::default()
        };
        store.save(prefs).unwrap();
        assert!(!store.load().unwrap().confirm_before_quit);
    });
}
