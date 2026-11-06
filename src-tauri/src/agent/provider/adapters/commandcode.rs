//! Command Code CLI provider adapter — neuro-symbolic coding agent CLI
//! installed on PATH as `cmdc` on Windows (a `.cmd` batch shim) and `cmd`
//! on macOS / Linux / WSL (wayfinder #1394, ticket #1397).
//!
//! **Platform binary & shell wrapping**:
//! - **Windows**: distributed as `cmdc.cmd` (or `cmdc.exe`); `CreateProcess`
//!   won't run a `.cmd` directly, so the recipe wraps with `WindowsShell::Cmd`
//!   → `cmd.exe /c cmdc …` (see `spawn_environment::wrap`'s
//!   `WindowsShell::Cmd` branch).
//! - **macOS / Linux / WSL**: real executable `cmd` on PATH; no wrapper needed,
//!   so `WindowsShell::Direct` is used.
//!
//! **Session lifecycle & CLI resumption**:
//! Command Code auto-assigns session IDs (`self_assigns_session_id() -> true`).
//! While the CLI supports `--continue` for resuming the latest conversation in
//! a directory, Buildmesh deliberately targets exact-ID resumption via
//! `--resume <id>` to guarantee strict node isolation and avoid cross-node
//! session collisions (issue #1500). `--session` accepts a transcript path or
//! id prefix, but `--resume` (`-r, --resume [id|name]`) is the documented
//! exact-ID resumption flag, so Buildmesh uses it for node isolation.
//!
//! **Session capture**:
//! Command Code mints UUID IDs and writes structured JSONL transcripts to
//! `~/.commandcode/projects/<encoded-cwd>/<session_id>.jsonl`. Because it
//! renders rich TUI output without printing standard UUID banners on stdout,
//! PTY capture is disabled (`captures_session_id_from_pty: false`) and session
//! IDs are captured via post-spawn directory polling (`after_fresh_spawn`).
//!
//! **Permissions (`--yolo`, issue #1419, setting in issue #2151)**:
//! The unattended permission mode contributes `--yolo` so spawned agents run
//! with permission prompts pre-approved. Without it the CLI stops on every
//! tool call waiting for interactive confirmation, which blocks unattended
//! agent loops — the same role `--dangerously-skip-permissions` plays for
//! the AGY/Claude-backed adapters. The bare `spawn_recipe` carries no
//! approval flags; `permission_args` owns the choice.
//!
//! **Model, effort & prefill**:
//! Accepts `--model <name>` for model overrides, `--effort <low|medium|high>`
//! for reasoning effort, and positional prompt text for prefill queries.

use crate::agent::capabilities::{
    EffortControlKind, PermissionModeOption, COMMANDCODE_EFFORT_ALLOWED, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::provider::{shell_for, AgentProvider, Platform, SpawnRecipe, UiMeta};
// The recipe tests assert the shell each platform produces; nothing outside
// `cfg(test)` names the type now that `shell_for` is shared.
#[cfg(test)]
use crate::agent::provider::WindowsShell;
use crate::models::EnvType;

pub struct CommandCodeAdapter;
pub static COMMANDCODE: CommandCodeAdapter = CommandCodeAdapter;

/// Binary name selection per platform and runtime environment.
/// On Windows native, Command Code installs as `cmdc` to avoid colliding with
/// `System32\cmd.exe`. In WSL or Unix, it installs as `cmd`.
fn binary_for(platform: Platform, env_type: EnvType) -> &'static str {
    match (platform, env_type) {
        (_, EnvType::Wsl) => "cmd",
        (Platform::Windows, _) => "cmdc",
        (Platform::Macos | Platform::Linux, _) => "cmd",
    }
}

impl AgentProvider for CommandCodeAdapter {
    fn prepare_launch(
        &self,
        input: crate::agent::launch::HarnessLaunchInput<'_>,
    ) -> crate::agent::launch::PreparedHarnessLaunch {
        let mut prepared = crate::agent::launch::default_prepare(self, input);
        // Ink's kitty-keyboard probe races ConPTY. VS Code terminals skip it.
        prepared
            .environment
            .env_set
            .insert(0, ("TERM_PROGRAM".into(), "vscode".into()));
        prepared
    }

    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: true,
                auto_resume_on_startup: true,
                circuit_observation: {
                    crate::circuit::strategy::ObservationStrategy {
                        passive_watcher: Some(
                            crate::circuit::strategy::PassiveWatcher::CommandCode,
                        ),
                        ..crate::circuit::strategy::ObservationStrategy::UNWIRED
                    }
                },
                produces_readable_transcript: true,
                supports_model_override: true,
                supports_extra_args: true,
                supports_prefill: true,
                available_on: { &[Platform::Windows, Platform::Macos, Platform::Linux] },
                effort_control: {
                    EffortControlKind::Closed {
                        allowed: COMMANDCODE_EFFORT_ALLOWED
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    }
                },
                permission_modes: {
                    vec![
            PermissionModeOption::unattended(
                "--yolo",
                "Prompts off — permission prompts pre-approved (today's behavior; required for unattended runs).",
            ),
            PermissionModeOption::prompt(
                "Prompts on (no flag)",
                "Command Code asks for confirmation like a human-launched session.",
            ),
        ]
                },
                ..Default::default()
            });
        &DESCRIPTOR
    }

    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Command Code".into(),
            color: "#8C4EDD".into(),
            icon: "C".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, env_type: EnvType) -> SpawnRecipe {
        // Issue #2151: bare — no approval flags. The effective permission
        // mode contributes `--yolo` via `permission_args` in
        // `default_prepare`.
        SpawnRecipe {
            binary: binary_for(platform, env_type),
            base_args: Vec::new(),
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == PERMISSION_MODE_UNATTENDED {
            vec!["--yolo".into()]
        } else {
            Vec::new()
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
        spawn.base_args = [
            "--print",
            "--output-format",
            "json",
            "--max-turns",
            "1",
            "--trust",
        ]
        .map(str::to_owned)
        .to_vec();
        Some(BackgroundRecipe::new(
            spawn,
            BackgroundPromptInput::Stdin,
            BackgroundResultOutput::ResultJsonLines,
        ))
    }

    fn on_spawn_activated(&self, node_id: i64) {
        crate::services::commandcode_watcher::activate(node_id);
    }

    fn on_process_terminated(&self, node_id: i64) {
        crate::services::commandcode_watcher::stop(node_id);
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn captures_session_id_from_pty(&self) -> bool {
        false
    }

    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        env_type: EnvType,
        app: &tauri::AppHandle,
    ) {
        crate::services::commandcode_session::start_capture_poller(
            node_id,
            spawn_path.to_string(),
            env_type,
            app.clone(),
        );
    }

    fn recover_suspended_session_id(
        &self,
        spawn_path: &str,
        env_type: EnvType,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        crate::services::commandcode_session::find_historic_id_for_directory(
            env_type,
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    fn before_resume_spawn<'a>(
        &'a self,
        node_id: i64,
        session_id: &str,
        spawn_path: &str,
        env_type: EnvType,
        app: &'a tauri::AppHandle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        let session_id = session_id.to_string();
        let spawn_path = spawn_path.to_string();
        let app = app.clone();
        Box::pin(async move {
            if let Err(error) =
                crate::services::commandcode_watcher::start_for_resumed_session_async(
                    node_id,
                    &session_id,
                    &spawn_path,
                    env_type,
                    app,
                )
                .await
            {
                tracing::warn!(
                    "commandcode watcher: could not resume watch for node {node_id}: {error}"
                );
            }
        })
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--resume".into(), id.into()]
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }
}

#[cfg(test)]
#[path = "commandcode_tests.rs"]
mod tests;
