//! Tests for the launch catalogue — the per-harness launch targets the Spawn
//! Menu's **Launch Configuration** submenus are built from (issue #2532 moved
//! the suite out of the owner file, per the sibling-test-file rule).

use super::*;

#[test]
fn claude_launch_target_exposes_every_supported_effort_level() {
    let prefs = serde_json::from_value(serde_json::json!({})).unwrap();
    let targets = targets_for(
        &prefs,
        vec![super::super::HarnessProfile {
            id: "claude".into(),
            name: "Claude Code".into(),
            harness: "anthropic".into(),
            runtime: None,
            wsl_distro: None,
            executable: None,
        }],
    );
    let claude = targets.iter().find(|target| target.id == "claude").unwrap();

    assert_eq!(claude.efforts, ["low", "medium", "high", "xhigh", "max"]);
    assert!(claude.supports_model_effort_discovery);
}

#[test]
fn launch_targets_only_advertise_catalogs_for_admitted_opencode_installations() {
    use crate::agent::detection;
    let previous = detection::detection_snapshot();
    detection::observe_installations(["opencode2".into()].into_iter().collect());
    let profiles = ["opencode", "opencode2"].map(|id| super::super::HarnessProfile {
        id: id.into(),
        name: "OpenCode".into(),
        harness: id.into(),
        runtime: None,
        wsl_distro: None,
        executable: None,
    });
    let installed = detection::currently_installed_profiles(profiles.to_vec());
    let targets = targets_for(&Default::default(), profiles.to_vec());
    detection::restore_detection_state(&previous);

    assert_eq!(
        installed
            .iter()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>(),
        ["opencode2"]
    );
    assert!(!targets[0].supports_model_effort_discovery);
    assert!(targets[1].supports_model_effort_discovery);
}

#[test]
fn launch_targets_offer_unattached_keyed_providers_without_mutating_routes() {
    let prefs: super::super::AppPreferences = serde_json::from_value(serde_json::json!({
        "provider_accounts": [
            {"id":"minimax","name":"MiniMax","enabled":true,"api_key":"test","billing_mode":"pay_as_you_go"},
            {"id":"custom","name":"Custom","enabled":true,"api_key":"test","claude_compatible":true,"billing_mode":"pay_as_you_go"},
            {"id":"kimi","name":"Kimi","enabled":true,"billing_mode":"pay_as_you_go"}
        ],
        "detached_provider_routes": ["claude:minimax", "codex:minimax"]
    })).unwrap();
    let profiles = ["claude", "codex"].map(|id| super::super::HarnessProfile {
        id: id.into(),
        name: id.into(),
        // The `claude` → `anthropic` mapping comes from the one alias
        // table (issue #2532) — not a hand-rolled copy of it.
        harness: crate::agent::harness_catalog::harness_adapter_id(id).to_string(),
        runtime: None,
        wsl_distro: None,
        executable: None,
    });
    let targets = targets_for(&prefs, profiles.to_vec());
    let claude = targets.iter().find(|t| t.id == "claude:minimax").unwrap();
    assert!(!claude.route_attached);
    assert_eq!(
        claude.default_model.as_deref(),
        Some("MiniMax-M3.1-Flash[1m]")
    );
    assert!(claude
        .models
        .iter()
        .any(|m| m.id == "MiniMax-M2.7-highspeed"));
    assert!(claude.models.iter().any(|m| m.id == "MiniMax-M3.1-Flash"));
    let codex = targets.iter().find(|t| t.id == "codex:minimax").unwrap();
    assert_eq!(codex.default_model.as_deref(), Some("MiniMax-M3.1-Flash"));
    let effort_of = |id: &str| {
        codex
            .models
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("codex offers {id}"))
            .efforts
            .clone()
            .unwrap()
    };
    assert_eq!(codex.models.len(), 2);
    assert_eq!(effort_of("MiniMax-M3"), ["none", "high"]);
    // M3.1 Flash always reasons: `none` is a 400 there, so it must not be offered.
    assert_eq!(
        effort_of("MiniMax-M3.1-Flash"),
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert!(targets
        .iter()
        .any(|t| t.id == "codex:custom" && t.route.as_ref().unwrap().base_url.is_none()));
    assert!(!targets.iter().any(|t| t.id.ends_with(":kimi")));
    assert!(prefs.provider_pairings.is_empty());
}

#[test]
fn stored_cline_openai_route_remains_editable_without_codex_verification() {
    let prefs = serde_json::from_value(serde_json::json!({
        "provider_accounts":[{"id":"minimax","name":"MiniMax","enabled":true,"api_key":"test","billing_mode":"pay_as_you_go"}],
        "provider_pairings":[{"harness_id":"cline","provider_id":"minimax","surface":"openai","base_url":"https://api.minimax.io/v1","model_tiers":{"default":"MiniMax-M3"}}]
    })).unwrap();
    let targets = targets_for(
        &prefs,
        vec![super::super::HarnessProfile {
            id: "cline".into(),
            name: "Cline".into(),
            harness: "cline".into(),
            runtime: None,
            wsl_distro: None,
            executable: None,
        }],
    );
    let target = targets.iter().find(|t| t.id == "cline:minimax").unwrap();
    assert!(target.route_attached);
    assert_eq!(target.route.as_ref().unwrap().surface, ApiSurface::OpenAI);
    assert_eq!(target.verification_required, None);
}

#[test]
fn catalog_login_resolves_the_pinned_home_only_for_native_windows() {
    let prefs: super::super::AppPreferences = serde_json::from_value(serde_json::json!({
        "harness_logins": [{
            "id": "work",
            "harness_id": "codex",
            "label": "Work",
            "home": "C:/Users/test/.codex-accounts/work"
        }]
    }))
    .unwrap();

    let login = catalog_login_for(
        &prefs,
        "codex",
        Some("work"),
        crate::models::EnvType::Windows,
    )
    .unwrap()
    .unwrap();
    assert_eq!(login.home, "C:/Users/test/.codex-accounts/work");
    assert!(catalog_login_for(
        &prefs,
        "codex",
        Some("default"),
        crate::models::EnvType::Windows
    )
    .unwrap()
    .is_none());
    assert!(catalog_login_for(
        &prefs,
        "codex",
        Some("missing"),
        crate::models::EnvType::Windows
    )
    .is_err());
    let refusal =
        catalog_login_for(&prefs, "codex", Some("work"), crate::models::EnvType::Wsl).unwrap_err();
    assert!(
        refusal.contains("only available for native Windows"),
        "{refusal}"
    );
}
