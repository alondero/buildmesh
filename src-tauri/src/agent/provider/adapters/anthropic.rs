use crate::agent::provider::{
    claude_direct_recipe, AgentProvider, Platform, SpawnRecipe, UiMeta,
};

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

    fn spawn_recipe(&self, platform: Platform) -> SpawnRecipe {
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

    fn produces_readable_transcript(&self) -> bool {
        true
    }

    fn supports_model_override(&self) -> bool {
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
}
