use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};

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

    fn supports_model_override(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Macos, Platform::Linux]
    }
}
