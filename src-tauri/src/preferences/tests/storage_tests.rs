//! Tests for the storage layer — load/save/update/cache invariants.
//!
//! Issue #1482: every test here drives a [`PreferencesStore`] it owns. The
//! handle is passed in, so the test isolates itself by *constructing* its
//! own store rather than by swapping an ambient global.

use super::super::model::AppPreferences;
use super::super::store::PreferencesStore;
use super::{profile_dir_name, test_dir, test_dir_for, with_store, TEST_DIR_COUNTER};

#[test]
fn preference_files_are_isolated_between_temp_directories() {
    let first_dir = with_store(|store| {
        store
            .save(AppPreferences {
                default_provider: Some("minimax".to_string()),
                ..Default::default()
            })
            .unwrap();
    });

    let second_dir = with_store(|store| {
        assert!(store.load().unwrap().spawn_configurations.is_empty());
    });

    assert_ne!(first_dir, second_dir);
}

/// Issue #2220: the profile fixture used to name its directories
/// `buildmesh-prefs-test-<pid>-<counter>`, and Windows recycles process ids.
/// A run whose pid matched an earlier run's computed the same name and
/// adopted that run's leftover profile directory — a `preferences.json` it
/// had not written and would read back as the current state.
///
/// The fixture must step around a survivor instead of adopting it.
#[test]
fn a_profile_fixture_steps_around_a_directory_an_earlier_run_left_behind() {
    let id = TEST_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // An earlier run with the same (recycled) process id left this behind,
    // under the name that run's fixture computed. This is the name the
    // fixture would hand out if it were built from the pid alone, so
    // adopting it would reproduce the reported failure exactly.
    let legacy =
        std::env::temp_dir().join(format!("buildmesh-prefs-test-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("earlier-run.txt"), "not mine to delete\n").unwrap();

    // This run's own name is occupied too, so the claim has to step around a
    // survivor rather than adopt it.
    let survivor = std::env::temp_dir().join(profile_dir_name(id));
    std::fs::create_dir_all(&survivor).unwrap();
    std::fs::write(survivor.join("earlier-run.txt"), "not mine to delete\n").unwrap();

    let claimed = test_dir_for(id);
    assert_ne!(
        claimed, legacy,
        "the fixture adopted the directory an earlier run left behind"
    );
    assert_ne!(
        claimed, survivor,
        "the fixture adopted a directory an earlier run left behind"
    );
    assert!(
        legacy.join("earlier-run.txt").exists(),
        "claiming a profile directory must not delete a survivor"
    );
    assert!(
        survivor.join("earlier-run.txt").exists(),
        "claiming a profile directory must not delete a survivor"
    );

    // The seeded survivors are removed through the retrying helper: a plain
    // delete can lose the same transient refusal the fixture rides out, and
    // then this test leaves litter in %TEMP% — the very thing it inspects.
    let _ = crate::env::test_helpers::remove_dir_all_with_retry(&legacy);
    let _ = crate::env::test_helpers::remove_dir_all_with_retry(&survivor);
}

#[test]
fn load_returns_default_when_file_missing() {
    with_store(|store| {
        let prefs = store.load().unwrap();
        assert!(prefs.spawn_configurations.is_empty());
        assert_eq!(prefs.default_provider, None);
        assert!(!store.preferences_path().exists());
    });
}

#[test]
fn failed_update_does_not_publish_candidate_to_cache() {
    // The store's directory is a *file*, so every path under it is
    // unreadable — the write fails at the I/O layer.
    let app_data_file = test_dir();
    std::fs::write(&app_data_file, "not a directory").unwrap();
    let store = PreferencesStore::new(app_data_file.clone());

    let loaded = store.load().unwrap();
    assert_eq!(loaded.default_provider, None);
    let error = store
        .update(|prefs| prefs.default_provider = Some("must-not-leak".into()))
        .unwrap_err();
    // The gate refuses an I/O failure too, but as a plain I/O error — not as
    // the issue-#1523 corruption refusal, which would tell the user their
    // file was damaged when it was not. The point of the assertion is that
    // the two are distinguishable and nothing reached the cache.
    assert!(
        !error.contains("PREFERENCES_CORRUPT"),
        "an unreadable file is not a corrupt one, got {error:?}"
    );
    assert_eq!(store.load().unwrap().default_provider, None);

    std::fs::remove_file(app_data_file).unwrap();
}

#[test]
fn save_then_load_round_trip() {
    with_store(|store| {
        let mut prefs = AppPreferences {
            default_provider: Some("claude".to_string()),
            harness_order: vec!["claude".to_string(), "codex".to_string()],
            ..Default::default()
        };
        store.save(prefs.clone()).unwrap();
        super::super::launch_configurations::reconcile(&mut prefs);
        assert_eq!(store.load().unwrap(), prefs);
    });
}

#[test]
fn default_provider_helper_strips_empty_strings() {
    with_store(|store| {
        store
            .save(AppPreferences {
                default_provider: Some("".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(store.default_provider(), None);
    });
}

#[test]
fn reviewer_provider_helper_strips_blank_strings() {
    with_store(|store| {
        store
            .save(AppPreferences {
                reviewer_provider: Some("  codex  ".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(store.reviewer_provider().as_deref(), Some("codex"));

        store
            .save(AppPreferences {
                reviewer_provider: Some(" \t".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(store.reviewer_provider(), None);
    });
}

/// A malformed file still yields defaults to a *reader* — spawn routing, the
/// circuit classifier, and the usage panel all keep working rather than
/// failing closed on a file the user can still repair. Those defaults are
/// not published to the writable cache, and no ordinary write can land on
/// top of the file; the refusal matrix lives in `recovery_tests`.
#[test]
fn malformed_json_read_falls_back_to_defaults_without_touching_the_file() {
    with_store(|store| {
        let original = "{not valid json";
        std::fs::write(store.preferences_path(), original).unwrap();
        let prefs = store.load().unwrap();
        assert_eq!(
            prefs.default_provider,
            AppPreferences::default().default_provider
        );
        assert!(prefs.spawn_configurations.is_empty());
        assert_eq!(
            std::fs::read_to_string(store.preferences_path()).unwrap(),
            original
        );
    });
}

#[test]
fn type_invalid_preferences_are_not_overwritten_by_a_read() {
    with_store(|store| {
        let original = r#"{"spawn_configurations":"not-an-array"}"#;
        std::fs::write(store.preferences_path(), original).unwrap();
        let prefs = store.load().unwrap();
        assert!(prefs.spawn_configurations.is_empty());
        assert_eq!(
            std::fs::read_to_string(store.preferences_path()).unwrap(),
            original
        );
    });
}

/// A deleted launch configuration is not a reviewer provider.
///
/// This one drives a production code path (`circuit::compatibility`) that
/// reads preferences, so it passes the store it owns into the store-scoped
/// twin rather than relying on an ambient store.
#[test]
fn deleted_launch_configuration_is_not_accepted_as_reviewer_provider() {
    with_store(|store| {
        let error = crate::circuit::compatibility::validate_reviewer_provider_id_from(
            store,
            "launch/deleted",
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Launch Configuration no longer exists; select another configuration"
        );
    });
}
