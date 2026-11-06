use super::*;
use crate::agent::capabilities::ResolvedAgentConfig;
use crate::agent::launch::{assert_flag_followed_by_value, HarnessLaunchInput, SessionIdModeRef};

#[test]
fn id_and_ui_metadata() {
    assert_eq!(COMMANDCODE.id(), "commandcode");
    let ui = COMMANDCODE.ui();
    assert_eq!(ui.label, "Command Code");
    assert_eq!(ui.color, "#8C4EDD");
    assert_eq!(ui.icon, "C");
}

#[test]
fn spawn_recipe_cmd_on_windows_native() {
    let recipe = COMMANDCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
    assert_eq!(recipe.binary, "cmdc");
    // Issue #2151: the base recipe is bare — `--yolo` comes from the
    // effective permission mode in `default_prepare`, never from a
    // hidden argv.
    assert!(
        recipe.base_args.is_empty(),
        "Windows native base recipe must carry no approval flags; got {:?}",
        recipe.base_args
    );
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Cmd),
        "Windows native must use WindowsShell::Cmd for the .cmd shim — got {:?}",
        recipe.windows_shell
    );
}

#[test]
fn spawn_recipe_direct_on_macos() {
    let recipe = COMMANDCODE.spawn_recipe(Platform::Macos, EnvType::Windows);
    assert_eq!(recipe.binary, "cmd");
    // Issue #2151: bare base recipe (see windows-native pin above).
    assert!(
        recipe.base_args.is_empty(),
        "macOS base recipe must carry no approval flags; got {:?}",
        recipe.base_args
    );
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Direct),
        "macOS must use WindowsShell::Direct — got {:?}",
        recipe.windows_shell
    );
}

#[test]
fn spawn_recipe_direct_on_linux() {
    let recipe = COMMANDCODE.spawn_recipe(Platform::Linux, EnvType::Windows);
    assert_eq!(recipe.binary, "cmd");
    // Issue #2151: bare base recipe (see windows-native pin above).
    assert!(
        recipe.base_args.is_empty(),
        "Linux base recipe must carry no approval flags; got {:?}",
        recipe.base_args
    );
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Direct),
        "Linux must use WindowsShell::Direct — got {:?}",
        recipe.windows_shell
    );
}

#[test]
fn spawn_recipe_wsl_uses_cmd_binary() {
    let recipe = COMMANDCODE.spawn_recipe(Platform::Windows, EnvType::Wsl);
    assert_eq!(recipe.binary, "cmd");
    // Issue #2151: bare base recipe (see windows-native pin above).
    assert!(
        recipe.base_args.is_empty(),
        "WSL base recipe must carry no approval flags; got {:?}",
        recipe.base_args
    );
}

#[test]
fn available_on_includes_all_three_platforms() {
    let platforms = COMMANDCODE.descriptor().available_on;
    assert_eq!(platforms.len(), 3);
    assert!(platforms.contains(&Platform::Windows));
    assert!(platforms.contains(&Platform::Linux));
    assert!(platforms.contains(&Platform::Macos));
}

#[test]
fn self_assigns_session_id() {
    assert!(COMMANDCODE.self_assigns_session_id());
    assert!(
        !COMMANDCODE.captures_session_id_from_pty(),
        "Command Code captures session id via post-spawn polling, not PTY regex"
    );
}

#[test]
fn resume_args_format() {
    let args = COMMANDCODE.resume_args("3fadada6-e0a3-44a2-ab68-ce1ecf7207a9");
    assert_eq!(
        args,
        vec!["--resume", "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9"]
    );
}

#[test]
fn session_assign_args_empty() {
    let args = COMMANDCODE.session_assign_args("any-id");
    assert!(args.is_empty());
}

#[test]
fn model_args_use_model_flag() {
    let args = COMMANDCODE.model_args("taste-1");
    assert_eq!(args, vec!["--model", "taste-1"]);
}

#[test]
fn effort_args_use_effort_flag() {
    let args = COMMANDCODE.effort_args("high");
    assert_eq!(args, vec!["--effort", "high"]);
}

#[test]
fn prefill_args_positional() {
    let args = COMMANDCODE.prefill_args("implement the feature");
    assert_eq!(args, vec!["implement the feature"]);
}

#[test]
fn supports_expected_capabilities() {
    assert!(COMMANDCODE.descriptor().supports_resume);
    assert!(COMMANDCODE.descriptor().auto_resume_on_startup);
    assert!(COMMANDCODE.descriptor().supports_model_override);
    assert_eq!(
        COMMANDCODE.descriptor().effort_control,
        EffortControlKind::Closed {
            allowed: COMMANDCODE_EFFORT_ALLOWED
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    );
    assert!(COMMANDCODE.descriptor().supports_extra_args);
    assert!(COMMANDCODE.descriptor().supports_prefill);
    assert!(!COMMANDCODE.descriptor().requires_attention_hook());
    assert!(COMMANDCODE.descriptor().produces_readable_transcript);
}

#[test]
fn capabilities_descriptor_matches() {
    let caps = crate::agent::capabilities::capabilities_for(&COMMANDCODE);
    assert_eq!(caps.harness_id, "commandcode");
    assert!(caps.supports_resume);
    assert!(caps.auto_resume_on_startup);
    assert!(caps.supports_model_override);
    assert!(caps.supports_effort_override);
    assert!(caps.supports_extra_args);
    assert!(caps.supports_prefill);
    assert!(!caps.requires_attention_hook);
    assert!(caps.produces_readable_transcript);
    assert!(!caps.is_plain_terminal);
    assert_eq!(
        caps.effort_control,
        crate::agent::capabilities::EffortControlKind::Closed {
            allowed: COMMANDCODE_EFFORT_ALLOWED
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    );
}

#[test]
fn resume_recipe_carries_session_flag() {
    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Resume {
            id: "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9",
            cause: crate::agent::spawn::ResumeCause::Explicit,
        },
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = COMMANDCODE.prepare_launch(input);
    // Issue #2151: the unattended permission default contributes
    // `--yolo` at the front (where the base recipe used to carry it,
    // issue #1419); `--resume <id>` follows (issue #1500).
    assert_eq!(
        prepared.recipe.base_args,
        vec![
            "--yolo".to_string(),
            "--resume".to_string(),
            "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9".to_string()
        ]
    );
}

/// Issue #2151 regression pin: changing the permission-mode setting
/// changes the argv of the next spawn. The default (no stored value)
/// keeps today's unattended `--yolo`; prompt drops it.
#[test]
fn permission_mode_changes_argv_of_next_spawn() {
    use crate::agent::capabilities::{PERMISSION_MODE_PROMPT, PERMISSION_MODE_UNATTENDED};

    fn prepared_argv(mode: Option<&str>) -> Vec<String> {
        let config = ResolvedAgentConfig {
            model: None,
            effort: None,
            extra_args: None,
            permission_mode: mode.map(str::to_string),
        };
        let input = HarnessLaunchInput {
            routing: None,
            node_id: None,
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        COMMANDCODE.prepare_launch(input).recipe.base_args
    }

    // No stored value: today's unattended behavior is preserved.
    assert!(
        prepared_argv(None).contains(&"--yolo".to_string()),
        "default spawn must carry --yolo"
    );
    // Explicit unattended: same flag.
    assert!(
        prepared_argv(Some(PERMISSION_MODE_UNATTENDED)).contains(&"--yolo".to_string()),
        "unattended spawn must carry --yolo"
    );
    // Prompt: the flag is gone, the binary is unchanged.
    let prompt_argv = prepared_argv(Some(PERMISSION_MODE_PROMPT));
    assert!(
        !prompt_argv.iter().any(|a| a == "--yolo"),
        "prompt spawn must not carry --yolo; got {prompt_argv:?}"
    );
    // The mode descriptor the setting UI renders stays in sync with
    // the argv the spawn path emits.
    let modes = COMMANDCODE.descriptor().permission_modes.clone();
    assert_eq!(modes.len(), 2);
    assert_eq!(modes[0].id, PERMISSION_MODE_UNATTENDED);
    assert!(modes[0].label.contains("--yolo"));
    assert_eq!(
        COMMANDCODE
            .descriptor()
            .default_permission_mode()
            .as_deref(),
        Some(PERMISSION_MODE_UNATTENDED)
    );
}

#[test]
fn fresh_recipe_forwards_model_and_prompt_without_session_id() {
    let config = ResolvedAgentConfig {
        model: Some("taste-1".to_string()),
        effort: None,
        extra_args: None,
        permission_mode: None,
    };
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: Some("refactor auth flow"),
        sandbox: false,
    };
    let prepared = COMMANDCODE.prepare_launch(input);
    let args = &prepared.recipe.base_args;
    // Issue #1419: `--yolo` must be present on every fresh-spawn
    // recipe alongside `--model` and the positional prefill.
    assert!(
        args.contains(&"--yolo".to_string()),
        "--yolo missing from fresh recipe; got {args:?}"
    );
    assert_flag_followed_by_value(args, "--model", "taste-1");
    assert!(args.contains(&"refactor auth flow".to_string()));
    assert!(!args
        .iter()
        .any(|a| a == "--session" || a == "--session-id" || a == "--resume"));
}
