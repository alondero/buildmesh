use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};

pub struct MinimaxAdapter;
pub static MINIMAX: MinimaxAdapter = MinimaxAdapter;

impl AgentProvider for MinimaxAdapter {
    fn id(&self) -> &'static str {
        "minimax"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "MiniMax".into(),
            color: "#6366f1".into(),
            icon: "M".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform) -> SpawnRecipe {
        SpawnRecipe {
            binary: "cwrap",
            base_args: vec!["--minimax".into()],
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
