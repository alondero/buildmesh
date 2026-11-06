use super::*;
use crate::agent::provider::adapters::TERMINAL;

/// Issue #1550 — the fork seam is opt-in, and "opt-in" has to mean
/// *byte-identical argv for everyone else*. For every adapter that
/// does not override `resume_args_fork`, a `Regenerate` resume must
/// compose exactly the argv its `Explicit` resume composes; otherwise
/// #1550 would silently change the launch for harnesses whose CLI
/// never saw a fork flag.
#[test]
fn regenerate_resume_matches_explicit_resume_for_adapters_without_a_fork() {
    let config = ResolvedAgentConfig::default();
    let mut checked = 0;
    for provider in crate::models::Provider::all() {
        let adapter = provider.adapter();
        // Skip the adapters that actually branch — their argv is
        // supposed to differ.
        if adapter.resume_args_fork("probe-session") != adapter.resume_args("probe-session") {
            continue;
        }
        if !adapter.descriptor().supports_resume {
            continue;
        }
        checked += 1;
        let args_for = |cause: ResumeCause| {
            adapter
                .prepare_launch(HarnessLaunchInput {
                    routing: None,
                    node_id: None,
                    platform: Platform::Windows,
                    runtime: EnvType::Windows,
                    session: SessionIdModeRef::Resume {
                        id: "probe-session",
                        cause,
                    },
                    config: &config,
                    prefill: None,
                    sandbox: false,
                })
                .recipe
                .argv()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            args_for(ResumeCause::Regenerate),
            args_for(ResumeCause::Explicit),
            "{} must keep its pre-#1550 in-place resume argv for a \
                 Regenerate cause (no resume_args_fork override)",
            adapter.id()
        );
    }
    assert!(
        checked > 1,
        "expected several fork-less resumable adapters to compare, saw {checked}"
    );
}

/// The inverse guard on the same table: OpenCode is the adapter that
/// opts in, so it must be the one whose `Regenerate` argv differs.
/// Without this the test above could pass vacuously if every adapter
/// silently lost its fork override.
#[test]
fn opencode_is_the_only_resumable_adapter_that_opts_into_forking() {
    let opting_in: Vec<&str> = crate::models::Provider::all()
        .iter()
        .filter(|provider| {
            let adapter = provider.adapter();
            adapter.descriptor().supports_resume
                && adapter.resume_args_fork("probe-session") != adapter.resume_args("probe-session")
        })
        .map(|provider| provider.adapter().id())
        .collect();
    assert_eq!(
        opting_in,
        vec!["opencode"],
        "fork opt-in is a per-adapter decision; a new entry here means a \
             harness genuinely supports branching and its resume shape needs \
             review"
    );
}

/// Empty session + empty config must produce a recipe that exactly
/// mirrors the adapter's `spawn_recipe` — no synthesised flags.
#[test]
fn default_prepare_empty_input_equals_base_recipe_for_terminal() {
    let adapter = &TERMINAL as &dyn AgentProvider;
    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
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

/// Prefill text with CRLF must be normalised before reaching the
/// adapter. Windows flattens to single space (the `cmd.exe /c`
/// end-of-command trap; issue #1773 review); macOS/Linux preserves
/// the line structure and only normalises CRLF→LF.
#[test]
fn default_prepare_normalises_prefill_crlf() {
    let adapter = &crate::agent::provider::adapters::ANTHROPIC as &dyn AgentProvider;
    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: Some("first\r\nsecond\rthird"),
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
    let last = prepared
        .recipe
        .base_args
        .last()
        .expect("prefill should produce a trailing arg");
    assert_eq!(last, "first second third");
}

/// Issue #1773 — Cline's base recipe already carries `-i`, so the
/// adapter's `prefill_args` returns just the positional text and
/// `default_prepare` extends the base with it. The composed argv must
/// contain exactly one `-i` and the prefill text as its trailing
/// argument. Regression-pin for a previous draft that re-emitted `-i`
/// inside `prefill_args`, producing `cline -i -i "<text>"`.
#[test]
fn cline_prefill_composes_without_repeating_the_tui_flag() {
    let adapter = &crate::agent::provider::adapters::CLINE as &dyn AgentProvider;
    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: Some("fix the auth bug"),
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
    let args: Vec<&str> = prepared.recipe.argv().collect();
    assert_eq!(
        args.iter().filter(|a| **a == "-i").count(),
        1,
        "exactly one -i must survive the prefill composition; got {args:?}"
    );
    assert_eq!(
        args.last().copied(),
        Some("fix the auth bug"),
        "prefill text must be the trailing argument; got {args:?}"
    );
}

/// Issue #1773 review — prefill newlines must be flattened on Windows
/// (`cmd.exe /c` end-of-command trap) but **preserved on macOS/Linux**
/// (direct spawn, argv elements preserve `\n`). The previous draft
/// flattened unconditionally inside the Cline adapter, destroying
/// multi-line prompts on Unix.
#[test]
fn prefill_newlines_are_flattended_on_windows_and_preserved_on_unix() {
    let adapter = &crate::agent::provider::adapters::CLINE as &dyn AgentProvider;
    let config = ResolvedAgentConfig::default();
    let prefill = "first\nsecond\r\nthird";

    let windows = adapter.prepare_launch(HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: Some(prefill),
        sandbox: false,
    });
    let windows_args: Vec<&str> = windows.recipe.argv().collect();
    assert_eq!(
        windows_args.last().copied(),
        Some("first second third"),
        "Windows prefill must be flattened to single space; got {windows_args:?}"
    );

    let linux = adapter.prepare_launch(HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Wsl,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: Some(prefill),
        sandbox: false,
    });
    let linux_args: Vec<&str> = linux.recipe.argv().collect();
    assert_eq!(
        linux_args.last().copied(),
        Some("first\nsecond\nthird"),
        "Unix prefill must preserve line structure (CRLF→LF only); got {linux_args:?}"
    );
}

/// Assign mode must forward the adapter's `session_assign_args`.
/// Anthropic uses the default `["--session-id", id]`.
#[test]
fn default_prepare_assign_mode_forwards_session_id() {
    let adapter = &crate::agent::provider::adapters::ANTHROPIC as &dyn AgentProvider;
    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Assign("abc-uuid"),
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
    let base = adapter.spawn_recipe(Platform::Windows, EnvType::Windows);
    // Issue #2151: the unattended permission default contributes its
    // flags at the front (where the base recipe used to carry them);
    // the session-assign pair follows. The flag content is literal
    // here (Anthropic's own unattended flag) so a wrong default is
    // caught at this seam too, not only in the adapter tests.
    let mut expected = vec!["--dangerously-skip-permissions".to_string()];
    expected.extend(base.base_args.clone());
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
        routing: None,
        node_id: None,
        platform: Platform::Macos,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Resume {
            id: "sess-xyz",
            cause: ResumeCause::Explicit,
        },
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
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
        routing: None,
        node_id: None,
        platform: Platform::Macos,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::Resume {
            id: "sess-xyz",
            cause: ResumeCause::Explicit,
        },
        config: &config,
        prefill: Some("continue"),
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
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
        routing: None,
        node_id: None,
        platform: Platform::Macos,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
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
        routing: None,
        node_id: None,
        platform: Platform::Macos,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
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
        routing: None,
        node_id: None,
        platform: Platform::Macos,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
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
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
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
        routing: None,
        node_id: None,
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
        adapter.prepare_launch(input)
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
        routing: None,
        node_id: None,
        platform: Platform::Macos,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = adapter.prepare_launch(input);
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

/// HarnessEnvironmentPolicy::default() has all defaults off and empty slices.
#[test]
fn environment_policy_none_is_inert() {
    let p = HarnessEnvironmentPolicy::default();
    assert!(!p.resets_backend_env);
    assert!(p.env_remove.is_empty());
    assert!(p.env_set.is_empty());
}

#[test]
fn commandcode_environment_identifies_as_vscode_to_skip_kitty_probe() {
    let config = ResolvedAgentConfig::default();
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Windows,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = crate::agent::provider::adapters::commandcode::COMMANDCODE.prepare_launch(input);
    assert_eq!(
        prepared.environment.env_set,
        vec![("TERM_PROGRAM".to_string(), "vscode".to_string())],
        "Command Code must set TERM_PROGRAM=vscode to skip the kitty probe; got {:?}",
        prepared.environment.env_set
    );
}

#[test]
fn initial_prompt_delivery_keeps_empty_prompts_fresh() {
    assert_eq!(
        initial_prompt_delivery("codex", "  \n\t"),
        InitialPromptDelivery::Fresh
    );
}

#[test]
fn initial_prompt_delivery_prefills_single_line_supported_prompts() {
    assert_eq!(
        initial_prompt_delivery("codex", "Review the PR"),
        InitialPromptDelivery::Prefill
    );
}

#[test]
fn claude_initial_prompts_submit_on_launch_without_an_enter_watcher() {
    for harness in ["anthropic", "claude", "anthropic:custom-provider"] {
        for prompt in ["Review the PR", "Review the diff\n+ added line"] {
            assert_eq!(
                initial_prompt_delivery(harness, prompt),
                InitialPromptDelivery::AutoSubmit
            );
        }
        assert_eq!(
            initial_prompt_delivery(harness, "  \n\t"),
            InitialPromptDelivery::Fresh
        );
    }
}

#[test]
fn initial_prompt_delivery_injects_codex_multiline_prompts_after_spawn() {
    assert_eq!(
        initial_prompt_delivery("codex", "Review the diff\n+ added line"),
        InitialPromptDelivery::InjectAfterSpawn
    );
}

#[test]
fn initial_prompt_delivery_injects_for_harnesses_without_prefill() {
    assert_eq!(
        initial_prompt_delivery("terminal", "Run the verification command"),
        InitialPromptDelivery::InjectAfterSpawn
    );
}
