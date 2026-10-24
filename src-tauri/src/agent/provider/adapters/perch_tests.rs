use super::*;
use crate::agent::capabilities::ResolvedAgentConfig;
use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};
use crate::agent::provider::WindowsShell;

fn config(
    permission_mode: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
) -> ResolvedAgentConfig {
    ResolvedAgentConfig {
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        extra_args: None,
        permission_mode: permission_mode.map(str::to_string),
    }
}

#[test]
fn id_and_ui_metadata() {
    assert_eq!(PERCH.id(), "perch");
    let ui = PERCH.ui();
    assert_eq!(ui.label, "Perch");
    assert_eq!(ui.color, "#c2410c");
    assert_eq!(ui.icon, "P");
}

#[test]
fn spawn_recipe_direct_on_macos_and_linux() {
    for platform in [Platform::Linux, Platform::Macos] {
        let recipe = PERCH.spawn_recipe(platform, EnvType::Windows);
        assert_eq!(recipe.binary, "perch");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "{platform:?} must spawn perch directly"
        );
    }
}

#[test]
fn spawn_recipe_cmd_on_windows_native() {
    let recipe = PERCH.spawn_recipe(Platform::Windows, EnvType::Windows);
    assert_eq!(recipe.binary, "perch");
    assert!(recipe.base_args.is_empty());
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Cmd),
        "Windows must use cmd.exe for the npm .cmd shim — got {:?}",
        recipe.windows_shell
    );
}

#[test]
fn session_args_use_thread_for_assign_and_resume() {
    assert_eq!(
        PERCH.session_assign_args("fresh-1"),
        vec!["--thread", "fresh-1"]
    );
    assert_eq!(PERCH.resume_args("fresh-1"), vec!["--thread", "fresh-1"]);
    assert!(!PERCH.self_assigns_session_id());
}

#[test]
fn fresh_spawn_is_unattended_and_assigns_a_thread() {
    // perchai-cli 2.4.116 accepts `--thread` on the interactive parser and
    // rejects a positional prompt. Unattended is `--permission take_the_wheel`.
    let config = config(None, None, None);
    let prepared = default_prepare(
        &PERCH,
        HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Assign("fresh-uuid-99"),
            config: &config,
            prefill: Some("start work"),
            sandbox: false,
        },
    );
    assert_eq!(
        prepared.recipe.base_args,
        vec![
            "--permission",
            "take_the_wheel",
            "--thread",
            "fresh-uuid-99"
        ]
    );
    assert!(
        !prepared
            .recipe
            .base_args
            .iter()
            .any(|arg| arg == "start work"),
        "interactive perch does not take a positional prompt"
    );
}

#[test]
fn resume_keeps_the_same_thread_flag() {
    let config = config(None, None, None);
    let prepared = default_prepare(
        &PERCH,
        HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume {
                id: "existing-uuid-100",
                cause: crate::agent::spawn::ResumeCause::Explicit,
            },
            config: &config,
            prefill: None,
            sandbox: false,
        },
    );
    assert_eq!(
        prepared.recipe.base_args,
        vec![
            "--permission",
            "take_the_wheel",
            "--thread",
            "existing-uuid-100"
        ]
    );
}

#[test]
fn prompt_mode_omits_the_permission_flag() {
    let config = config(Some("prompt"), Some("standard 1m"), Some("high"));
    let prepared = default_prepare(
        &PERCH,
        HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Assign("thread-7"),
            config: &config,
            prefill: None,
            sandbox: false,
        },
    );
    assert_eq!(
        prepared.recipe.base_args,
        vec![
            "--thread",
            "thread-7",
            "--model",
            "standard 1m",
            "--effort",
            "high"
        ]
    );
}

#[test]
fn model_override_rejects_flags_and_newlines() {
    assert!(PERCH.validate_model_override("standard 1m").is_ok());
    assert!(PERCH.validate_model_override("pro").is_ok());
    assert!(PERCH.validate_model_override("").is_err());
    assert!(PERCH.validate_model_override("--model").is_err());
    assert!(PERCH.validate_model_override("pro\n1m").is_err());
}

#[test]
fn capabilities_descriptor_shape() {
    let caps = PERCH.capabilities();
    assert_eq!(caps.harness_id, "perch");
    assert!(caps.supports_resume);
    assert!(caps.auto_resume_on_startup);
    assert!(!caps.supports_prefill);
    assert!(caps.supports_extra_args);
    assert!(caps.supports_model_override);
    assert!(caps.supports_effort_override);
    assert!(!caps.requires_attention_hook);
    assert!(!caps.produces_readable_transcript);
    assert!(!caps.supports_passive_turn_watcher);
    assert!(caps.background_inference.is_none());
    assert!(!caps.is_plain_terminal);
    assert_eq!(
        caps.effort_control,
        EffortControlKind::Closed {
            allowed: PERCH_EFFORT_ALLOWED
                .iter()
                .map(|level| level.to_string())
                .collect(),
        }
    );
    assert_eq!(caps.permission_modes.len(), 2);
    assert_eq!(caps.permission_modes[0].id, PERMISSION_MODE_UNATTENDED);
}
