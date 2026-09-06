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
    use crate::agent::launch_routing::PreparedLaunchRouting;
    use crate::agent::spawn::{
        build_spawn_command, build_spawn_command_prepared, open_pty_pair, spawn_child,
        SessionIdMode,
    };
    use crate::env::ResolvedPath;
    use crate::models::{EnvType, Provider};
    use crate::preferences::{PairingVerification, PairingVerificationStatus};

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

    fn codex_proxy(profile_name: &str, credential: &str) -> PreparedLaunchRouting {
        let descriptor = crate::agent::provider::compatibility::EndpointModelDescriptor {
            provider_id: "minimax".into(),
            endpoint: "https://api.minimax.io/v1".into(),
            wire_api: crate::agent::provider::compatibility::WireApi::Responses,
            model_id: "MiniMax-M3".into(),
            capabilities: crate::agent::provider::compatibility::complete_agent_capabilities(),
            auth_modes: vec![crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv],
            context_window: None,
            reasoning_effort: None,
        };
        PreparedLaunchRouting::CodexProxy {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            profile_name: profile_name.into(),
            verification: PairingVerification {
                harness_id: "codex".into(),
                provider_id: "minimax".into(),
                pairing_signature: "test-signature".into(),
                endpoint: descriptor.endpoint.clone(),
                model_id: descriptor.model_id.clone(),
                auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
                runtime: "wsl:Ubuntu:/home/user/.codex".into(),
                executable: "/usr/bin/codex".into(),
                codex_version: "0.144.0".into(),
                capability_result: crate::agent::provider::compatibility::CompatibilityDecision {
                    compatible: true,
                    reason: None,
                },
                status: PairingVerificationStatus::Verified,
                verified_at: Some(chrono::Utc::now()),
                reason: None,
            },
            descriptor,
            runtime: EnvType::Wsl,
            install: crate::agent::provider::adapters::codex::CodexInstall {
                executable: "/usr/bin/codex".into(),
                version: "0.144.0".into(),
                runtime_identity: "wsl:Ubuntu:/home/user/.codex".into(),
                codex_home: "/home/user/.codex".into(),
                wsl_distro: Some("Ubuntu".into()),
            },
            credential_reference: "BUILDMESH_CODEX_PROVIDER_KEY".into(),
            credential: credential.into(),
        }
    }

    fn compile_fake_codex(temp: &tempfile::TempDir) -> std::path::PathBuf {
        let source = temp.path().join("fake_codex.rs");
        std::fs::write(
            &source,
            r#"fn main() {
    let log = std::env::var_os("FAKE_CODEX_LOG").expect("FAKE_CODEX_LOG");
    let args = std::env::args().skip(1).collect::<Vec<_>>().join("\n");
    let credential = std::env::var("BUILDMESH_CODEX_PROVIDER_KEY").unwrap_or_default();
    let inherited_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let inherited_url = std::env::var("OPENAI_BASE_URL").unwrap_or_default();
    std::fs::write(log, format!("ARGS\n{args}\nCREDENTIAL\n{credential}\nOPENAI_KEY\n{inherited_key}\nOPENAI_URL\n{inherited_url}\n")).unwrap();
}"#,
        )
        .unwrap();
        let executable = temp.path().join(if cfg!(windows) {
            "fake-codex.exe"
        } else {
            "fake-codex"
        });
        let status = std::process::Command::new("rustc")
            .args([
                source.as_os_str(),
                std::ffi::OsStr::new("-o"),
                executable.as_os_str(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        executable
    }

    fn native_fake_proxy(executable: &std::path::Path, credential: &str) -> PreparedLaunchRouting {
        let mut routing = codex_proxy("buildmesh_fake", credential);
        let PreparedLaunchRouting::CodexProxy {
            verification,
            runtime,
            install,
            ..
        } = &mut routing
        else {
            unreachable!()
        };
        let executable = executable.to_string_lossy().into_owned();
        verification.runtime = "native-test".into();
        verification.executable = executable.clone();
        *runtime = EnvType::Windows;
        *install = crate::agent::provider::adapters::codex::CodexInstall {
            executable,
            version: "0.147.0".into(),
            runtime_identity: "native-test".into(),
            codex_home: temp_codex_home_for_test(),
            wsl_distro: None,
        };
        routing
    }

    fn temp_codex_home_for_test() -> String {
        std::env::temp_dir()
            .join("buildmesh-unused-fake-codex-home")
            .to_string_lossy()
            .into_owned()
    }

    /// Windows-native (non-WSL) resolution — the path where claude-backed
    /// providers (Anthropic, MiniMax) launch `claude.exe` directly through
    /// ConPTY. Pre-#531, a multi-line `--prefill` argv would have been
    /// truncated at the first newline by the `cwrap.cmd` → cmd.exe chain;
    /// post-#531 the argv goes straight into the owned ConPTY untouched.
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

    /// `build_spawn_command` with no per-profile backend env — the default for
    /// argv / recipe / session-id assertions, which don't depend on backend
    /// selection. The `ANTHROPIC_*` injection from a custom provider account has
    /// its own focused tests (`*_injects_backend_env`). Keeps the bulk of the
    /// suite terse while still exercising the real composition function.
    ///
    /// Model + effort run through the per-field cascade (issue #1149): the
    /// resolver masks unsupported values before they reach
    /// `build_spawn_command`. Tests that want to verify the resolver
    /// itself use [`crate::agent::capabilities`] directly.
    #[allow(clippy::too_many_arguments)]
    fn cmd_for(
        resolved: &ResolvedPath,
        provider: Provider,
        mode: &SessionIdMode,
        session_id: i64,
        model: Option<&str>,
        effort: Option<&str>,
        prefill: Option<&str>,
        sandbox: bool,
    ) -> portable_pty::CommandBuilder {
        let capabilities = crate::agent::capabilities::capabilities_for(provider.adapter());
        let config = crate::agent::capabilities::resolve_agent_config(
            &capabilities,
            crate::agent::capabilities::AgentConfigInputs {
                model: crate::agent::capabilities::FieldInputs {
                    explicit: model,
                    mesh_override: None,
                    mesh: None,
                    application: None,
                },
                effort: crate::agent::capabilities::FieldInputs {
                    explicit: effort,
                    mesh_override: None,
                    mesh: None,
                    application: None,
                },
            },
            None,
        );
        build_spawn_command(
            resolved,
            provider,
            &[],
            mode,
            session_id,
            &config,
            prefill,
            sandbox,
        )
    }

    /// Assigning a fresh session id appends `--session-id <uuid>` after the
    /// provider's base flag, and the whole thing is wrapped for WSL.
    #[test]
    fn anthropic_assign_builds_full_wsl_command() {
        let (binary, flag) = anthropic_recipe();
        let cmd = cmd_for(
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
        let sandboxed = cmd_for(
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
        let cmd = cmd_for(
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
        assert_eq!(
            args,
            expected_wsl(binary, &[flag, "--resume", "uuid-resume"])
        );
        assert!(
            !args.iter().any(|a| a == "--session-id"),
            "resume must not pass --session-id: {:?}",
            args
        );
    }

    /// A custom Claude-compatible profile (MiniMax/DeepSeek) resolves to the
    /// `anthropic` executor and injects its account's backend env. This is the
    /// AC for issue #538: `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` reach
    /// the spawned `claude`. The recipe is otherwise the plain claude command.
    #[test]
    fn custom_profile_injects_backend_env() {
        let backend_env = vec![
            (
                "ANTHROPIC_BASE_URL".to_string(),
                "https://api.minimax.io/anthropic".to_string(),
            ),
            (
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                "sk-custom-123".to_string(),
            ),
            ("ANTHROPIC_MODEL".to_string(), "MiniMax-M3[1m]".to_string()),
        ];
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Anthropic,
            &backend_env,
            &SessionIdMode::Assign("mm-1".to_string()),
            SESSION_ID,
            &crate::agent::capabilities::ResolvedAgentConfig::default(),
            None,
            false,
        );

        // Plain claude recipe — the backend is selected via env, not argv.
        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "claude",
                &["--dangerously-skip-permissions", "--session-id", "mm-1"]
            )
        );
        assert_eq!(
            env_of(&cmd, "ANTHROPIC_BASE_URL").as_deref(),
            Some("https://api.minimax.io/anthropic"),
            "custom profile base URL must be injected"
        );
        assert_eq!(
            env_of(&cmd, "ANTHROPIC_AUTH_TOKEN").as_deref(),
            Some("sk-custom-123"),
            "custom profile auth token must be injected"
        );
        // WSL bridges the injected keys via WSLENV so they cross the boundary.
        let wslenv = env_of(&cmd, "WSLENV").unwrap_or_default();
        assert!(
            wslenv.contains("ANTHROPIC_BASE_URL") && wslenv.contains("ANTHROPIC_AUTH_TOKEN"),
            "backend env keys must be appended to WSLENV: {wslenv:?}"
        );
    }

    /// An empty backend env (built-in Anthropic subscription) sets no
    /// `ANTHROPIC_*` overrides and leaves WSLENV untouched — vanilla claude.
    #[test]
    fn empty_backend_env_injects_nothing() {
        let cmd = build_spawn_command(
            &wsl_resolved(),
            Provider::Anthropic,
            &[],
            &SessionIdMode::None,
            SESSION_ID,
            &crate::agent::capabilities::ResolvedAgentConfig::default(),
            None,
            false,
        );
        assert!(env_of(&cmd, "ANTHROPIC_BASE_URL").is_none());
        assert!(env_of(&cmd, "ANTHROPIC_AUTH_TOKEN").is_none());
    }

    #[test]
    fn verified_codex_proxy_applies_profile_model_and_scoped_credential_on_fresh_and_resume() {
        let routing = codex_proxy("buildmesh_1234", "sentinel-secret");
        let debug = format!("{routing:?}");
        assert!(!debug.contains("sentinel-secret"));
        assert!(debug.contains("BUILDMESH_CODEX_PROVIDER_KEY"));
        for mode in [
            SessionIdMode::None,
            SessionIdMode::Resume("codex-session".into()),
        ] {
            let cmd = build_spawn_command_prepared(
                &wsl_resolved(),
                Provider::Codex,
                &routing,
                &mode,
                SESSION_ID,
                &crate::agent::capabilities::ResolvedAgentConfig::default(),
                None,
                false,
            );
            let args = argv(&cmd);
            assert_eq!(
                &args[..7],
                [
                    "wsl.exe",
                    "-d",
                    "Ubuntu",
                    "--cd",
                    SPAWN_PATH,
                    "--",
                    "/usr/bin/codex",
                ]
            );
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--profile", "buildmesh_1234"]));
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--model", "MiniMax-M3"]));
            assert_eq!(
                env_of(&cmd, "BUILDMESH_CODEX_PROVIDER_KEY").as_deref(),
                Some("sentinel-secret")
            );
            assert!(env_of(&cmd, "OPENAI_API_KEY").is_none());
        }
    }

    #[test]
    fn native_codex_receives_no_proxy_routing() {
        let cmd = build_spawn_command_prepared(
            &wsl_resolved(),
            Provider::Codex,
            &PreparedLaunchRouting::Native,
            &SessionIdMode::None,
            SESSION_ID,
            &crate::agent::capabilities::ResolvedAgentConfig::default(),
            None,
            false,
        );
        let args = argv(&cmd);
        assert!(!args.iter().any(|arg| arg == "--profile"));
        assert!(!args.iter().any(|arg| arg == "--model"));
        assert!(env_of(&cmd, "BUILDMESH_CODEX_PROVIDER_KEY").is_none());
    }

    #[test]
    fn proxied_codex_credentials_are_isolated_per_command() {
        let command_for = |profile: &str, credential: &str| {
            build_spawn_command_prepared(
                &wsl_resolved(),
                Provider::Codex,
                &codex_proxy(profile, credential),
                &SessionIdMode::None,
                SESSION_ID,
                &crate::agent::capabilities::ResolvedAgentConfig::default(),
                None,
                false,
            )
        };
        let first = command_for("buildmesh_first", "first-secret");
        let second = command_for("buildmesh_second", "second-secret");

        assert_eq!(
            env_of(&first, "BUILDMESH_CODEX_PROVIDER_KEY").as_deref(),
            Some("first-secret")
        );
        assert_eq!(
            env_of(&second, "BUILDMESH_CODEX_PROVIDER_KEY").as_deref(),
            Some("second-secret")
        );
        assert!(!argv(&first).iter().any(|arg| arg.contains("second-secret")));
        assert!(!argv(&second).iter().any(|arg| arg.contains("first-secret")));
    }

    #[test]
    fn prepared_proxy_executes_exact_fake_codex_for_fresh_and_resume() {
        let temp = tempfile::TempDir::new().unwrap();
        let executable = compile_fake_codex(&temp);
        let path = temp.path().to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: EnvType::Windows,
        };

        for (index, mode) in [
            SessionIdMode::None,
            SessionIdMode::Resume("resume-session".into()),
        ]
        .into_iter()
        .enumerate()
        {
            let routing = native_fake_proxy(&executable, &format!("credential-{index}"));
            let mut command = build_spawn_command_prepared(
                &resolved,
                Provider::Codex,
                &routing,
                &mode,
                -915_4300 - index as i64,
                &crate::agent::capabilities::ResolvedAgentConfig::default(),
                None,
                false,
            );
            let log = temp.path().join(format!("invocation-{index}.log"));
            command.env("FAKE_CODEX_LOG", &log);
            let pair = open_pty_pair(24, 80).unwrap();
            let mut child = spawn_child(&pair, command).unwrap();
            drop(pair.slave);
            assert!(child.wait().unwrap().success());
            drop(pair.master);

            let invocation = std::fs::read_to_string(log).unwrap();
            assert!(invocation.contains("--profile\nbuildmesh_fake"));
            assert!(invocation.contains("--model\nMiniMax-M3"));
            assert!(invocation.contains(&format!("CREDENTIAL\ncredential-{index}")));
            assert!(invocation.contains("OPENAI_KEY\n\n"));
            assert!(invocation.contains("OPENAI_URL\n\n"));
            if index == 0 {
                assert!(!invocation.contains("resume-session"));
            } else {
                assert!(
                    invocation.contains("resume\n") && invocation.contains("\nresume-session"),
                    "resume subcommand must carry the session id; got {invocation:?}"
                );
                let resume_at = invocation.find("resume\n").expect("resume subcommand");
                let id_at = invocation.find("\nresume-session").expect("session id");
                let profile_at = invocation.find("--profile").expect("proxy profile");
                let model_at = invocation.find("--model").expect("proxy model");
                assert!(
                    resume_at < id_at,
                    "session id must follow the resume subcommand; got {invocation:?}"
                );
                assert!(
                    profile_at < id_at && model_at < id_at,
                    "proxy --profile/--model are options and must precede the resume UUID; got {invocation:?}"
                );
            }
        }
    }

    /// Model + effort overrides are appended (in that order) for a provider that
    /// declares `supports_model_override()`.
    #[test]
    fn model_and_effort_overrides_appended_for_supporting_provider() {
        let cmd = cmd_for(
            &wsl_resolved(),
            Provider::Anthropic,
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
        let cmd = cmd_for(
            &wsl_resolved(),
            Provider::Anthropic,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            Some("hello world"),
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "claude",
                &["--dangerously-skip-permissions", "--prefill", "hello world"]
            )
        );
    }

    /// Prefill CRLF (and bare CR) are normalised to LF before reaching the provider.
    #[test]
    fn prefill_crlf_normalised_to_lf() {
        let cmd = cmd_for(
            &wsl_resolved(),
            Provider::Anthropic,
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
                &[
                    "--dangerously-skip-permissions",
                    "--prefill",
                    "Title\n\nLine 1\nLine 2\nLine 3"
                ]
            )
        );
    }

    // -----------------------------------------------------------------------
    // Windows direct execution (no wrapper, no bash): Claude-compatible
    // providers (Anthropic, MiniMax) reach claude.exe directly. These
    // pin the direct composition. Windows-only: the direct recipe is gated
    // on a Windows host. Replaces the `cwrap → bash → claude.exe` chain
    // that PR #531 absorbed into buildmesh — see
    // `sandbox::spawn::tests::repro_claude_exit_in_sandbox` for the live
    // AppContainer repro of why that chain can't run sandboxed.
    // -----------------------------------------------------------------------

    /// Anthropic on the Windows-native path spawns `claude.exe` directly.
    #[cfg(target_os = "windows")]
    #[test]
    fn anthropic_spawns_claude_exe_directly() {
        let cmd = cmd_for(
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
        unsafe {
            std::env::remove_var("BUILDMESH_PREFILL");
        }

        let cmd = cmd_for(
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
        let pos = args
            .iter()
            .position(|a| a == "--prefill")
            .expect("--prefill present in argv");
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

    /// A custom Claude-compatible profile spawns claude.exe directly on the
    /// Windows-native path and injects its account's backend env (issue #538) —
    /// the dynamic replacement for the deleted MiniMax adapter's hardcoded env.
    #[cfg(target_os = "windows")]
    #[test]
    fn custom_profile_injects_backend_env_on_windows_native() {
        let backend_env = vec![
            (
                "ANTHROPIC_BASE_URL".to_string(),
                "https://api.minimax.io/anthropic".to_string(),
            ),
            (
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                "sk-custom-123".to_string(),
            ),
            ("ANTHROPIC_MODEL".to_string(), "MiniMax-M3[1m]".to_string()),
        ];
        let cmd = build_spawn_command(
            &windows_resolved(),
            Provider::Anthropic,
            &backend_env,
            &SessionIdMode::None,
            SESSION_ID,
            &crate::agent::capabilities::ResolvedAgentConfig::default(),
            None,
            false,
        );

        let args = argv(&cmd);
        assert_eq!(args.first().map(String::as_str), Some("claude.exe"));
        assert_eq!(
            env_of(&cmd, "ANTHROPIC_MODEL").as_deref(),
            Some("MiniMax-M3[1m]"),
            "custom profile backend model env must be injected"
        );
        assert_eq!(
            env_of(&cmd, "ANTHROPIC_BASE_URL").as_deref(),
            Some("https://api.minimax.io/anthropic"),
            "custom profile backend base URL env must be injected"
        );
        assert_eq!(
            env_of(&cmd, "ANTHROPIC_AUTH_TOKEN").as_deref(),
            Some("sk-custom-123"),
            "custom profile backend auth token env must be injected"
        );
    }

    /// Regression guard: even with sandbox OFF, the Windows-native Anthropic spawn
    /// now goes to claude.exe directly instead of through PowerShell.
    #[cfg(target_os = "windows")]
    #[test]
    fn anthropic_unsandboxed_windows_spawns_claude_exe_directly() {
        let cmd = cmd_for(
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
        unsafe {
            std::env::set_var("ANTHROPIC_BASE_URL", "https://leaked.example/anthropic");
        }
        let cmd = cmd_for(
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
        unsafe {
            std::env::remove_var("ANTHROPIC_BASE_URL");
        }
        assert_eq!(
            leaked, None,
            "inherited ANTHROPIC_BASE_URL must be cleared for the Anthropic spawn (cwrap `unset` parity), not leaked: {:?}",
            leaked
        );
    }

    /// WSL keeps the `--prefill` CLI arg: `wsl.exe` passes a multi-line argv
    /// through intact (no cmd.exe in the chain), and a Windows env var does not
    /// cross into the WSL environment without `WSLENV`. So the env transport must
    /// NOT engage for WSL spawns. With no per-profile backend env, WSLENV stays
    /// unset (nothing to bridge) — the injection case is covered by
    /// `custom_profile_injects_backend_env`.
    #[test]
    fn prefill_stays_argv_for_wsl() {
        unsafe {
            std::env::remove_var("BUILDMESH_PREFILL");
        }

        let cmd = cmd_for(
            &wsl_resolved(),
            Provider::Anthropic,
            &SessionIdMode::None,
            SESSION_ID,
            None,
            None,
            Some("hello world"),
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "claude",
                &["--dangerously-skip-permissions", "--prefill", "hello world"]
            )
        );
        assert!(
            env_of(&cmd, "BUILDMESH_PREFILL").is_none(),
            "the WSL path must not set the prefill env var"
        );
    }

    /// Empty override/prefill strings are treated as absent — no flags emitted.
    #[test]
    fn empty_overrides_are_ignored() {
        let cmd = cmd_for(
            &wsl_resolved(),
            Provider::Anthropic,
            &SessionIdMode::None,
            SESSION_ID,
            Some(""),
            Some(""),
            Some(""),
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl("claude", &["--dangerously-skip-permissions"])
        );
    }

    /// Agy applies model and prefill overrides when passed.
    #[test]
    fn agy_applies_model_override_and_prefill() {
        let cmd = cmd_for(
            &wsl_resolved(),
            Provider::Agy,
            &SessionIdMode::None,
            SESSION_ID,
            Some("opus"),
            // Issue #1286: AGY's CLI accepts `--effort <low|medium|high>`.
            // This test focuses on model + prefill only (effort layer is
            // None). The end-to-end pin that AGY *does* forward `--effort`
            // when the layer is set lives in
            // `adapters::agy::tests::agy_recipe_appends_effort_arg_when_resolved`;
            // the "non-effort harness drops effort" pin is the matrix-level
            // `resolver_drops_effort_for_harness_without_effort_control` in
            // `capabilities::tests`.
            None,
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
                    "--prompt-interactive",
                    "prefill text"
                ]
            )
        );
        assert!(
            !args.iter().any(|a| a == "--effort"),
            "Agy with no effort layer must not emit --effort; got argv = {:?}",
            args
        );
    }

    /// Agy self-assigns session IDs — Assign mode must NOT inject `--session-id`.
    #[test]
    fn agy_assign_omits_session_flag() {
        let cmd = cmd_for(
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
        assert_eq!(
            args,
            expected_wsl("agy", &["--dangerously-skip-permissions"])
        );
        assert!(
            !args.iter().any(|a| a == "--session-id" || a == "ignored"),
            "agy self-assigns; Assign must not add --session-id: {:?}",
            args
        );
    }

    /// Codex provides a dedicated resume recipe (`codex resume [OPTIONS] <id>`)
    /// via `spawn_recipe_for_resume`, which `build_spawn_command` must use
    /// instead of the default `spawn_recipe` + `--resume`.
    #[test]
    fn codex_resume_uses_resume_recipe() {
        let cmd = cmd_for(
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
                    "--ask-for-approval",
                    "never",
                    "--sandbox",
                    "danger-full-access",
                    "--no-alt-screen",
                    "--dangerously-bypass-hook-trust",
                    "codex-sess",
                ]
            )
        );
    }

    /// Codex self-assigns its session id, so `session_assign_args` is empty:
    /// Assign mode must NOT inject `--session-id`.
    #[test]
    fn codex_assign_omits_session_flag() {
        let cmd = cmd_for(
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
                    "--no-alt-screen",
                    "--dangerously-bypass-hook-trust",
                ]
            )
        );
        assert!(
            !args.iter().any(|a| a == "--session-id" || a == "ignored"),
            "codex self-assigns; Assign must not add --session-id: {:?}",
            args
        );
    }

    #[test]
    fn codex_mesh_effort_uses_reasoning_config_override() {
        let cmd = cmd_for(
            &wsl_resolved(),
            Provider::Codex,
            &SessionIdMode::None,
            SESSION_ID,
            Some("gpt-5.6-sol"),
            Some("xhigh"),
            None,
            false,
        );

        assert_eq!(
            argv(&cmd),
            expected_wsl(
                "codex",
                &[
                    "--ask-for-approval",
                    "never",
                    "--sandbox",
                    "danger-full-access",
                    "--no-alt-screen",
                    "--dangerously-bypass-hook-trust",
                    "--model",
                    "gpt-5.6-sol",
                    "-c",
                    "model_reasoning_effort=\"xhigh\"",
                ]
            )
        );
    }

    // The issue/PR prefill contract (issue #1180) lives at the source
    // of truth: `agent::spawn::intent::tests`. Pre-#1180 these tests
    // pinned the wording via the now-removed `commands::agent::format_*_prefill`
    // helpers; the contract is unchanged but the canonical home is
    // `SpawnIntent::initial_prompt` and the regression tests moved with
    // it (they exercise the same shapes: URL with title hint, empty
    // title falls back to number-only, whitespace trims, quotes pass
    // through verbatim).
    //
    // The old `format_pr_prefill_includes_number_title_and_url` and
    // `format_pr_prefill_handles_empty_title` tests were deleted; their
    // wording assertions are duplicated in
    // `crate::agent::spawn::intent::tests::pull_request_prefill_uses_canonical_pull_url_and_trimmed_title`
    // plus the new contract pinning (issue #1180 AC #1–7). Keeping a
    // second copy in `commands::agent_tests` would invite drift — the
    // single source of truth principle is the whole point.

    /// On a Windows-native host a Claude-compatible provider is launched through
    /// `powershell.exe -NoLogo -NoProfile -EncodedCommand <base64>`. The
    /// `-NoProfile` flag is load-bearing for spawn latency: without it every
    /// agent spawn first runs the user's PowerShell profile (modules, prompt
    /// frameworks) before claude, adding hundreds of ms per node. This shell
    /// only relays ANSI output, so the profile is dead weight.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn windows_powershell_launcher_uses_no_profile() {
        let cmd = cmd_for(
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
        assert_eq!(
            args.len(),
            5,
            "expected the Base64 payload as the 5th arg: {:?}",
            args
        );
    }

    /// The wrapper sets the spawn cwd and the BUILDMESH_SESSION_ID / BUILDMESH_PORT
    /// env vars that the agent and its hooks rely on.
    #[test]
    fn sets_cwd_and_buildmesh_env() {
        let cmd = cmd_for(
            &wsl_resolved(),
            Provider::Anthropic,
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
        assert_eq!(
            url.as_deref(),
            Some("https://github.com/alice/buildmesh.git")
        );
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
