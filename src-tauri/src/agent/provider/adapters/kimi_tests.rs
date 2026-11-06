use super::*;

#[test]
fn native_hook_command_delivers_stdin_to_runtime_node_without_stdout() {
    use std::io::{Read, Seek, Write};
    use std::time::{Duration, Instant};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("Kimi hook did not reach listener: {error}"),
            }
        };
        // Windows accepted sockets inherit the listener's nonblocking mode.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        // Even a nonempty successful response must not leak into Kimi's hook protocol.
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .unwrap();
        (headers, body)
    });

    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    ensure_hooks(&path, &hook_command(cfg!(windows))).unwrap();
    let config = std::fs::read_to_string(path)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let hooks = config["hooks"].as_array_of_tables().unwrap();
    let command = hooks
        .iter()
        .find(|hook| hook["event"].as_str() == Some("Stop"))
        .unwrap()["command"]
        .as_str()
        .unwrap();
    let payload = br#"{"hook_event_name":"Stop","session_id":"session_8a979720-1cb0-408c-b29c-9f0f68f2982b","message":"literal $HOME & %PATH%"}"#;
    let mut input = tempfile::tempfile().unwrap();
    input.write_all(payload).unwrap();
    input.rewind().unwrap();
    let mut shell =
        crate::process_util::command_no_window(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" });
    if cfg!(windows) {
        shell.args(["/d", "/c"]);
        // `cmd /c` has special quoting rules for its final argument;
        // preserve the hook command byte-for-byte in this probe.
        #[cfg(windows)]
        std::os::windows::process::CommandExt::raw_arg(&mut shell, command);
    } else {
        shell.args(["-c", command]);
    }
    shell
        .env("BUILDMESH_PORT", port.to_string())
        .env("BUILDMESH_SESSION_ID", "741")
        .env_remove("BUILDMESH_WSL_HOST")
        .env("NO_PROXY", "localhost,127.0.0.1")
        .env("no_proxy", "localhost,127.0.0.1")
        .stdin(std::process::Stdio::from(input));
    let output = crate::process_util::run_command_with_timeout(
        shell,
        "Kimi attention command",
        Duration::from_secs(10),
    );
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (headers, body) = server.join().unwrap();
    assert!(output.stdout.is_empty(), "hook stdout: {:?}", output.stdout);
    assert!(
        headers.starts_with("POST /api/attention/741 HTTP/1.1\r\n"),
        "{headers}"
    );
    assert!(headers
        .to_ascii_lowercase()
        .contains("content-type: application/json\r\n"));
    assert_eq!(body, payload);
}

#[test]
#[ignore = "requires native Kimi Code 0.27.0+; optionally set BUILDMESH_KIMI_TEST_BIN"]
fn native_kimi_doctor_accepts_production_hook_configuration() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    ensure_hooks(&path, &hook_command(cfg!(windows))).unwrap();
    let binary = std::env::var("BUILDMESH_KIMI_TEST_BIN").unwrap_or_else(|_| "kimi".into());
    let mut command = crate::process_util::command_no_window(&binary);
    command
        .args(["doctor", "config"])
        .arg(&path)
        .env("KIMI_CODE_HOME", home.path());
    let output = crate::process_util::run_command_with_timeout(
        command,
        "Kimi config contract",
        std::time::Duration::from_secs(20),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("All checked config files are valid."));
}

fn provision_test_home(home: &Path, node_id: i64) -> Result<(), String> {
    let path = home.to_string_lossy().to_string();
    KIMI.provision_attention_hooks(
        &ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path.clone(),
            env_type: EnvType::Windows,
        },
        &LaunchRuntime {
            harness_home: Some(path),
            wsl_distro: None,
        },
        node_id,
    )
}

#[test]
fn native_hooks_preserve_user_configuration_and_are_shared_safely() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    let user = "# keep my preferences\ndefault_model = 'custom-model'\n\n[[hooks]]\n# user hook\nevent = 'Stop'\ncommand = 'echo user'\ntimeout = 2\n\n[providers.custom]\nbase_url = 'https://example.invalid'\n";
    std::fs::write(&path, user).unwrap();
    provision_test_home(home.path(), 101).unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    provision_test_home(home.path(), 202).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    assert!(first.contains("# keep my preferences"));
    assert!(first.contains("# user hook"));
    let parsed = first.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(parsed["default_model"].as_str(), Some("custom-model"));
    assert_eq!(
        parsed["providers"]["custom"]["base_url"].as_str(),
        Some("https://example.invalid")
    );
    let hooks = parsed["hooks"].as_array_of_tables().unwrap();
    assert_eq!(hooks.len(), 12);
    assert_eq!(hooks.get(0).unwrap()["command"].as_str(), Some("echo user"));
    let events: Vec<_> = hooks
        .iter()
        .skip(1)
        .map(|hook| hook["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        events,
        [
            "SessionStart",
            "Stop",
            "StopFailure",
            "Interrupt",
            "PermissionRequest",
            "PermissionResult",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "Notification"
        ]
    );
    for hook in hooks.iter().skip(1) {
        assert_eq!(hook["timeout"].as_integer(), Some(10));
        let command = hook["command"].as_str().unwrap();
        assert!(command.contains("BUILDMESH_SESSION_ID"));
        assert!(!command.contains("/attention/101"));
        if matches!(
            hook["event"].as_str(),
            Some("PreToolUse" | "PostToolUse" | "PostToolUseFailure")
        ) {
            assert_eq!(
                hook["matcher"].as_str(),
                Some("AskUserQuestion|ExitPlanMode")
            );
        }
        if hook["event"].as_str() == Some("Notification") {
            assert_eq!(
                hook["matcher"].as_str(),
                Some(r"^task\.(completed|failed|killed|timed_out|lost)$")
            );
        }
        assert!(hook
            .iter()
            .all(|(key, _)| ["event", "matcher", "command", "timeout"].contains(&key)));
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
}

#[test]
fn native_hooks_refuse_malformed_user_files() {
    for original in [
        "hooks = [",
        "hooks = 'user-defined-value'",
        "[hooks]\nStop = 'user-hook'\n",
    ] {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        std::fs::write(&path, original).unwrap();
        assert!(provision_test_home(home.path(), 1).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}

#[test]
fn native_hooks_accept_inline_arrays() {
    for original in [
        "hooks = []\n",
        "hooks = [{event = 'Stop', command = 'echo user'}]\n",
    ] {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        std::fs::write(&path, original).unwrap();
        provision_test_home(home.path(), 1).unwrap();
        let updated = std::fs::read_to_string(&path).unwrap();
        let config = updated.parse::<toml_edit::DocumentMut>().unwrap();
        let count = if original.contains("echo user") {
            12
        } else {
            11
        };
        assert_eq!(config["hooks"].as_array_of_tables().unwrap().len(), count);
        if original.contains("echo user") {
            assert!(updated.contains("echo user"));
        }
    }
}

#[test]
fn native_hook_commands_forward_stdin_and_guard_unmanaged_sessions() {
    let windows = hook_command(true);
    assert!(windows.starts_with("if defined BUILDMESH_SESSION_ID curl.exe "));
    // Encoded Windows interop callbacks carry the same runtime URL inside PowerShell.
    if crate::env::is_wsl_host() {
        let start = windows.find("powershell.exe ").unwrap();
        let decoded = crate::env::decode_powershell_command(&windows[start..]).unwrap();
        assert!(decoded.contains("$env:BUILDMESH_PORT"));
        assert!(decoded.contains("$env:BUILDMESH_SESSION_ID"));
        assert!(decoded.contains("--data-binary '@-'"));
    } else {
        assert!(windows.contains("%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID%"));
        assert!(windows.contains("--data-binary @-"));
        assert!(windows.contains("-o NUL"));
    }
    let unix = hook_command(false);
    assert!(unix.starts_with("if [ -n \"$BUILDMESH_SESSION_ID\" ]; then "));
    assert!(unix.contains("$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID"));
    assert!(unix.contains("--data-binary @-"));
    assert!(unix.contains("-o /dev/null"));
}

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
    let platforms = KIMI.descriptor().available_on;
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
    assert!(
        args.is_empty(),
        "Kimi self-assigns; session_assign_args must be empty"
    );
}

#[test]
fn no_prefill_support() {
    assert!(!KIMI.descriptor().supports_prefill);
}

#[test]
fn supports_resume_model_override_and_native_attention_hooks() {
    assert!(KIMI.descriptor().supports_resume);
    assert!(KIMI.descriptor().supports_model_override);
    assert!(KIMI.descriptor().requires_attention_hook());
}

#[test]
fn produces_readable_transcript() {
    // #911 research confirmed Kimi's wire.jsonl is standard JSONL, but
    // the transcript reader's path resolver isn't wired for `~/.kimi/`
    // yet — so we claim `false` to match the current wire behaviour
    // (Node Digest rich layer degrades to spine-only with `unsupported`).
    // When the follow-up wires `TranscriptFormat::Kimi`, flip this back
    // to `true` and add a reader test that parses a fixture wire.jsonl.
    assert!(!KIMI.descriptor().produces_readable_transcript);
}

/// Issue #1186: pin the harness-specific model-flag shape. The
/// table-driven `capability_recipe_coherence` only asserts *some*
/// model flag exists in the recipe — a silent `-m` ↔ `--model`
/// flip on the adapter would pass. This pin catches the drift
/// before it reaches the wire.
#[test]
fn kimi_interactive_recipe_carries_short_m_model_arg() {
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{
        assert_flag_followed_by_value, HarnessLaunchInput, SessionIdModeRef,
    };

    let config = ResolvedAgentConfig {
        model: Some("kimi-k2".to_string()),
        effort: None,
        extra_args: None,
        permission_mode: None,
    };
    let input = HarnessLaunchInput {
        routing: None,
        node_id: None,
        platform: Platform::Linux,
        runtime: EnvType::Windows,
        session: SessionIdModeRef::None,
        config: &config,
        prefill: None,
        sandbox: false,
    };
    let prepared = KIMI.prepare_launch(input);
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
    let caps = crate::agent::capabilities::capabilities_for(&KIMI);
    assert_eq!(caps.harness_id, "kimi");
    assert!(caps.supports_resume);
    assert!(caps.supports_model_override);
    assert!(!caps.supports_effort_override);
    assert!(!caps.supports_prefill);
    assert!(caps.requires_attention_hook);
    assert!(!caps.produces_readable_transcript);
    assert!(!caps.is_plain_terminal);
    assert_eq!(
        caps.effort_control,
        crate::agent::capabilities::EffortControlKind::None
    );
}
