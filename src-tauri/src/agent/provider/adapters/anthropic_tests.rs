use super::*;
use crate::agent::capabilities::{
    ResolvedAgentConfig, PERMISSION_MODE_PROMPT, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::launch::{HarnessLaunchInput, SessionIdModeRef};
use crate::models::EnvType;

#[test]
fn prompted_launch_keeps_settings_before_the_positional_prompt() {
    let config = ResolvedAgentConfig {
        model: Some("sonnet".into()),
        effort: Some("high".into()),
        extra_args: Some("--verbose".into()),
        permission_mode: Some(PERMISSION_MODE_PROMPT.into()),
    };
    for platform in [Platform::Windows, Platform::Macos, Platform::Linux] {
        for session in [
            SessionIdModeRef::None,
            SessionIdModeRef::Assign("123e4567-e89b-12d3-a456-426614174000"),
            SessionIdModeRef::Resume {
                id: "123e4567-e89b-12d3-a456-426614174000",
                cause: crate::agent::spawn::ResumeCause::Explicit,
            },
        ] {
            let prepared = ANTHROPIC.prepare_launch(HarnessLaunchInput {
                routing: None,
                node_id: None,
                platform,
                runtime: EnvType::Windows,
                session,
                config: &config,
                prefill: Some("- review this diff\r\n+ added line"),
                sandbox: false,
            });
            let mut expected = Vec::new();
            match session {
                SessionIdModeRef::Assign(id) => expected.extend(["--session-id", id]),
                SessionIdModeRef::Resume { id, .. } => expected.extend(["--resume", id]),
                SessionIdModeRef::None => {}
            }
            expected.extend(["--model", "sonnet", "--effort", "high", "--verbose"]);
            expected.extend([
                "--",
                match platform {
                    Platform::Windows => "- review this diff + added line",
                    Platform::Macos | Platform::Linux => "- review this diff\n+ added line",
                },
            ]);
            assert_eq!(prepared.recipe.base_args, expected);
        }
    }
}

/// Issue #2151: the Claude-backed base recipe is bare — no adapter
/// keeps a hidden unattended argv.
#[test]
fn spawn_recipe_is_bare_on_every_platform() {
    for platform in [Platform::Windows, Platform::Macos, Platform::Linux] {
        let recipe = ANTHROPIC.spawn_recipe(platform, EnvType::Windows);
        assert!(
            recipe.base_args.is_empty(),
            "base recipe must carry no approval flags on {platform:?}; got {:?}",
            recipe.base_args
        );
    }
}

/// Issue #2151 regression pin: changing the permission-mode setting
/// changes the argv of the next spawn. The default (no stored value)
/// keeps today's `--dangerously-skip-permissions`; prompt drops it.
#[test]
fn permission_mode_changes_argv_of_next_spawn() {
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
        ANTHROPIC.prepare_launch(input).recipe.base_args
    }

    // No stored value: today's unattended behavior is preserved.
    assert!(
        prepared_argv(None).contains(&"--dangerously-skip-permissions".to_string()),
        "default spawn must carry --dangerously-skip-permissions"
    );
    // Explicit unattended: same flag.
    assert!(
        prepared_argv(Some(PERMISSION_MODE_UNATTENDED))
            .contains(&"--dangerously-skip-permissions".to_string()),
        "unattended spawn must carry --dangerously-skip-permissions"
    );
    // Prompt: the flag is gone.
    let prompt_argv = prepared_argv(Some(PERMISSION_MODE_PROMPT));
    assert!(
        !prompt_argv
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"),
        "prompt spawn must not carry --dangerously-skip-permissions; got {prompt_argv:?}"
    );
    // The mode descriptor the setting UI renders stays in sync with
    // the argv the spawn path emits.
    let modes = ANTHROPIC.descriptor().permission_modes.clone();
    assert_eq!(modes.len(), 2);
    assert_eq!(modes[0].id, PERMISSION_MODE_UNATTENDED);
    assert!(modes[0].label.contains("--dangerously-skip-permissions"));
    assert_eq!(
        ANTHROPIC.descriptor().default_permission_mode().as_deref(),
        Some(PERMISSION_MODE_UNATTENDED)
    );
}
