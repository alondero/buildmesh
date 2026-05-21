use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};

pub struct CodexAdapter;
pub static CODEX: CodexAdapter = CodexAdapter;

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::PowerShell,
    }
}

fn base_flags() -> Vec<String> {
    vec![
        "--ask-for-approval".into(),
        "never".into(),
        "--sandbox".into(),
        "danger-full-access".into(),
    ]
}

impl AgentProvider for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "OpenAI Codex".into(),
            color: "#10a37f".into(),
            icon: "X".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform) -> SpawnRecipe {
        SpawnRecipe {
            binary: "codex",
            base_args: base_flags(),
            windows_shell: shell_for(platform),
        }
    }

    fn spawn_recipe_for_resume(
        &self,
        platform: Platform,
        session_id: &str,
    ) -> Option<SpawnRecipe> {
        let mut args = vec!["resume".into(), session_id.into()];
        args.extend(base_flags());
        Some(SpawnRecipe {
            binary: "codex",
            base_args: args,
            windows_shell: shell_for(platform),
        })
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn requires_attention_hook(&self) -> bool {
        false
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Macos, Platform::Windows, Platform::Linux]
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn resume_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }
}
