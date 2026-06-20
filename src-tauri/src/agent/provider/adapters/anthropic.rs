use crate::agent::provider::{
    claude_direct_recipe, AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell,
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
        match platform {
            Platform::Macos => SpawnRecipe {
                binary: "claude",
                base_args: vec!["--dangerously-skip-permissions".into()],
                windows_shell: WindowsShell::Direct,
            },
            Platform::Windows | Platform::Linux => SpawnRecipe {
                binary: "cwrap",
                base_args: vec!["--anthropic".into()],
                windows_shell: WindowsShell::PowerShell,
            },
        }
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

    /// In the Windows AppContainer sandbox, reach claude.exe directly — the
    /// cwrap → MSYS2 bash chain can't initialize there. Anthropic uses the
    /// built-in subscription, so no backend env override is needed (the default
    /// empty `sandbox_provider_env` applies).
    fn sandbox_direct_recipe(&self, _platform: Platform) -> Option<SpawnRecipe> {
        Some(claude_direct_recipe())
    }
}
