//! MiniMax Code CLI provider adapter — MiniMax's full-screen interactive
//! coding agent, installed on PATH as a single `mcode` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. Buildmesh's PTY backend (ConPTY on
//! Windows, native PTY on macOS/Linux) fully supports full-screen TUI rendering,
//! so we launch in interactive mode everywhere.
//!
//! **Session resumption** uses `--session [<id>]` or `-c` / `--continue`.
//! MiniMax Code auto-assigns its own session ids (captured from PTY output by
//! `session_naming`), so `self_assigns_session_id()` is `true` and
//! `session_assign_args()` is a no-op.
//!
//! **Model override** uses `mcode exec --model <provider>/<model>`. mcode's
//! flag is *only* available on the `exec` subcommand (interactive TUI mode has
//! no `--model` flag) and requires the `<provider>/<model>` format — the
//! provider is the id from `mcode provider list` (default `minimax`). We emit
//! the correct syntax today so an `exec`-mode spawn (the upcoming attention-hook
//! work, see memory) end-to-end fires the override; against the current TUI
//! recipe the flag is rejected upstream, which mirrors the existing
//! pre-fix failure mode (`-m` was also rejected). `model_args()` prefixes a
//! bare model id with the default provider; if the user writes
//! `<provider>/<model>` explicitly we round-trip it without re-prefixing.
//!
//! **Prefill** is the trailing positional `[prompt]` — there is no `--prefill`
//! flag. We override `prefill_args()` to return the text as a single positional
//! arg (the trait default `["--prefill", text]` would be rejected upstream).
//!
//! **Shell wrapping**: `mcode` is distributed as a `.cmd` batch shim on
//! Windows (`mcode.cmd`), which `CreateProcess` won't run directly; the recipe
//! wraps with `WindowsShell::Cmd` -> `cmd.exe /c mcode …` on Windows. On macOS /
//! Linux it is an executable on PATH so `WindowsShell::Direct` is used.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct McodeAdapter;
pub static MCODE: McodeAdapter = McodeAdapter;

/// Default provider id we prefix bare model names with. Matches the
/// `providerId: "minimax_api"` returned by `mcode provider list --json` and
/// the `kind: "minimax-api-key"` row. Pinned here (not derived from the
/// CLI's runtime output) so a future mcode default change doesn't silently
/// shift Buildmesh's emitted `--model` strings — the regression-test surface
/// in `model_args_format_*` catches any drift.
const DEFAULT_MCODE_PROVIDER: &str = "minimax";

/// Per-platform shell selection. Mirrors the OpenCode / Antigravity pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

/// If `model` already contains a `/`, treat it as `<provider>/<model>` and
/// return as-is; otherwise prefix with [`DEFAULT_MCODE_PROVIDER`]. Pure helper
/// so the prefix logic is unit-testable in isolation from the trait impl.
fn normalize_mcode_model(model: &str) -> String {
    if model.contains('/') {
        model.to_string()
    } else {
        format!("{DEFAULT_MCODE_PROVIDER}/{model}")
    }
}

impl AgentProvider for McodeAdapter {
    fn id(&self) -> &'static str {
        "mcode"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "MiniMax Code".into(),
            color: "#6366f1".into(),
            icon: "M".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "mcode",
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

    fn produces_readable_transcript(&self) -> bool {
        false
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        // mcode accepts `[prompt]` as a trailing positional on both TUI and
        // exec modes. Override below emits the text verbatim, no `--prefill`
        // flag.
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    /// MiniMax Code auto-assigns session ids — captured from PTY output.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        // `--model <provider>/<model>` per `mcode exec --help`. The flag is
        // only valid on the `exec` subcommand (TUI mode rejects it), so end-
        // to-end override needs the `exec` recipe — that lands with the
        // attention-hook work. Emitting the correct syntax today means the
        // recipe switch is the only thing standing between us and a working
        // override, not a CLI-shape fix too.
        vec!["--model".into(), normalize_mcode_model(model)]
    }

    /// No `--session-id` flag — MiniMax Code assigns its own.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // mcode's prompt is the trailing positional `[prompt]` on both TUI
        // and exec subcommands — there is no `--prefill` flag. The trait
        // default would emit `["--prefill", text]` which mcode rejects.
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(MCODE.id(), "mcode");
        let ui = MCODE.ui();
        assert_eq!(ui.label, "MiniMax Code");
        assert_eq!(ui.color, "#6366f1");
        assert_eq!(ui.icon, "M");
    }

    #[test]
    fn spawn_recipe_direct_on_macos_and_linux() {
        for platform in [Platform::Linux, Platform::Macos] {
            let recipe = MCODE.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "mcode");
            assert!(recipe.base_args.is_empty());
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "{:?} must use WindowsShell::Direct — got {:?}",
                platform,
                recipe.windows_shell
            );
        }
    }

    #[test]
    fn spawn_recipe_cmd_on_windows() {
        let recipe = MCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "mcode");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn available_on_all_three_platforms() {
        let platforms = MCODE.available_on();
        assert_eq!(
            platforms.len(),
            3,
            "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
            platforms
        );
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    #[test]
    fn self_assigns_session_id() {
        assert!(MCODE.self_assigns_session_id());
    }

    #[test]
    fn resume_args_format() {
        let args = MCODE.resume_args("abc-123");
        assert_eq!(args, vec!["--session", "abc-123"]);
    }

    #[test]
    fn model_args_format_prefixes_default_provider() {
        // Bare model id → `--model <default-provider>/<model>` per mcode's
        // `exec --help`. The default provider (`minimax`) is the documented
        // fallback from `mcode provider list --json` (kind: minimax-api-key).
        let args = MCODE.model_args("MiniMax-Text-01");
        assert_eq!(args, vec!["--model", "minimax/MiniMax-Text-01"]);
    }

    #[test]
    fn model_args_format_passes_through_explicit_provider_prefix() {
        // If the user writes `<provider>/<model>` explicitly, don't double-prefix.
        let args = MCODE.model_args("minimax/MiniMax-Text-01");
        assert_eq!(
            args,
            vec!["--model", "minimax/MiniMax-Text-01"],
            "explicit <provider>/<model> must round-trip without re-prefixing"
        );
    }

    #[test]
    fn model_args_format_passes_through_custom_provider() {
        // A user-configured provider via `mcode provider add` flows through
        // verbatim. Buildmesh doesn't pin the provider list — mcode owns it.
        let args = MCODE.model_args("openai-compatible/gpt-4o");
        assert_eq!(args, vec!["--model", "openai-compatible/gpt-4o"]);
    }

    #[test]
    fn session_assign_args_empty() {
        let args = MCODE.session_assign_args("any-id");
        assert!(
            args.is_empty(),
            "MiniMax Code self-assigns; session_assign_args must be empty"
        );
    }

    #[test]
    fn prefill_args_is_positional() {
        // mcode accepts `[prompt]` as a positional on both TUI and exec modes —
        // there is no `--prefill` flag. The trait default (`vec!["--prefill", t]`)
        // is wrong here.
        let args = MCODE.prefill_args("fix the auth bug");
        assert_eq!(args, vec!["fix the auth bug"]);
    }

    #[test]
    fn prefill_args_preserves_multiline_text() {
        // Issue/handover prefills are often multi-line; the adapter must not
        // collapse or wrap them — they're appended verbatim.
        let multi = "first line\nsecond line\n  indented";
        let args = MCODE.prefill_args(multi);
        assert_eq!(args, vec![multi]);
    }

    #[test]
    fn supports_prefill_via_positional() {
        assert!(MCODE.supports_prefill());
    }

    #[test]
    fn supports_resume_and_model_override_but_no_attention_hook() {
        assert!(MCODE.supports_resume());
        assert!(MCODE.supports_model_override());
        assert!(!MCODE.requires_attention_hook());
    }

    #[test]
    fn produces_readable_transcript() {
        assert!(!MCODE.produces_readable_transcript());
    }
}
