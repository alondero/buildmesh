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
    assert_eq!(claude.default_model.as_deref(), Some("MiniMax-M3[1m]"));
    assert!(claude
        .models
        .iter()
        .any(|m| m.id == "MiniMax-M2.7-highspeed"));
    let codex = targets.iter().find(|t| t.id == "codex:minimax").unwrap();
    assert_eq!(codex.models.len(), 1);
    assert_eq!(codex.models[0].efforts.as_ref().unwrap(), &["none", "high"]);
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
