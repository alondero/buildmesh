//! Coherent Agent Harness launch preparation (issue #1179).
//!
//! The previous architecture had the per-harness launch surface split across
//! 11 separate `AgentProvider` trait methods (`spawn_recipe`, `*_args`,
//! `supports_*`, `resets_backend_env`, etc.) plus an adapter-id switch in
//! `agent::capabilities::effort_control_for`. The capability descriptor
//! could disagree with the recipe the spawn path actually launched — the
//! `mcode` `supports_model_override() == true` while the interactive TUI
//! recipe rejects `--model` is the named example.
//!
//! This module introduces one adapter-owned struct — [`PreparedHarnessLaunch`]
//! — that bundles the recipe, the capability contract, and the environment
//! policy the selected harness advertises for its normal launch mode. A
//! shared default implementation [`default_prepare`] composes these by
//! consulting the existing `*_args` helpers on the trait, so every adapter
//! that follows the Claude-shaped recipe gets the correct behaviour for
//! free; adapters whose CLI keeps positionals after options (Codex
//! `resume [OPTIONS] <id>`) put those positionals in
//! [`crate::agent::provider::SpawnRecipe::trailing_args`] rather than
//! overriding [`AgentProvider::prepare_launch`].
//!
//! The capability contract travels with the recipe so a single test
//! (`spawn::tests::capability_recipe_coherence`) can prove they agree:
//! for every adapter, every session mode, and every value the resolver
//! might forward, the final `recipe.base_args` either contains the
//! expected flag or — when the capability descriptor says the harness
//! does not support the control — never does.

use crate::agent::capabilities::{EffortControlKind, ResolvedAgentConfig};
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe};
use crate::models::EnvType;

/// A `SessionIdMode` reference. Owns no allocation; borrows the orchestrator's
/// mode value. Split out from `agent::spawn::SessionIdMode` (which owns the
/// `String`) because the launch helper is pure and must not allocate — the
/// orchestrator has already done so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIdModeRef<'a> {
    /// Assign a new session id (orchestrator pre-writes it; harness CLI
    /// consumes it via `--session-id <uuid>` or the adapter's equivalent).
    Assign(&'a str),
    /// Resume an existing session id (orchestrator already has it; harness
    /// CLI consumes it via `--resume <uuid>` / `--session <id>` / subcommand).
    Resume(&'a str),
    /// No session id — the harness self-assigns (e.g. mcode, kimi)
    /// and the orchestrator relies on the reader's PTY capture to learn it.
    None,
}

/// Inputs the harness needs to build its launch contribution. The resolver
/// has already applied the capability mask (issue #1149), so a `Some(model)`
/// here means "the harness accepts a model override AND the value is in its
/// vocabulary" — `prepare_launch` forwards it verbatim without re-consulting
/// capability flags. Pure (no I/O, no globals).
pub struct HarnessLaunchInput<'a> {
    /// Host platform the binary runs on. WSL meshes map to
    /// `Platform::Linux` so adapter recipes can use Linux-style flags.
    pub platform: Platform,
    /// Runtime spawn target (Windows vs WSL). WSL propagates additional
    /// env (see `apply_routing_env`); native Windows picks the WSL-aware
    /// Windows shell from each adapter's recipe.
    pub runtime: EnvType,
    /// Session id mode (assign / resume / none). The adapter decides
    /// whether its CLI accepts the id, the flag shape, or ignores it.
    pub session: SessionIdModeRef<'a>,
    /// Already-capability-masked resolved config. `Some(_)` values are
    /// safe to forward verbatim.
    pub config: &'a ResolvedAgentConfig,
    /// Prefill prompt text, if any. Only forwarded when the harness
    /// reports `supports_prefill`. The shared default trims and
    /// normalises CRLF before forwarding.
    pub prefill: Option<&'a str>,
    /// `true` when the parent mesh has its `sandbox` toggle on. The
    /// shared default appends the adapter's [`AgentProvider::sandbox_args`]
    /// contribution when this is set; the orchestrator's outer wrapper
    /// (`spawn_environment::wrap`) consults it independently for its
    /// own platform-level containment (macOS Seatbelt, Windows
    /// restricted-token). Issue #1287.
    pub sandbox: bool,
}

/// The environment policy a harness declares for its launch. A future
/// `env_set` slot is reserved for harness-specific env injections but
/// is empty for every current adapter — per-profile backend env
/// continues to flow through `PreparedLaunchRouting::Environment`.
pub struct HarnessEnvironmentPolicy {
    /// Reset the cwrap unset list (`CLAUDE_BACKEND_ENV_VARS`) before the
    /// spawn path injects the per-profile backend env. True for the
    /// Claude-backed `anthropic` adapter; false for native-binary
    /// providers (Codex, OpenCode, Agy) that never went through cwrap.
    pub resets_backend_env: bool,
    /// Keys to strip from the child env (e.g. Codex Proxy strips generic
    /// `OPENAI_API_KEY` / `OPENAI_BASE_URL` so the pairing-scoped
    /// credential reference is the only OpenAI auth path).
    pub env_remove: &'static [&'static str],
    /// Per-harness extra env to set. Reserved for future harnesses; empty
    /// for every current adapter.
    pub env_set: &'static [(&'static str, &'static str)],
}

impl HarnessEnvironmentPolicy {
    /// Sentinel: an empty policy. Use for harnesses with no env work.
    pub const NONE: &'static HarnessEnvironmentPolicy = &HarnessEnvironmentPolicy {
        resets_backend_env: false,
        env_remove: &[],
        env_set: &[],
    };
}

/// The coherent launch contribution a harness produces for a single spawn:
/// the command the orchestrator will execute, the capability contract the
/// Spawn Menu / resolver / autopilot already advertised (so the consumer
/// can cross-check), and the env policy the spawn path applies after
/// routing. The capability descriptor travels with the recipe to make
/// the coherence testable: the same `AgentProvider` instance that
/// produced the recipe also produced the descriptor.
pub struct PreparedHarnessLaunch {
    pub recipe: SpawnRecipe,
    pub capabilities: crate::agent::capabilities::HarnessCapabilities,
    pub environment: &'static HarnessEnvironmentPolicy,
}

/// Collapse `\r\n` and bare `\r` to `\n` in prefill text. GitHub issue/PR
/// bodies arrive with CRLF line endings; a bare `\r` reaching an agent's
/// TUI input on Windows is interpreted as Enter, submitting the prompt
/// after the first line. macOS / Linux (claude spawned directly) tolerate
/// CRLF, which is why this only bit Windows. Lives here so every adapter
/// path that consumes prefill goes through the same normalisation.
fn normalize_prefill_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Shared default implementation of [`AgentProvider::prepare_launch`] for
/// the Claude-shaped recipe (every adapter that follows the base-recipe +
/// flag-arg pattern). Subcommand-style resume (Codex) returns options in
/// `base_args` and the session id in `trailing_args`; this helper layers
/// flags onto `base_args` and never relocates trailing positionals.
///
/// The function is pure: no I/O, no DB, no globals. It consumes the
/// capability-masked `ResolvedAgentConfig` and the session mode by
/// reference; the orchestrator's owned values live outside this module.
///
/// Behaviour pinned by [`crate::agent::spawn::tests::capability_recipe_coherence`]
/// and the per-adapter tests under `adapters/`.
pub fn default_prepare(
    adapter: &dyn AgentProvider,
    input: HarnessLaunchInput<'_>,
) -> PreparedHarnessLaunch {
    let capabilities = adapter.capabilities();
    let base_recipe = adapter.spawn_recipe(input.platform, input.runtime);

    // The base recipe before session-id / override / prefill args are layered on.
    // Subcommand resume (Codex) returns options in `base_args` and the session
    // id in `trailing_args`. Layer flags onto `base_args` only — never pop
    // from the adapter's argv.
    let mut recipe = match input.session {
        SessionIdModeRef::Resume(id) => {
            if let Some(resume_recipe) = adapter.spawn_recipe_for_resume(input.platform, id) {
                resume_recipe
            } else {
                let mut r = base_recipe;
                let args = adapter.resume_args(id);
                if !args.is_empty() {
                    r.base_args.extend(args);
                }
                r
            }
        }
        SessionIdModeRef::Assign(id) => {
            let mut r = base_recipe;
            let args = adapter.session_assign_args(id);
            if !args.is_empty() {
                r.base_args.extend(args);
            }
            r
        }
        SessionIdModeRef::None => base_recipe,
    };

    // The resolver already applied the capability mask; `Some` here means
    // the harness accepts this control AND the value is in its vocabulary
    // (issue #1149 acceptance criteria 6, 7, 9). The mask is the single
    // guarantee that unsupported values never reach a harness process.
    //
    // Defence in depth (issue #1179): the helper also re-asserts the
    // capability descriptor on the forward, so a caller that bypasses
    // the resolver (e.g. a future internal call site, a test) cannot
    // smuggle a model arg into a harness that advertised no support.
    // The capability and the recipe come from the same adapter — they
    // cannot disagree by construction.
    if capabilities.supports_model_override {
        if let Some(model) = input.config.model.as_deref().filter(|s| !s.is_empty()) {
            recipe.base_args.extend(adapter.model_args(model));
        }
    }
    if !matches!(capabilities.effort_control, EffortControlKind::None) {
        if let Some(effort) = input.config.effort.as_deref().filter(|s| !s.is_empty()) {
            recipe.base_args.extend(adapter.effort_args(effort));
        }
    }
    // Issue #1358: forward the per-step `extra_args` verbatim through
    // the adapter's tokenise helper. Mirrors the model / effort
    // branches above — capability-masked at the resolver (no
    // harness-side check needed here) and only forwarded when the
    // capability descriptor advertises support. Placed between
    // `effort_args` and `sandbox_args` so the semantic-then-positional
    // layer order survives: model / effort / extras are user-supplied,
    // sandbox is mesh-driven, prefill is the trailing positional.
    if capabilities.supports_extra_args {
        if let Some(extra) = input.config.extra_args.as_deref().filter(|s| !s.is_empty()) {
            // Gracefully degrade rather than crashing the spawn worker
            // thread on a user typo (unclosed quote, dangling escape).
            // PR #1362 round 2 review: a `panic!` here is a fatal runtime
            // failure for a circuit-author-authored value. We log and
            // fall back to whitespace splitting so the spawn still
            // proceeds (the user's other args land correctly; only the
            // malformed token group is dropped).
            match adapter.extra_args_args(extra) {
                Ok(args) => recipe.base_args.extend(args),
                Err(e) => {
                    tracing::warn!(
                        "extra_args tokenisation failed ({}); falling back to whitespace split for {:?}",
                        e, extra
                    );
                    recipe
                        .base_args
                        .extend(extra.split_whitespace().map(String::from));
                }
            }
        }
    }

    // Issue #1287 — adapter-level sandbox flag. When the parent mesh
    // has its `sandbox` toggle on, append the adapter's declared
    // sandbox contribution (e.g. Antigravity's `--sandbox`). Layered
    // on top of the orchestrator's outer containment wrapper (macOS
    // Seatbelt / Windows restricted-token) — the two are independent
    // layers: the outer wrapper confines the filesystem, the adapter
    // flag confines the agent's own terminal-side operations.
    if input.sandbox {
        let sandbox = adapter.sandbox_args();
        if !sandbox.is_empty() {
            recipe.base_args.extend(sandbox);
        }
    }

    if capabilities.supports_prefill {
        if let Some(text) = input.prefill.filter(|s| !s.is_empty()) {
            let normalized = normalize_prefill_newlines(text);
            let args = adapter.prefill_args(&normalized);
            // Prefill after a trailing session id is also positional
            // (`codex resume [OPTIONS] <id> [PROMPT]`). Flag-shaped prefill
            // (`--prefill text`) stays on `base_args` when there are no
            // trailing positionals.
            if recipe.trailing_args.is_empty() {
                recipe.base_args.extend(args);
            } else {
                recipe.trailing_args.extend(args);
            }
        }
    }

    let environment: &'static HarnessEnvironmentPolicy = if adapter.resets_backend_env() {
        // Claude-backed adapter: reset cwrap's unset list before env injection.
        static CLAUDE: HarnessEnvironmentPolicy = HarnessEnvironmentPolicy {
            resets_backend_env: true,
            env_remove: &[],
            env_set: &[],
        };
        &CLAUDE
    } else if matches!(adapter.id(), "codex") {
        // Codex (both native and proxy): the spawn path strips generic
        // OpenAI variables so a verified Codex Proxy pairing's
        // credential reference is the only auth path. The proxy-specific
        // credential injection is handled in `apply_codex_proxy_credential`
        // (it has routing data this helper does not see).
        static CODEX: HarnessEnvironmentPolicy = HarnessEnvironmentPolicy {
            resets_backend_env: false,
            env_remove: &["OPENAI_API_KEY", "OPENAI_BASE_URL"],
            env_set: &[],
        };
        &CODEX
    } else {
        HarnessEnvironmentPolicy::NONE
    };

    PreparedHarnessLaunch {
        recipe,
        capabilities,
        environment,
    }
}

/// Assert `args` contains `flag` immediately followed by `value` (no
/// other arg interleaved). Used by the per-adapter recipe pins (grok
/// `--model`, kimi `-m`) to catch the specific failure mode the
/// table-driven `capability_recipe_coherence` cannot — silent
/// short↔long flag swaps that the adapter would silently follow.
#[cfg(test)]
pub(crate) fn assert_flag_followed_by_value(args: &[String], flag: &str, value: &str) {
    let idx = args
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("prepared recipe must contain {flag} flag; got args = {args:?}"));
    assert_eq!(
        args.get(idx + 1).map(String::as_str),
        Some(value),
        "prepared recipe must put {value:?} immediately after {flag}; got args = {args:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::adapters::TERMINAL;

    /// Empty session + empty config must produce a recipe that exactly
    /// mirrors the adapter's `spawn_recipe` — no synthesised flags.
    #[test]
    fn default_prepare_empty_input_equals_base_recipe_for_terminal() {
        let adapter = &TERMINAL as &dyn AgentProvider;
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        let base = adapter.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(prepared.recipe.binary, base.binary);
        assert_eq!(prepared.recipe.windows_shell, base.windows_shell);
        assert!(
            prepared.recipe.base_args.is_empty() && prepared.recipe.trailing_args.is_empty(),
            "terminal harness must not synthesise any args for empty input, got {:?} + {:?}",
            prepared.recipe.base_args,
            prepared.recipe.trailing_args
        );
        assert!(!prepared.environment.resets_backend_env);
        assert!(prepared.environment.env_remove.is_empty());
    }

    /// Prefill text with CRLF must be normalised to LF before reaching
    /// the adapter, mirroring the prior inline behaviour in
    /// `build_spawn_command_prepared`.
    #[test]
    fn default_prepare_normalises_prefill_crlf() {
        let adapter = &crate::agent::provider::adapters::ANTHROPIC as &dyn AgentProvider;
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("first\r\nsecond\rthird"),
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        let last = prepared
            .recipe
            .base_args
            .last()
            .expect("prefill should produce a trailing arg");
        assert_eq!(last, "first\nsecond\nthird");
    }

    /// Assign mode must forward the adapter's `session_assign_args`.
    /// Anthropic uses the default `["--session-id", id]`.
    #[test]
    fn default_prepare_assign_mode_forwards_session_id() {
        let adapter = &crate::agent::provider::adapters::ANTHROPIC as &dyn AgentProvider;
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Assign("abc-uuid"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        let base = adapter.spawn_recipe(Platform::Windows, EnvType::Windows);
        let mut expected = base.base_args.clone();
        expected.extend(["--session-id".to_string(), "abc-uuid".to_string()]);
        assert_eq!(prepared.recipe.base_args, expected);
    }

    /// Resume mode must call `spawn_recipe_for_resume` when the adapter
    /// overrides it (Codex), or fall back to `spawn_recipe + resume_args`
    /// when it does not.
    #[test]
    fn default_prepare_resume_mode_uses_subcommand_for_codex() {
        let adapter = &crate::agent::provider::adapters::CODEX as &dyn AgentProvider;
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("sess-xyz"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        // Codex's resume recipe is `codex resume [OPTIONS] sess-xyz`
        // — the "resume" positional is a subcommand the base recipe
        // would not include, and the id stays after options.
        assert!(
            prepared.recipe.base_args.iter().any(|a| a == "resume"),
            "Codex resume must use the subcommand recipe, got {:?}",
            prepared.recipe.base_args
        );
        let args: Vec<&str> = prepared.recipe.argv().collect();
        assert_eq!(
            args.last().copied(),
            Some("sess-xyz"),
            "session id must be last among options, got {args:?}"
        );
        assert_eq!(
            prepared.recipe.trailing_args,
            vec!["sess-xyz".to_string()],
            "session id belongs in trailing_args, not base_args: {:?}",
            prepared.recipe.base_args
        );
    }

    #[test]
    fn default_prepare_codex_resume_keeps_session_id_after_model() {
        let adapter = &crate::agent::provider::adapters::CODEX as &dyn AgentProvider;
        let config = ResolvedAgentConfig {
            model: Some("gpt-5.4".into()),
            ..Default::default()
        };
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("sess-xyz"),
            config: &config,
            prefill: Some("continue"),
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        let args: Vec<&str> = prepared.recipe.argv().collect();
        let model_at = args
            .iter()
            .position(|arg| *arg == "--model")
            .expect("model flag");
        let id_at = args
            .iter()
            .position(|arg| *arg == "sess-xyz")
            .expect("session id");
        let prefill_at = args
            .iter()
            .position(|arg| *arg == "continue")
            .expect("prefill");
        assert!(
            model_at < id_at && id_at < prefill_at,
            "expected resume [OPTIONS] <id> <prompt>, got {args:?}"
        );
        assert_eq!(
            prepared.recipe.trailing_args,
            vec!["sess-xyz".to_string(), "continue".to_string()],
            "session id and prompt must stay in trailing_args so later option layers cannot overtake them"
        );
    }

    /// Model config must be forwarded verbatim through the adapter's
    /// `model_args`. The capability mask has already run, so a `Some(m)`
    /// value here is safe to forward — no re-mask.
    #[test]
    fn default_prepare_forwards_model_arg() {
        let adapter = &crate::agent::provider::adapters::ANTHROPIC as &dyn AgentProvider;
        let config = ResolvedAgentConfig {
            model: Some("claude-sonnet-4-5".to_string()),
            ..Default::default()
        };
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        // Anthropic's default model_args is `["--model", m]`.
        let i = prepared
            .recipe
            .base_args
            .iter()
            .position(|a| a == "--model")
            .expect("forwarded model must be present in base_args");
        assert_eq!(prepared.recipe.base_args[i + 1], "claude-sonnet-4-5");
    }

    /// `extra_args` (issue #1358) must be forwarded verbatim through
    /// the adapter's `extra_args_args`. The capability mask has already
    /// run — the descriptor reports `supports_extra_args = true` for
    /// Anthropic — so a `Some(raw)` value here is safe to forward
    /// without a re-mask. Whitespace tokenisation is the default impl,
    /// so a multi-flag string becomes multiple argv entries.
    #[test]
    fn default_prepare_forwards_extra_args() {
        let adapter = &crate::agent::provider::adapters::ANTHROPIC as &dyn AgentProvider;
        let sentinel_a = "--bm-test-extra-a";
        let sentinel_b = "--bm-test-extra-b";
        let config = ResolvedAgentConfig {
            extra_args: Some(format!("{sentinel_a} {sentinel_b}")),
            ..Default::default()
        };
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        let base = &prepared.recipe.base_args;
        // Find first sentinel and assert the second follows it (so the
        // whitespace tokenisation emitted both as separate argv entries).
        let i = base
            .iter()
            .position(|a| a == sentinel_a)
            .expect("forwarded extra_args must include the literal flag");
        assert_eq!(
            base[i + 1],
            sentinel_b,
            "whitespace tokenisation emits each token as its own argv element"
        );
    }

    /// Quote-aware tokenisation (issue #1362 code review): a quoted
    /// argv element must NOT be split on the embedded whitespace. A
    /// user-supplied `--append "fix the bug"` keeps the quoted phrase
    /// as a single argv entry, exactly as the shell would. Without
    /// the seam, naive `split_whitespace` would chop the value
    /// silently and the harness would see `--append fix the bug`
    /// (three argv elements) which has very different semantics on
    /// most CLIs.
    #[test]
    fn extra_args_tokenisation_preserves_quoted_phrases() {
        let adapter = &crate::agent::provider::adapters::ANTHROPIC as &dyn AgentProvider;
        let config = ResolvedAgentConfig {
            extra_args: Some(r#"--append "fix the bug" --verbose"#.to_string()),
            ..Default::default()
        };
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        let i = prepared
            .recipe
            .base_args
            .iter()
            .position(|a| a == "--append")
            .expect("--append must be tokenised as a separate argv element");
        assert_eq!(
            &prepared.recipe.base_args[i + 1],
            "fix the bug",
            "quoted phrase must remain a single argv element after shell-style tokenisation"
        );
        assert_eq!(
            &prepared.recipe.base_args[i + 2],
            "--verbose",
            "trailing unquoted token must not be glued to the prior quoted phrase"
        );
    }

    /// Terminal's `supports_extra_args = false` must drop the
    /// `extra_args` value at the capability gate (issue #1358). Without
    /// this pin a future regression that forgot the gate would splice
    /// the user's flags into `powershell.exe` — a footgun.
    #[test]
    fn default_prepare_drops_extra_args_for_terminal() {
        let adapter = &crate::agent::provider::adapters::TERMINAL as &dyn AgentProvider;
        let config = ResolvedAgentConfig {
            extra_args: Some("--anything --the-user-typed".to_string()),
            ..Default::default()
        };
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        assert!(
            !prepared
                .recipe
                .base_args
                .contains(&"--anything".to_string())
                && !prepared
                    .recipe
                    .base_args
                    .contains(&"--the-user-typed".to_string()),
            "Terminal drops extra_args at the capability mask; got {:?}",
            prepared.recipe.base_args
        );
    }

    /// **PR #1362 round 2 review (panic-on-typo fix):** A malformed
    /// shell-syntax input (unclosed quote, dangling escape) MUST NOT
    /// crash the spawn worker thread. The fallback path:
    /// 1. Returns `Err` from `extra_args_args(raw)` (not `panic!`)
    /// 2. `default_prepare` logs a warning at `tracing::warn!`
    /// 3. Falls back to whitespace splitting so the user's other
    ///    args still reach the harness — only the malformed token
    ///    group is dropped, NOT the entire spawn.
    ///
    /// CRITICAL: this test catches a regression where a `panic!`
    /// would turn every user typo into a fatal worker crash.
    #[test]
    fn extra_args_malformed_shell_syntax_does_not_panic() {
        let adapter = &crate::agent::provider::adapters::ANTHROPIC as &dyn AgentProvider;
        // Unclosed quote — shell_words::split returns Err(MismatchedQuotes).
        let config = ResolvedAgentConfig {
            extra_args: Some(r#"--message "Let's test this or foo\"#.to_string()),
            ..Default::default()
        };
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        // The test exists to PROVE no panic. If a future refactor
        // reintroduces `panic!` or `.unwrap()` on the parse error,
        // this test traps the panic (catch_unwind) and reports it
        // instead of taking down the test runner.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            default_prepare(adapter, input)
        }));
        let prepared = outcome.expect(
            "extra_args tokenisation MUST NOT panic on malformed input — \
             PR #1362 round 2 review fix: log + fall back to whitespace, never panic.",
        );
        // Whitespace-split fallback keeps the well-formed tokens.
        // `--message` tokenises as a separate argv element (it's
        // unquoted in the fallback path).
        assert!(
            prepared.recipe.base_args.contains(&"--message".to_string()),
            "fallback must still forward the well-formed `--message` token; got {:?}",
            prepared.recipe.base_args
        );
        // The malformed quote's contents drop with the quote — that's
        // the expected graceful degradation. The critical property is
        // that the spawn *proceeds* (no panic) and other valid args
        // survive.
    }

    /// Effort config must be forwarded through the adapter's
    /// `effort_args`. Codex's inline config shape
    /// (`-c model_reasoning_effort="..."`) is the regression pin.
    #[test]
    fn default_prepare_forwards_codex_inline_effort() {
        let adapter = &crate::agent::provider::adapters::CODEX as &dyn AgentProvider;
        let config = ResolvedAgentConfig {
            effort: Some("high".to_string()),
            ..Default::default()
        };
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(adapter, input);
        let found = prepared
            .recipe
            .base_args
            .iter()
            .any(|a| a.contains("model_reasoning_effort") && a.contains("high"));
        assert!(
            found,
            "Codex effort must be forwarded as inline config, got {:?}",
            prepared.recipe.base_args
        );
    }

    /// HarnessEnvironmentPolicy::NONE has all defaults off and empty slices.
    #[test]
    fn environment_policy_none_is_inert() {
        let p = HarnessEnvironmentPolicy::NONE;
        assert!(!p.resets_backend_env);
        assert!(p.env_remove.is_empty());
        assert!(p.env_set.is_empty());
    }
}
