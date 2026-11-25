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
    assert!(descriptor.auto_resume_on_startup);
    assert!(!descriptor.requires_attention_hook());
    assert!(descriptor.supports_model_override);
    assert!(descriptor.supports_prefill);
    assert!(descriptor.prefill_auto_submits);
    assert!(matches!(descriptor.effort_control, EffortControlKind::None));
    assert!(DEVIN.background_recipe(Platform::Linux).is_none());
    assert!(!descriptor.circuit_observation.has_foreground_source());
}
