use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};

pub struct KimiAdapter;
pub static KIMI: KimiAdapter = KimiAdapter;

impl AgentProvider for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Kimi".into(),
            color: "#00c4c4".into(),
            icon: "K".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform) -> SpawnRecipe {
        SpawnRecipe {
            binary: "cwrap",
            base_args: vec!["--kimi".into()],
            windows_shell: WindowsShell::PowerShell,
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
        &[Platform::Windows, Platform::Linux]
    }
}
