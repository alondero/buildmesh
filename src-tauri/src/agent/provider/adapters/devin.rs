use crate::agent::attention_hooks::{
    provision_attention_hooks, EventMerge, EventRule, HandlerShape, HookFormat,
    HookProvisioningSpec, JsonHookShape, Ownership,
};
use crate::agent::capabilities::{
    AttentionCapability, AttentionLaunchMode, EffortControlKind, PermissionModeOption,
    PERMISSION_MODE_UNATTENDED,
};
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::agent::session_lifecycle::LifecycleKind;
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::path::Path;

pub struct DevinAdapter;
pub static DEVIN: DevinAdapter = DevinAdapter;

pub const DEVIN_MIN_HOOK_VERSION: &str = "3000.14.30821";

const BUILDMESH_HOOK_ANCHORS: &[&str] =
    &["/api/attention/", "BUILDMESH_PORT", "BUILDMESH_SESSION_ID"];

fn hook_command(env_type: EnvType) -> String {
    if env_type == EnvType::WindowsInterop {
        if let Some(command) = crate::env::windows_attention_command(None) {
            return command;
        }
    }
    let curl = crate::env::unix_attention_curl();
    format!("{curl} -sf --connect-timeout 1 --max-time 2 -o /dev/null -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true")
}

const DEVIN_HOOKS: HookProvisioningSpec<'static> = HookProvisioningSpec {
    harness: "devin",
    file: "hooks.v1.json",
    label: "devin hooks.v1.json",
    command: "",
    event_commands: &[],
    format: HookFormat::Json(JsonHookShape {
        root_key: None,
        replace_root: false,
        required_root: &[],
        handler: HandlerShape::TypedCommand,
        ownership: Ownership {
            fields: &["command"],
            anchors: BUILDMESH_HOOK_ANCHORS,
        },
        events: &[
            EventRule {
                name: "SessionStart",
                optional: false,
                merge: EventMerge::MatcherGroup { matcher: None },
            },
            EventRule {
                name: "UserPromptSubmit",
                optional: false,
                merge: EventMerge::MatcherGroup { matcher: None },
            },
            EventRule {
                name: "Stop",
                optional: false,
                merge: EventMerge::MatcherGroup { matcher: None },
            },
            EventRule {
                name: "PermissionRequest",
                optional: false,
                merge: EventMerge::MatcherGroup { matcher: None },
            },
            EventRule {
                name: "PreToolUse",
                optional: false,
                merge: EventMerge::MatcherGroup {
                    matcher: Some("^(ask_user_question|exit_plan_mode)$"),
                },
            },
            EventRule {
                name: "PostToolUse",
                optional: false,
                merge: EventMerge::MatcherGroup {
                    matcher: Some("^(ask_user_question|exit_plan_mode)$"),
                },
            },
        ],
    }),
};

fn ensure_hooks_v1_json(path: &Path, command: &str) -> Result<(), String> {
    let target_dir = path
        .parent()
        .ok_or_else(|| "devin hooks.v1.json has no parent directory".to_string())?;
    let spec = HookProvisioningSpec {
        command,
        ..DEVIN_HOOKS
    };
    provision_attention_hooks(&spec, target_dir)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

impl AgentProvider for DevinAdapter {
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| {
                crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                attention_capability: {
                    AttentionCapability::Hook {
                        events: vec![
                            LifecycleKind::TurnCompleted,
                            LifecycleKind::WorkResumed,
                            LifecycleKind::QuestionRequested,
                            LifecycleKind::PermissionRequested,
                        ],
                        launch_mode: AttentionLaunchMode::SkipPermissions,
                        trust: Some("workspace trust".into()),
                        min_version: Some(DEVIN_MIN_HOOK_VERSION.into()),
                    }
                },
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

    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        let devin_dir = Path::new(&resolved.host_path).join(".devin");
        std::fs::create_dir_all(&devin_dir)
            .map_err(|e| format!("failed to create .devin dir: {e}"))?;
        ensure_hooks_v1_json(
            &devin_dir.join("hooks.v1.json"),
            &hook_command(resolved.env_type),
        )
    }

    fn confirms_prompt_submission(&self) -> bool {
        true
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
