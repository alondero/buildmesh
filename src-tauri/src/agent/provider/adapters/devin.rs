use crate::agent::capabilities::{
    EffortControlKind, PermissionModeOption, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct DevinAdapter;
pub static DEVIN: DevinAdapter = DevinAdapter;

impl AgentProvider for DevinAdapter {
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| {
                crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                produces_readable_transcript: false,
                supports_model_override: true,
                supports_extra_args: true,
                supports_prefill: true,
                prefill_auto_submits: true,
                available_on: { &[Platform::Windows, Platform::Macos, Platform::Linux] },
                effort_control: EffortControlKind::None,
                permission_modes: vec![
                    PermissionModeOption::unattended(
                        "--permission-mode dangerous --respect-workspace-trust false",
                        "Prompts off — tools run without asking and the workspace trust dialog is skipped (required for unattended runs).",
                    ),
                    PermissionModeOption::prompt(
                        "Prompts on (no flag)",
                        "Devin asks for approval the way a human-launched session does.",
                    ),
                ],
                ..Default::default()
            }
            });
        &DESCRIPTOR
    }

    fn id(&self) -> &'static str {
        "devin"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Devin".into(),
            color: "#2563eb".into(),
            icon: "D".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "devin",
            base_args: Vec::new(),
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        }
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == PERMISSION_MODE_UNATTENDED {
            vec![
                "--permission-mode".into(),
                "dangerous".into(),
                "--respect-workspace-trust".into(),
                "false".into(),
            ]
        } else {
            Vec::new()
        }
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn captures_session_id_from_pty(&self) -> bool {
        false
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        Vec::new()
    }

    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        env_type: EnvType,
        _app: &tauri::AppHandle,
    ) {
        crate::services::devin_session::start_capture_poller(
            node_id,
            spawn_path.to_string(),
            env_type,
        );
    }

    fn recover_suspended_session_id(
        &self,
        spawn_path: &str,
        env_type: EnvType,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        crate::services::devin_session::find_historic_id_for_directory(
            env_type,
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec!["--".into(), text.into()]
    }
}

#[cfg(test)]
#[path = "devin_tests.rs"]
mod tests;
