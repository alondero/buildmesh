use super::hook_contract;
use crate::agent::capabilities::{
    EffortControlKind, PermissionModeOption, CLAUDE_EFFORT_ALLOWED, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::provider::{
    claude_direct_recipe, AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta,
};
use crate::circuit::strategy::{
    FinalReportSource, HookPush, ObservationStrategy, OwnedWorkCoverage, PullSource, PushSource,
    ReconciliationPolicy, StrategyNotes, TurnIdentity,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;

pub struct AnthropicAdapter;
pub static ANTHROPIC: AnthropicAdapter = AnthropicAdapter;

impl AgentProvider for AnthropicAdapter {
    fn prepare_launch(
        &self,
        input: crate::agent::launch::HarnessLaunchInput<'_>,
    ) -> crate::agent::launch::PreparedHarnessLaunch {
        let mut prepared = crate::agent::launch::default_prepare(self, input);
        prepared.environment.env_remove.extend(
            crate::agent::provider::CLAUDE_BACKEND_ENV_VARS
                .iter()
                .map(|key| key.to_string()),
        );
        prepared
    }

    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                attention_capability: {
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
                },
                circuit_observation: {
                    ObservationStrategy {
            push: PushSource::Hooks(HookPush {
                parse: |value| hook_contract::parse("anthropic", true, value),
                source: "claude_native_hook",
                request_source: "anthropic_request_hook",
                ownership_gap: None,
                foreground: true,
            }),
            pull: PullSource::None,
            turn_identity: TurnIdentity::SubmissionEcho,
            owned_work: OwnedWorkCoverage::HookRegistry,
            final_report: FinalReportSource::HookMessage,
            reconciliation: ReconciliationPolicy {
                yielded_budget_ms: 90_000,
            },
            passive_watcher: None,
            notes: StrategyNotes {
                foreground: "UserPromptSubmit prompt echo bound to a recorded submission, then prompt_id inherited by Stop; receipts without a provable binding stay reduced confidence; live delivery unverified",
                owned_work: "Child hooks and explicit task/cron registries; missing coverage remains unverified",
                final_report: "Scrubbed Stop response when supplied; otherwise unavailable",
                reconciliation: "Durable hook receipt replay bound through the recorded submission; no authoritative ownership pull is available",
            },
        }
                },
                produces_readable_transcript: true,
                supports_model_override: true,
                supports_extra_args: true,
                supports_prefill: true,
                available_on: { &[Platform::Windows, Platform::Macos, Platform::Linux] },
                effort_control: {
                    EffortControlKind::Closed {
                        allowed: CLAUDE_EFFORT_ALLOWED
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    }
                },
                permission_modes: {
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
                },
                prefill_auto_submits: true,
                ..Default::default()
            });
        &DESCRIPTOR
    }

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

    /// Claude Code fires `UserPromptSubmit` when a prompt starts a turn.
    fn confirms_prompt_submission(&self) -> bool {
        true
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // --prefill stages a draft; a positional prompt starts the first turn.
        // End option parsing so a handover beginning with '-' remains prompt text.
        vec!["--".into(), text.into()]
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
#[path = "anthropic_tests.rs"]
mod tests;
