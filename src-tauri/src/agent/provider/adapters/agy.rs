use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};

pub struct AgyAdapter;
pub static AGY: AgyAdapter = AgyAdapter;

impl AgentProvider for AgyAdapter {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Antigravity CLI".into(),
            color: "#10b981".into(),
            icon: "G".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform) -> SpawnRecipe {
        SpawnRecipe {
            binary: "agy",
            base_args: vec!["--dangerously-skip-permissions".into()],
            windows_shell: WindowsShell::Cmd,
        }
    }

    fn supports_resume(&self) -> bool {
        true
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

    fn supports_prefill(&self) -> bool {
        false
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }
}