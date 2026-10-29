use super::*;
use crate::agent::capabilities::ResolvedAgentConfig;
use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

#[test]
fn id_and_ui_metadata() {
    assert_eq!(FREEBUFF.id(), "freebuff");
    let ui = FREEBUFF.ui();
    assert_eq!(ui.label, "Freebuff");
    assert_eq!(ui.color, "#f97316");
    assert_eq!(ui.icon, "F");
}

#[test]
fn spawn_recipe_direct_on_macos_and_linux() {
    for platform in [Platform::Linux, Platform::Macos] {
        let recipe = FREEBUFF.spawn_recipe(platform, EnvType::Windows);
        assert_eq!(recipe.binary, "freebuff");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "{platform:?} must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }
}

#[test]
fn spawn_recipe_cmd_on_windows_native() {
    let recipe = FREEBUFF.spawn_recipe(Platform::Windows, EnvType::Windows);
    assert_eq!(recipe.binary, "freebuff");
    assert!(recipe.base_args.is_empty());
    assert!(
        matches!(recipe.windows_shell, WindowsShell::Cmd),
        "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
        recipe.windows_shell
    );
}

#[test]
fn spawn_recipe_on_windows_host_with_wsl_runtime() {
    let recipe = FREEBUFF.spawn_recipe(Platform::Windows, EnvType::Wsl);
    assert_eq!(recipe.binary, "freebuff");
    // Windows platform emits WindowsShell::Cmd in adapter, runtime wrapping
    // by spawn_environment handles wsl.exe redirection.
    assert!(matches!(recipe.windows_shell, WindowsShell::Cmd));
}

#[test]
fn available_on_all_three_platforms() {
    let platforms = FREEBUFF.available_on();
    assert_eq!(platforms.len(), 3);
    assert!(platforms.contains(&Platform::Windows));
    assert!(platforms.contains(&Platform::Linux));
    assert!(platforms.contains(&Platform::Macos));
}

#[test]
fn does_not_self_assign_session_id() {
    // Must be false so Buildmesh generates the session UUID on fresh spawn.
    assert!(!FREEBUFF.self_assigns_session_id());
}

#[test]
fn session_assign_args_format() {
    let args = FREEBUFF.session_assign_args("sess-1234");
    assert_eq!(args, vec!["--continue", "sess-1234"]);
}

#[test]
fn resume_args_format() {
    let args = FREEBUFF.resume_args("sess-1234");
    assert_eq!(args, vec!["--continue", "sess-1234"]);
}

#[test]
fn initial_prompt_is_typed_not_prefilled() {
    assert!(!FREEBUFF.supports_prefill());
    assert!(FREEBUFF.accepts_initial_prompt());
}

#[test]
fn capabilities_descriptor_shape() {
    let caps = FREEBUFF.capabilities();
    assert_eq!(caps.harness_id, "freebuff");
    assert!(caps.supports_resume);
    assert!(caps.auto_resume_on_startup);
    assert!(!caps.supports_prefill);
    assert!(caps.accepts_initial_prompt);
    assert!(caps.supports_extra_args);
    assert!(!caps.supports_model_override);
    assert!(!caps.supports_effort_override);
    assert!(!caps.requires_attention_hook);
    assert!(!caps.produces_readable_transcript);
    assert!(!caps.is_plain_terminal);
    assert_eq!(
        caps.effort_control,
        crate::agent::capabilities::EffortControlKind::None
    );
}

#[test]
fn fresh_spawn_prepare_wires_session_assign_args() {
    let config = ResolvedAgentConfig {
        model: None,
        effort: None,
        extra_args: None,
        permission_mode: None,
    };
    let input = HarnessLaunchInput {
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Assign("fresh-uuid-99"),
        config: &config,
        prefill: Some("start work"),
        sandbox: false,
    };
    let prepared = default_prepare(&FREEBUFF, input);
    assert_eq!(
        prepared.recipe.base_args,
        vec!["--continue", "fresh-uuid-99"],
        "fresh spawn must pass the assigned session id and never the prompt on argv"
    );
}

#[test]
fn resume_prepare_wires_resume_args() {
    let config = ResolvedAgentConfig {
        model: None,
        effort: None,
        extra_args: None,
        permission_mode: None,
    };
    let input = HarnessLaunchInput {
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Resume {
            id: "existing-uuid-100",
            cause: crate::agent::spawn::ResumeCause::Explicit,
        },
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = default_prepare(&FREEBUFF, input);
    assert_eq!(
        prepared.recipe.base_args,
        vec!["--continue", "existing-uuid-100"],
        "resume spawn must pass resume args via --continue"
    );
}

#[test]
fn produces_readable_transcript_is_false() {
    assert!(!FREEBUFF.produces_readable_transcript());
}
