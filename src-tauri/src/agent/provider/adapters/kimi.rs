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
//! `default_model` from `~/.kimi/config.toml` for that one session).
//! Credentials and provider mapping live in `~/.kimi/config.toml`;
//! Buildmesh does not manage those — Kimi's own login flow owns them.
//!
//! **Shell wrapping**: `kimi` is a native binary on all platforms (not a
//! `.cmd` shim), so `WindowsShell::Direct` is correct everywhere — matching
//! the AGY and Grok adapter patterns.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct KimiAdapter;
pub static KIMI: KimiAdapter = KimiAdapter;

impl AgentProvider for KimiAdapter {
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
            windows_shell: WindowsShell::Direct,
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

    /// Kimi Code stores its session log under `~/.kimi/sessions/wire.jsonl`
    /// in standard JSONL form (#911 research). The on-disk *format* matches
    /// what the shared transcript_reader parses, but the *path* is
    /// `~/.kimi/...` not `~/.claude/projects/<encoded-cwd>/<session>.jsonl`,
    /// and the reader's path resolver isn't wired for Kimi yet — so the
    /// Node Digest rich layer currently degrades to spine-only with the
    /// `unsupported` flag set, not silent omission. Returns `false` to
    /// match the wire behaviour; follow-up wires the Kimi case into
    /// `services::transcript_reader::TranscriptFormat::for_harness`.
    fn produces_readable_transcript(&self) -> bool {
        false
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        false
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
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
mod tests {
    use super::*;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(KIMI.id(), "kimi");
        let ui = KIMI.ui();
        assert_eq!(ui.label, "Kimi Code");
        assert_eq!(ui.color, "#00c4c4");
        assert_eq!(ui.icon, "K");
    }

    #[test]
    fn spawn_recipe_direct_on_all_platforms() {
        for platform in [Platform::Windows, Platform::Linux, Platform::Macos] {
            let recipe = KIMI.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "kimi");
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
    fn available_on_all_three_platforms() {
        let platforms = KIMI.available_on();
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
        assert!(KIMI.self_assigns_session_id());
    }

    #[test]
    fn resume_args_format() {
        // Kimi uses `-S` (uppercase) as the explicit-resume flag, NOT `--resume`.
        let args = KIMI.resume_args("abc-123");
        assert_eq!(args, vec!["-S", "abc-123"]);
    }

    #[test]
    fn model_args_format() {
        // Kimi uses `-m` (short) for the model override, matching `--help`.
        let args = KIMI.model_args("kimi-k2");
        assert_eq!(args, vec!["-m", "kimi-k2"]);
    }

    #[test]
    fn session_assign_args_empty() {
        let args = KIMI.session_assign_args("any-id");
        assert!(args.is_empty(), "Kimi self-assigns; session_assign_args must be empty");
    }

    #[test]
    fn no_prefill_support() {
        assert!(!KIMI.supports_prefill());
    }

    #[test]
    fn supports_resume_and_model_override_but_no_attention_hook() {
        // Kimi Code is a native TUI binary (issue #911) — it doesn't read
        // Claude-style hooks, so the attention callback (#886) doesn't apply.
        assert!(KIMI.supports_resume());
        assert!(KIMI.supports_model_override());
        assert!(!KIMI.requires_attention_hook());
    }

    #[test]
    fn produces_readable_transcript() {
        // #911 research confirmed Kimi's wire.jsonl is standard JSONL, but
        // the transcript reader's path resolver isn't wired for `~/.kimi/`
        // yet — so we claim `false` to match the current wire behaviour
        // (Node Digest rich layer degrades to spine-only with `unsupported`).
        // When the follow-up wires `TranscriptFormat::Kimi`, flip this back
        // to `true` and add a reader test that parses a fixture wire.jsonl.
        assert!(!KIMI.produces_readable_transcript());
    }

    /// Issue #1186: pin the harness-specific model-flag shape. The
    /// table-driven `capability_recipe_coherence` only asserts *some*
    /// model flag exists in the recipe — a silent `-m` ↔ `--model`
    /// flip on the adapter would pass. This pin catches the drift
    /// before it reaches the wire.
    #[test]
    fn kimi_interactive_recipe_carries_short_m_model_arg() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("kimi-k2".to_string()),
            effort: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
        };
        let prepared = default_prepare(&KIMI, input);
        let args = &prepared.recipe.base_args;
        assert_flag_followed_by_value(args, "-m", "kimi-k2");
        // Short form is canonical — a refactor to `--model` would
        // silently flip the wire shape; this catches it.
        assert!(
            !args.iter().any(|a| a == "--model"),
            "Kimi must use -m (short form), not --model; got args = {args:?}"
        );
    }

    /// Issue #1179 (mirror): end-to-end descriptor pin. The Spawn Menu,
    /// resolver, and autopilot compatibility gate all consume this
    /// descriptor — drift here means the menu misroutes Kimi.
    #[test]
    fn capabilities_descriptor_advertises_model_override() {
        let caps = KIMI.capabilities();
        assert_eq!(caps.harness_id, "kimi");
        assert!(caps.supports_resume);
        assert!(caps.supports_model_override);
        assert!(!caps.supports_effort_override);
        assert!(!caps.supports_prefill);
        assert!(!caps.requires_attention_hook);
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(caps.effort_control, crate::agent::capabilities::EffortControlKind::None);
    }
}
