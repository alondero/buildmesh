//! Tests for the spawn configurations — validation, sparse-value round trips
//! and the atomic configuration + Provider Route save.
//!
//! Moved out of the owner file so the rust-test-layout gate holds, as
//! `launch_configurations_tests.rs` was.
//!
//! Issue #1482: every test here constructs its own
//! [`super::super::store::PreferencesStore`] over a temporary directory and
//! passes the handle in. No test swaps a process-wide current store.

use super::*;

fn configuration(option: &str) -> SpawnConfiguration {
    SpawnConfiguration {
        id: "sol".into(),
        name: " Sol ".into(),
        spawn_option_id: option.into(),
        model: Some(" gpt-5.6-sol ".into()),
        effort: None,
        extra_args: None,
        ..Default::default()
    }
}

#[test]
fn mcode_configuration_rejects_shell_characters_and_accepts_variants() {
    let mut value = configuration("mcode");
    value.model = Some(" minimax/MiniMax-M3#variant ".into());
    assert_eq!(
        validate(value.clone()).unwrap().model.as_deref(),
        Some("minimax/MiniMax-M3#variant")
    );

    value.model = Some("minimax/M3&echo x".into());
    assert!(validate(value).unwrap_err().contains("model reference"));

    let mut prefs = AppPreferences::default();
    let valid = super::super::HarnessConfigValue {
        model: Some(" minimax/MiniMax-M3#variant ".into()),
        effort: None,
        permission_mode: None,
        agent: None,
    };
    super::super::upsert_harness_default(&mut prefs, "mcode", valid).unwrap();
    let saved = prefs.clone();
    let invalid = super::super::HarnessConfigValue {
        model: Some("minimax/%PATH%".into()),
        effort: None,
        permission_mode: None,
        agent: None,
    };
    assert!(
        super::super::upsert_harness_default(&mut prefs, "mcode", invalid)
            .unwrap_err()
            .contains("model reference")
    );
    assert_eq!(prefs, saved);
}

#[test]
fn configuration_and_pairing_save_atomically_and_preserve_other_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = super::super::store::PreferencesStore::new(tmp.path().to_path_buf());
    // The credential-store double is per-thread; a fresh vault keeps the
    // seeded key invisible to the sibling suites.
    super::super::secrets::test_support::reset();
    let mut prefs: AppPreferences = serde_json::from_value(serde_json::json!({
        "provider_accounts":[{"id":"minimax","name":"MiniMax","enabled":true,"api_key":"test","billing_mode":"pay_as_you_go"}],
        "detached_provider_routes":["claude:minimax", "codex:minimax"]
    })).unwrap();
    store.save(prefs.clone()).unwrap();
    let endpoint = super::super::first_class_surfaces("minimax").remove(0);
    let route = super::super::ProviderPairing {
        harness_id: "claude".into(),
        provider_id: "minimax".into(),
        surface: endpoint.surface,
        base_url: Some(endpoint.base_url),
        model_tiers: endpoint.model_tiers,
    };
    let mut value = configuration("claude:minimax");
    value.model = Some("MiniMax-M2.7-highspeed".into());
    value.effort = Some("high".into());
    assert!(
        save_with_route_from(&store, value.clone(), Some(route.clone()))
            .unwrap_err()
            .contains("Effort")
    );
    prefs = store.load().unwrap();
    assert!(prefs.provider_pairings.is_empty());
    assert!(prefs.spawn_configurations.is_empty());
    value.effort = None;
    let saved = save_with_route_from(&store, value.clone(), Some(route.clone())).unwrap();
    prefs = store.load().unwrap();
    assert_eq!(prefs.provider_pairings, vec![route.clone()]);
    assert_eq!(prefs.spawn_configurations, vec![saved]);
    let mut changed = route;
    changed.base_url = Some("https://other.example/v1".into());
    assert!(save_with_route_from(&store, value, Some(changed))
        .unwrap_err()
        .contains("changed"));
    assert_eq!(store.load().unwrap(), prefs);
    super::super::secrets::test_support::reset();
}

#[test]
fn spawn_configurations_preserve_sparse_values_and_roundtrip_old_preferences() {
    let mut prefs: AppPreferences = serde_json::from_str("{}").unwrap();
    assert!(prefs.spawn_configurations.is_empty());
    let mut value = configuration("codex");
    value.effort = Some("  ".into());
    value.extra_args = Some("  ".into());
    let value = validate(value).unwrap();
    assert_eq!(value.name, "Sol");
    assert_eq!(value.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(value.effort, None);
    assert_eq!(value.extra_args, None);
    prefs.spawn_configurations.push(value.clone());
    let restored: AppPreferences =
        serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
    assert_eq!(
        resolve(&restored, "codex", Some("sol")).unwrap(),
        Some(value)
    );
    assert_eq!(resolve(&restored, "codex", None).unwrap(), None);
}

#[test]
fn spawn_configurations_reject_unsupported_fields_and_effort_vocabulary() {
    assert!(validate(configuration("terminal"))
        .unwrap_err()
        .contains("model"));
    let mut value = configuration("codex");
    value.effort = Some("not-an-effort".into());
    assert!(validate(value).unwrap_err().contains("not allowed"));
    let mut value = configuration("opencode");
    value.effort = Some("high".into());
    assert!(validate(value).unwrap_err().contains("does not support"));
    let mut value = configuration("terminal");
    value.model = None;
    value.extra_args = Some("--model unsafe".into());
    assert!(validate(value).unwrap_err().contains("extra arguments"));
    let mut value = configuration("codex");
    value.name = " ".into();
    assert!(validate(value).is_err());
}

#[test]
fn save_spawn_configuration_replaces_whitespace_only_ids() {
    let mut value = configuration("codex");
    value.id = "   ".into();
    normalize_id(&mut value);
    assert!(!value.id.is_empty());
    assert!(uuid::Uuid::parse_str(value.id.strip_prefix("launch/").unwrap()).is_ok());

    value.id = " sol ".into();
    normalize_id(&mut value);
    assert_eq!(value.id, "sol");
}

#[test]
fn spawn_configurations_cannot_cross_pairings_or_silently_fall_back_when_deleted() {
    let mut prefs = AppPreferences::default();
    prefs
        .spawn_configurations
        .push(configuration("claude:openrouter"));
    assert!(resolve(&prefs, "claude", Some("sol"))
        .unwrap_err()
        .contains("different"));
    assert!(resolve(&prefs, "codex:openrouter", Some("sol")).is_err());
    assert!(resolve(&prefs, "claude:openrouter", Some("missing"))
        .unwrap_err()
        .contains("no longer exists"));
}

#[test]
fn menu_selection_freezes_a_login_for_defaults_and_recipes_and_rejects_stale_picks() {
    let mut prefs = AppPreferences::default();
    prefs
        .harness_logins
        .push(super::super::harness_logins::HarnessLogin {
            id: "work".into(),
            harness_id: "codex".into(),
            label: "Work".into(),
            home: "C:/Users/u/.codex-accounts/work".into(),
        });
    prefs.spawn_configurations.push(SpawnConfiguration {
        id: "launch/codex-high".into(),
        name: "High".into(),
        spawn_option_id: "codex".into(),
        effort: Some("high".into()),
        login_id: Some("work".into()),
        ..Default::default()
    });
    for recipe in [None, Some("launch/codex-high")] {
        let picked = resolve_menu_selection(&prefs, "codex", recipe, Some("work"))
            .unwrap()
            .unwrap();
        let plan = picked.resolved.unwrap();
        assert_eq!(plan.harness_login.unwrap().id, "work");
        assert_eq!(plan.effort.as_deref(), recipe.map(|_| "high"));
        assert!(resolve_menu_selection(&prefs, "codex", recipe, Some("deleted")).is_err());
    }
    let default =
        resolve_menu_selection(&prefs, "codex", Some("launch/codex-high"), Some("default"))
            .unwrap()
            .unwrap();
    assert!(
        default.resolved.unwrap().harness_login.is_none(),
        "the explicit default pick overrides a pin"
    );
    assert_eq!(
        prefs.spawn_configurations[0].login_id.as_deref(),
        Some("work"),
        "menu selection never edits the recipe"
    );
    assert!(resolve_menu_selection(&prefs, "claude", None, Some("work")).is_err());
    assert!(resolve_menu_selection(&prefs, "codex", Some("launch/deleted"), Some("work")).is_err());
}
