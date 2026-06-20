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

    /// The Anthropic recipe binary and arg inside WSL.
    fn anthropic_recipe() -> (&'static str, &'static str) {
        ("claude", "--dangerously-skip-permissions")
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
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(binary, &[flag, "--session-id", "uuid-assign"])
        );
    }

    /// The macOS Seatbelt `sandbox` flag (issue #497) is consumed only by
    /// `spawn_environment::wrap`'s macOS branch. On the WSL path it must be a
    /// no-op: turning it on must not leak `sandbox-exec` or any extra token into
    /// the command. (The macOS `sandbox-exec` assembly itself is unit-tested in
    /// `agent::sandbox`, which runs on every host since it isn't cfg-gated.)
    #[test]
    fn sandbox_flag_is_ignored_on_wsl_path() {
        let (binary, flag) = anthropic_recipe();
        let sandboxed = build_spawn_command(
            &wsl_resolved(),
            Provider::Anthropic,
            &SessionIdMode::Assign("uuid-assign".to_string()),
            SESSION_ID,
            None,
            None,
            None,
            true,
        );
        assert_eq!(
            argv(&sandboxed),
            expected_wsl(binary, &[flag, "--session-id", "uuid-assign"]),
            "sandbox=true must not alter the WSL-wrapped command"
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
            false,
        );

        let args = argv(&cmd);
        assert_eq!(args, expected_wsl(binary, &[flag, "--resume", "uuid-resume"]));
        assert!(
            !args.iter().any(|a| a == "--session-id"),
            "resume must not pass --session-id: {:?}",
            args
        );
    }

    /// Minimax is a claude-backed provider on every host, so it builds a direct
    /// claude command.
    #[test]
    fn minimax_assign_builds_claude_command() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::Assign("mm-1".to_string()),
            SESSION_ID,
            None,
            None,
            None,
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("claude", &["--dangerously-skip-permissions", "--session-id", "mm-1"])
        );
    }

    /// Kimi is a claude-backed provider on every host, so it builds a direct
    /// claude command.
    #[test]
    fn kimi_assign_builds_claude_command() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Kimi,
            &SessionIdMode::Assign("ki-1".to_string()),
            SESSION_ID,
            None,
            None,
            None,
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("claude", &["--dangerously-skip-permissions", "--session-id", "ki-1"])
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
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "claude",
                &[
                    "--dangerously-skip-permissions",
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
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("claude", &["--dangerously-skip-permissions", "--prefill", "hello world"])
        );
    }

    /// Prefill CRLF (and bare CR) are normalised to LF before reaching the provider.
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
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "claude",
                &["--dangerously-skip-permissions", "--prefill", "Title\n\nLine 1\nLine 2\nLine 3"]
            )
        );
    }

    // -----------------------------------------------------------------------
    // Windows direct execution (no cwrap, no bash): cwrap-backed providers reach
    // claude.exe directly. These pin the direct composition. Windows-only:
    // the direct recipe is gated on a Windows host and the cwrap recipe is retired.
    // (GH: absorb cwrap into buildmesh.)
    // -----------------------------------------------------------------------

    /// Anthropic on the Windows-native path spawns `claude.exe` directly.
    #[cfg(target_os = "windows")]
    #[test]
    fn anthropic_spawns_claude_exe_directly() {
        let cmd = build_spawn_command(
            &windows_resolved(),
            Provider::Anthropic,
            &SessionIdMode::Assign("uuid-assign".to_string()),
            SESSION_ID,
            None,
            None,
            None,
            false,
        );

        assert_eq!(
            argv(&cmd),
            vec![
                "claude.exe".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--session-id".to_string(),
                "uuid-assign".to_string(),
            ],
            "Anthropic must spawn claude.exe directly"
        );
    }

    /// Multi-line prefill survives as a normal argv — no env-var transport engages.
    ///
    /// `portable_pty::CommandBuilder` inherits the parent env, so a
    /// `BUILDMESH_PREFILL` leaked into the test runner's shell would make
    /// `get_env` return `Some(...)` and fail the "must not set" assertion
    /// below. Clear it so the test is hermetic regardless of the runner's
    /// env (mirrors the WSL sibling test).
    #[cfg(target_os = "windows")]
    #[test]
    fn anthropic_prefill_goes_argv_not_env() {
        unsafe { std::env::remove_var("BUILDMESH_PREFILL"); }

        let cmd = build_spawn_command(
            &windows_resolved(),
            Provider::Anthropic,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            Some("Title\r\n\r\nLine 1\rLine 2"),
            false,
        );

        let args = argv(&cmd);
        let pos = args.iter().position(|a| a == "--prefill").expect("--prefill present in argv");
        assert_eq!(
            args.get(pos + 1).map(String::as_str),
            Some("Title\n\nLine 1\nLine 2"),
            "full multi-line prefill must ride on argv (native claude.exe, no cmd.exe to truncate it): {:?}",
            args
        );
        assert!(
            env_of(&cmd, "BUILDMESH_PREFILL").is_none(),
            "must NOT use the env-var prefill transport: {:?}",
            args
        );
        assert_eq!(args.first().map(String::as_str), Some("claude.exe"));
    }

    /// MiniMax spawns claude.exe directly and injects the backend env
    /// cwrap would have exported.
    #[cfg(target_os = "windows")]
    #[test]
    fn minimax_spawns_claude_directly_with_backend_env() {
        let cmd = build_spawn_command(
            &windows_resolved(),
            Provider::Minimax,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            None,
            false,
        );

        let args = argv(&cmd);
        assert_eq!(args.first().map(String::as_str), Some("claude.exe"));
        assert_eq!(
            env_of(&cmd, "ANTHROPIC_MODEL").as_deref(),
            Some("MiniMax-M3[1m]"),
            "MiniMax backend model env must be injected for the bash-free path"
        );
        assert!(
            env_of(&cmd, "ANTHROPIC_BASE_URL").is_some(),
            "MiniMax backend base URL env must be injected"
        );
    }

    /// Regression guard: even with sandbox OFF, the Windows-native Anthropic spawn
    /// now goes to claude.exe directly instead of PowerShell/cwrap.
    #[cfg(target_os = "windows")]
    #[test]
    fn anthropic_unsandboxed_windows_spawns_claude_exe_directly() {
        let cmd = build_spawn_command(
            &windows_resolved(),
            Provider::Anthropic,
            &SessionIdMode::Assign("uuid".to_string()),
            SESSION_ID,
            None,
            None,
            None,
            false,
        );
        assert_eq!(
            argv(&cmd).first().map(String::as_str),
            Some("claude.exe"),
            "unsandboxed Windows path must spawn claude.exe directly"
        );
    }

    /// cwrap `unset` the claude backend env vars before `exec claude`, so a value
    /// inherited from the launching shell never reached the agent. The direct
    /// spawn must reproduce that clean slate: an `ANTHROPIC_*` value present in
    /// buildmesh's own process environment must NOT leak into a claude-backed
    /// agent. Anthropic exports nothing of its own, so the reset is the whole job
    /// — the inherited value must be cleared, not passed through.
    ///
    /// Native (non-WSL) path only: a `claude.exe` child inherits buildmesh's full
    /// environment block, whereas a WSL child only receives vars bridged via
    /// `WSLENV`. So this leak is observable only on the direct-inherit path.
    #[cfg(target_os = "windows")]
    #[test]
    fn anthropic_clears_inherited_backend_env() {
        // Simulate buildmesh launched from a shell that already exported a
        // provider override (e.g. a developer who ran `cwrap --minimax` in the
        // same terminal before starting the app).
        unsafe { std::env::set_var("ANTHROPIC_BASE_URL", "https://leaked.example/anthropic"); }
        let cmd = build_spawn_command(
            &windows_resolved(),
            Provider::Anthropic,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            None,
            false,
        );
        let leaked = env_of(&cmd, "ANTHROPIC_BASE_URL");
        unsafe { std::env::remove_var("ANTHROPIC_BASE_URL"); }
        assert_eq!(
            leaked, None,
            "inherited ANTHROPIC_BASE_URL must be cleared for the Anthropic spawn (cwrap `unset` parity), not leaked: {:?}",
            leaked
        );
    }

    /// WSL keeps the `--prefill` CLI arg: `wsl.exe` passes a multi-line argv
    /// through intact (no cmd.exe in the chain), and a Windows env var does not
    /// cross into the WSL environment without `WSLENV`. So the env transport must
    /// NOT engage for WSL spawns.
    #[test]
    fn prefill_stays_argv_for_wsl() {
        unsafe { std::env::remove_var("BUILDMESH_PREFILL"); }

        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Minimax,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            Some("hello world"),
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("claude", &["--dangerously-skip-permissions", "--prefill", "hello world"])
        );
        assert!(
            env_of(&cmd, "BUILDMESH_PREFILL").is_none(),
            "the WSL path must not set the prefill env var"
        );
        // MiniMax env keys must be propagated to WSLENV
        assert!(
            env_of(&cmd, "WSLENV").is_some(),
            "WSLENV must be set to propagate MiniMax environment variables"
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
            false,
        );

        assert_eq!(argv(&cmd), expected_wsl("claude", &["--dangerously-skip-permissions"]));
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
            false,
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
            false,
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
            false,
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
            false,
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

    // ----- format_pr_prefill (issue #420) ---------------------------------

    /// `format_pr_prefill` mirrors `format_issue_prefill` (the issue-spawn
    /// flow): one-line imperative + the canonical URL. The body is
    /// intentionally NOT included — multi-KB markdown is the worst case for
    /// the PowerShell `-EncodedCommand` argv path.
    #[test]
    fn format_pr_prefill_includes_number_title_and_url() {
        let prefill = crate::commands::agent::format_pr_prefill(
            "alondero",
            "buildmesh",
            420,
            "spawn on PR",
        );
        assert_eq!(
            prefill,
            "Please review pull request #420 — spawn on PR\n\
             https://github.com/alondero/buildmesh/pull/420"
        );
    }

    /// Empty / whitespace-only title degrades to the bare `#N\n<url>` form
    /// so the prefill never carries a dangling em-dash artifact. Mirrors
    /// the issue-spawn behaviour so both spawn paths produce uniform output.
    #[test]
    fn format_pr_prefill_handles_empty_title() {
        let prefill = crate::commands::agent::format_pr_prefill("alondero", "buildmesh", 420, "");
        assert_eq!(
            prefill,
            "Please review pull request #420\n\
             https://github.com/alondero/buildmesh/pull/420"
        );
        let prefill_ws = crate::commands::agent::format_pr_prefill(
            "alondero",
            "buildmesh",
            420,
            "   \t  ",
        );
        assert_eq!(prefill_ws, prefill, "whitespace title trims like empty");
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
            Provider::Codex,
            &SessionIdMode::Resume("ps-1".to_string()),
            SESSION_ID,
            None,
            None,
            None,
            false,
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
            "Codex PowerShell launcher must pass -NoProfile to skip the user profile: {:?}",
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
            false,
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

    // ----- validate_pr_spawn_inputs (issue #471) -------------------------
    //
    // Regression: the previous fork-info gate in `create_pr_node` had two
    // guards whose composition let a malformed request through. Guard 1
    // (XOR on `head_repo_owner` / `head_repo_clone_url` Some-ness) checked
    // fork-info completeness; guard 2 rejected `head_ref=""` ONLY when
    // `head_repo_owner` was None — meaning a request with an empty
    // `head_ref` but populated fork info skipped both guards and stage-2
    // (`spawn_agent_inner`) tried to check out a branch named "".
    //
    // `validate_pr_spawn_inputs` splits the gate into two independent
    // rejections:
    //   1. fork info must be both-present or both-absent (XOR),
    //   2. `head_ref` must be non-empty (unconditional).
    //
    // Truth table (× = accept, ✗ = reject):
    //
    //   head_ref  owner  url      |  before  |  after (#471)
    //   -----------------------------------------------
    //   "feat/x"  None   None    |    ×     |    ×
    //   "feat/x"  Some   None    |    ✗     |    ✗
    //   "feat/x"  None   Some    |    ✗     |    ✗
    //   "feat/x"  Some   Some    |    ×     |    ×
    //   ""        None   None    |    ✗     |    ✗
    //   ""        Some   None    |    ✗     |    ✗
    //   ""        None   Some    |    ✗     |    ✗
    //   ""        Some   Some    |  **×**   |  **✗**   ← the #471 fix

    fn run_gate(
        head_ref: &str,
        owner: Option<&str>,
        url: Option<&str>,
    ) -> Result<(String, Option<String>, Option<String>), String> {
        crate::commands::agent::validate_pr_spawn_inputs(
            head_ref,
            owner.map(str::to_string),
            url.map(str::to_string),
        )
    }

    /// Regression for issue #471. The previous fork-info gate let
    /// `head_ref=""` through when fork info was populated (a stale
    /// `head` object from a previously-rendered fork row). After the
    /// fix, this is rejected outright because stage-2 cannot check out
    /// a branch named "".
    #[test]
    fn empty_head_ref_with_populated_fork_info_is_rejected() {
        let result = run_gate(
            "",
            Some("alice"),
            Some("https://github.com/alice/buildmesh.git"),
        );
        let err = result.expect_err(
            "head_ref=\"\" with fork info populated must be rejected \
             (issue #471: previous gate let this through to stage-2)",
        );
        assert!(
            err.contains("head_ref") && err.to_lowercase().contains("required"),
            "error must clearly name the missing head_ref, got: {:?}",
            err
        );
    }

    /// Whitespace-only `head_ref` is the same class of malformed input —
    /// the trim happens at the gate, not at the service layer.
    #[test]
    fn whitespace_only_head_ref_with_populated_fork_info_is_rejected() {
        let result = run_gate(
            "   \t  ",
            Some("alice"),
            Some("https://github.com/alice/buildmesh.git"),
        );
        assert!(
            result.is_err(),
            "whitespace-only head_ref must be rejected like an empty one",
        );
    }

    /// Existing behaviour preserved: same-repo PR with an empty
    /// `head_ref` is still rejected.
    #[test]
    fn empty_head_ref_with_no_fork_info_is_rejected() {
        let result = run_gate("", None, None);
        assert!(result.is_err(), "existing rejection must be preserved");
    }

    /// Same-repo PR (the common case) is accepted.
    #[test]
    fn same_repo_pr_with_head_ref_is_accepted() {
        let (head_ref, owner, url) =
            run_gate("feat/x", None, None).expect("same-repo PR with head_ref must be accepted");
        assert_eq!(head_ref, "feat/x");
        assert_eq!(owner, None);
        assert_eq!(url, None);
    }

    /// Fork PR with both fields populated is accepted.
    #[test]
    fn fork_pr_with_head_ref_and_full_fork_info_is_accepted() {
        let (head_ref, owner, url) = run_gate(
            "feat/x",
            Some("alice"),
            Some("https://github.com/alice/buildmesh.git"),
        )
        .expect("fork PR with head_ref and full fork info must be accepted");
        assert_eq!(head_ref, "feat/x");
        assert_eq!(owner.as_deref(), Some("alice"));
        assert_eq!(url.as_deref(), Some("https://github.com/alice/buildmesh.git"));
    }

    /// Fork-info completeness gate: only one of the two fields is an
    /// unfixable request — we need the clone URL to register the remote
    /// and the owner login for the remote alias. Same behaviour on both
    /// sides of the XOR.
    #[test]
    fn fork_info_only_owner_without_url_is_rejected() {
        let err = run_gate("feat/x", Some("alice"), None)
            .expect_err("owner without clone_url must be rejected");
        assert!(
            err.contains("fork info is incomplete"),
            "error must clearly name the fork-info completeness gate: {:?}",
            err
        );
    }

    #[test]
    fn fork_info_only_url_without_owner_is_rejected() {
        let err = run_gate("feat/x", None, Some("https://github.com/alice/x.git"))
            .expect_err("clone_url without owner must be rejected");
        assert!(
            err.contains("fork info is incomplete"),
            "error must clearly name the fork-info completeness gate: {:?}",
            err
        );
    }

    /// Surrounding whitespace is trimmed from the fork-info strings so
    /// `Some(" alice ")` and `Some("alice")` reach the service layer as
    /// identical values. (The empty-after-trim case collapses to `None`,
    /// matching the behaviour at the existing trim site.)
    #[test]
    fn fork_info_strings_are_trimmed() {
        let (head_ref, owner, url) = run_gate(
            "feat/x",
            Some("  alice  "),
            Some("  https://github.com/alice/x.git  "),
        )
        .expect("trimmed fork info must be accepted");
        assert_eq!(head_ref, "feat/x");
        assert_eq!(owner.as_deref(), Some("alice"));
        assert_eq!(url.as_deref(), Some("https://github.com/alice/x.git"));
    }

    /// A whitespace-only fork-info value collapses to `None` so it doesn't
    /// produce an `Some("")` for the service layer. This matches the
    /// pre-existing behaviour of the inline `filter(|s| !s.is_empty())`.
    #[test]
    fn whitespace_only_fork_info_collapses_to_none() {
        let (head_ref, owner, url) = run_gate("feat/x", Some("   "), None)
            .expect("whitespace-only owner collapses to None (no fork info)");
        assert_eq!(head_ref, "feat/x");
        assert_eq!(owner, None);
        assert_eq!(url, None);
    }

    /// `head_ref` is trimmed before being returned so a whitespace-padded
    /// branch name (e.g. an upstream payload that wrapped `head.ref` in
    /// spaces) lands on `node.branch` as the bare ref. Without this, the
    /// gate's empty-check passes on the trimmed value but `node.branch`
    /// keeps the padding, and stage-2's `git fetch origin <padded>`
    /// fails to match the real ref.
    #[test]
    fn padded_head_ref_is_trimmed() {
        let (head_ref, owner, url) = run_gate("  feat/x  ", None, None)
            .expect("padded head_ref must be accepted (trimmed, not rejected)");
        assert_eq!(
            head_ref, "feat/x",
            "padded head_ref must be trimmed before reaching the service layer"
        );
        assert_eq!(owner, None);
        assert_eq!(url, None);
    }
}
