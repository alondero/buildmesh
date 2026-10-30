//! Tests for the launch configurations — recipe round trips, plan resolution
//! and generated-catalogue reconciliation (moved out of the owner file so the
//! rust-test-layout gate holds, as `launch_catalog_tests.rs` was).

use super::*;

#[test]
fn mcode_configuration_round_trips_and_resolves_model_before_defaults() {
    use crate::preferences::{
        launch_catalog, launch_configurations, spawn_configurations, AppPreferences,
        HarnessConfigValue,
    };

    let mut prefs = AppPreferences::default();
    let saved = spawn_configurations::validate(spawn_configurations::SpawnConfiguration {
        id: "launch/mcode-review".into(),
        name: "MiniMax review".into(),
        spawn_option_id: "mcode".into(),
        model: Some(" minimax/MiniMax-M3#variant ".into()),
        ..Default::default()
    })
    .unwrap();
    prefs.spawn_configurations.push(saved);
    let mut prefs: AppPreferences =
        serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
    let plan = launch_configurations::resolve_for_edit(&prefs, "launch/mcode-review").unwrap();
    assert_eq!(plan.model.as_deref(), Some("minimax/MiniMax-M3#variant"));
    assert_eq!(plan.effort, None);
    assert!(plan.route.is_none());

    let targets = launch_catalog::targets_for(&prefs, vec![plan.harness.clone()]);
    assert_eq!(targets.len(), 1);
    assert!(targets[0].supports_model);
    assert!(targets[0].manual_model);
    assert!(targets[0].efforts.is_empty());

    prefs.harness_defaults.insert(
        "mcode".into(),
        HarnessConfigValue {
            model: Some("minimax/application-default".into()),
            effort: None,
            permission_mode: None,
        },
    );
    let plan = launch_configurations::resolve_for_edit(&prefs, "launch/mcode-review").unwrap();
    assert_eq!(plan.model.as_deref(), Some("minimax/MiniMax-M3#variant"));
    prefs.spawn_configurations[0].model = None;
    let plan = launch_configurations::resolve_for_edit(&prefs, "launch/mcode-review").unwrap();
    assert_eq!(plan.model.as_deref(), Some("minimax/application-default"));
    prefs.harness_defaults.clear();
    let plan = launch_configurations::resolve_for_edit(&prefs, "launch/mcode-review").unwrap();
    assert_eq!(plan.model, None);
}

#[test]
fn unavailable_runtime_is_rejected_at_the_resolver_not_only_in_the_menu() {
    let mut prefs = generated_preferences();
    prefs.harness_profiles[0].runtime = Some(if cfg!(windows) {
        crate::models::EnvType::WindowsInterop
    } else {
        crate::models::EnvType::Wsl
    });
    reconcile(&mut prefs);
    assert!(resolve(&prefs, "claude", &Default::default())
        .unwrap_err()
        .contains("unavailable"));
    assert!(resolve_for_edit(&prefs, "claude").is_ok());
}

fn generated_preferences() -> AppPreferences {
    serde_json::from_value(serde_json::json!({
        "harness_profiles": [{"id":"claude","name":"Claude Code","harness":"anthropic"}],
        "provider_accounts": [{"id":"minimax","name":"MiniMax","enabled":true,"billing_mode":"pay_as_you_go","api_key":"private-key"}]
    })).unwrap()
}

#[test]
fn reconciliation_materializes_routes_once_and_creates_no_configurations() {
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    assert_eq!(prefs.provider_pairings.len(), 1);
    assert!(prefs.spawn_configurations.is_empty());
    let once = prefs.clone();
    reconcile(&mut prefs);
    assert_eq!(prefs, once);
    // A user-saved recipe survives reconciliation untouched.
    prefs.spawn_configurations.push(
        serde_json::from_value(serde_json::json!({
            "id": "launch/my-review", "name": "My review", "spawn_option_id": "claude",
            "model": null, "effort": null, "extra_args": null
        }))
        .unwrap(),
    );
    reconcile(&mut prefs);
    assert_eq!(
        prefs
            .spawn_configurations
            .iter()
            .find(|c| c.id == "launch/my-review")
            .unwrap()
            .name,
        "My review"
    );
    // A deleted route id is not re-materialized.
    prefs.provider_pairings.clear();
    prefs
        .deleted_launch_configurations
        .push("launch/claude:minimax".into());
    reconcile(&mut prefs);
    assert!(prefs.provider_pairings.is_empty());
}

#[test]
fn reconcile_leaves_custom_routes_untouched_and_creates_no_configurations() {
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    prefs.provider_pairings[0].base_url = Some("https://custom.example/anthropic".into());
    prefs.provider_pairings[0].model_tiers.default = Some("custom-model".into());
    let route = prefs.provider_pairings[0].clone();
    reconcile(&mut prefs);
    assert_eq!(prefs.provider_pairings[0], route);
    assert!(prefs.spawn_configurations.is_empty());
}

#[test]
fn new_harness_creates_no_configuration_and_preserves_user_ids() {
    let mut prefs = generated_preferences();
    prefs.default_provider = Some("custom-choice".into());
    prefs.spawn_configurations.push(serde_json::from_value(serde_json::json!({
        "id":"custom-choice","name":"Personal","spawn_option_id":"claude","model":"sonnet","effort":"high","extra_args":null
    })).unwrap());
    prefs.harness_profiles.push(HarnessProfile {
        id: "codex".into(),
        name: "Codex".into(),
        harness: "codex".into(),
        runtime: None,
        wsl_distro: None,
        executable: None,
    });
    reconcile(&mut prefs);
    assert_eq!(prefs.default_provider.as_deref(), Some("custom-choice"));
    assert!(prefs
        .spawn_configurations
        .iter()
        .any(|c| c.id == "custom-choice"));
    assert!(
        !prefs
            .spawn_configurations
            .iter()
            .any(|c| c.id == "launch/codex:minimax"),
        "a new harness must not gain a pre-made configuration: {:?}",
        prefs
            .spawn_configurations
            .iter()
            .map(|c| &c.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        prefs.provider_accounts[0].api_key.as_deref(),
        Some("private-key")
    );
}

fn user_minimax_configuration() -> serde_json::Value {
    serde_json::json!({
        "id": "launch/my-minimax", "name": "My MiniMax", "spawn_option_id": "claude:minimax",
        "model": null, "effort": null, "extra_args": null
    })
}

#[test]
fn proxy_defaults_do_not_inherit_native_harness_models() {
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    prefs.harness_defaults.insert(
        "claude".into(),
        crate::preferences::HarnessConfigValue {
            model: Some("sonnet".into()),
            effort: Some("high".into()),
            permission_mode: None,
        },
    );
    prefs
        .spawn_configurations
        .push(serde_json::from_value(user_minimax_configuration()).unwrap());
    let plan = resolve_plan(&prefs, "launch/my-minimax", &Default::default(), false).unwrap();
    assert_eq!(plan.model.as_deref(), Some("MiniMax-M3.1-Flash[1m]"));
    assert_eq!(plan.effort, None);
}

/// M3.1 Flash always reasons, so `none` (a 400 from MiniMax) must be
/// refused while the real levels pass.
#[test]
fn minimax_codex_m3_1_flash_accepts_real_efforts_and_rejects_none() {
    let mut prefs = generated_preferences();
    prefs.harness_profiles.push(HarnessProfile {
        id: "codex".into(),
        name: "Codex".into(),
        harness: "codex".into(),
        runtime: None,
        wsl_distro: None,
        executable: None,
    });
    reconcile(&mut prefs);
    let resolve = |effort: &str| {
        resolve_plan(
            &prefs,
            "codex:minimax",
            &LaunchOverrides {
                model: Some("MiniMax-M3.1-Flash".into()),
                effort: Some(effort.into()),
                extra_args: None,
            },
            false,
        )
    };
    for effort in ["low", "medium", "high", "xhigh"] {
        assert_eq!(resolve(effort).unwrap().effort.as_deref(), Some(effort));
    }
    assert!(resolve("none").unwrap_err().contains("Effort"));
}

#[test]
fn minimax_codex_configuration_accepts_documented_thinking_choices() {
    let mut prefs = generated_preferences();
    prefs.harness_profiles.push(HarnessProfile {
        id: "codex".into(),
        name: "Codex".into(),
        harness: "codex".into(),
        runtime: None,
        wsl_distro: None,
        executable: None,
    });
    reconcile(&mut prefs);
    for effort in ["none", "high"] {
        let plan = resolve_plan(
            &prefs,
            "codex:minimax",
            &LaunchOverrides {
                model: Some("MiniMax-M3".into()),
                effort: Some(effort.into()),
                extra_args: None,
            },
            false,
        )
        .unwrap();
        assert_eq!(plan.effort.as_deref(), Some(effort));
    }
    assert!(resolve_plan(
        &prefs,
        "codex:minimax",
        &LaunchOverrides {
            model: Some("MiniMax-M3".into()),
            effort: Some("xhigh".into()),
            extra_args: None,
        },
        false
    )
    .unwrap_err()
    .contains("Effort"));
}

#[test]
fn resolver_reports_missing_dependencies_without_native_fallback() {
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    prefs
        .spawn_configurations
        .push(serde_json::from_value(user_minimax_configuration()).unwrap());
    let resolve_current =
        |prefs: &AppPreferences| resolve(prefs, "launch/my-minimax", &Default::default());
    assert!(resolve_current(&prefs).is_ok());
    prefs.provider_accounts[0].api_key = None;
    assert!(resolve_current(&prefs).unwrap_err().contains("credential"));
    prefs.provider_accounts[0].api_key = Some("rotated".into());
    prefs.provider_pairings.clear();
    assert!(resolve_current(&prefs)
        .unwrap_err()
        .contains("Route is missing"));
    prefs.spawn_configurations.clear();
    assert!(resolve_current(&prefs)
        .unwrap_err()
        .contains("no longer exists"));
}

#[test]
fn explicit_overrides_win_and_terminal_masks_unsupported_fields() {
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    prefs.harness_defaults.insert(
        "claude".into(),
        crate::preferences::HarnessConfigValue {
            model: Some("app".into()),
            effort: Some("low".into()),
            permission_mode: None,
        },
    );
    let plan = resolve(&prefs, "claude", &Default::default()).unwrap();
    assert_eq!(plan.model.as_deref(), Some("app"));
    assert_eq!(plan.effort.as_deref(), Some("low"));
    let overrides = LaunchOverrides {
        model: Some("explicit".into()),
        effort: Some("high".into()),
        extra_args: Some("--verbose".into()),
    };
    let plan = resolve(&prefs, "claude", &overrides).unwrap();
    assert_eq!(plan.model.as_deref(), Some("explicit"));
    assert_eq!(plan.extra_args.as_deref(), Some("--verbose"));
    let plan = resolve(&prefs, "terminal", &overrides).unwrap();
    assert_eq!(
        (plan.model, plan.effort, plan.extra_args),
        (None, None, None)
    );
}

#[test]
fn snapshot_survives_deleted_recipe_and_route_but_uses_current_account() {
    let dir = tempfile::tempdir().unwrap();
    super::super::init_for_tests(dir.path().into());
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    prefs
        .spawn_configurations
        .push(serde_json::from_value(user_minimax_configuration()).unwrap());
    let plan = resolve(&prefs, "launch/my-minimax", &Default::default()).unwrap();
    prefs.harness_profiles.clear();
    prefs.provider_pairings.clear();
    prefs.spawn_configurations.clear();
    prefs
        .deleted_launch_configurations
        .push(plan.configuration_id.clone());
    prefs.provider_accounts[0].api_key = Some("rotated".into());
    super::super::save(prefs.clone()).unwrap();
    let resumed = resolve_snapshot(&plan, &Default::default()).unwrap();
    assert_eq!(resumed, plan);
    prefs.provider_accounts.clear();
    super::super::save(prefs).unwrap();
    assert!(resolve_snapshot(&plan, &Default::default())
        .unwrap_err()
        .contains("account"));
    super::super::reset_for_tests();
}

#[test]
fn proxied_configuration_snapshots_route_without_credentials() {
    let prefs: AppPreferences = serde_json::from_value(serde_json::json!({
        "provider_accounts": [{"id":"custom", "name":"Private", "enabled":true, "billing_mode":"pay_as_you_go", "api_key":"secret-test-key"}],
        "provider_pairings": [{"harness_id":"claude", "provider_id":"custom", "surface":"anthropic", "base_url":"https://example.test/anthropic", "model_tiers":{"default":"route-model"}}],
        "spawn_configurations": [{"id":"launch/private", "name":"Private", "spawn_option_id":"claude:custom", "model":"chosen-model", "effort":"high", "extra_args":null}]
    })).unwrap();
    let plan = resolve(&prefs, "launch/private", &LaunchOverrides::default()).unwrap();
    assert_eq!(
        plan.route.as_ref().unwrap().base_url.as_deref(),
        Some("https://example.test/anthropic")
    );
    assert_eq!(
        plan.route.as_ref().unwrap().model_tiers.default.as_deref(),
        Some("chosen-model")
    );
    assert!(!serde_json::to_string(&plan)
        .unwrap()
        .contains("secret-test-key"));
    let mut disabled = prefs.clone();
    disabled.provider_accounts[0].enabled = false;
    assert!(
        resolve(&disabled, "launch/private", &LaunchOverrides::default())
            .unwrap_err()
            .contains("disabled")
    );
}

#[test]
fn fresh_preferences_start_with_no_spawn_configurations() {
    // Spawn-menu submenus list only what the user saved. A fresh install
    // must not materialize one generated recipe per harness.
    let mut prefs = AppPreferences::default();
    reconcile(&mut prefs);
    assert!(
        prefs.spawn_configurations.is_empty(),
        "fresh prefs must not pre-create configurations, got {:?}",
        prefs.spawn_configurations
    );
    let mut known = generated_preferences();
    reconcile(&mut known);
    assert!(
        known.spawn_configurations.is_empty(),
        "a known provider must materialize its route, not a configuration: {:?}",
        known.spawn_configurations
    );
    assert_eq!(known.provider_pairings.len(), 1);
}

#[test]
fn reconcile_purges_catalogue_generated_configurations_but_keeps_user_ones() {
    let mut prefs = generated_preferences();
    prefs.spawn_configurations.push(
        crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/claude".into(),
            name: "Claude Code".into(),
            spawn_option_id: "claude".into(),
            generated: Some(GeneratedLaunch {
                source: "claude".into(),
                catalogue_revision: 1,
                user_owned: false,
                route: None,
            }),
            ..Default::default()
        },
    );
    prefs.spawn_configurations.push(
        crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/my-review".into(),
            name: "My review".into(),
            spawn_option_id: "claude".into(),
            generated: Some(GeneratedLaunch {
                source: "claude".into(),
                catalogue_revision: 1,
                user_owned: true,
                route: None,
            }),
            ..Default::default()
        },
    );
    prefs.spawn_configurations.push(
        serde_json::from_value(serde_json::json!({
            "id": "launch/personal", "name": "Personal", "spawn_option_id": "claude",
            "model": null, "effort": null, "extra_args": null
        }))
        .unwrap(),
    );
    reconcile(&mut prefs);
    let ids: Vec<_> = prefs
        .spawn_configurations
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert!(
        !ids.contains(&"launch/claude"),
        "catalogue-generated recipes must be purged: {ids:?}"
    );
    assert!(
        ids.contains(&"launch/my-review"),
        "user-owned recipes must survive: {ids:?}"
    );
    assert!(
        ids.contains(&"launch/personal"),
        "user-created recipes must survive: {ids:?}"
    );
}

#[test]
fn detached_route_is_not_rematerialized() {
    // Detaching a route must stick: the save following a detach runs
    // reconcile, which must not recreate the just-removed pairing.
    // Re-attaching stores an explicit pairing, which the detach record
    // never blocks.
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    assert_eq!(prefs.provider_pairings.len(), 1);
    let pairing = prefs.provider_pairings.clone();
    super::super::remove_provider_pairing(&mut prefs, "claude", "minimax");
    reconcile(&mut prefs);
    assert!(
        prefs.provider_pairings.is_empty(),
        "detach must stick, got {:?}",
        prefs.provider_pairings
    );
    assert!(prefs
        .detached_provider_routes
        .contains(&"claude:minimax".to_string()));
    for route in pairing {
        super::super::upsert_provider_pairing(&mut prefs, route);
    }
    reconcile(&mut prefs);
    assert_eq!(
        prefs.provider_pairings.len(),
        1,
        "explicit re-attach must win over the detach record"
    );
}

#[test]
fn purge_carries_pre_upgrade_detach_into_detach_record() {
    // Old install: the route was detached (pairing gone) while its
    // catalogue recipe remained as the marker blocking re-creation.
    // Purging that marker must preserve the detach decision — without
    // tombstoning the recipe id, so saved references still heal.
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    prefs.provider_pairings.clear();
    prefs.spawn_configurations.push(
        crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/claude:minimax".into(),
            name: "MiniMax".into(),
            spawn_option_id: "claude:minimax".into(),
            generated: Some(GeneratedLaunch {
                source: "claude:minimax".into(),
                catalogue_revision: 1,
                user_owned: false,
                route: None,
            }),
            ..Default::default()
        },
    );
    reconcile(&mut prefs);
    assert!(
        prefs.provider_pairings.is_empty(),
        "pre-upgrade detach must survive the purge, got {:?}",
        prefs.provider_pairings
    );
    assert!(prefs
        .detached_provider_routes
        .contains(&"claude:minimax".to_string()));
    assert!(
        !prefs
            .deleted_launch_configurations
            .contains(&"launch/claude:minimax".to_string()),
        "a detach record must not masquerade as a recipe deletion"
    );
    assert_eq!(
        retired_configuration_aliases(&prefs)
            .get("launch/claude:minimax")
            .map(String::as_str),
        Some("claude:minimax"),
        "detached routes must still heal to their bare option"
    );
}

#[test]
fn retired_aliases_heal_purged_recipes_but_respect_user_choices() {
    let mut prefs = generated_preferences();
    reconcile(&mut prefs);
    // The pairing materialized but no configurations exist: the retired
    // catalogue ids for the harness, its route, and Terminal all heal
    // to their bare Spawn Option.
    let aliases = retired_configuration_aliases(&prefs);
    assert_eq!(
        aliases.get("launch/claude").map(String::as_str),
        Some("claude")
    );
    assert_eq!(
        aliases.get("launch/claude:minimax").map(String::as_str),
        Some("claude:minimax")
    );
    assert_eq!(
        aliases.get("launch/terminal").map(String::as_str),
        Some("terminal")
    );
    // A user-owned edit keeps its id live: no alias reroutes it.
    prefs.spawn_configurations.push(
        crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/claude".into(),
            name: "My Claude".into(),
            spawn_option_id: "claude".into(),
            generated: Some(GeneratedLaunch {
                source: "claude".into(),
                catalogue_revision: 1,
                user_owned: true,
                route: None,
            }),
            ..Default::default()
        },
    );
    assert!(!retired_configuration_aliases(&prefs).contains_key("launch/claude"));
    // An explicitly deleted recipe is never resurrected as an alias.
    prefs.spawn_configurations.clear();
    prefs
        .deleted_launch_configurations
        .push("launch/claude:minimax".into());
    assert!(!retired_configuration_aliases(&prefs).contains_key("launch/claude:minimax"));
    // A detached (not deleted) route still heals, even with no pairing
    // stored: references migrate to the bare option and resolve again
    // once the route is re-attached.
    prefs.deleted_launch_configurations.clear();
    prefs.provider_pairings.clear();
    prefs.detached_provider_routes.push("claude:minimax".into());
    assert_eq!(
        retired_configuration_aliases(&prefs)
            .get("launch/claude:minimax")
            .map(String::as_str),
        Some("claude:minimax")
    );
}

#[test]
fn native_configuration_resolves_selected_values_before_defaults() {
    let prefs: super::super::AppPreferences = serde_json::from_value(serde_json::json!({
        "spawn_configurations": [{"id":"launch/fast", "name":"Fast", "spawn_option_id":"codex", "model":"gpt-5.6-sol", "effort":"high", "extra_args":null}]
    })).unwrap();
    let plan = resolve(&prefs, "launch/fast", &LaunchOverrides::default()).unwrap();
    assert_eq!(plan.configuration_id, "launch/fast");
    assert_eq!(plan.harness.harness, "codex");
    assert_eq!(plan.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(plan.effort.as_deref(), Some("high"));
    assert!(plan.route.is_none());
}
