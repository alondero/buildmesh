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
            env_type: EnvType::Wsl,
        }
    }

    /// Collect a CommandBuilder's argv as plain strings.
    fn argv(cmd: &portable_pty::CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
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

    /// Agy self-assigns session IDs and ignores model/prefill overrides:
    /// even when a caller passes those, the args must NOT appear.
    /// Guards against a mutation that drops the capability gating.
    #[test]
    fn agy_ignores_model_override_and_prefill() {
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
        assert_eq!(args, expected_wsl("agy", &["--dangerously-skip-permissions"]));
        for forbidden in ["--model", "--effort", "--prefill", "--session-id", "--resume", "opus", "high", "prefill text"] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "agy should not emit {:?}, got {:?}",
                forbidden,
                args
            );
        }
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
