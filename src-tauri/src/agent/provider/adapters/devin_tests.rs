use super::*;
use crate::agent::capabilities::PERMISSION_MODE_PROMPT;
use serde_json::json;

#[test]
fn identity_and_recipe_are_the_native_binary_everywhere() {
    assert_eq!(DEVIN.id(), "devin");
    for platform in [Platform::Windows, Platform::Macos, Platform::Linux] {
        let recipe = DEVIN.spawn_recipe(platform, EnvType::Windows);
        assert_eq!(recipe.binary, "devin");
        assert!(recipe.base_args.is_empty());
        assert!(matches!(recipe.windows_shell, WindowsShell::Direct));
    }
}

#[test]
fn unattended_mode_skips_prompts_and_the_trust_dialog() {
    assert_eq!(
        DEVIN.permission_args(PERMISSION_MODE_UNATTENDED),
        vec![
            "--permission-mode",
            "dangerous",
            "--respect-workspace-trust",
            "false"
        ]
    );
    assert!(DEVIN.permission_args(PERMISSION_MODE_PROMPT).is_empty());
}

#[test]
fn session_identity_is_hook_reported_and_resume_is_explicit() {
    assert!(DEVIN.self_assigns_session_id());
    assert!(!DEVIN.captures_session_id_from_pty());
    assert_eq!(DEVIN.resume_args("abc"), vec!["--resume", "abc"]);
    assert!(DEVIN.session_assign_args("abc").is_empty());
}

#[test]
fn prefill_ends_option_parsing_so_a_dashed_prompt_stays_text() {
    assert_eq!(DEVIN.prefill_args("-fix it"), vec!["--", "-fix it"]);
}

#[test]
fn descriptor_advertises_the_verified_capabilities() {
    let descriptor = DEVIN.descriptor();
    assert!(descriptor.supports_resume);
    assert!(descriptor.supports_model_override);
    assert!(descriptor.supports_prefill);
    assert!(descriptor.prefill_auto_submits);
    assert!(matches!(descriptor.effort_control, EffortControlKind::None));
    assert!(DEVIN.background_recipe(Platform::Linux).is_none());
}

#[test]
fn hook_command_is_curl_to_the_node_route() {
    let url = attention_url(7, 1992);
    assert_eq!(url, "http://localhost:1992/api/attention/7");
    let unix = hook_command(&url, false);
    assert!(unix.starts_with("curl -fsS"));
    assert!(unix.contains("--data-binary @-"));
    assert!(unix.ends_with("|| true"));
    let windows = hook_command(&url, true);
    assert!(windows.starts_with("curl.exe -fsS"));
    assert!(windows.contains("-o NUL"));
    assert!(windows.contains(&url));
}

#[test]
fn provisioning_writes_matcher_groups_and_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join(".devin");
    std::fs::create_dir_all(&dir).unwrap();
    let spec = HookProvisioningSpec {
        command: "curl http://localhost:1/api/attention/1 --data-binary @-",
        ..DEVIN_HOOKS
    };
    let first = provision_attention_hooks(&spec, &dir).unwrap();
    assert!(first.written);
    let raw = std::fs::read_to_string(dir.join("config.local.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let hooks = &value["hooks"];
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "Stop",
        "PermissionRequest",
        "PreToolUse",
        "PostToolUse",
    ] {
        assert_eq!(hooks[event][0]["hooks"][0]["type"], "command", "{event}");
    }
    assert_eq!(hooks["PreToolUse"][0]["matcher"], QUESTION_TOOL_MATCHER);
    assert!(hooks["Stop"][0].get("matcher").is_none());
    let second = provision_attention_hooks(&spec, &dir).unwrap();
    assert!(!second.written);
}

#[test]
fn provisioning_preserves_user_hooks_and_refuses_malformed_files() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join(".devin");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.local.json");
    std::fs::write(
        &path,
        json!({"model": "opus", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo mine"}]}]}})
            .to_string(),
    )
    .unwrap();
    let spec = HookProvisioningSpec {
        command: "curl http://localhost:1/api/attention/1 --data-binary @-",
        ..DEVIN_HOOKS
    };
    provision_attention_hooks(&spec, &dir).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["model"], "opus");
    let rendered = value["hooks"]["Stop"].to_string();
    assert!(rendered.contains("echo mine"));
    assert!(rendered.contains("/api/attention/1"));

    std::fs::write(&path, "{ not json").unwrap();
    assert!(provision_attention_hooks(&spec, &dir).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
}

#[test]
fn circuit_observation_stays_unwired_until_live_verified() {
    let strategy = &DEVIN.descriptor().circuit_observation;
    assert!(!strategy.has_foreground_source());
    assert!(strategy.hooks().is_none());
}
