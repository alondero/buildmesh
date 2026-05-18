use crate::agent::provider::{AgentProvider, Platform, ProviderInfo, SpawnRecipe, WindowsShell};

pub struct GeminiAdapter;
pub static GEMINI: GeminiAdapter = GeminiAdapter;

impl AgentProvider for GeminiAdapter {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn ui(&self) -> ProviderInfo {
        ProviderInfo {
            id: "gemini".into(),
            label: "Google Gemini".into(),
            color: "#10b981".into(),
            icon: "G".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform) -> SpawnRecipe {
        SpawnRecipe {
            binary: "gemini",
            base_args: vec!["--yolo".into()],
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
        &[Platform::Windows, Platform::Macos, Platform::Linux]
    }
}
