use super::*;

#[test]
fn one_task_owns_the_runner_until_it_finishes_even_after_cancel() {
    let state = CompanionState::default();
    let (task, cancellation) = state.start("status".into()).unwrap();
    assert!(state.start("another".into()).is_err());
    let cancelled = state.cancel(&task.id).unwrap();
    assert_eq!(cancelled.status, "cancelling");
    assert_eq!(cancelled.revision, task.revision + 1);
    assert_eq!(state.snapshot().unwrap().revision, cancelled.revision);
    assert!(*cancellation.borrow());
    assert!(state.start("another".into()).is_err());
    state.0.lock().unwrap().as_mut().unwrap().snapshot.status = "cancelled".into();
    assert!(state.cancel(&task.id).is_err());
    assert_eq!(state.snapshot().unwrap().status, "cancelled");
    let (next, _) = state.start("next".into()).unwrap();
    assert!(next.generation > task.generation);
    assert!(state.cancel(&task.id).is_err());
}

#[tokio::test]
async fn confirmation_is_bound_to_task_revision_and_consumed_once() {
    let state = CompanionState::default();
    let (task, _) = state.start("stop failed nodes".into()).unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    state.0.lock().unwrap().as_mut().unwrap().confirmation = Some(sender);
    assert!(state.respond("retired", 0, true).is_err());
    assert!(state.respond(&task.id, 1, true).is_err());
    state.respond(&task.id, 0, true).unwrap();
    assert!(receiver.await.unwrap());
    assert!(state.respond(&task.id, 0, true).is_err());
}

#[tokio::test]
async fn cancellation_drops_pending_confirmation_without_approval() {
    let state = CompanionState::default();
    let (task, _) = state.start("stop nodes".into()).unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    state.0.lock().unwrap().as_mut().unwrap().confirmation = Some(sender);
    state.cancel(&task.id).unwrap();
    assert!(receiver.await.is_err());
    assert!(state.respond(&task.id, 0, true).is_err());
}

#[test]
fn spawn_budget_cannot_be_evaded_with_multiple_calls_or_oversized_settings() {
    let mut used = 0;
    tools::reserve_spawns(&mut used, 3, 5).unwrap();
    assert!(tools::reserve_spawns(&mut used, 3, 5).is_err());
    assert_eq!(used, 3);
    tools::reserve_spawns(&mut used, 2, 5).unwrap();
    assert!(tools::reserve_spawns(&mut used, 1, 5).is_err());
    assert!(tools::reserve_spawns(&mut 0, 11, 100).is_err());
    assert!(tools::reserve_spawns(&mut 0, 0, 5).is_err());
}

#[test]
fn model_cannot_supply_approval_or_unknown_actions() {
    assert!(serde_json::from_str::<tools::Tool>(
        r#"{"tool":"terminate_nodes","node_ids":[1],"approved":true}"#
    )
    .is_err());
    assert!(
        serde_json::from_str::<tools::Tool>(r#"{"tool":"shell","command":"echo bad"}"#).is_err()
    );
    assert!(serde_json::from_str::<tools::Tool>(r#"{"tool":"terminate_nodes"}"#).is_err());
}

#[test]
fn invalid_prompts_do_not_claim_the_runner() {
    let state = CompanionState::default();
    assert!(state.start(" ".into()).is_err());
    assert!(state.start("x".repeat(16_001)).is_err());
    assert!(state.snapshot().is_none());
}

#[test]
fn legacy_preferences_default_to_five_spawns() {
    let prefs: crate::preferences::AppPreferences = serde_json::from_str("{}").unwrap();
    assert_eq!(prefs.companion.spawn_limit, 5);
}

#[test]
fn assistant_uses_the_background_capability_instead_of_a_claude_allowlist() {
    let mut prefs = crate::preferences::AppPreferences::default();
    for harness in [
        "claude",
        "codex",
        "opencode",
        "agy",
        "grok",
        "kimi",
        "commandcode",
        "mcode",
    ] {
        prefs.companion.launch_configuration = Some(harness.into());
        assert!(runner::resolve_launch(&prefs).is_ok(), "{harness}");
    }
    prefs.companion.launch_configuration = Some("terminal".into());
    assert!(runner::resolve_launch(&prefs)
        .err()
        .unwrap()
        .contains("one-shot background inference"));
    prefs.companion.launch_configuration = None;
    prefs.default_provider = Some("codex".into());
    assert!(runner::resolve_launch(&prefs).is_ok());
    prefs.default_provider = None;
    assert!(runner::resolve_launch(&prefs).is_ok());
}

#[test]
fn assistant_preserves_saved_models_and_rejects_unsupported_background_settings() {
    use crate::preferences::launch_configurations::{capture, snapshot, LaunchOverrides};
    let mut prefs = crate::preferences::AppPreferences::default();
    let mut plan = capture(&prefs, "codex", &LaunchOverrides::default()).unwrap();
    plan.configuration_id = "launch/assistant".into();
    plan.configuration_name = "Assistant".into();
    plan.model = Some("gpt-5.6-sol".into());
    plan.effort = Some("high".into());
    prefs.companion.launch_configuration = Some(plan.configuration_id.clone());
    prefs.spawn_configurations = vec![snapshot(plan.clone())];
    let launch = runner::resolve_launch(&prefs).unwrap();
    assert!(launch
        .args
        .windows(2)
        .any(|args| args == ["--model", "gpt-5.6-sol"]));
    assert!(launch.args.iter().any(|arg| arg.contains("high")));
    plan.extra_args = Some("--json".into());
    prefs.spawn_configurations = vec![snapshot(plan.clone())];
    assert!(runner::resolve_launch(&prefs)
        .err()
        .unwrap()
        .contains("extra CLI arguments"));
    plan.extra_args = None;
    plan.harness.runtime = Some(crate::models::EnvType::Wsl);
    prefs.harness_profiles = vec![plan.harness.clone()];
    prefs.spawn_configurations = vec![snapshot(plan)];
    assert!(runner::resolve_launch(&prefs).is_err());
}

#[test]
fn assistant_keeps_supported_provider_route_credentials() {
    use crate::preferences::{
        launch_configurations::{capture, snapshot, LaunchOverrides},
        ProviderAccount, ProviderPairing,
    };
    let mut prefs = crate::preferences::AppPreferences::default();
    prefs.provider_accounts.push(ProviderAccount {
        id: "assistant-test".into(),
        name: "Assistant Test".into(),
        api_key: Some("test-secret".into()),
        enabled: true,
        billing_mode: Default::default(),
        claude_compatible: true,
    });
    prefs.provider_pairings.push(ProviderPairing {
        harness_id: "claude".into(),
        provider_id: "assistant-test".into(),
        surface: crate::preferences::ApiSurface::Anthropic,
        base_url: Some("https://example.test".into()),
        model_tiers: crate::preferences::ModelTiers {
            default: Some("test-model".into()),
            ..Default::default()
        },
    });
    let mut plan = capture(&prefs, "claude:assistant-test", &LaunchOverrides::default()).unwrap();
    plan.configuration_id = "launch/assistant-route".into();
    prefs.companion.launch_configuration = Some(plan.configuration_id.clone());
    prefs.spawn_configurations = vec![snapshot(plan)];
    let launch = runner::resolve_launch(&prefs).unwrap();
    assert!(launch
        .env
        .iter()
        .any(|(key, value)| key == "ANTHROPIC_AUTH_TOKEN" && value == "test-secret"));
}

#[test]
fn assistant_settings_updates_preserve_other_fields_in_either_order() {
    for launch_first in [true, false] {
        let mut settings = CompanionSettings::default();
        let launch: CompanionSettingsUpdate =
            serde_json::from_str(r#"{"launch_configuration":"codex"}"#).unwrap();
        let limit: CompanionSettingsUpdate = serde_json::from_str(r#"{"spawn_limit":8}"#).unwrap();
        launch.validate().unwrap();
        limit.validate().unwrap();
        if launch_first {
            launch.apply(&mut settings);
            limit.apply(&mut settings);
        } else {
            limit.apply(&mut settings);
            launch.apply(&mut settings);
        }
        assert_eq!(settings.launch_configuration.as_deref(), Some("codex"));
        assert_eq!(settings.spawn_limit, 8);
        let clear: CompanionSettingsUpdate =
            serde_json::from_str(r#"{"launch_configuration":null}"#).unwrap();
        clear.apply(&mut settings);
        assert_eq!(settings.launch_configuration, None);
        assert_eq!(settings.spawn_limit, 8);
    }
    for limit in [0, 11] {
        let update: CompanionSettingsUpdate =
            serde_json::from_value(serde_json::json!({"spawn_limit":limit})).unwrap();
        assert!(update.validate().is_err());
    }
}

#[tokio::test]
async fn assistant_inference_uses_adapter_transports_and_decodes_large_replies() {
    use crate::preferences::{
        launch_configurations::{capture, LaunchOverrides},
        AppPreferences,
    };
    let directory = Arc::new(tempfile::tempdir().unwrap());
    let script = directory.path().join("fixture.cjs");
    std::fs::write(&script, r#"
const fs = require('fs');
const args = process.argv.slice(2);
const value = flag => args[args.indexOf(flag) + 1];
if (args.includes('--version')) { console.log('1.18.3'); process.exit(0); }
if (args.includes('--safe-mode') || args.includes('auth')) process.exit(21);
let prompt;
if (args.includes('--prompt-file')) prompt = fs.readFileSync(value('--prompt-file'), 'utf8');
else if (args.includes('--prompt')) prompt = value('--prompt');
else if (args.includes('--print') && value('--print') !== '--output-format') prompt = value('--print');
else prompt = fs.readFileSync(0, 'utf8');
const reply = JSON.stringify({response: prompt});
if (args.includes('--output-last-message')) {
  if (prompt !== 'omit-final-message') fs.writeFileSync(value('--output-last-message'), reply);
  console.log('diagnostic output, not the final answer');
} else if (args.includes('--format')) console.log(JSON.stringify({type:'text',part:{text:reply}}));
else if (args.includes('stream-json')) console.log(JSON.stringify({role:'assistant',content:reply}));
else if (args.includes('json')) console.log(JSON.stringify({type:'result',result:reply}));
else console.log(reply);
"#).unwrap();
    let executable = directory.path().join(if cfg!(windows) {
        "fixture.cmd"
    } else {
        "fixture"
    });
    if cfg!(windows) {
        std::fs::write(
            &executable,
            format!("@echo off\r\nnode \"{}\" %*\r\n", script.display()),
        )
        .unwrap();
    } else {
        std::fs::write(
            &executable,
            format!("#!/bin/sh\nexec node '{}' \"$@\"\n", script.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let (_sender, mut cancel) = watch::channel(false);
    for (index, harness) in [
        "claude",
        "codex",
        "opencode",
        "agy",
        "grok",
        "kimi",
        "commandcode",
        "mcode",
    ]
    .into_iter()
    .enumerate()
    {
        let prefs = AppPreferences::default();
        let mut plan = capture(&prefs, harness, &LaunchOverrides::default()).unwrap();
        plan.harness.executable = Some(executable.clone());
        let launch = Arc::new(crate::agent::background::resolve_plan(plan, &prefs).unwrap());
        let prompt = format!("{harness} {}", "x".repeat(5000));
        let raw = runner::infer(
            launch.clone(),
            directory.clone(),
            index,
            prompt.clone(),
            &mut cancel,
        )
        .await
        .unwrap_or_else(|error| panic!("{harness}: {error}"));
        let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(reply["response"], prompt, "{harness}");
        if harness == "codex" {
            let missing = runner::infer(
                launch,
                directory.clone(),
                100,
                "omit-final-message".into(),
                &mut cancel,
            )
            .await
            .unwrap_err();
            assert!(
                missing.contains("final message is unavailable"),
                "{missing}"
            );
        }
    }
}

#[test]
fn companion_creation_preserves_frozen_launch_overrides() {
    use crate::preferences::{
        launch_configurations::{snapshot, ResolvedLaunchPlan},
        HarnessProfile,
    };
    let _isolation = crate::db::test_support::isolated();
    crate::db::write_conn().execute_batch("INSERT INTO meshes(id,name,path,use_worktree) VALUES(1,'test','/companion-fixture',0);").unwrap();
    let configuration = snapshot(ResolvedLaunchPlan {
        configuration_id: "launch/companion-test".into(),
        configuration_name: "Configured".into(),
        spawn_option_id: "codex".into(),
        harness: HarnessProfile {
            id: "codex".into(),
            name: "Codex".into(),
            harness: "codex".into(),
            runtime: Some(crate::models::EnvType::Windows),
            wsl_distro: None,
            executable: None,
        },
        route: None,
        verification: None,
        model: Some("gpt-5.6-sol".into()),
        effort: Some("high".into()),
        extra_args: None,
        harness_login: None,
    });
    let request = tools::SpawnInput {
        issue_number: None,
        initial_prompt: Some("test".into()),
        launch_config_id: Some(configuration.id.clone()),
    };
    let intent = crate::agent::spawn::SpawnIntent::Prompt {
        text: "test".into(),
    };
    let node = tools::create_node(1, &request, None, &intent, Some(&configuration)).unwrap();
    let saved = crate::db::node_spawn_configuration(node.id, "codex")
        .unwrap()
        .unwrap();
    assert_eq!(saved.id, configuration.id);
    assert_eq!(saved.model, configuration.model);
    assert_eq!(saved.effort, configuration.effort);
    assert_eq!(node.env, crate::models::EnvType::Windows);
    assert!(
        node.use_worktree,
        "Companion forces a Worktree Node even on a root-node Mesh"
    );
}
