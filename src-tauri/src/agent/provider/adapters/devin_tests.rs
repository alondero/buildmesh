use super::*;
use crate::agent::capabilities::{ResolvedAgentConfig, PERMISSION_MODE_PROMPT};
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::agent::spawn::ResumeCause;

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
fn prepare_launch_passes_resume_flag_and_omits_session_id_on_assign() {
    assert!(DEVIN.self_assigns_session_id());
    assert!(!DEVIN.captures_session_id_from_pty());
    assert!(DEVIN.session_assign_args("abc").is_empty());
    let config = ResolvedAgentConfig::default();

    let assigned = DEVIN.prepare_launch(HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Assign("phantom-jellyfish"),
        config: &config,
        prefill: None,
        sandbox: false,
    });
    assert!(!assigned
        .recipe
        .base_args
        .contains(&"--session-id".to_string()));

    let resumed = DEVIN.prepare_launch(HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Resume {
            id: "phantom-jellyfish",
            cause: ResumeCause::Explicit,
        },
        config: &config,
        prefill: None,
        sandbox: false,
    });
    assert_eq!(
        resumed.recipe.base_args,
        vec![
            "--permission-mode",
            "dangerous",
            "--respect-workspace-trust",
            "false",
            "--resume",
            "phantom-jellyfish"
        ]
    );
}

#[test]
fn prefill_ends_option_parsing_so_a_dashed_prompt_stays_text() {
    assert_eq!(DEVIN.prefill_args("-fix it"), vec!["--", "-fix it"]);
}

#[test]
fn descriptor_advertises_only_live_verified_capabilities() {
    let descriptor = DEVIN.descriptor();
    assert!(descriptor.supports_resume);
    assert!(descriptor.requires_attention_hook());
    assert!(descriptor.supports_model_override);
    assert!(descriptor.supports_prefill);
    assert!(descriptor.prefill_auto_submits);
    assert!(matches!(descriptor.effort_control, EffortControlKind::None));
    assert!(DEVIN.background_recipe(Platform::Linux).is_none());
    assert!(!descriptor.circuit_observation.has_foreground_source());
    match &descriptor.attention_capability {
        crate::agent::capabilities::AttentionCapability::Hook {
            events,
            launch_mode,
            min_version,
            ..
        } => {
            for kind in [
                crate::agent::session_lifecycle::LifecycleKind::TurnCompleted,
                crate::agent::session_lifecycle::LifecycleKind::WorkResumed,
                crate::agent::session_lifecycle::LifecycleKind::QuestionRequested,
                crate::agent::session_lifecycle::LifecycleKind::PermissionRequested,
            ] {
                assert!(events.contains(&kind), "missing {kind:?}");
            }
            assert!(matches!(
                launch_mode,
                crate::agent::capabilities::AttentionLaunchMode::SkipPermissions
            ));
            assert_eq!(min_version.as_deref(), Some(DEVIN_MIN_HOOK_VERSION));
        }
        other => panic!("devin attention hook is wired: {other:?}"),
    }
}

fn read_hooks_v1_json(project: &std::path::Path) -> serde_json::Value {
    let content = std::fs::read_to_string(project.join(".devin").join("hooks.v1.json"))
        .expect("hooks.v1.json not written");
    serde_json::from_str(&content).expect("hooks.v1.json is not valid JSON")
}

fn provision_devin(project: &std::path::Path, env_type: EnvType) {
    let path = project.to_string_lossy().into_owned();
    let resolved = crate::env::ResolvedPath {
        host_path: path.clone(),
        spawn_path: path.clone(),
        raw_path: path,
        env_type,
    };
    DEVIN
        .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0)
        .unwrap();
}

#[test]
fn provision_writes_devin_shaped_hooks_v1_json() {
    let temp = tempfile::TempDir::new().unwrap();
    provision_devin(temp.path(), EnvType::Windows);

    let doc = read_hooks_v1_json(temp.path());
    assert!(doc.is_object(), "top-level must be an object: {doc}");
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "Stop",
        "PermissionRequest",
        "PreToolUse",
        "PostToolUse",
    ] {
        let groups = doc[event]
            .as_array()
            .unwrap_or_else(|| panic!("`{event}` must be a matcher-group array: {doc}"));
        assert!(!groups.is_empty(), "`{event}` must carry a group");
        let handler = groups[0]["hooks"][0].clone();
        assert_eq!(handler["type"], serde_json::json!("command"));
        let cmd = handler["command"]
            .as_str()
            .unwrap_or_else(|| panic!("`{event}` handler missing command: {doc}"));
        assert!(
            cmd.contains("/api/attention/"),
            "{event} must POST to the attention endpoint: {cmd}"
        );
        assert!(
            cmd.contains("--data-binary @-"),
            "{event} must forward the hook stdin as the POST body: {cmd}"
        );
    }

    assert_eq!(
        doc["PreToolUse"][0]["matcher"],
        serde_json::json!("^(ask_user_question|exit_plan_mode)$")
    );
    assert_eq!(
        doc["PostToolUse"][0]["matcher"],
        serde_json::json!("^(ask_user_question|exit_plan_mode)$")
    );
}

#[test]
fn provisioned_command_is_posix_on_windows() {
    let temp = tempfile::TempDir::new().unwrap();
    provision_devin(temp.path(), EnvType::Windows);
    let doc = read_hooks_v1_json(temp.path());
    let cmd = doc["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .expect("stop command");
    assert!(cmd.contains("$BUILDMESH_PORT"), "POSIX expansion: {cmd}");
    assert!(
        cmd.contains("$BUILDMESH_SESSION_ID"),
        "POSIX expansion: {cmd}"
    );
    assert!(cmd.ends_with("|| true"), "POSIX exit swallow: {cmd}");
    assert!(!cmd.contains("%BUILDMESH_PORT%"), "cmd.exe syntax: {cmd}");
    assert!(!cmd.contains(">nul"), "cmd.exe redirect: {cmd}");
    assert!(!cmd.contains("|| exit 0"), "cmd.exe swallow: {cmd}");
}

#[test]
fn provision_is_idempotent_and_does_not_rewrite() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(".devin").join("hooks.v1.json");
    provision_devin(temp.path(), EnvType::Windows);
    let first_bytes = std::fs::read(&path).unwrap();
    let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    provision_devin(temp.path(), EnvType::Windows);
    assert_eq!(first_bytes, std::fs::read(&path).unwrap());
    assert_eq!(
        first_mtime,
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        "idempotent re-run must NOT rewrite the file"
    );
}

#[test]
fn provision_preserves_user_authored_groups_and_events() {
    let temp = tempfile::TempDir::new().unwrap();
    let devin_dir = temp.path().join(".devin");
    std::fs::create_dir_all(&devin_dir).unwrap();
    std::fs::write(
        devin_dir.join("hooks.v1.json"),
        serde_json::json!({
            "Stop": [
                { "matcher": "", "hooks": [{ "type": "command", "command": "echo user-stop" }] }
            ],
            "SessionEnd": [
                { "matcher": "", "hooks": [{ "type": "command", "command": "echo user-end" }] }
            ]
        })
        .to_string(),
    )
    .unwrap();

    provision_devin(temp.path(), EnvType::Windows);
    let doc = read_hooks_v1_json(temp.path());
    let stop_commands: Vec<&str> = doc["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["hooks"].as_array().unwrap().iter())
        .filter_map(|handler| handler["command"].as_str())
        .collect();
    assert!(
        stop_commands.contains(&"echo user-stop"),
        "user Stop handler must survive: {stop_commands:?}"
    );
    assert!(
        stop_commands
            .iter()
            .any(|cmd| cmd.contains("/api/attention/")),
        "Buildmesh Stop handler must be added: {stop_commands:?}"
    );
    assert_eq!(
        doc["SessionEnd"][0]["hooks"][0]["command"],
        serde_json::json!("echo user-end"),
        "events Buildmesh does not own survive byte-for-byte"
    );
}

#[test]
fn provision_refuses_a_malformed_existing_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let devin_dir = temp.path().join(".devin");
    std::fs::create_dir_all(&devin_dir).unwrap();
    let path = devin_dir.join("hooks.v1.json");
    std::fs::write(&path, "{ not json").unwrap();

    let project = temp.path().to_string_lossy().into_owned();
    let resolved = crate::env::ResolvedPath {
        host_path: project.clone(),
        spawn_path: project.clone(),
        raw_path: project,
        env_type: EnvType::Windows,
    };
    let result = DEVIN.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0);
    assert!(result.is_err(), "malformed file must be refused");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{ not json",
        "the malformed user file is never overwritten"
    );
}

#[test]
fn confirms_prompt_submission_userpromptsubmit_fires() {
    assert!(DEVIN.confirms_prompt_submission());
}
