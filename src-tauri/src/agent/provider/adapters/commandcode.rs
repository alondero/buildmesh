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
//! **Permissions (`--yolo`, issue #1419)**:
//! The recipe carries `--yolo` so spawned agents run with permission prompts
//! pre-approved. Without it the CLI stops on every tool call waiting for
//! interactive confirmation, which blocks unattended agent loops — the same
//! role `--dangerously-skip-permissions` plays for the AGY/Claude-backed
//! adapters.
//!
//! **Model, effort & prefill**:
//! Accepts `--model <name>` for model overrides, `--effort <low|medium|high>`
//! for reasoning effort, and positional prompt text for prefill queries.

use crate::agent::capabilities::{EffortControlKind, COMMANDCODE_EFFORT_ALLOWED};
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct CommandCodeAdapter;
pub static COMMANDCODE: CommandCodeAdapter = CommandCodeAdapter;

/// Per-platform shell selection. Mirrors the OpenCode / MiniMax pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

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
        SpawnRecipe {
            binary: binary_for(platform, env_type),
            base_args: vec!["--yolo".into()],
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
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

    fn supports_passive_turn_watcher(&self) -> bool {
        true
    }

    fn on_spawn_activated(&self, node_id: i64) {
        crate::services::commandcode_watcher::activate(node_id);
    }

    fn on_process_terminated(&self, node_id: i64) {
        crate::services::commandcode_watcher::stop(node_id);
    }

    fn produces_readable_transcript(&self) -> bool {
        true
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::Closed {
            allowed: COMMANDCODE_EFFORT_ALLOWED
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Macos, Platform::Linux]
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
mod tests {
    use super::*;
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{
        assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef,
    };

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(COMMANDCODE.id(), "commandcode");
        let ui = COMMANDCODE.ui();
        assert_eq!(ui.label, "Command Code");
        assert_eq!(ui.color, "#8C4EDD");
        assert_eq!(ui.icon, "C");
    }

    #[test]
    fn spawn_recipe_cmd_on_windows_native() {
        let recipe = COMMANDCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "cmdc");
        assert_eq!(
            recipe.base_args,
            vec!["--yolo".to_string()],
            "Windows native recipe must carry --yolo so unattended agents \
             don't block on permission prompts; got {:?}",
            recipe.base_args
        );
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows native must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_direct_on_macos() {
        let recipe = COMMANDCODE.spawn_recipe(Platform::Macos, EnvType::Windows);
        assert_eq!(recipe.binary, "cmd");
        assert_eq!(
            recipe.base_args,
            vec!["--yolo".to_string()],
            "macOS recipe must carry --yolo; got {:?}",
            recipe.base_args
        );
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "macOS must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_direct_on_linux() {
        let recipe = COMMANDCODE.spawn_recipe(Platform::Linux, EnvType::Windows);
        assert_eq!(recipe.binary, "cmd");
        assert_eq!(
            recipe.base_args,
            vec!["--yolo".to_string()],
            "Linux recipe must carry --yolo; got {:?}",
            recipe.base_args
        );
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "Linux must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_wsl_uses_cmd_binary() {
        let recipe = COMMANDCODE.spawn_recipe(Platform::Windows, EnvType::Wsl);
        assert_eq!(recipe.binary, "cmd");
        assert_eq!(
            recipe.base_args,
            vec!["--yolo".to_string()],
            "WSL recipe must carry --yolo; got {:?}",
            recipe.base_args
        );
    }

    #[test]
    fn available_on_includes_all_three_platforms() {
        let platforms = COMMANDCODE.available_on();
        assert_eq!(platforms.len(), 3);
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    #[test]
    fn self_assigns_session_id() {
        assert!(COMMANDCODE.self_assigns_session_id());
        assert!(
            !COMMANDCODE.captures_session_id_from_pty(),
            "Command Code captures session id via post-spawn polling, not PTY regex"
        );
    }

    #[test]
    fn resume_args_format() {
        let args = COMMANDCODE.resume_args("3fadada6-e0a3-44a2-ab68-ce1ecf7207a9");
        assert_eq!(
            args,
            vec!["--resume", "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9"]
        );
    }

    #[test]
    fn session_assign_args_empty() {
        let args = COMMANDCODE.session_assign_args("any-id");
        assert!(args.is_empty());
    }

    #[test]
    fn model_args_use_model_flag() {
        let args = COMMANDCODE.model_args("taste-1");
        assert_eq!(args, vec!["--model", "taste-1"]);
    }

    #[test]
    fn effort_args_use_effort_flag() {
        let args = COMMANDCODE.effort_args("high");
        assert_eq!(args, vec!["--effort", "high"]);
    }

    #[test]
    fn prefill_args_positional() {
        let args = COMMANDCODE.prefill_args("implement the feature");
        assert_eq!(args, vec!["implement the feature"]);
    }

    #[test]
    fn supports_expected_capabilities() {
        assert!(COMMANDCODE.supports_resume());
        assert!(COMMANDCODE.auto_resume_on_startup());
        assert!(COMMANDCODE.supports_model_override());
        assert_eq!(
            COMMANDCODE.effort_control(),
            EffortControlKind::Closed {
                allowed: COMMANDCODE_EFFORT_ALLOWED
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            }
        );
        assert!(COMMANDCODE.supports_extra_args());
        assert!(COMMANDCODE.supports_prefill());
        assert!(!COMMANDCODE.requires_attention_hook());
        assert!(COMMANDCODE.produces_readable_transcript());
    }

    #[test]
    fn capabilities_descriptor_matches() {
        let caps = COMMANDCODE.capabilities();
        assert_eq!(caps.harness_id, "commandcode");
        assert!(caps.supports_resume);
        assert!(caps.auto_resume_on_startup);
        assert!(caps.supports_model_override);
        assert!(caps.supports_effort_override);
        assert!(caps.supports_extra_args);
        assert!(caps.supports_prefill);
        assert!(!caps.requires_attention_hook);
        assert!(caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::Closed {
                allowed: COMMANDCODE_EFFORT_ALLOWED
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            }
        );
    }

    #[test]
    fn resume_recipe_carries_session_flag() {
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("3fadada6-e0a3-44a2-ab68-ce1ecf7207a9"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&COMMANDCODE, input);
        // `--yolo` is the always-on base flag (issue #1419); `--resume <id>`
        // is appended by `default_prepare` after the base recipe (issue #1500).
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--yolo".to_string(),
                "--resume".to_string(),
                "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9".to_string()
            ]
        );
    }

    #[test]
    fn fresh_recipe_forwards_model_and_prompt_without_session_id() {
        let config = ResolvedAgentConfig {
            model: Some("taste-1".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("refactor auth flow"),
            sandbox: false,
        };
        let prepared = default_prepare(&COMMANDCODE, input);
        let args = &prepared.recipe.base_args;
        // Issue #1419: `--yolo` must be present on every fresh-spawn
        // recipe alongside `--model` and the positional prefill.
        assert!(
            args.contains(&"--yolo".to_string()),
            "--yolo missing from fresh recipe; got {args:?}"
        );
        assert_flag_followed_by_value(args, "--model", "taste-1");
        assert!(args.contains(&"refactor auth flow".to_string()));
        assert!(!args
            .iter()
            .any(|a| a == "--session" || a == "--session-id" || a == "--resume"));
    }
}
