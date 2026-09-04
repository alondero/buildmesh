//! OpenCode provider adapter — open-source terminal coding agent installed on
//! PATH as a single `opencode` binary. The binary shape differs by host:
//!
//! - **Windows**: distributed as `opencode.cmd` (resolved via `PATHEXT`);
//!   `CreateProcess` won't run a `.cmd` directly, so the recipe wraps with
//!   `WindowsShell::Cmd` → `cmd.exe /c opencode …` (see
//!   `spawn_environment::wrap`'s `WindowsShell::Cmd` branch).
//! - **macOS / Linux**: a real executable on PATH; no wrapper needed, so
//!   `WindowsShell::Direct` is the right choice (on those platforms
//!   `spawn_environment::wrap` short-circuits to a direct spawn anyway).
//!
//! Prior to issue #827 the adapter hardcoded `WindowsShell::Cmd` for *all*
//! platforms — fine on Windows, but on Linux `cmd.exe /c opencode` doesn't
//! exist, so the provider was advertised as available yet silently failed
//! at spawn time. The adapter also omitted `Platform::Macos` from
//! `available_on()`, so the spawn menu on macOS never showed OpenCode even
//! when the binary was installed — users on macOS were told nothing.
//!
//! **Session identity** (2026-08 research, `docs/learning/opencode-harness-capabilities.md`):
//! OpenCode *self-assigns* IDs of the form `ses_<hex+base62>`. There is no
//! Claude-style `--session-id` assign flag; `opencode --session <uuid>` is
//! rejected as an invalid ID, and `--session ses_unknown` fails with
//! "Session not found" rather than creating that ID. Resume uses
//! `--session <id>` / `-s <id>` (not `--resume`). Capture is *not* the
//! PTY UUID regex — those IDs never match — it is a post-spawn SQLite
//! read of OpenCode's local `opencode.db` (`services::opencode_session`),
//! started from [`AgentProvider::after_fresh_spawn`].
//!
//! **Model / prefill**: TUI accepts `--model provider/model` and `--prompt`.
//! There is no TUI `--variant` / `--effort` flag (that's `opencode run` only).
//! Windows still wraps with `cmd.exe /c` (`.cmd` shim); `--prompt` therefore
//! flattens CR/LF to spaces so a multi-line handover cannot split the
//! `cmd.exe` command line.
//!
//! **Permission policy**: `--auto` is baked into the base recipe. Mirrors
//! how AGY's `--dangerously-skip-permissions` is wired — a session-wide
//! policy the harness applies for the whole invocation, not a per-call
//! toggle. The orchestrator's outer sandbox (macOS Seatbelt, Windows
//! restricted-token, mesh-level toggle) is the independent OS-level
//! containment layer.

use crate::agent::provider::{AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::env::ResolvedPath;
use crate::models::EnvType;

pub struct OpenCodeAdapter;
pub static OPENCODE: OpenCodeAdapter = OpenCodeAdapter;

/// Per-platform shell selection. Mirrors the Codex adapter's pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

impl AgentProvider for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "OpenCode".into(),
            color: "#f59e0b".into(),
            icon: "O".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "opencode",
            base_args: vec!["--auto".into()],
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
        true
    }

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        // Issue #1295: OpenCode's project plugin forwards two lifecycle
        // events back to the attention endpoint:
        //   - `session.idle` → `InputRequired` (turn ended, user is needed)
        //   - `permission.asked` → `PermissionRequested` (tool approval)
        // `PermissionAsk` is the honest launch mode: OpenCode's `--auto`
        // auto-approves most permissions, but the rare non-auto case still
        // raises a `permission.asked` signal that the plugin forwards.
        AttentionCapability::Hook {
            events: vec![
                LifecycleKind::InputRequired,
                LifecycleKind::PermissionRequested,
            ],
            launch_mode: AttentionLaunchMode::PermissionAsk,
            trust: None,
            min_version: None,
        }
    }

    /// Install the OpenCode attention plugin (issue #1295). The plugin is
    /// loaded from `.opencode/plugins/` and forwards `session.idle` and
    /// `permission.asked` to the local attention endpoint via env vars set
    /// per-agent by `spawn_environment` (no node_id baking needed — unlike
    /// Codex, the OpenCode TUI does NOT env_clear `BUILDMESH_*`).
    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        _runtime: &LaunchRuntime,
        _node_id: i64,
    ) -> Result<(), String> {
        crate::agent::spawn::inject_opencode_attention_plugin(std::path::Path::new(
            &resolved.host_path,
        ))
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    /// OpenCode mints `ses_…` IDs itself. There is no `--session-id` assign
    /// flag; a fresh spawn must capture the ID after the TUI starts.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    /// The PTY UUID regex (`session id: 01a0-…`) cannot see `ses_…` IDs, and
    /// the TUI is not documented to print them. Capture runs from
    /// [`Self::after_fresh_spawn`].
    fn captures_session_id_from_pty(&self) -> bool {
        false
    }

    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        env_type: EnvType,
        _app: &tauri::AppHandle,
    ) {
        crate::services::opencode_session::start_capture_poller(
            node_id,
            spawn_path.to_string(),
            env_type,
        );
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // `cmd.exe /c` treats a newline as end-of-command. Flatten so a
        // multi-line handover/issue prefill cannot split the argv.
        vec!["--prompt".into(), flatten_cmd_prefill(text)]
    }
}

/// Collapse CR/LF so `--prompt` stays a single `cmd.exe /c` argument.
fn flatten_cmd_prefill(text: &str) -> String {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS always direct-spawns the binary via the `cfg!(target_os = "macos")`
    /// short-circuit in `spawn_environment::wrap` (the `windows_shell` field is
    /// never consulted on macOS hosts). Linux reaches the `WindowsShell::Direct`
    /// arm of the wrap function's `match`, which likewise direct-spawns. So both
    /// platforms produce a direct spawn — no shell wrapper needed because
    /// `opencode` is a real executable on PATH on both.
    ///
    /// Regression for issue #827: before the fix `spawn_recipe` ignored its
    /// `platform` arg and returned `WindowsShell::Cmd` on Linux, so a
    /// native-Linux mesh got `cmd.exe /c opencode …` — silent failure at
    /// spawn time on a platform that was advertised as supported.
    #[test]
    fn spawn_recipe_direct_on_macos() {
        let recipe = OPENCODE.spawn_recipe(Platform::Macos, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert_eq!(recipe.base_args, vec!["--auto".to_string()]);
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "macOS must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_direct_on_linux() {
        let recipe = OPENCODE.spawn_recipe(Platform::Linux, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert_eq!(recipe.base_args, vec!["--auto".to_string()]);
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "Linux must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    /// Windows keeps `WindowsShell::Cmd` because `opencode` is a `.cmd` batch
    /// file on Windows; `CreateProcess` won't execute it directly. Pinning
    /// this guards against the easy mistake of normalising every provider
    /// onto `Direct` and breaking the Windows path.
    #[test]
    fn spawn_recipe_cmd_on_windows() {
        let recipe = OPENCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert_eq!(recipe.base_args, vec!["--auto".to_string()]);
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    /// `available_on` must list every platform OpenCode runs on. Pre-#827
    /// macOS was missing, so the spawn-menu composer in
    /// `agent::provider_menu::available_providers` filtered OpenCode out for a
    /// macOS user even when the binary was detected at startup. Pin the exact
    /// set (not just membership) so a future "while we're here" addition
    /// forces an explicit test update — matches the equivalent assertion on
    /// `TERMINAL` (`terminal_available_on_three_platforms`).
    #[test]
    fn available_on_includes_macos() {
        let platforms = OPENCODE.available_on();
        assert_eq!(
            platforms.len(),
            3,
            "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
            platforms
        );
        assert!(platforms.contains(&Platform::Macos), "OpenCode must be available on macOS (issue #827); got {:?}", platforms);
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Windows));
    }

    /// Pin id + UI metadata so the wire type / frontend icon mapping can't
    /// drift silently (matches the equivalent assertion on `TERMINAL`).
    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(OPENCODE.id(), "opencode");
        let ui = OPENCODE.ui();
        assert_eq!(ui.label, "OpenCode");
        assert_eq!(ui.color, "#f59e0b");
        assert_eq!(ui.icon, "O");
    }

    #[test]
    fn self_assigns_session_id() {
        assert!(OPENCODE.self_assigns_session_id());
        assert!(
            !OPENCODE.captures_session_id_from_pty(),
            "OpenCode ses_ IDs are not PTY UUID banners; capture is after_fresh_spawn"
        );
    }

    #[test]
    fn resume_args_format() {
        let args = OPENCODE.resume_args("ses_fc52ccfb9ffek1jl23ZwpRuSP7");
        assert_eq!(
            args,
            vec!["--session", "ses_fc52ccfb9ffek1jl23ZwpRuSP7"],
            "OpenCode resume is --session <id>, not --resume"
        );
    }

    #[test]
    fn session_assign_args_empty() {
        let args = OPENCODE.session_assign_args("any-id");
        assert!(
            args.is_empty(),
            "OpenCode self-assigns; session_assign_args must be empty (no --session-id)"
        );
    }

    #[test]
    fn prefill_args_use_prompt_flag() {
        let args = OPENCODE.prefill_args("fix the auth bug");
        assert_eq!(args, vec!["--prompt", "fix the auth bug"]);
    }

    #[test]
    fn prefill_args_flatten_newlines_for_cmd_exe() {
        let args = OPENCODE.prefill_args("fix auth\nthen run tests\r\nand push");
        assert_eq!(args, vec!["--prompt", "fix auth then run tests and push"]);
        assert!(
            !args[1].contains('\n') && !args[1].contains('\r'),
            "cmd.exe /c must not see a newline in --prompt; got {:?}",
            args[1]
        );
    }

    #[test]
    fn model_args_use_long_form() {
        let args = OPENCODE.model_args("anthropic/claude-sonnet-4-5");
        assert_eq!(args, vec!["--model", "anthropic/claude-sonnet-4-5"]);
    }

    /// `--auto` is the OpenCode analogue of AGY's
    /// `--dangerously-skip-permissions` — see the module docstring. This
    /// test pins the exact argv so a future edit that smuggles an extra
    /// flag in (e.g. `--auto --garbage`) trips here, not at runtime.
    #[test]
    fn spawn_recipe_carries_auto_flag_on_every_platform() {
        for platform in [Platform::Windows, Platform::Linux, Platform::Macos] {
            let recipe = OPENCODE.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(
                recipe.base_args,
                vec!["--auto".to_string()],
                "OpenCode base recipe must be exactly `[\"--auto\"]` on {platform:?}; got {:?}",
                recipe.base_args
            );
        }
    }

    #[test]
    fn supports_resume_model_prefill_and_attention_hook() {
        assert!(OPENCODE.supports_resume());
        assert!(OPENCODE.auto_resume_on_startup());
        assert!(OPENCODE.supports_model_override());
        assert!(OPENCODE.supports_prefill());
        // Issue #1295: plugin hook unblocks the Autopilot gate.
        assert!(OPENCODE.requires_attention_hook());
        assert!(!OPENCODE.produces_readable_transcript());
    }

    #[test]
    fn capabilities_descriptor_advertises_resume_model_prefill_attention_hook() {
        let caps = OPENCODE.capabilities();
        assert_eq!(caps.harness_id, "opencode");
        assert!(caps.supports_resume);
        assert!(caps.auto_resume_on_startup);
        assert!(caps.supports_model_override);
        assert!(caps.supports_prefill);
        assert!(!caps.supports_effort_override);
        // Issue #1295: descriptor must mirror the adapter's `requires_attention_hook`.
        assert!(caps.requires_attention_hook);
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
    }

    // -- Attention plugin (issue #1295) ------------------------------------

    /// The plugin file MUST be a complete ESM module — OpenCode's loader
    /// fails on syntax errors and surfaces them as a startup fault. Pin
    /// the export name and the two event handlers we depend on so a
    /// refactor that renames `BuildmeshAttention` (or drops one of the
    /// event kinds) trips here, not on a real spawn.
    #[test]
    fn plugin_template_exports_idle_and_permission_handlers() {
        let template = crate::agent::spawn::OPENCODE_ATTENTION_PLUGIN;
        assert!(
            template.contains("export const BuildmeshAttention"),
            "plugin must export BuildmeshAttention (OpenCode plugin contract); got:\n{template}"
        );
        assert!(
            template.contains("event.type === \"session.idle\""),
            "plugin must handle session.idle (turn-ended → InputRequired)"
        );
        assert!(
            template.contains("event.type === \"permission.asked\""),
            "plugin must handle permission.asked (tool approval → PermissionRequested)"
        );
        // Idempotency pin: the file is rewritten on every spawn; if it
        // ever accidentally acquires side effects at import time, those
        // fire on every OpenCode startup.
        assert!(
            !template.contains("console.log(") && !template.contains("process.exit("),
            "plugin must stay side-effect-free at import time; got:\n{template}"
        );
    }

    /// `provision_attention_hooks` writes `.opencode/plugins/buildmesh-attention.js`
    /// into the project directory. Issue #1295 requires idempotency: a
    /// re-spawn with a byte-identical file MUST NOT rewrite (so mtime is
    /// preserved and the working tree stays clean), and a re-spawn after
    /// the template changes MUST overwrite.
    #[test]
    fn provision_attention_hooks_writes_plugin_and_is_idempotent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let resolved = ResolvedPath {
            host_path: temp.path().to_string_lossy().into_owned(),
            spawn_path: temp.path().to_string_lossy().into_owned(),
            raw_path: temp.path().to_string_lossy().into_owned(),
            env_type: EnvType::Windows,
        };

        // First provision — creates the directory tree and writes the file.
        OPENCODE
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
            .expect("first provision");

        let plugin_path = temp
            .path()
            .join(".opencode")
            .join("plugins")
            .join("buildmesh-attention.js");
        assert!(
            plugin_path.exists(),
            "plugin file not written to {}",
            plugin_path.display()
        );
        let first = std::fs::read(&plugin_path).expect("read plugin");
        assert_eq!(
            first.as_slice(),
            crate::agent::spawn::OPENCODE_ATTENTION_PLUGIN.as_bytes(),
            "written plugin must be byte-equal to the embedded template"
        );

        // Idempotency: a second provision with the same template must
        // NOT touch the file (so the user's working tree stays clean
        // across repeated spawns).
        let mtime_before = std::fs::metadata(&plugin_path)
            .expect("stat")
            .modified()
            .expect("mtime");
        OPENCODE
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
            .expect("second provision");
        let mtime_after = std::fs::metadata(&plugin_path)
            .expect("stat")
            .modified()
            .expect("mtime");
        assert_eq!(
            mtime_before, mtime_after,
            "idempotent provision must not rewrite the plugin file (mtime preserved)"
        );

        std::fs::remove_file(&plugin_path).expect("delete");
        OPENCODE
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
            .expect("third provision");
        assert!(plugin_path.exists(), "plugin file must be re-created after removal");
    }

    /// Provision must surface a real filesystem error (a leaf file used
    /// as a project root is unrecoverable). The spawn caller treats this
    /// as best-effort: a failure is logged, the spawn proceeds, and the
    /// attention callback is the only casualty.
    #[test]
    fn provision_attention_hooks_surfaces_filesystem_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let blocker = temp.path().join("blocker");
        std::fs::write(&blocker, b"not a dir").expect("create blocker");
        let resolved = ResolvedPath {
            host_path: blocker.to_string_lossy().into_owned(),
            spawn_path: blocker.to_string_lossy().into_owned(),
            raw_path: blocker.to_string_lossy().into_owned(),
            env_type: EnvType::Windows,
        };
        let result =
            OPENCODE.provision_attention_hooks(&resolved, &LaunchRuntime::default(), 0);
        assert!(
            result.is_err(),
            "provision must surface a filesystem error when project path is a leaf file; got Ok"
        );
        // The helper must NOT panic or corrupt the blocker.
        assert!(
            std::fs::read(&blocker).is_ok(),
            "provision error path must not mutate the blocker"
        );
    }

    /// Resume recipe is `opencode --auto --session <id>` — the base
    /// recipe's `--auto` (issue #1297) survives the `default_prepare`
    /// composition, and the resume adds `--session <id>` (flag, not
    /// subcommand) on top.
    #[test]
    fn resume_recipe_carries_session_flag() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&OPENCODE, input);
        let args = &prepared.recipe.base_args;
        assert_eq!(
            args,
            &[
                "--auto".to_string(),
                "--session".to_string(),
                "ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()
            ]
        );
        assert!(
            !args.iter().any(|a| a == "--resume" || a == "--session-id"),
            "must not emit Claude/Codex resume flags; got {args:?}"
        );
    }

    /// Fresh spawn argv order: `--auto` (base recipe) → `--model <id>` →
    /// `--prompt <text>`. No session-assign flag (self-assign). Pin the
    /// exact vector so a future reorder that pushes `--auto` past
    /// `--model` (or drops it) trips here, not in production.
    #[test]
    fn fresh_recipe_forwards_model_and_prompt_without_session_id() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("anthropic/claude-sonnet-4-5".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("fix the auth bug"),
            sandbox: false,
        };
        let prepared = default_prepare(&OPENCODE, input);
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--auto".to_string(),
                "--model".to_string(),
                "anthropic/claude-sonnet-4-5".to_string(),
                "--prompt".to_string(),
                "fix the auth bug".to_string(),
            ]
        );
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--session"
                || a == "--session-id"
                || a == "--prefill"),
            "fresh spawn must not assign a session id or emit --prefill; got {:?}",
            prepared.recipe.base_args
        );
    }
}
