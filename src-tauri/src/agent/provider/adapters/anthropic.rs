use crate::agent::capabilities::{
    EffortControlKind, PermissionModeOption, CLAUDE_EFFORT_ALLOWED, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::provider::{
    claude_direct_recipe, AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;

pub struct AnthropicAdapter;
pub static ANTHROPIC: AnthropicAdapter = AnthropicAdapter;

impl AgentProvider for AnthropicAdapter {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Anthropic (Claude)".into(),
            color: "#1d7cfc".into(),
            icon: "A".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        claude_direct_recipe(platform)
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn background_recipe(
        &self,
        platform: Platform,
    ) -> Option<crate::agent::background::BackgroundRecipe> {
        use crate::agent::{
            background::BackgroundRecipe,
            capabilities::{BackgroundPromptInput, BackgroundResultOutput},
        };
        let mut spawn = self.spawn_recipe(platform, EnvType::Windows);
        spawn.base_args = [
            "--print",
            "--output-format",
            "text",
            "--no-session-persistence",
            "--tools=",
            "--disallowedTools=mcp__*",
        ]
        .map(str::to_owned)
        .to_vec();
        let mut recipe = BackgroundRecipe::new(
            spawn,
            BackgroundPromptInput::Stdin,
            BackgroundResultOutput::Stdout,
        );
        recipe.capability.supports_provider_routing = true;
        recipe.env_remove = crate::agent::provider::CLAUDE_BACKEND_ENV_VARS
            .iter()
            .map(|key| (*key).to_owned())
            .collect();
        Some(recipe)
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn requires_attention_hook(&self) -> bool {
        true
    }

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        AttentionCapability::Hook {
            events: vec![
                LifecycleKind::TurnCompleted,
                LifecycleKind::InputRequired,
                LifecycleKind::QuestionRequested,
                LifecycleKind::PermissionRequested,
                LifecycleKind::BackgroundRunning,
            ],
            launch_mode: AttentionLaunchMode::SkipPermissions,
            trust: Some("workspace trust".into()),
            min_version: None,
        }
    }

    fn ensure_workspace_trusted(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
    ) -> Result<(), String> {
        crate::agent::workspace_trust::ensure_trusted(resolved);
        Ok(())
    }

    /// Claude Code reads its hooks from `.claude/settings.local.json`; the
    /// shared helper in `agent::spawn` owns that format (the mesh commands
    /// also call it directly to pre-provision at mesh creation).
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        crate::agent::spawn::inject_attention_hook(std::path::Path::new(&resolved.host_path))
    }

    fn produces_readable_transcript(&self) -> bool {
        true
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Macos, Platform::Linux]
    }

    /// Reset the inherited claude backend env (cwrap `unset` parity). Anthropic
    /// exports nothing of its own — `provider_env` is empty — so clearing any
    /// inherited `ANTHROPIC_*` override is its whole contribution, keeping the
    /// built-in subscription on the default Anthropic endpoint.
    fn resets_backend_env(&self) -> bool {
        true
    }

    /// Claude Code's reasoning-effort knob is the closed-vocabulary
    /// `--effort` flag. The accepted list lives in
    /// `agent::capabilities::CLAUDE_EFFORT_ALLOWED` and is consumed by both
    /// this method and the resolver.
    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::Closed {
            allowed: CLAUDE_EFFORT_ALLOWED
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }

    /// Issue #2151: Buildmesh passes Claude Code's own flag through.
    /// Unattended keeps today's `--dangerously-skip-permissions` (required
    /// for human-out-of-the-loop runs, including Circuits); prompt launches
    /// the CLI bare so it asks like a human-launched session.
    fn permission_modes(&self) -> Vec<PermissionModeOption> {
        vec![
            PermissionModeOption::unattended(
                "--dangerously-skip-permissions",
                "Prompts off — tools run without asking (today's behavior; required for unattended runs).",
            ),
            PermissionModeOption::prompt(
                "Prompts on (no flag)",
                "Claude Code asks for approval like a human-launched session.",
            ),
        ]
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == PERMISSION_MODE_UNATTENDED {
            vec!["--dangerously-skip-permissions".into()]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::capabilities::{
        ResolvedAgentConfig, PERMISSION_MODE_PROMPT, PERMISSION_MODE_UNATTENDED,
    };
    use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};
    use crate::models::EnvType;

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
                platform: Platform::Linux,
                runtime: EnvType::Windows,
                session: SessionIdModeRef::None,
                config: &config,
                prefill: None,
                sandbox: false,
            };
            default_prepare(&ANTHROPIC, input).recipe.base_args
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
        let modes = ANTHROPIC.permission_modes();
        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0].id, PERMISSION_MODE_UNATTENDED);
        assert!(modes[0].label.contains("--dangerously-skip-permissions"));
        assert_eq!(
            ANTHROPIC.default_permission_mode().as_deref(),
            Some(PERMISSION_MODE_UNATTENDED)
        );
    }
}
