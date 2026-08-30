use crate::agent::capabilities::{EffortControlKind, CLAUDE_EFFORT_ALLOWED};
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

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn requires_attention_hook(&self) -> bool {
        true
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

    /// Claude Code's reasoning-effort knob is the closed-vocab
    /// `--effort <low|medium|high>` flag. The vocabulary list lives in
    /// `agent::capabilities::CLAUDE_EFFORT_ALLOWED` (issue #1143 research)
    /// and is consumed by both this method and the resolver.
    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::Closed {
            allowed: CLAUDE_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
        }
    }
}
