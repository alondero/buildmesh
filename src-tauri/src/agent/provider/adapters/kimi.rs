//! Kimi Code provider adapter — Moonshot AI's full-screen interactive coding
//! agent, installed on PATH as a single `kimi` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. The non-interactive `-p <prompt>` mode
//! exists but is *not* used here: the #914 prototype verified that Buildmesh's
//! PTY backend (ConPTY on Windows, native PTY on macOS/Linux) fully supports
//! full-screen TUI rendering, so we launch in interactive mode everywhere.
//!
//! **Session resumption** uses `-S [<id>]` / `--session [<id>]` (cwd-scoped,
//! both forms optional-id selector or explicit resumption) or `-c` /
//! `--continue` for the most-recent session. Kimi auto-assigns its own
//! session ids (captured from PTY output by `session_naming`), so
//! `self_assigns_session_id()` is `true` and `session_assign_args()` is a no-op.
//!
//! **Model override** uses `-m <model-id>` / `--model <model-id>` (Kimi's
//! `--help` advertises the short form first, so the adapter emits `-m`).
//! Kimi Code accepts Buildmesh-level model overrides passed via the spawn
//! path — the `-m <model>` flag is forwarded to the Kimi CLI, which then
//! runs that model for the invocation (overriding the harness's
//! `default_model` from `~/.kimi-code/config.toml` for that one session).
//! Credentials and provider mapping live in `~/.kimi-code/config.toml`;
//! Kimi's own login flow owns those. Buildmesh merges only native hooks.
//!
//! **Shell wrapping**: `kimi` is a native binary on all platforms (not a
//! `.cmd` shim), so `WindowsShell::Direct` is correct everywhere — matching
//! the AGY and Grok adapter patterns.

use crate::agent::attention_hooks::{
    provision_attention_hooks, HookFormat, HookProvisioningSpec, Ownership, TomlEventRule,
    TomlHookShape,
};
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use std::path::Path;

fn hook_command(windows: bool) -> String {
    if windows {
        let callback = crate::env::windows_attention_command(None).unwrap_or_else(||
            "curl.exe -fsS --connect-timeout 2 --max-time 5 -o NUL -X POST -H \"Content-Type: application/json\" --data-binary @- --url \"http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID%\"".into());
        format!("if defined BUILDMESH_SESSION_ID {callback}")
    } else {
        format!("if [ -n \"$BUILDMESH_SESSION_ID\" ]; then {} -fsS --connect-timeout 2 --max-time 5 -o /dev/null -X POST -H 'Content-Type: application/json' --data-binary @- \"http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID\"; fi", crate::env::unix_attention_curl())
    }
}

/// Kimi Code 0.27.0 reads flat `[[hooks]]` entries from its global config.
/// Declared once as provisioning data: the shared TOML adapter performs the
/// read/compare/atomic-write sequence, and the callback stays runtime-bound
/// because this file is shared by all Kimi sessions.
const KIMI_HOOKS: HookProvisioningSpec<'static> = HookProvisioningSpec {
    harness: "kimi",
    file: "config.toml",
    label: "Kimi config",
    command: "",
    event_commands: &[],
    format: HookFormat::Toml(TomlHookShape {
        root_key: "hooks",
        timeout: 10,
        ownership: Ownership {
            fields: &["command"],
            anchors: &["BUILDMESH_SESSION_ID", "BUILDMESH_PORT"],
        },
        events: &[
            TomlEventRule {
                name: "SessionStart",
                matcher: None,
            },
            TomlEventRule {
                name: "Stop",
                matcher: None,
            },
            TomlEventRule {
                name: "StopFailure",
                matcher: None,
            },
            TomlEventRule {
                name: "Interrupt",
                matcher: None,
            },
            TomlEventRule {
                name: "PermissionRequest",
                matcher: None,
            },
            TomlEventRule {
                name: "PermissionResult",
                matcher: None,
            },
            TomlEventRule {
                name: "UserPromptSubmit",
                matcher: None,
            },
            TomlEventRule {
                name: "PreToolUse",
                matcher: Some("AskUserQuestion|ExitPlanMode"),
            },
            TomlEventRule {
                name: "PostToolUse",
                matcher: Some("AskUserQuestion|ExitPlanMode"),
            },
            TomlEventRule {
                name: "PostToolUseFailure",
                matcher: Some("AskUserQuestion|ExitPlanMode"),
            },
            // Background AskUserQuestion returns before the answer. Its
            // terminal task notification, correlated by task id, is the
            // resolution signal.
            TomlEventRule {
                name: "Notification",
                matcher: Some(r"^task\.(completed|failed|killed|timed_out|lost)$"),
            },
        ],
    }),
};

fn ensure_hooks(path: &Path, command: &str) -> Result<(), String> {
    let target_dir = path
        .parent()
        .ok_or_else(|| "Kimi config has no parent directory".to_string())?;
    let spec = HookProvisioningSpec {
        command,
        ..KIMI_HOOKS
    };
    provision_attention_hooks(&spec, target_dir)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub struct KimiAdapter;
pub static KIMI: KimiAdapter = KimiAdapter;

impl AgentProvider for KimiAdapter {
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
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
                        launch_mode: AttentionLaunchMode::PermissionAsk,
                        trust: None,
                        min_version: Some("0.27.0".into()),
                    }
                },
                produces_readable_transcript: false,
                supports_model_override: true,
                supports_extra_args: true,
                supports_prefill: false,
                available_on: { &[Platform::Windows, Platform::Linux, Platform::Macos] },
                ..Default::default()
            });
        &DESCRIPTOR
    }

    fn id(&self) -> &'static str {
        "kimi"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Kimi Code".into(),
            color: "#00c4c4".into(),
            icon: "K".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "kimi",
            base_args: vec![],
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        }
    }

    fn background_recipe(
        &self,
        platform: Platform,
    ) -> Option<crate::agent::background::BackgroundRecipe> {
        use crate::agent::{
            background::BackgroundRecipe,
            capabilities::{BackgroundPromptInput, BackgroundResultOutput},
        };
        let mut spawn = self.spawn_recipe(platform, EnvType::Windows);
        spawn.base_args = ["--output-format", "stream-json"]
            .map(str::to_owned)
            .to_vec();
        Some(BackgroundRecipe::new(
            spawn,
            BackgroundPromptInput::Argument {
                flag: "--prompt".into(),
            },
            BackgroundResultOutput::AssistantJsonLines,
        ))
    }

    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        let home = match runtime.harness_home.as_deref() {
            Some(home) => std::path::PathBuf::from(crate::env::to_host_path(home)),
            None => {
                crate::env::kimi_home_for_spawn(&resolved.spawn_path, runtime.wsl_distro.as_deref())
                    .ok_or("could not resolve Kimi Code configuration home")?
            }
        };
        let windows = crate::env::runtime_for_spawn_path(&resolved.spawn_path)
            == EnvType::WindowsInterop
            || (cfg!(windows)
                && crate::env::runtime_for_spawn_path(&resolved.spawn_path) != EnvType::Wsl);
        ensure_hooks(&home.join("config.toml"), &hook_command(windows))
    }

    /// Kimi auto-assigns session ids — captured from PTY output.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    /// Kimi's explicit resume flag is `-S <id>` / `--session <id>` (long form
    /// is `--session`, not `--resume`). The bare `-c` / `--continue` form
    /// (cwd-most-recent) is intentionally not modelled here — auto-resume
    /// always passes the captured session id explicitly, so the resolver
    /// never needs to fall back to the implicit selector.
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["-S".into(), id.into()]
    }

    /// Kimi's model flag is `-m <model>` (short) or `--model <model>` (long).
    /// Use the short form — matches Kimi Code's own CLI examples and the
    /// `-m` short flag is what `--help` advertises first.
    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["-m".into(), model.into()]
    }

    /// No `--session-id` flag — Kimi assigns its own.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
#[path = "kimi_tests.rs"]
mod tests;
