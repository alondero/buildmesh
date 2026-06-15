//! Tests for `build_spawn_command` — the function that composes a provider's
//! spawn recipe with session-id mode, model/effort/prefill overrides, and the
//! runtime-environment wrapper into the final `CommandBuilder`.
//!
//! These tests call the real `build_spawn_command` and assert on the resulting
//! `CommandBuilder` (argv / cwd / env). They are NOT re-implementations of the
//! logic under test: every expectation is a literal value, so a mutation to the
//! composition (dropping `--session-id`, mis-ordering args, applying an override
//! to a provider that doesn't support it, forgetting the cwd/env) fails the suite.
//!
//! `env_type = Wsl` is used throughout because `spawn_environment::wrap`'s WSL
//! branch is host-independent (`wsl.exe --cd <path> -- <binary> <args...>`),
//! keeping these assertions deterministic regardless of where `cargo test` runs.
//! The only host-dependent input is the provider *recipe* (binary + base flag),
//! which differs on macOS — handled via [`anthropic_recipe`].
//!
//! Run with: cd src-tauri && cargo test

#[cfg(test)]
mod tests {
    use crate::agent::spawn::{build_spawn_command, SessionIdMode};
    use crate::env::ResolvedPath;
    use crate::models::{EnvType, Provider};

    const SPAWN_PATH: &str = "/home/user/repo/.claude/worktrees/wt-1";
    const SESSION_ID: i64 = 42;

    fn wsl_resolved() -> ResolvedPath {
        ResolvedPath {
            host_path: SPAWN_PATH.to_string(),
            spawn_path: SPAWN_PATH.to_string(),
            raw_path: SPAWN_PATH.to_string(),
            env_type: EnvType::Wsl,
        }
    }

    /// Windows-native (non-WSL) resolution — the path where cwrap providers are
    /// launched through `cwrap.cmd` → cmd.exe and a multi-line `--prefill` argv
    /// would be truncated at the first newline.
    fn windows_resolved() -> ResolvedPath {
        ResolvedPath {
            host_path: SPAWN_PATH.to_string(),
            spawn_path: SPAWN_PATH.to_string(),
            raw_path: SPAWN_PATH.to_string(),
            env_type: EnvType::Windows,
        }
    }

    /// Collect a CommandBuilder's argv as plain strings.
    fn argv(cmd: &portable_pty::CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    /// Read a single env var the spawn command will set on the child process.
    fn env_of(cmd: &portable_pty::CommandBuilder, key: &str) -> Option<String> {
        cmd.get_env(key).map(|v| v.to_string_lossy().into_owned())
    }

    /// The Anthropic recipe is the one piece of expected data that varies by host:
    /// macOS spawns `claude --dangerously-skip-permissions`, everywhere else
    /// `cwrap --anthropic`. Returned as literal test data, not derived from code under test.
    fn anthropic_recipe() -> (&'static str, &'static str) {
        if cfg!(target_os = "macos") {
            ("claude", "--dangerously-skip-permissions")
        } else {
            ("cwrap", "--anthropic")
        }
    }

    /// Build the expected WSL-wrapped argv: `wsl.exe --cd <path> -- <binary> <inner...>`.
    fn expected_wsl(binary: &str, inner: &[&str]) -> Vec<String> {
        let mut v = vec![
            "wsl.exe".to_string(),
            "--cd".to_string(),
            SPAWN_PATH.to_string(),
            "--".to_string(),
            binary.to_string(),
        ];
        v.extend(inner.iter().map(|s| s.to_string()));
        v
    }

    /// Assigning a fresh session id appends `--session-id <uuid>` after the
    /// provider's base flag, and the whole thing is wrapped for WSL.
    #[test]
    fn anthropic_assign_builds_full_wsl_command() {
        let (binary, flag) = anthropic_recipe();
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Anthropic,
            &SessionIdMode::Assign("uuid-assign".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(binary, &[flag, "--session-id", "uuid-assign"])
        );
    }

    /// Resuming appends `--resume <id>` (and never `--session-id`).
    #[test]
    fn anthropic_resume_appends_resume_args() {
        let (binary, flag) = anthropic_recipe();
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Anthropic,
            &SessionIdMode::Resume("uuid-resume".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        let args = argv(&cmd);
        assert_eq!(args, expected_wsl(binary, &[flag, "--resume", "uuid-resume"]));
        assert!(
            !args.iter().any(|a| a == "--session-id"),
            "resume must not pass --session-id: {:?}",
            args
        );
    }

    /// Minimax is a cwrap provider on every host, so its recipe is stable:
    /// `cwrap --minimax --session-id <uuid>`.
    #[test]
    fn minimax_assign_builds_cwrap_command() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::Assign("mm-1".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("cwrap", &["--minimax", "--session-id", "mm-1"])
        );
    }

    /// Kimi is a cwrap provider on every host, so its recipe is stable:
    /// `cwrap --kimi --session-id <uuid>`.
    #[test]
    fn kimi_assign_builds_cwrap_command() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Kimi,
            &SessionIdMode::Assign("ki-1".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("cwrap", &["--kimi", "--session-id", "ki-1"])
        );
    }

    /// Model + effort overrides are appended (in that order) for a provider that
    /// declares `supports_model_override()`.
    #[test]
    fn model_and_effort_overrides_appended_for_supporting_provider() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::Assign("mm-2".to_string()),
            SESSION_ID,
            Some("opus"),
            Some("high"),
            None,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "cwrap",
                &[
                    "--minimax",
                    "--session-id",
                    "mm-2",
                    "--model",
                    "opus",
                    "--effort",
                    "high",
                ]
            )
        );
    }

    /// Prefill text is appended as `--prefill <text>` for a supporting provider.
    #[test]
    fn prefill_appended_for_supporting_provider() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            Some("hello world"),
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("cwrap", &["--minimax", "--prefill", "hello world"])
        );
    }

    /// Prefill CRLF (and bare CR) are normalised to LF before reaching the provider.
    ///
    /// Regression for: handover prefills containing Windows line endings only
    /// pre-filled the first line in the agent's TUI. A bare `\r` typed into
    /// cwrap → ConPTY submits the prompt after line one. The argv the spawn
    /// command builds must contain LF-only prefill text.
    ///
    /// (The GitHub-issue spawn path no longer ships the issue body — just a
    /// short URL-bearing instruction — but selected text in the handover flow
    /// can still carry CRLF, so this guarantee is still meaningful.)
    #[test]
    fn prefill_crlf_normalised_to_lf() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            Some("Title\r\n\r\nLine 1\r\nLine 2\rLine 3"),
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "cwrap",
                &["--minimax", "--prefill", "Title\n\nLine 1\nLine 2\nLine 3"]
            )
        );
    }

    /// On the Windows-native cwrap path, multi-line prefill is delivered through
    /// the `BUILDMESH_PREFILL` environment variable — NOT as a `--prefill` CLI
    /// arg. The `cwrap.cmd` → cmd.exe launcher truncates a multi-line argv at the
    /// first newline (cmd.exe's command line is line-oriented), so an agent
    /// seeded with a handover/issue body would only ever see its first line. The
    /// environment block is inherited intact by every shell layer, so cwrap reads
    /// the full text from `$BUILDMESH_PREFILL` and forwards it to `claude`.
    ///
    /// Also asserts CRLF / bare-CR normalisation still applies to the env value.
    #[test]
    fn prefill_for_cwrap_goes_via_env_on_windows_native() {
        let cmd = build_spawn_command(
            &windows_resolved(),
            Provider::Minimax,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            Some("Title\r\n\r\nLine 1\r\nLine 2\rLine 3"),
        );

        assert_eq!(
            env_of(&cmd, "BUILDMESH_PREFILL").as_deref(),
            Some("Title\n\nLine 1\nLine 2\nLine 3"),
            "multi-line prefill must be delivered via the environment on the cwrap Windows path"
        );

        let args = argv(&cmd);
        assert!(
            !args.iter().any(|a| a == "--prefill"),
            "prefill must NOT be passed as a CLI flag on the cwrap Windows path (cmd.exe truncates it): {:?}",
            args
        );
    }

    /// WSL keeps the `--prefill` CLI arg: `wsl.exe` passes a multi-line argv
    /// through intact (no cmd.exe in the chain), and a Windows env var does not
    /// cross into the WSL environment without `WSLENV`. So the env transport must
    /// NOT engage for WSL spawns.
    #[test]
    fn prefill_stays_argv_for_wsl() {
        // portable_pty::CommandBuilder inherits the parent process env, so
        // a BUILDMESH_PREFILL leaked into the test runner's shell — e.g. by
        // a Claude Code attention-hook spawn (which is how the orchestrating
        // agent sets *its* prefill) — would make `get_env` return Some(...)
        // and fail the "must not set" assertion below. Clear it so the test
        // is hermetic regardless of the runner's env. Safe to scope: no
        // other test in this module reads BUILDMESH_PREFILL, and the
        // sibling Windows test always sets the value via `cmd.env()`, so a
        // clear here doesn't change its observed outcome.
        unsafe { std::env::remove_var("BUILDMESH_PREFILL"); }

        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            Some("hello world"),
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("cwrap", &["--minimax", "--prefill", "hello world"])
        );
        assert!(
            env_of(&cmd, "BUILDMESH_PREFILL").is_none(),
            "the WSL path must not set the prefill env var"
        );
    }

    /// Empty override/prefill strings are treated as absent — no flags emitted.
    #[test]
    fn empty_overrides_are_ignored() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::None,
            SESSION_ID,
            Some(""),
            Some(""),
            Some(""),
        );

        assert_eq!(argv(&cmd), expected_wsl("cwrap", &["--minimax"]));
    }

    /// Agy applies model and prefill overrides when passed.
    #[test]
    fn agy_applies_model_override_and_prefill() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Agy,
            &SessionIdMode::None,
            SESSION_ID,
            Some("opus"),
            Some("high"),
            Some("prefill text"),
        );

        let args = argv(&cmd);
        assert_eq!(
            args,
            expected_wsl(
                "agy",
                &[
                    "--dangerously-skip-permissions",
                    "--model",
                    "opus",
                    "--effort",
                    "high",
                    "--prompt-interactive",
                    "prefill text"
                ]
            )
        );
    }

    /// Agy self-assigns session IDs — Assign mode must NOT inject `--session-id`.
    #[test]
    fn agy_assign_omits_session_flag() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Agy,
            &SessionIdMode::Assign("ignored".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        let args = argv(&cmd);
        assert_eq!(args, expected_wsl("agy", &["--dangerously-skip-permissions"]));
        assert!(
            !args.iter().any(|a| a == "--session-id" || a == "ignored"),
            "agy self-assigns; Assign must not add --session-id: {:?}",
            args
        );
    }

    /// Codex provides a dedicated resume recipe (`codex resume <id> ...flags`)
    /// via `spawn_recipe_for_resume`, which `build_spawn_command` must use
    /// instead of the default `spawn_recipe` + `--resume`.
    #[test]
    fn codex_resume_uses_resume_recipe() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Codex,
            &SessionIdMode::Resume("codex-sess".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "codex",
                &[
                    "resume",
                    "codex-sess",
                    "--ask-for-approval",
                    "never",
                    "--sandbox",
                    "danger-full-access",
                ]
            )
        );
    }

    /// Codex self-assigns its session id, so `session_assign_args` is empty:
    /// Assign mode must NOT inject `--session-id`.
    #[test]
    fn codex_assign_omits_session_flag() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Codex,
            &SessionIdMode::Assign("ignored".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        let args = argv(&cmd);
        assert_eq!(
            args,
            expected_wsl(
                "codex",
                &[
                    "--ask-for-approval",
                    "never",
                    "--sandbox",
                    "danger-full-access",
                ]
            )
        );
        assert!(
            !args.iter().any(|a| a == "--session-id" || a == "ignored"),
            "codex self-assigns; Assign must not add --session-id: {:?}",
            args
        );
    }

    /// Spawning from a GitHub issue prefills the agent with a short URL-bearing
    /// instruction, NOT the full issue body. The body used to be concatenated in
    /// (title + "\n\n" + body) which forced megabytes of markdown through the
    /// Windows PowerShell -EncodedCommand path (see memory: powershell-encoding-fix).
    /// The URL is the canonical source the agent fetches on demand and the cite
    /// it uses when opening the PR that closes the issue.
    #[test]
    fn issue_prefill_is_url_with_title_hint_not_body() {
        let prefill = crate::commands::agent::format_issue_prefill(
            "alondero",
            "buildmesh",
            123,
            "Add dark mode to settings",
        );

        assert_eq!(
            prefill,
            "Please work on GitHub issue #123 — Add dark mode to settings\n\
             https://github.com/alondero/buildmesh/issues/123"
        );
    }

    /// An empty title falls back to a number-only imperative — no dangling
    /// em-dash artifact.
    #[test]
    fn issue_prefill_with_empty_title_falls_back_to_number_only() {
        let prefill = crate::commands::agent::format_issue_prefill(
            "alondero",
            "buildmesh",
            7,
            "",
        );

        assert_eq!(
            prefill,
            "Please work on GitHub issue #7\n\
             https://github.com/alondero/buildmesh/issues/7"
        );
    }

    /// Titles with double quotes pass through verbatim — the prefill format
    /// uses an em-dash separator rather than surrounding quotes, so there is
    /// nothing to escape. The consumer is an LLM, not a parser; ensuring
    /// `\"` doesn't leak into the prompt is the explicit goal here.
    #[test]
    fn issue_prefill_preserves_quotes_in_title_verbatim() {
        let prefill = crate::commands::agent::format_issue_prefill(
            "alondero",
            "buildmesh",
            42,
            "Fix the \"weird\" race in spawn",
        );

        assert_eq!(
            prefill,
            "Please work on GitHub issue #42 — Fix the \"weird\" race in spawn\n\
             https://github.com/alondero/buildmesh/issues/42"
        );
        assert!(
            !prefill.contains('\\'),
            "title must reach the LLM without backslash escapes: {:?}",
            prefill
        );
    }

    /// On a Windows-native host a cwrap provider is launched through
    /// `powershell.exe -NoLogo -NoProfile -EncodedCommand <base64>`. The
    /// `-NoProfile` flag is load-bearing for spawn latency: without it every
    /// agent spawn first runs the user's PowerShell profile (modules, prompt
    /// frameworks) before cwrap, adding hundreds of ms per node. This shell
    /// only relays ANSI output, so the profile is dead weight.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn windows_powershell_launcher_uses_no_profile() {
        let cmd = build_spawn_command(
            &windows_resolved(),
            Provider::Anthropic,
            &SessionIdMode::Assign("ps-1".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        let args = argv(&cmd);
        assert_eq!(
            &args[..4],
            &[
                "powershell.exe".to_string(),
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-EncodedCommand".to_string(),
            ],
            "cwrap PowerShell launcher must pass -NoProfile to skip the user profile: {:?}",
            args
        );
        assert_eq!(args.len(), 5, "expected the Base64 payload as the 5th arg: {:?}", args);
    }

    /// The wrapper sets the spawn cwd and the BUILDMESH_SESSION_ID / BUILDMESH_PORT
    /// env vars that the agent and its hooks rely on.
    #[test]
    fn sets_cwd_and_buildmesh_env() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::Assign("mm-env".to_string()),
            SESSION_ID,
            None,
            None,
            None,
        );

        assert_eq!(
            cmd.get_cwd().map(|c| c.to_string_lossy().into_owned()),
            Some(SPAWN_PATH.to_string())
        );
        assert_eq!(
            cmd.get_env("BUILDMESH_SESSION_ID")
                .map(|v| v.to_string_lossy().into_owned()),
            Some(SESSION_ID.to_string())
        );
        assert_eq!(
            cmd.get_env("BUILDMESH_PORT")
                .map(|v| v.to_string_lossy().into_owned()),
            Some(crate::http_server::current_http_port().to_string())
        );
    }
}
