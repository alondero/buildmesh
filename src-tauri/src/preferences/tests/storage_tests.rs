//! Tests for the storage layer — load/save/update/cache invariants.

use super::super::model::AppPreferences;
use super::super::storage::{init_for_tests, load, reset_for_tests, save, update};
use super::{test_dir, with_temp_dir};

#[test]
fn preference_files_are_isolated_between_temp_directories() {
    let first_dir = with_temp_dir(|_| {
        save(AppPreferences {
            default_provider: Some("minimax".to_string()),
            ..Default::default()
        })
        .unwrap();
    });

    let second_dir = with_temp_dir(|_| {
        assert_eq!(load().unwrap(), AppPreferences::default());
    });

    assert_ne!(first_dir, second_dir);
}

#[test]
fn load_returns_default_when_file_missing() {
    with_temp_dir(|_| {
        let prefs = load().unwrap();
        assert_eq!(prefs, AppPreferences::default());
        assert_eq!(prefs.default_provider, None);
    });
}

#[test]
fn failed_update_does_not_publish_candidate_to_cache() {
    // Issue #1386: per-thread storage means no `lock_test_state` needed —
    // this test owns its own `APP_DATA_DIR` slot for its duration.
    let app_data_file = test_dir();
    std::fs::write(&app_data_file, "not a directory").unwrap();
    init_for_tests(app_data_file.clone());

    assert_eq!(load().unwrap().default_provider, None);
    let error = update(|prefs| prefs.default_provider = Some("must-not-leak".into())).unwrap_err();
    assert!(error.contains("app data dir") || error.contains("temporary preferences"));
    assert_eq!(load().unwrap().default_provider, None);

    reset_for_tests();
    std::fs::remove_file(app_data_file).unwrap();
}

#[test]
fn save_then_load_round_trip() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.default_provider = Some("claude".to_string());
        prefs.harness_order = vec!["claude".to_string(), "codex".to_string()];
        save(prefs.clone()).unwrap();
        assert_eq!(load().unwrap(), prefs);
    });
}

#[test]
fn default_provider_helper_strips_empty_strings() {
    with_temp_dir(|_| {
        save(AppPreferences {
            default_provider: Some("".to_string()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(super::super::storage::default_provider(), None);
    });
}

#[test]
fn malformed_json_falls_back_to_default() {
    with_temp_dir(|tmp| {
        std::fs::write(tmp.join("preferences.json"), "{not valid json").unwrap();
        assert_eq!(load().unwrap(), AppPreferences::default());
    });
}
