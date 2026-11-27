use super::hook_contract;
use crate::agent::capabilities::{
    EffortControlKind, PermissionModeOption, CLAUDE_EFFORT_ALLOWED, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::model_catalog::{
    HarnessCatalogEffort, HarnessCatalogModel, ModelCatalogProbe, ModelEffortStorage,
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

const CATALOG_REQUEST_ID: &str = "model-catalog";

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

    fn model_catalog_probe(&self) -> Option<ModelCatalogProbe> {
        Some(ModelCatalogProbe {
            binary: None,
            args: [
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
                "--setting-sources",
                "user",
                "--tools=",
                "--strict-mcp-config",
                "--mcp-config",
                r#"{"mcpServers":{}}"#,
                "--settings",
                r#"{"disableAllHooks":true}"#,
            ]
            .map(str::to_owned)
            .to_vec(),
            stdin: Some(concat!(
                r#"{"type":"control_request","request_id":"model-catalog","request":{"subtype":"initialize"}}"#,
                "\n"
            )),
            effort_storage: ModelEffortStorage::Separate,
            allow_unlisted_models: true,
            login_home_env: None,
            env_remove: crate::agent::provider::CLAUDE_BACKEND_ENV_VARS,
        })
    }

    fn parse_model_catalog(&self, stdout: &str) -> Result<Vec<HarnessCatalogModel>, String> {
        let models = stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|event| {
                event["type"] == "control_response"
                    && event["response"]["request_id"] == CATALOG_REQUEST_ID
                    && event["response"]["subtype"] == "success"
            })
            .ok_or("Claude Code returned no model catalog response")?;
        let models = models["response"]["response"]["models"]
            .as_array()
            .ok_or("Claude Code model catalog has no models array")?;
        Ok(models
            .iter()
            .filter_map(|model| {
                let id = model["value"]
                    .as_str()
                    .filter(|id| !id.is_empty() && *id != "default")?;
                let efforts = model["supportedEffortLevels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|level| level.as_str())
                    .filter(|level| CLAUDE_EFFORT_ALLOWED.contains(level))
                    .map(|level| HarnessCatalogEffort {
                        id: level.into(),
                        name: level.into(),
                        description: None,
                        model: None,
                    })
                    .collect();
                Some(HarnessCatalogModel {
                    id: id.into(),
                    name: model["displayName"].as_str().unwrap_or(id).into(),
                    description: model["description"].as_str().map(str::to_owned),
                    efforts,
                })
            })
            .collect())
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
