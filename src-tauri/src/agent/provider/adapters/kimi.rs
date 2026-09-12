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
//! `default_model` from `~/.kimi-code/config.toml` for that one session).
//! Credentials and provider mapping live in `~/.kimi-code/config.toml`;
//! Kimi's own login flow owns those. Buildmesh merges only native hooks.
//!
//! **Shell wrapping**: `kimi` is a native binary on all platforms (not a
//! `.cmd` shim), so `WindowsShell::Direct` is correct everywhere — matching
//! the AGY and Grok adapter patterns.

use crate::agent::provider::{AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;
use crate::env::ResolvedPath;
use std::path::Path;

static HOOK_CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hook_command(windows: bool) -> String {
    if windows {
        let callback = crate::env::windows_attention_command(None).unwrap_or_else(||
            "curl.exe -fsS --connect-timeout 2 --max-time 5 -o NUL -X POST -H \"Content-Type: application/json\" --data-binary @- --url \"http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID%\"".into());
        format!("if defined BUILDMESH_SESSION_ID {callback}")
    } else {
        format!("if [ -n \"$BUILDMESH_SESSION_ID\" ]; then {} -fsS --connect-timeout 2 --max-time 5 -o /dev/null -X POST -H 'Content-Type: application/json' --data-binary @- \"http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID\"; fi", crate::env::unix_attention_curl())
    }
}

/// Kimi Code 0.27.0 reads flat [[hooks]] entries from its global config.
/// Keep callbacks runtime-bound: this file is shared by all Kimi sessions.
fn ensure_hooks(path: &Path, command: &str) -> Result<(), String> {
    use std::io::Write;
    use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};
    let _guard = HOOK_CONFIG_LOCK.lock().map_err(|e| e.to_string())?;
    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("failed to read Kimi config {path:?}: {e}")),
    };
    let mut config = original.parse::<DocumentMut>()
        .map_err(|e| format!("refusing to overwrite malformed Kimi config {path:?}: {e}"))?;
    if !config.contains_key("hooks") || config["hooks"].as_array().is_some_and(|array| array.is_empty()) {
        config["hooks"] = Item::ArrayOfTables(ArrayOfTables::new());
    } else if !config["hooks"].is_array_of_tables() {
        let hooks = config["hooks"].clone().into_array_of_tables()
            .map_err(|_| "Kimi hooks must be an array of tables".to_string())?;
        config["hooks"] = Item::ArrayOfTables(hooks);
    }
    let hooks = config["hooks"].as_array_of_tables_mut()
        .ok_or_else(|| "Kimi hooks must be an array of tables".to_string())?;
    hooks.retain(|hook| !hook.get("command").and_then(Item::as_str).is_some_and(|command| {
        let decoded = command.find("powershell.exe ")
            .and_then(|start| crate::env::decode_powershell_command(&command[start..]));
        let command = decoded.as_deref().unwrap_or(command);
        command.contains("BUILDMESH_SESSION_ID") && command.contains("BUILDMESH_PORT")
    }));
    for (event, matcher) in [
        ("SessionStart", None), ("Stop", None), ("StopFailure", None),
        ("Interrupt", None), ("PermissionRequest", None), ("PermissionResult", None),
        ("UserPromptSubmit", None),
        ("PreToolUse", Some("AskUserQuestion|ExitPlanMode")),
        ("PostToolUse", Some("AskUserQuestion|ExitPlanMode")),
        ("PostToolUseFailure", Some("AskUserQuestion|ExitPlanMode")),
        // Background AskUserQuestion returns before the answer. Its terminal
        // task notification, correlated by task id, is the resolution signal.
        ("Notification", Some(r"^task\.(completed|failed|killed|timed_out|lost)$")),
    ] {
        let mut hook = Table::new();
        hook["event"] = value(event);
        if let Some(matcher) = matcher { hook["matcher"] = value(matcher); }
        hook["command"] = value(command);
        hook["timeout"] = value(10);
        hooks.push(hook);
    }
    let updated = config.to_string();
    if updated == original { return Ok(()); }
    let parent = path.parent().ok_or("Kimi config has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.write_all(updated.as_bytes()).map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist(path).map_err(|e| format!("failed to replace Kimi config {path:?}: {e}"))?;
    Ok(())
}

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
            trailing_args: Vec::new(),
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
        true
    }

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        AttentionCapability::Hook {
            events: vec![LifecycleKind::TurnCompleted, LifecycleKind::InputRequired, LifecycleKind::QuestionRequested, LifecycleKind::PermissionRequested],
            launch_mode: AttentionLaunchMode::PermissionAsk,
            trust: None,
            min_version: Some("0.27.0".into()),
        }
    }

    fn provision_attention_hooks(&self, resolved: &ResolvedPath, runtime: &LaunchRuntime, _node_id: i64) -> Result<(), String> {
        let home = match runtime.harness_home.as_deref() {
            Some(home) => std::path::PathBuf::from(crate::env::to_host_path(home)),
            None => crate::env::kimi_home_for_spawn(&resolved.spawn_path, runtime.wsl_distro.as_deref())
                .ok_or("could not resolve Kimi Code configuration home")?,
        };
        let windows = crate::env::runtime_for_spawn_path(&resolved.spawn_path) == EnvType::WindowsInterop
            || (cfg!(windows) && crate::env::runtime_for_spawn_path(&resolved.spawn_path) != EnvType::Wsl);
        ensure_hooks(&home.join("config.toml"), &hook_command(windows))
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

    fn supports_extra_args(&self) -> bool {
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
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("Kimi hook did not reach listener: {error}"),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length: usize = headers.lines().find_map(|line| {
                line.to_ascii_lowercase().strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            }).unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            // Even a nonempty successful response must not leak into Kimi's hook protocol.
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK").unwrap();
            (headers, body)
        });

        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        ensure_hooks(&path, &hook_command(cfg!(windows))).unwrap();
        let config = std::fs::read_to_string(path).unwrap().parse::<toml_edit::DocumentMut>().unwrap();
        let hooks = config["hooks"].as_array_of_tables().unwrap();
        let command = hooks.iter().find(|hook| hook["event"].as_str() == Some("Stop"))
            .unwrap()["command"].as_str().unwrap();
        let payload = br#"{"hook_event_name":"Stop","session_id":"session_8a979720-1cb0-408c-b29c-9f0f68f2982b","message":"literal $HOME & %PATH%"}"#;
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(payload).unwrap();
        input.rewind().unwrap();
        let mut shell = crate::process_util::command_no_window(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" });
        if cfg!(windows) {
            shell.args(["/d", "/c"]);
            // `cmd /c` has special quoting rules for its final argument;
            // preserve the hook command byte-for-byte in this probe.
            #[cfg(windows)]
            std::os::windows::process::CommandExt::raw_arg(&mut shell, command);
        } else { shell.args(["-c", command]); }
        shell.env("BUILDMESH_PORT", port.to_string())
            .env("BUILDMESH_SESSION_ID", "741")
            .env_remove("BUILDMESH_WSL_HOST")
            .env("NO_PROXY", "localhost,127.0.0.1")
            .env("no_proxy", "localhost,127.0.0.1")
            .stdin(std::process::Stdio::from(input));
        let output = crate::process_util::run_command_with_timeout(shell, "Kimi attention command", Duration::from_secs(10));
        let output = output.unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let (headers, body) = server.join().unwrap();
        assert!(output.stdout.is_empty(), "hook stdout: {:?}", output.stdout);
        assert!(headers.starts_with("POST /api/attention/741 HTTP/1.1\r\n"), "{headers}");
        assert!(headers.to_ascii_lowercase().contains("content-type: application/json\r\n"));
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
        command.args(["doctor", "config"]).arg(&path).env("KIMI_CODE_HOME", home.path());
        let output = crate::process_util::run_command_with_timeout(command, "Kimi config contract", std::time::Duration::from_secs(20)).unwrap();
        assert!(output.status.success(), "stdout: {}\nstderr: {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("All checked config files are valid."));
    }

    fn provision_test_home(home: &Path, node_id: i64) -> Result<(), String> {
        let path = home.to_string_lossy().to_string();
        KIMI.provision_attention_hooks(&ResolvedPath {
            host_path: path.clone(), spawn_path: path.clone(), raw_path: path.clone(), env_type: EnvType::Windows,
        }, &LaunchRuntime { harness_home: Some(path), wsl_distro: None }, node_id)
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
        assert_eq!(parsed["providers"]["custom"]["base_url"].as_str(), Some("https://example.invalid"));
        let hooks = parsed["hooks"].as_array_of_tables().unwrap();
        assert_eq!(hooks.len(), 12);
        assert_eq!(hooks.get(0).unwrap()["command"].as_str(), Some("echo user"));
        let events: Vec<_> = hooks.iter().skip(1).map(|hook| hook["event"].as_str().unwrap()).collect();
        assert_eq!(events, ["SessionStart", "Stop", "StopFailure", "Interrupt", "PermissionRequest", "PermissionResult", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure", "Notification"]);
        for hook in hooks.iter().skip(1) {
            assert_eq!(hook["timeout"].as_integer(), Some(10));
            let command = hook["command"].as_str().unwrap();
            assert!(command.contains("BUILDMESH_SESSION_ID"));
            assert!(!command.contains("/attention/101"));
            if matches!(hook["event"].as_str(), Some("PreToolUse" | "PostToolUse" | "PostToolUseFailure")) {
                assert_eq!(hook["matcher"].as_str(), Some("AskUserQuestion|ExitPlanMode"));
            }
            if hook["event"].as_str() == Some("Notification") {
                assert_eq!(hook["matcher"].as_str(), Some(r"^task\.(completed|failed|killed|timed_out|lost)$"));
            }
            assert!(hook.iter().all(|(key, _)| ["event", "matcher", "command", "timeout"].contains(&key)));
        }
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
    }

    #[test]
    fn native_hooks_refuse_malformed_user_files() {
        for original in ["hooks = [", "hooks = 'user-defined-value'", "[hooks]\nStop = 'user-hook'\n"] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("config.toml");
            std::fs::write(&path, original).unwrap();
            assert!(provision_test_home(home.path(), 1).is_err());
            assert_eq!(std::fs::read_to_string(path).unwrap(), original);
        }
    }

    #[test]
    fn native_hooks_accept_inline_arrays() {
        for original in ["hooks = []\n", "hooks = [{event = 'Stop', command = 'echo user'}]\n"] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("config.toml");
            std::fs::write(&path, original).unwrap();
            provision_test_home(home.path(), 1).unwrap();
            let updated = std::fs::read_to_string(&path).unwrap();
            let config = updated.parse::<toml_edit::DocumentMut>().unwrap();
            let count = if original.contains("echo user") { 12 } else { 11 };
            assert_eq!(config["hooks"].as_array_of_tables().unwrap().len(), count);
            if original.contains("echo user") { assert!(updated.contains("echo user")); }
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
    fn supports_resume_model_override_and_native_attention_hooks() {
        assert!(KIMI.supports_resume());
        assert!(KIMI.supports_model_override());
        assert!(KIMI.requires_attention_hook());
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
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
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
        assert!(caps.requires_attention_hook);
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(caps.effort_control, crate::agent::capabilities::EffortControlKind::None);
    }
}
