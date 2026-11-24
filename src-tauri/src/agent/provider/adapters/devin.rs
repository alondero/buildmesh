use crate::agent::attention_hooks::{
    provision_attention_hooks, EventMerge, EventRule, HandlerShape, HookFormat,
    HookProvisioningSpec, JsonHookShape, Ownership,
};
use crate::agent::capabilities::{
    EffortControlKind, PermissionModeOption, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::path::Path;

pub struct DevinAdapter;
pub static DEVIN: DevinAdapter = DevinAdapter;

pub const DEVIN_MIN_HOOK_VERSION: &str = "3000.14.30821";

const QUESTION_TOOL_MATCHER: &str = "^(ask_user_question|exit_plan_mode)$";

const DEVIN_OWNERSHIP: Ownership = Ownership {
    fields: &["command"],
    anchors: &["/api/attention/", "--data-binary"],
};

const DEVIN_HOOKS: HookProvisioningSpec<'static> = HookProvisioningSpec {
    harness: "devin",
    file: "config.local.json",
    label: "devin config.local.json",
    command: "",
    event_commands: &[],
    format: HookFormat::Json(JsonHookShape {
        root_key: Some("hooks"),
        replace_root: false,
        required_root: &[],
        handler: HandlerShape::TypedCommand,
        ownership: DEVIN_OWNERSHIP,
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
                    matcher: Some(QUESTION_TOOL_MATCHER),
                },
            },
            EventRule {
                name: "PostToolUse",
                optional: false,
                merge: EventMerge::MatcherGroup {
                    matcher: Some(QUESTION_TOOL_MATCHER),
                },
            },
        ],
    }),
};

fn attention_url(node_id: i64, port: u16) -> String {
    format!("http://localhost:{port}/api/attention/{node_id}")
}

fn hook_command(url: &str, windows: bool) -> String {
    if windows {
        format!("curl.exe -fsS --connect-timeout 2 --max-time 10 -o NUL -X POST --data-binary \"@-\" {url} >NUL 2>NUL || exit 0")
    } else {
        format!("curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST --data-binary @- {url} >/dev/null 2>&1 || true")
    }
}

fn windows_runtime(env_type: EnvType) -> bool {
    cfg!(windows) && matches!(env_type, EnvType::Windows | EnvType::WindowsInterop)
}

impl AgentProvider for DevinAdapter {
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| {
                crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                attention_capability: {
                    use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
                    use crate::agent::session_lifecycle::LifecycleKind;
                    AttentionCapability::Hook {
                        events: vec![
                            LifecycleKind::TurnCompleted,
                            LifecycleKind::InputRequired,
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

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn captures_session_id_from_pty(&self) -> bool {
        false
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        Vec::new()
    }

    fn confirms_prompt_submission(&self) -> bool {
        true
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec!["--".into(), text.into()]
    }

    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        node_id: i64,
    ) -> Result<(), String> {
        let dir = Path::new(&resolved.host_path).join(".devin");
        std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create .devin dir: {e}"))?;
        let url = attention_url(node_id, crate::http_server::current_http_port());
        let command = hook_command(&url, windows_runtime(resolved.env_type));
        let spec = HookProvisioningSpec {
            command: &command,
            ..DEVIN_HOOKS
        };
        provision_attention_hooks(&spec, &dir)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "devin_tests.rs"]
mod tests;
