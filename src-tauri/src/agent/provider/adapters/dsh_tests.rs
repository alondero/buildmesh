use super::*;
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};

#[test]
fn id_and_ui_metadata() {
    assert_eq!(DSH.id(), "dsh");
    let ui = DSH.ui();
    assert_eq!(ui.label, "DeepSeek Harness");
    assert_eq!(ui.color, "#1E88E5");
    assert_eq!(ui.icon, "D");
}

#[test]
fn spawn_recipe_is_bare_dsh_with_per_platform_shell() {
    for platform in [Platform::Linux, Platform::Macos] {
        let recipe = DSH.spawn_recipe(platform, EnvType::Windows);
        assert_eq!(recipe.binary, "dsh");
        assert!(
            recipe.base_args.is_empty(),
            "no profile / no app args while gated; got {:?}",
            recipe.base_args
        );
        assert!(recipe.trailing_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "{:?} must use WindowsShell::Direct — got {:?}",
            platform,
            recipe.windows_shell
        );
    }
    let win_recipe = DSH.spawn_recipe(Platform::Windows, EnvType::Windows);
    assert_eq!(win_recipe.binary, "dsh");
    assert!(win_recipe.base_args.is_empty());
    assert!(win_recipe.trailing_args.is_empty());
    assert!(matches!(win_recipe.windows_shell, WindowsShell::Cmd));
}

#[test]
fn available_on_all_three_platforms() {
    let platforms = DSH.descriptor().available_on;
    assert_eq!(
        platforms.len(),
        3,
        "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
        platforms
    );
    assert!(platforms.contains(&Platform::Windows));
    assert!(platforms.contains(&Platform::Linux));
    assert!(platforms.contains(&Platform::Macos));
}

/// End-to-end: when `supports_resume = false`, the orchestrator
/// routes `SessionIdModeRef::None` (prepare.rs:249) and the
/// resolver drops `--model` — so the prepared recipe carries
/// neither flag regardless of whether the user supplied values.
/// This is the production path; gating the test to `Assign` would
/// exercise a code path the orchestrator never takes.
#[test]
fn default_prepare_emits_no_session_id_or_model() {
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &crate::agent::capabilities::ResolvedAgentConfig {
            model: Some("deepseek-chat".into()),
            effort: None,
            extra_args: None,
            permission_mode: None,
        },
        prefill: None,
        sandbox: false,
    };
    let prepared = DSH.prepare_launch(input);
    assert_eq!(prepared.recipe.binary, "dsh");
    let args = &prepared.recipe.base_args;
    assert!(
        !args.iter().any(|a| a == "--session-id"),
        "--session-id must not be in the recipe when supports_resume = false; \
             orchestrator routes SessionIdModeRef::None; got {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--model"),
        "--model must be dropped by the resolver while supports_model_override = false; \
             got {args:?}"
    );
}

/// Capabilities descriptor end-to-end (issue #1149 + #1365).
/// Pins the gated state: no resume, no model, no attention hook,
/// no transcript, no prefill. A maintainer flipping a profile in
/// must flip these back too.
#[test]
fn capabilities_descriptor_advertises_gated_state() {
    let caps = crate::agent::capabilities::capabilities_for(&DSH);
    assert_eq!(caps.harness_id, "dsh");
    assert!(!caps.supports_resume);
    assert!(!caps.auto_resume_on_startup);
    assert!(!caps.requires_attention_hook);
    assert!(!caps.produces_readable_transcript);
    assert!(!caps.supports_model_override);
    assert!(!caps.supports_effort_override);
    assert!(caps.supports_extra_args);
    assert!(!caps.supports_prefill);
    assert!(!caps.is_plain_terminal);
    assert_eq!(
        caps.effort_control,
        crate::agent::capabilities::EffortControlKind::None
    );
    assert_eq!(
        caps.available_on,
        vec![
            "windows".to_string(),
            "linux".to_string(),
            "macos".to_string()
        ]
    );
}
