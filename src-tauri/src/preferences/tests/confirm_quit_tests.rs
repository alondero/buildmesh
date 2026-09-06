//! Confirm-before-quit preference (issue #1501) — default-true contract.

use super::super::model::AppPreferences;
use super::super::storage::{load, save};
use super::with_temp_dir;

#[test]
fn confirm_before_quit_defaults_to_true() {
    assert!(AppPreferences::default().confirm_before_quit);
}

#[test]
fn missing_field_in_legacy_json_loads_as_true() {
    with_temp_dir(|tmp| {
        std::fs::write(tmp.join("preferences.json"), "{}").unwrap();
        assert!(load().unwrap().confirm_before_quit);
    });
}

#[test]
fn explicit_false_round_trips() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.confirm_before_quit = false;
        save(prefs).unwrap();
        assert!(!load().unwrap().confirm_before_quit);
    });
}
