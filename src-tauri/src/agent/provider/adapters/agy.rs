use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

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

    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "agy",
            base_args: vec!["--dangerously-skip-permissions".into()],
            windows_shell: WindowsShell::Direct,
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
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--conversation".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec!["--prompt-interactive".into(), text.into()]
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }
}