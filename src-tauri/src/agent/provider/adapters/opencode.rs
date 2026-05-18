use crate::agent::provider::{AgentProvider, Platform, ProviderInfo, SpawnRecipe, WindowsShell};

pub struct OpenCodeAdapter;
pub static OPENCODE: OpenCodeAdapter = OpenCodeAdapter;

impl AgentProvider for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn ui(&self) -> ProviderInfo {
        ProviderInfo {
            id: "opencode".into(),
            label: "OpenCode".into(),
            color: "#f59e0b".into(),
            icon: "O".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform) -> SpawnRecipe {
        SpawnRecipe {
            binary: "opencode",
            base_args: vec![],
            windows_shell: WindowsShell::Cmd,
        }
    }

    fn supports_resume(&self) -> bool {
        false
    }

    fn auto_resume_on_startup(&self) -> bool {
        false
    }

    fn requires_attention_hook(&self) -> bool {
        false
    }

    fn supports_model_override(&self) -> bool {
        false
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux]
    }
}
