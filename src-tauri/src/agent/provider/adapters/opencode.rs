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

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
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
            base_args: vec![],
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

    fn supports_model_override(&self) -> bool {
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

    fn after_fresh_spawn(&self, node_id: i64, spawn_path: &str, env_type: EnvType) {
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
        assert!(recipe.base_args.is_empty());
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
        assert!(recipe.base_args.is_empty());
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

    #[test]
    fn supports_resume_model_and_prefill_but_no_attention_hook() {
        assert!(OPENCODE.supports_resume());
        assert!(OPENCODE.auto_resume_on_startup());
        assert!(OPENCODE.supports_model_override());
        assert!(OPENCODE.supports_prefill());
        assert!(!OPENCODE.requires_attention_hook());
        assert!(!OPENCODE.produces_readable_transcript());
    }

    #[test]
    fn capabilities_descriptor_advertises_resume_model_prefill() {
        let caps = OPENCODE.capabilities();
        assert_eq!(caps.harness_id, "opencode");
        assert!(caps.supports_resume);
        assert!(caps.auto_resume_on_startup);
        assert!(caps.supports_model_override);
        assert!(caps.supports_prefill);
        assert!(!caps.supports_effort_override);
        assert!(!caps.requires_attention_hook);
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
    }

    /// Resume recipe is `opencode --session <id>` (flag, not a subcommand).
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
        };
        let prepared = default_prepare(&OPENCODE, input);
        let args = &prepared.recipe.base_args;
        assert_eq!(
            args,
            &["--session".to_string(), "ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()]
        );
        assert!(
            !args.iter().any(|a| a == "--resume" || a == "--session-id"),
            "must not emit Claude/Codex resume flags; got {args:?}"
        );
    }

    /// Fresh spawn with model + prefill: `--model provider/model --prompt text`.
    /// No session-assign flag (self-assign).
    #[test]
    fn fresh_recipe_forwards_model_and_prompt_without_session_id() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("anthropic/claude-sonnet-4-5".to_string()),
            effort: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("fix the auth bug"),
        };
        let prepared = default_prepare(&OPENCODE, input);
        let args = &prepared.recipe.base_args;
        assert_flag_followed_by_value(args, "--model", "anthropic/claude-sonnet-4-5");
        assert_flag_followed_by_value(args, "--prompt", "fix the auth bug");
        assert!(
            !args.iter().any(|a| a == "--session" || a == "--session-id" || a == "--prefill"),
            "fresh spawn must not assign a session id or emit --prefill; got {args:?}"
        );
    }
}
