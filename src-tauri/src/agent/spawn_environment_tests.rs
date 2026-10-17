//! Tests for `agent::spawn_environment`.
//!
//! Split out of `src-tauri/src/agent/spawn_environment.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test agent::spawn_environment::tests

use super::{
    append_to_wslenv, apply_wsl_env, encode_for_powershell, format_powershell_command,
    CLAUDE_SESSION_MARKER_ENV_VARS,
};
use crate::agent::provider::{SpawnRecipe, WindowsShell};
use crate::models::EnvType;
use base64::Engine;

/// Every marker as an explicit "present with this value" env override, so a
/// test can assert the scrub cleared something that was genuinely
/// inherited rather than something the command never carried.
fn markers_in_the_launching_session() -> Vec<(&'static str, Option<&'static std::ffi::OsStr>)> {
    CLAUDE_SESSION_MARKER_ENV_VARS
        .iter()
        .map(|key| {
            (
                *key,
                Some(std::ffi::OsStr::new("inherited-from-parent-session")),
            )
        })
        .collect()
}

/// The regression from issue #2136: an agent spawned by a Buildmesh that
/// was itself launched from a Claude Code session inherits
/// `CLAUDE_CODE_CHILD_SESSION`, starts with "Transcript saving is off",
/// and writes no transcript for a Circuit gate to read.
#[test]
fn agent_spawn_drops_the_launching_claude_code_sessions_markers() {
    let _env_guard = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::env::with_env_vars(&markers_in_the_launching_session(), || {
        let recipe = SpawnRecipe {
            binary: "claude.exe",
            base_args: vec!["--dangerously-skip-permissions".into()],
            trailing_args: vec![],
            windows_shell: WindowsShell::Direct,
        };
        let cmd = super::wrap(recipe, EnvType::Windows, None, None, ".", 4242, false)
            .expect("an unsandboxed command always assembles");
        for key in CLAUDE_SESSION_MARKER_ENV_VARS {
            assert!(cmd.get_env(key).is_none(), "{key} reached a spawned agent: an agent launched from inside a Claude Code session would inherit the launching session's identity and, for CLAUDE_CODE_CHILD_SESSION, save no transcript (issue #2136)");
        }
    });
}

/// Node 5351: a Codex reviewer spawned for PR #2261 printed *The command
/// line is too long.* into its own terminal and exited. Its launch —
/// attention hooks, model flags and the review policy prompt — crossed the
/// 8,191-character line `cmd.exe` accepts from the PowerShell that runs an
/// npm `codex.CMD` shim. The launch is now modelled against both budgets,
/// so it fits, and one that genuinely cannot is refused up front.
#[test]
fn a_realistic_codex_launch_fits_the_windows_command_line_budgets() {
    let adapter = crate::models::Provider::Codex.adapter();
    let mut base_args = vec![
        "--sandbox".to_string(),
        "danger-full-access".into(),
        "--dangerously-bypass-hook-trust".into(),
        "--model".into(),
        "gpt-6-luna".into(),
    ];
    base_args.extend(adapter.launch_hook_args(5351, EnvType::Windows));
    let prompt = format!(
        "Review PR #2261\n{}\nhttps://github.com/alondero/buildmesh/pull/2261",
        crate::review_contract::REVIEW_POLICY
    );
    base_args.push(prompt);
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args,
        trailing_args: vec![],
        windows_shell: WindowsShell::PowerShell,
    };
    let cmd = super::wrap(
        recipe,
        EnvType::Windows,
        None,
        Some(r"C:\Users\alond\AppData\Roaming\npm\codex.CMD"),
        ".",
        5351,
        false,
    )
    .expect("a Codex review launch fits both command-line budgets");
    let argv: Vec<String> = cmd
        .get_argv()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(argv[0], "powershell.exe", "{argv:?}");
}

#[test]
fn mapped_codex_launch_accepts_a_prompt_that_fits_the_child_budget() {
    // Windows means host-native on other hosts; WindowsInterop explicitly
    // selects the Windows wrapper without assuming the host uses it.
    let env_types = if cfg!(windows) {
        &[EnvType::Windows, EnvType::WindowsInterop][..]
    } else {
        &[EnvType::WindowsInterop][..]
    };
    for &env_type in env_types {
        let args = vec![format!("{}x", "review homelab\n".repeat(229))];
        let executable = r"C:\Users\alond\AppData\Roaming\npm\codex.CMD";
        super::ensure_cmd_command_line_fits(executable, &args).unwrap();
        let script = super::format_powershell_command(executable, &args);
        assert_eq!(
            super::powershell_command_line_chars(&super::encode_for_powershell(&script)),
            9350
        );
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args: args,
            trailing_args: vec![],
            windows_shell: WindowsShell::PowerShell,
        };
        let command = super::wrap(
            recipe,
            env_type,
            None,
            Some(executable),
            UNC_SPAWN_PATH,
            5406,
            false,
        )
        .expect("Base64 expansion must not consume the mapped batch-line budget");
        assert_maps_through_pushd(&command, UNC_SPAWN_PATH, &encoded_script(&command));
        remove_mapped_batch(&command);
    }
}

#[test]
fn windows_interop_mapped_percent_directory_is_batch_escaped() {
    let spawn_path = r"\\server\share\50%off\%USERNAME%\repo";
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args: vec![],
        trailing_args: vec![],
        windows_shell: WindowsShell::PowerShell,
    };
    let command = super::wrap(
        recipe,
        EnvType::WindowsInterop,
        None,
        Some(r"C:\tools\codex.cmd"),
        spawn_path,
        5408,
        false,
    )
    .expect("a mapped percent-containing directory should assemble");
    assert_maps_through_pushd(&command, spawn_path, &encoded_script(&command));
    remove_mapped_batch(&command);
}

#[test]
fn mapped_script_does_not_spend_the_encoded_process_budget_on_literal_quotes() {
    let executable = r"C:\Users\alond\AppData\Roaming\npm\codex.CMD";
    let args = vec!["'".repeat(7_000)];
    super::ensure_cmd_command_line_fits(executable, &args).unwrap();
    let script = super::format_powershell_command(executable, &args);
    assert!(
        super::ensure_powershell_command_line_fits(&script).is_err(),
        "doubled literal quotes exceed the inline encoded process budget"
    );
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args: args,
        trailing_args: vec![],
        windows_shell: WindowsShell::PowerShell,
    };
    let command = super::wrap(
        recipe,
        EnvType::WindowsInterop,
        None,
        Some(executable),
        UNC_SPAWN_PATH,
        5406,
        false,
    )
    .expect("the mapped script is on disk, so only its loader occupies the process line");
    assert_eq!(
        encoded_script(&command),
        format!("{script}; exit $LASTEXITCODE")
    );
    remove_mapped_batch(&command);
}

/// The guard's other half: an oversized launch fails here, naming the
/// budget it crossed, instead of handing a shell a line it will refuse and
/// leaving the node to report a cryptic message.
#[cfg(windows)]
#[test]
fn a_launch_over_the_cmd_exe_budget_is_refused_with_that_budget() {
    for spawn_path in [".", UNC_SPAWN_PATH] {
        let mut base_args = vec!["--sandbox".into(), "danger-full-access".into()];
        base_args.push("review this".repeat(1_000));
        let recipe = SpawnRecipe {
            binary: "codex",
            base_args,
            trailing_args: vec![],
            windows_shell: WindowsShell::PowerShell,
        };
        let error = super::wrap(
            recipe,
            EnvType::Windows,
            None,
            Some(r"C:\Users\alond\AppData\Roaming\npm\codex.CMD"),
            spawn_path,
            5351,
            false,
        )
        .expect_err("a launch cmd.exe cannot parse must be refused before it is spawned");
        assert!(
            error.contains("cmd.exe") && error.contains("8191"),
            "{error}"
        );
    }
}

#[test]
fn windows_interop_cmd_launch_accounts_for_the_outer_powershell_script() {
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args: vec!["--model".into(), "gpt-6-luna".into()],
        trailing_args: vec![],
        windows_shell: WindowsShell::Cmd,
    };
    // Single quotes are valid Windows path characters. PowerShell doubles
    // each one in the encoded script, making the outer process line cross
    // 32,767 while the inner cmd.exe line remains below 8,191.
    let spawn_path = format!("C:\\{}", "'".repeat(7_000));
    let error = super::wrap(
        recipe,
        EnvType::WindowsInterop,
        None,
        Some(r"C:\\Users\\alond\\AppData\\Roaming\\npm\\codex.CMD"),
        &spawn_path,
        5351,
        false,
    )
    .expect_err("the encoded Windows interop PowerShell script exceeds CreateProcess");
    assert!(error.contains("32767"), "{error}");
}

#[cfg(windows)]
#[test]
fn cmd_budget_accounts_for_windows_quote_and_backslash_escaping() {
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args: vec!["\\\"".repeat(2_200)],
        trailing_args: vec![],
        windows_shell: WindowsShell::Cmd,
    };
    let error = super::wrap(recipe, EnvType::Windows, None, None, ".", 5351, false)
        .expect_err("serialized cmd.exe arguments exceed the shell budget");
    assert!(error.contains("8191"), "{error}");
}

#[cfg(windows)]
#[test]
fn background_windows_launch_over_the_process_budget_is_refused() {
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args: vec!["x".repeat(40_000)],
        trailing_args: vec![],
        windows_shell: WindowsShell::PowerShell,
    };
    let error = super::background_command(&recipe, None)
        .expect_err("background launches must use the same Windows process budget");
    assert!(error.contains("32767"), "{error}");
}

#[cfg(windows)]
#[test]
fn native_cmd_unc_launch_accounts_for_its_pushd_path() {
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args: vec!["--model".into(), "gpt-6-luna".into()],
        trailing_args: vec![],
        windows_shell: WindowsShell::Cmd,
    };
    let spawn_path = format!("\\\\server\\repo-{}", "x".repeat(8_200));
    let error = super::wrap(
        recipe,
        EnvType::Windows,
        None,
        Some(r"C:\Users\alond\AppData\Roaming\npm\codex.CMD"),
        &spawn_path,
        5351,
        false,
    )
    .expect_err("cmd.exe must budget the UNC pushd command it actually receives");
    assert!(error.contains("8191"), "{error}");
}

#[cfg(not(windows))]
#[test]
fn native_linux_direct_launch_does_not_use_windows_command_line_limits() {
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args: vec!["x".repeat(40_000)],
        trailing_args: vec![],
        windows_shell: WindowsShell::Direct,
    };
    super::wrap(recipe, EnvType::Windows, None, None, ".", 5351, false)
        .expect("native Linux direct launches do not use Windows command-line limits");
}

/// The background launch path is a second instance of the same leak: it
/// builds a `std::process::Command`, which does not share `wrap`'s loop.
#[test]
fn background_launch_drops_the_same_markers() {
    let recipe = SpawnRecipe {
        binary: "claude.exe",
        base_args: vec!["--print".into()],
        trailing_args: vec![],
        windows_shell: WindowsShell::Direct,
    };
    let cmd = super::background_command(&recipe, None).expect("assemble background command");
    // `get_envs` reports the command's explicit overrides, where a removal
    // is the entry `(key, None)`.
    let cleared: std::collections::BTreeMap<String, Option<String>> = cmd
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();
    for key in CLAUDE_SESSION_MARKER_ENV_VARS {
        assert_eq!(
            cleared.get(*key),
            Some(&None),
            "{key} must be explicitly removed from the background launch env"
        );
    }
}

/// `BackgroundProcessGuard` signals the group led by the child. Background
/// inference is built here, so this command has to be that leader. Otherwise
/// `kill(-pid)` names a group that does not exist and cancellation leaves
/// the CLI's descendants running.
#[cfg(unix)]
#[test]
fn background_inference_leads_its_own_group_so_cancel_reaps_descendants() {
    use std::io::{BufRead, Read};
    use std::process::Stdio;

    let _env = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let recipe = SpawnRecipe {
        binary: "sh",
        base_args: vec![
            "-c".into(),
            "echo ready; sh -c 'echo child-ready; sleep 60' & wait".into(),
        ],
        trailing_args: vec![],
        windows_shell: WindowsShell::Direct,
    };
    let mut command =
        super::background_command(&recipe, None).expect("assemble background command");
    command.stdout(Stdio::piped());
    let mut child = Reap(
        command
            .spawn()
            .expect("spawn background inference in its own process group"),
    );
    let pid = child.0.id();
    // SAFETY: `getpgid` only reads the group id of this live child. A pid
    // that has already exited returns -1, which the assertion rejects.
    let pgid = unsafe { getpgid(i32::try_from(pid).unwrap()) };
    let mut reader = std::io::BufReader::new(child.0.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ready");
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "child-ready");
    assert_eq!(
        pgid,
        i32::try_from(pid).unwrap(),
        "background inference must lead its own process group (pgid {pgid}, pid {pid})"
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let drain = std::thread::spawn(move || {
        let mut remaining = String::new();
        tx.send(reader.read_to_string(&mut remaining)).unwrap();
    });
    drop(crate::agent::background::BackgroundProcessGuard::new(pid));
    let closed = rx.recv_timeout(std::time::Duration::from_secs(5));
    closed
        .expect("cancelling background inference must reap descendants")
        .unwrap();
    drain.join().unwrap();
}

#[cfg(unix)]
struct Reap(std::process::Child);

#[cfg(unix)]
impl Drop for Reap {
    fn drop(&mut self) {
        crate::process_util::kill_process_group(self.0.id());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
extern "C" {
    fn getpgid(pid: i32) -> i32;
}

/// The scrub is an explicit list precisely because Buildmesh sets `CLAUDE_*`
/// variables on purpose. A prefix rule over `CLAUDE*` would look tidier and
/// would silently undo the MiniMax naming side-channel and the Windows
/// sandbox config dir; this test is what keeps the list explicit.
#[test]
fn the_scrub_never_covers_a_variable_buildmesh_sets_deliberately() {
    for deliberate in [
        // provider_conf::minimax_backend_env — the naming side-channel.
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
        // sandbox::spawn — the Windows agent sandbox.
        "CLAUDE_CONFIG_DIR",
    ] {
        assert!(
            !CLAUDE_SESSION_MARKER_ENV_VARS.contains(&deliberate),
            "{deliberate} is set by buildmesh on purpose and must survive the session-marker scrub"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires WSL with Windows interop enabled"]
fn live_wsl_host_launches_windows_harnesses() {
    assert!(crate::env::is_wsl_host());
    let windows_temp = crate::env::windows_cli_home("AppData/Local/Temp").unwrap();
    let script_dir = tempfile::tempdir_in(&windows_temp).unwrap();
    let script = script_dir.path().join("probe script.cmd");
    std::fs::write(
        &script,
        "@echo off\r\necho %BUILDMESH_SESSION_ID%> probe.txt\r\n",
    )
    .unwrap();
    for root in [std::path::Path::new("/tmp"), windows_temp.as_path()] {
        let directory = tempfile::Builder::new()
            .prefix("buildmesh reverse ")
            .tempdir_in(root)
            .unwrap();
        let spawn_path = crate::env::windows_path_from_wsl(directory.path().to_str().unwrap());
        for shell in [WindowsShell::PowerShell, WindowsShell::Cmd] {
            let (binary, args) = if shell == WindowsShell::Cmd {
                (
                    crate::env::windows_path_from_wsl(script.to_str().unwrap()),
                    vec![],
                )
            } else {
                ("powershell.exe".to_string(), vec!["-NoProfile".into(), "-EncodedCommand".into(),
                        encode_for_powershell("[IO.File]::WriteAllText((Join-Path $PWD.ProviderPath 'probe.txt'), $env:BUILDMESH_SESSION_ID)")])
            };
            let recipe = SpawnRecipe {
                binary: "probe",
                base_args: args,
                trailing_args: vec![],
                windows_shell: shell,
            };
            let mut command = super::wrap(
                recipe,
                EnvType::WindowsInterop,
                None,
                Some(&binary),
                &spawn_path,
                8125,
                false,
            )
            .expect("an unsandboxed command always assembles");
            apply_wsl_env(&mut command, EnvType::WindowsInterop, &[], &[]);
            let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
            let mut child = crate::agent::spawn::spawn_child(&pair, command).unwrap();
            use std::io::{Read, Write};
            let mut reader = pair.master.try_clone_reader().unwrap();
            let mut writer = pair.master.take_writer().unwrap();
            drop(pair.slave);
            let drain = std::thread::spawn(move || {
                let mut buffer = [0; 4096];
                let mut output = Vec::new();
                while let Ok(count) = reader.read(&mut buffer) {
                    if count == 0 {
                        break;
                    }
                    output.extend_from_slice(&buffer[..count]);
                    if output.ends_with(b"\x1b[6n") {
                        writer.write_all(b"\x1b[1;1R").unwrap();
                    }
                }
                output
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    panic!("Windows PTY timed out: {shell:?} {spawn_path}");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            assert!(
                status.success(),
                "{shell:?}: {spawn_path}: {}",
                String::from_utf8_lossy(&drain.join().unwrap())
            );
            let result = directory.path().join("probe.txt");
            assert_eq!(std::fs::read_to_string(&result).unwrap().trim(), "8125");
            std::fs::remove_file(result).unwrap();
        }
    }
}

#[test]
fn wslenv_reverses_direction_preserving_path_flags() {
    let mut value = "TOKEN/u:CODEX_HOME/pu:OTHER/l".to_string();
    super::set_wslenv_direction(&mut value, "TOKEN", "w");
    super::set_wslenv_direction(&mut value, "CODEX_HOME", "w");
    assert_eq!(value, "OTHER/l:TOKEN/w:CODEX_HOME/pw");
}

#[test]
#[cfg(windows)]
#[ignore = "requires an installed default WSL distribution"]
fn live_wsl_pty_preserves_cwd_environment_and_literal_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let mut resolved = crate::env::resolve_raw_path(directory.path().to_str().unwrap());
    crate::env::apply_harness_runtime(&mut resolved, crate::models::EnvType::Wsl);
    let payload = "spaces 'quotes' \"double\" $HOME `literal`\nsecond line";
    let recipe = crate::agent::provider::SpawnRecipe {
        binary: "sh",
        base_args: vec![
            "-c".into(),
            "printf '%s\\n' \"$PWD\" \"$BUILDMESH_SESSION_ID\" \"$1\" > probe.txt".into(),
            "probe".into(),
            payload.into(),
        ],
        trailing_args: vec![],
        windows_shell: crate::agent::provider::WindowsShell::Direct,
    };
    let mut command = super::wrap(
        recipe,
        resolved.env_type,
        None,
        None,
        &resolved.spawn_path,
        8123,
        false,
    )
    .unwrap();
    apply_wsl_env(&mut command, resolved.env_type, &[], &[]);
    let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
    let mut child = crate::agent::spawn::spawn_child(&pair, command).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("probe.txt")).unwrap(),
        format!("{}\n8123\n{payload}\n", resolved.spawn_path)
    );
}

#[test]
#[cfg(windows)]
#[ignore = "requires an installed default WSL distribution"]
fn live_wsl_directory_runs_windows_cmd_harness() {
    let home = crate::env::wsl_home().unwrap();
    let directory = tempfile::Builder::new()
        .prefix("buildmesh-native-")
        .tempdir_in(crate::env::to_host_path(&home.to_string_lossy()))
        .unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let script = script_dir.path().join("native probe.cmd");
    std::fs::write(&script, "@echo off\r\necho native-in-guest> probe.txt\r\n").unwrap();
    let mut resolved = crate::env::resolve_raw_path(directory.path().to_str().unwrap());
    crate::env::apply_harness_runtime(&mut resolved, crate::models::EnvType::Windows);
    let recipe = crate::agent::provider::SpawnRecipe {
        binary: "probe",
        base_args: vec![],
        trailing_args: vec![],
        windows_shell: crate::agent::provider::WindowsShell::Cmd,
    };
    let command = super::wrap(
        recipe,
        resolved.env_type,
        None,
        script.to_str(),
        &resolved.spawn_path,
        8124,
        false,
    )
    .unwrap();
    let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
    let mut child = crate::agent::spawn::spawn_child(&pair, command).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("probe.txt"))
            .unwrap()
            .trim(),
        "native-in-guest"
    );
}

/// The same Windows-runtime-in-a-WSL-directory node as
/// `live_wsl_directory_runs_windows_cmd_harness`, but on the
/// `WindowsShell::PowerShell` arm Codex uses. PowerShell runs the `.cmd`
/// shim by delegating to cmd.exe, which rejects the UNC directory and
/// starts the agent in `C:\Windows`; the fix maps the share with `pushd`
/// first, so the probe must land in the node's directory.
#[test]
#[cfg(windows)]
#[ignore = "requires an installed default WSL distribution"]
fn live_wsl_directory_runs_windows_powershell_harness_in_the_node_directory() {
    let home = crate::env::wsl_home().unwrap();
    let directory = tempfile::Builder::new()
        .prefix("buildmesh-native-ps-")
        .tempdir_in(crate::env::to_host_path(&home.to_string_lossy()))
        .unwrap();
    let npm_dir = tempfile::tempdir().unwrap();
    let cmd_shim = npm_dir.path().join("probe.cmd");
    std::fs::write(
        &cmd_shim,
        "@echo off\r\necho shim-ran-in-node-dir> probe.txt\r\necho %~1> prompt.txt\r\n",
    )
    .unwrap();
    let mut resolved = crate::env::resolve_raw_path(directory.path().to_str().unwrap());
    crate::env::apply_harness_runtime(&mut resolved, crate::models::EnvType::Windows);
    let prompt = "Review homelab. ".repeat(240);
    let recipe = crate::agent::provider::SpawnRecipe {
        binary: "probe",
        base_args: vec![prompt.clone()],
        trailing_args: vec![],
        windows_shell: crate::agent::provider::WindowsShell::PowerShell,
    };
    let command = super::wrap(
        recipe,
        resolved.env_type,
        None,
        cmd_shim.to_str(),
        &resolved.spawn_path,
        8125,
        false,
    )
    .unwrap();
    run_and_expect_probe(&command, &directory.path().join("probe.txt"));
    // Existence first: a `read_to_string().unwrap()` here would report
    // NotFound, which is exactly the symptom and hides the cause.
    let probe = directory.path().join("probe.txt");
    assert!(
        probe.is_file(),
        "the .cmd shim never wrote into the node directory at {} — cmd.exe rejected the \
             UNC working directory and ran in its own default instead",
        probe.display()
    );
    assert_eq!(
        std::fs::read_to_string(&probe).unwrap().trim(),
        "shim-ran-in-node-dir"
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("prompt.txt"))
            .unwrap()
            .trim_end(),
        prompt.trim_end()
    );
}

/// The command `wrap` built, so a test can assert on the working directory as
/// well as the argv. `CommandBuilder::get_cwd` is the only place the
/// decision is observable — a mapped spawn leaves it unset on purpose.
fn wrap_command(
    executable: &str,
    spawn_path: &str,
    env_type: EnvType,
    windows_shell: WindowsShell,
) -> portable_pty::CommandBuilder {
    let recipe = SpawnRecipe {
        binary: "codex",
        base_args: vec!["--sandbox".into(), "danger-full-access".into()],
        trailing_args: vec![],
        windows_shell,
    };
    super::wrap(
        recipe,
        env_type,
        None,
        Some(executable),
        spawn_path,
        8126,
        false,
    )
    .unwrap()
}

/// The argv `wrap` built, so a test can assert on the chosen shell as well
/// as on the decoded `-EncodedCommand` payload.
fn wrap_argv(
    executable: &str,
    spawn_path: &str,
    env_type: EnvType,
    windows_shell: WindowsShell,
) -> Vec<String> {
    wrap_command(executable, spawn_path, env_type, windows_shell)
        .get_argv()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

/// The whole batch file a mapped command runs.
fn batch_file(command: &portable_pty::CommandBuilder) -> String {
    let path = mapped_batch_path(command).expect("the mapped arm passes a batch file to cmd.exe");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading the mapped batch {}: {error}", path.display()))
}

/// The decoded PowerShell script, wherever in the command it ended up: the
/// plain arm carries it as an argument, the mapped arm buries it in a batch
/// file that argv never shows.
fn encoded_script(command: &portable_pty::CommandBuilder) -> String {
    let argv: Vec<String> = command
        .get_argv()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if let Some(index) = argv.iter().position(|arg| arg == "-EncodedCommand") {
        return decode_ps(&argv[index + 1]);
    }
    // Mapped arm: the original script is beside the batch, at the host
    // spelling of the Windows path in argv.
    let batch = mapped_batch_path(command).expect("the mapped arm passes a batch file to cmd.exe");
    std::fs::read_to_string(batch.with_extension("ps1"))
        .expect("the mapped batch has a companion PowerShell script")
}

/// Assert the spawn maps the UNC directory through cmd's ``pushd`` without
/// ever putting the directory on a command line.
///
/// The absence assertions are the load-bearing ones. portable-pty quotes an
/// argument only when it contains whitespace or a quote, so ``&``, ``|``,
/// ``<``, ``>``, ``^``, ``(``, ``)`` and ``%`` in a node's directory reach
/// cmd.exe bare, where cmd treats them as operators — a node in ``R&D``
/// would run whatever followed the ampersand. Quoting inside the argument
/// is not available either: portable-pty escapes a ``"`` inside an
/// argument, so a pre-quoted path arrives as ``"\"<dir>\""`` and ``pushd``
/// rejects the line outright. The directory is therefore quoted inside
/// the batch, with percent signs doubled to prevent expansion. Only the
/// batch file's path travels on the outer command line.
fn assert_maps_through_pushd(
    command: &portable_pty::CommandBuilder,
    spawn_path: &str,
    script: &str,
) {
    let argv: Vec<String> = command
        .get_argv()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        argv.first().map(String::as_str),
        Some("cmd.exe"),
        "a batch shim in a UNC directory has to be reached through cmd.exe, because \
             PowerShell delegates to it and cmd.exe is what rejects the directory; got: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg.contains(spawn_path)),
        "the node directory must not appear on the command line at all: cmd parses \
             & | < > ^ ( ) % in an argument as operators once portable-pty leaves it \
             unquoted. It belongs quoted inside the batch file; got: {argv:?}"
    );
    assert!(
        batch_file(command).contains(&format!("pushd \"{}\"", spawn_path.replace('%', "%%"))),
        "the batch must pushd the node directory as one quoted argument, which is the \
             only form cmd parses without splitting on a metacharacter; got: {}",
        batch_file(command)
    );
    assert!(
        !script.contains("Set-Location"),
        "once pushd has mapped the share, re-setting the location to the UNC form would \
             hand the inner cmd.exe the directory it rejects; got: {script}"
    );
}
/// Actually runs the mapped form: cmd.exe `pushd`s the directory and the
/// PowerShell inside it writes a probe.
///
/// This is the test that catches a malformed argument, and it is the one
/// the earlier revision was missing — `get_argv()` is the serialiser's
/// *input*, so an argv assertion cannot see quoting applied afterwards. A
/// directory with a space is used deliberately, because that is the case
/// that tempts a fix into pre-quoting the path.
///
/// The directory is local, so the test needs neither WSL nor a share and
/// runs on any Windows machine. `pushd` treats a local path and a UNC one
/// the same way, which is the whole of the difference that matters here.
///
/// It runs through `spawn_child`, the production spawn, rather than
/// re-serialising the argv through `std::process::Command`. That re-serialisation
/// tests an equivalent quoting algorithm, not the one that actually runs:
/// portable-pty has its own. Reintroducing the pre-quoted `pushd` argument
/// fails this test with cmd's own error.
#[test]
#[cfg(windows)]
fn mapped_powershell_spawn_runs_the_script_in_the_mapped_directory() {
    // A space, which portable-pty quotes on its own.
    let directory = tempfile::Builder::new()
        .prefix("bm mapped pushd ")
        .tempdir()
        .unwrap();
    let script = "[IO.File]::WriteAllText((Join-Path ($PWD).Path 'probe.txt'), 'mapped')";
    let command = super::powershell_spawn(
        EnvType::Windows,
        script,
        Some(directory.path().to_str().unwrap()),
    )
    .expect("the mapped spawn should be buildable");
    run_and_expect_probe(&command, &directory.path().join("probe.txt"));
    assert_eq!(
        std::fs::read_to_string(directory.path().join("probe.txt")).unwrap(),
        "mapped"
    );
}

/// The same run with a directory name carrying `&` and **no** space.
///
/// The space is what makes this matter: portable-pty quotes an argument
/// that contains whitespace, so a round-3 test whose directory had one
/// would have passed even with the directory on the command line. `&` with
/// no whitespace is the case cmd splits on, and the one the node's real
/// directory decides.
#[test]
#[cfg(windows)]
fn mapped_powershell_spawn_survives_shell_metacharacters_in_the_directory() {
    // `&` is legal in a Windows directory name and is the character cmd
    // splits on. (`|` and `>` are not legal at all, so they cannot be
    // tested this way — which is a small comfort: a node's real directory
    // cannot contain them either.)
    let directory = tempfile::Builder::new()
        .prefix("bm&R&D-")
        .tempdir()
        .unwrap();
    assert!(
        !directory.path().to_string_lossy().contains(' '),
        "a space here would let portable-pty quote the argument and hide the defect \
             this test exists to catch"
    );
    let script = "[IO.File]::WriteAllText((Join-Path ($PWD).Path 'probe.txt'), 'mapped')";
    let command = super::powershell_spawn(
        EnvType::Windows,
        script,
        Some(directory.path().to_str().unwrap()),
    )
    .expect("the mapped spawn should be buildable");
    run_and_expect_probe(&command, &directory.path().join("probe.txt"));
}

/// The batch must fail closed when the share cannot be mapped.
///
/// The alternative is the failure this guard exists for: the agent starts in
/// whatever directory cmd fell back to and reports success, so the node
/// looks launched while silently working somewhere else. The host is on a
/// reserved `.invalid` TLD and resolves to NXDOMAIN, so `pushd` fails
/// immediately instead of waiting on a real network share.
#[test]
#[cfg(windows)]
fn mapped_spawn_of_an_unmappable_directory_fails_closed() {
    let unmappable = ["\\\\", "nope.invalid", "share", "directory"].join("\\");
    let command = super::powershell_spawn(EnvType::Windows, "exit 0", Some(&unmappable))
        .expect("the mapped spawn should be buildable");
    let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
    let mut child = crate::agent::spawn::spawn_child(&pair, command.clone()).unwrap();
    let success = child.wait().unwrap().success();
    remove_mapped_batch(&command);
    assert!(
        !success,
        "a directory cmd cannot pushd must fail the spawn rather than start the agent \
             somewhere else and report success"
    );
}

/// The same runtime check for the interop route, through `wrap` rather than
/// the builder, so the `Set-Location`/`exit` wrapper is exercised too. It
/// needs no WSL: a drive-letter directory is not mapped, so the interop arm
/// takes its normal path and the agent still has to land in the node's
/// directory.
#[test]
#[cfg(windows)]
fn interop_drive_letter_spawn_runs_in_the_node_directory() {
    let directory = tempfile::Builder::new()
        .prefix("bm R&D interop ")
        .tempdir()
        .unwrap();
    let shim = directory.path().join("probe.cmd");
    std::fs::write(&shim, "@echo off\r\necho shim-ran> probe.txt\r\n").unwrap();
    let command = wrap_command(
        shim.to_str().unwrap(),
        directory.path().to_str().unwrap(),
        EnvType::WindowsInterop,
        WindowsShell::PowerShell,
    );
    run_and_expect_probe(&command, &directory.path().join("probe.txt"));
}

/// Spawn `command` through the production PTY path and assert it wrote the
/// probe where it was supposed to be, then remove the batch file the child
/// could not remove for itself.
#[cfg(windows)]
fn run_and_expect_probe(command: &portable_pty::CommandBuilder, probe: &std::path::Path) {
    let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
    let mut child = crate::agent::spawn::spawn_child(&pair, command.clone()).unwrap();
    let status = child.wait().unwrap().success();
    // The batch cannot delete itself — cmd reads it incrementally, so a
    // `del "%~f0"` leaves cmd unable to read the next line — and in
    // production the file is swept by age instead. A test knows its child
    // has exited, so it can clean up directly rather than leaving 28 files
    // behind per run.
    remove_mapped_batch(command);
    assert!(
        status,
        "the mapped spawn exited non-zero, so pushd or the agent failed"
    );
    assert!(
        probe.is_file(),
        "no probe at {}. The node directory never took effect: a directory name \
             containing & is a command separator to cmd.exe unless the mapping script \
             quotes it, so this is the metacharacter defect.",
        probe.display()
    );
}

/// The batch file a mapped command runs, as a path **this host** can open.
///
/// The argv entry is a Windows path — it has to be, since it is what
/// `cmd.exe` is told — but a Linux host opens the same file through its
/// host spelling. Converting is not belt-and-braces: reading the Windows
/// spelling directly is exactly what broke these tests on Linux CI.
///
/// Not gated on Windows: this is path inspection with no platform API, and
/// the structural tests that call it run on every platform.
fn mapped_batch_path(command: &portable_pty::CommandBuilder) -> Option<std::path::PathBuf> {
    let windows = command.get_argv().last()?;
    let host = crate::env::to_host_path(&windows.to_string_lossy());
    // Structural tests can build the native Windows arm on Linux: its
    // batch path combines the Linux temp directory with a Windows separator.
    let host = if cfg!(windows) {
        host
    } else {
        host.replace('\\', "/")
    };
    let host = std::path::PathBuf::from(host);
    let is_batch = host.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .starts_with(super::MAPPED_BATCH_PREFIX)
    });
    is_batch.then_some(host)
}

/// Remove a batch file a test created but never ran. The child cannot clean
/// up after itself and production reclaims these by age, so a test that only
/// inspects the command would otherwise leave one behind per run.
fn remove_mapped_batch(command: &portable_pty::CommandBuilder) {
    if let Some(batch) = mapped_batch_path(command) {
        let _ = std::fs::remove_file(batch.with_extension("ps1"));
        let _ = std::fs::remove_file(batch);
    }
}

#[test]
#[cfg(windows)]
fn mapped_long_script_preserves_unicode_and_literal_prompt_text() {
    let directory = tempfile::Builder::new()
        .prefix("bm R&D long script ")
        .tempdir()
        .unwrap();
    let prompt = "homelab: café 🦀 'quoted' `code` $env:PATH <tag> | & ;\n".repeat(80);
    let script = format!(
        "[IO.File]::WriteAllText((Join-Path ($PWD).Path 'probe.txt'), {})",
        super::ps_single_quote(&prompt)
    );
    assert!(
        super::powershell_command_line_chars(&super::encode_for_powershell(&script))
            > super::CMD_EXE_COMMAND_LINE_CHARS
    );
    let command = super::powershell_spawn(
        EnvType::Windows,
        &script,
        Some(directory.path().to_str().unwrap()),
    )
    .expect("a long script should be loaded from disk");
    let batch = batch_file(&command);
    assert!(batch
        .lines()
        .all(|line| line.encode_utf16().count() <= super::CMD_EXE_COMMAND_LINE_CHARS));
    run_and_expect_probe(&command, &directory.path().join("probe.txt"));
    assert_eq!(
        std::fs::read_to_string(directory.path().join("probe.txt")).unwrap(),
        prompt
    );
}

#[test]
#[cfg(windows)]
fn powershell_prompt_preserves_smart_apostrophes_through_the_pty() {
    let prompt = "don't; don\u{2018}t; don\u{2019}t; don\u{201a}t; don\u{201b}t";
    for mapped in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let probe = directory.path().join("probe.txt");
        let script = format!(
            "[IO.File]::WriteAllText({}, {})",
            super::ps_single_quote(probe.to_str().unwrap()),
            super::ps_single_quote(prompt)
        );
        let command = super::powershell_spawn(
            EnvType::Windows,
            &script,
            mapped.then_some(directory.path().to_str().unwrap()),
        )
        .unwrap();
        run_and_expect_probe(&command, &probe);
        assert_eq!(std::fs::read_to_string(&probe).unwrap(), prompt);
    }
}

#[test]
#[cfg(windows)]
fn mapped_percent_directory_reaches_the_harness_literally() {
    for prefix in ["bm 50%off ", "bm %USERNAME% "] {
        let directory = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let command = super::powershell_spawn(
            EnvType::Windows,
            "[IO.File]::WriteAllText((Join-Path ($PWD).Path 'probe.txt'), 'literal directory')",
            Some(directory.path().to_str().unwrap()),
        )
        .unwrap();
        let probe = directory.path().join("probe.txt");
        run_and_expect_probe(&command, &probe);
        assert_eq!(
            std::fs::read_to_string(&probe).unwrap(),
            "literal directory"
        );
    }
}

#[test]
#[cfg(windows)]
fn mapped_loader_propagates_child_failure_and_missing_script_errors() {
    let directory = tempfile::tempdir().unwrap();
    for remove_script in [false, true] {
        let command = super::powershell_spawn(
            EnvType::Windows,
            "cmd.exe /d /c exit 23",
            Some(directory.path().to_str().unwrap()),
        )
        .unwrap();
        if remove_script {
            std::fs::remove_file(mapped_batch_path(&command).unwrap().with_extension("ps1"))
                .unwrap();
        }
        let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
        let mut child = crate::agent::spawn::spawn_child(&pair, command.clone()).unwrap();
        let status = child.wait().unwrap();
        remove_mapped_batch(&command);
        assert!(!status.success());
        if !remove_script {
            assert_eq!(status.exit_code(), 23);
        }
    }
}

/// The mapped interop route end to end: Buildmesh on the Linux host, a
/// Windows-side batch shim, and a node directory reached through the WSL
/// boundary.
///
/// The companion files must be written through a host path Windows can
/// open and referenced by their Windows spelling. The directory is quoted
/// in the batch with percent signs escaped; it needs no directory variable
/// forwarded through WSLENV. A long prompt must reach the shim intact in
/// the mapped directory. This test needs a Linux Buildmesh host in WSL to
/// exercise interop process launch and PTY behavior.
#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires WSL with Windows interop enabled"]
fn live_wsl_mapped_interop_spawn_runs_in_the_node_directory() {
    assert!(crate::env::is_wsl_host());
    // The Windows user profile is what makes this test possible at all: the
    // batch file has to land somewhere Windows can open. A bare `unwrap`
    // here would report only "called Option::unwrap() on a None value", so
    // name the real cause — `windows_home` runs PowerShell through interop
    // under a 10s timeout, and a cold WSL start can miss it.
    let windows_temp = crate::env::windows_cli_home("AppData/Local/Temp").unwrap_or_else(|| {
        panic!(
            "the Windows user profile did not resolve from WSL, so no Windows-visible \
                 batch file can be written. PowerShell interop either failed or missed its \
                 10s timeout; a warm start usually resolves this."
        )
    });
    let npm_dir = tempfile::tempdir_in(&windows_temp).unwrap();
    let shim = npm_dir.path().join("probe.cmd");
    std::fs::write(
        &shim,
        "@echo off\r\necho %~1> prompt.txt\r\necho shim-ran> probe.txt\r\n",
    )
    .unwrap();
    // PowerShell is a Windows process, so it needs the *Windows* spelling
    // of the shim. `windows_cli_home` returns the host spelling — correct
    // for writing the file, wrong for telling Windows where it is — which
    // is the same trap the batch file had.
    let shim_windows = crate::env::windows_path_from_wsl(shim.to_str().unwrap());
    // A WSL-side directory whose Windows form is UNC, with an `&` in the
    // name so a directory that reached cmd.exe on the command line would be
    // split rather than mapped.
    let directory = tempfile::Builder::new()
        .prefix("buildmesh mapped interop R&D 50%off %USERNAME%-")
        .tempdir()
        .unwrap();
    let spawn_path = crate::env::windows_path_from_wsl(directory.path().to_str().unwrap());
    let prompt = "Review homelab. ".repeat(240);
    let full_script =
        super::format_powershell_command(&shim_windows, std::slice::from_ref(&prompt));
    assert!(
        super::powershell_command_line_chars(&super::encode_for_powershell(&full_script))
            > super::CMD_EXE_COMMAND_LINE_CHARS
    );
    let recipe = SpawnRecipe {
        binary: "probe",
        base_args: vec![prompt.clone()],
        trailing_args: vec![],
        windows_shell: WindowsShell::PowerShell,
    };
    let mut command = super::wrap(
        recipe,
        EnvType::WindowsInterop,
        None,
        Some(&shim_windows),
        &spawn_path,
        8127,
        false,
    )
    .expect("an unsandboxed command always assembles");
    apply_wsl_env(&mut command, EnvType::WindowsInterop, &[], &[]);
    // The batch file carries the directory itself, quoted, so this route
    // needs no WSLENV entry for it and cannot be broken by a forwarding
    // regression. Assert the batch is where cmd.exe can actually open it:
    // a Windows path on the command line, even though Buildmesh wrote the
    // file through the host spelling.
    let batch = command
        .get_argv()
        .last()
        .map(|arg| arg.to_string_lossy().into_owned())
        .expect("the mapped spawn passes a batch file to cmd.exe");
    assert!(
        batch.starts_with(r"C:\") || batch.contains(":\\"),
        "cmd.exe must be given a Windows path, not the host spelling; got: {batch}"
    );
    assert!(
        std::path::Path::new(&crate::env::to_host_path(&batch)).exists(),
        "the batch file was written through the host spelling, which is the same directory; \
             it should be openable from here: {batch}"
    );
    let script = std::fs::read_to_string(crate::env::to_host_path(&batch)).unwrap();
    assert!(
        script.contains(&format!("pushd \"{}\"", spawn_path.replace('%', "%%"))),
        "the batch must pushd the node directory quoted, which is the only form cmd parses \
             without splitting on a metacharacter; got: {script}"
    );
    let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
    // Stands in for xterm.js: the only thing that answers the child's
    // cursor-position query. `take_writer` may be called once per master.
    let mut console = pair
        .master
        .take_writer()
        .expect("a PTY master hands out one writer");
    let mut master = pair.master.try_clone_reader().unwrap();
    let mut child = crate::agent::spawn::spawn_child(&pair, command.clone()).unwrap();
    // Drain the PTY. Without this the console output is discarded, and a
    // failure inside the batch — `pushd` refusing a share, `powershell.exe`
    // not resolving — is invisible: all the test can report is that the
    // probe never appeared.
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = std::sync::Arc::clone(&captured);
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Ok(mut text) = sink.lock() {
                text.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
    });
    // Poll for the probe rather than waiting on the child. cmd.exe holds the
    // console open for the life of an interactive agent — that is normal
    // for a node, not a failure — so `wait()` would never return here even
    // on a completely successful spawn. What matters is whether the shim
    // ran *in the node's directory*, which is what the probe records.
    //
    // Answer the cursor-position query while polling. cmd.exe opens with
    // `ESC[6n` ("Device Status Report": where is your cursor?) and blocks
    // until a terminal replies. In the app that reply comes from xterm.js
    // in the frontend; a bare PTY has no terminal behind it and so nobody
    // answers, which is the whole of this test's hang. Answering it here is
    // emulating the frontend, not patching the spawn.
    const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";
    use std::io::Write as _;
    // Reply once. The captured text keeps containing the query, so a sticky
    // check spams the reply and buries the real output under repaints.
    let mut answered_cursor_query = false;
    let probe = directory.path().join("probe.txt");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        if probe.is_file() {
            break;
        }
        if !answered_cursor_query {
            let asked = captured
                .lock()
                .map(|text| text.contains("\u{1b}[6n"))
                .unwrap_or(false);
            if asked {
                answered_cursor_query = true;
                if console.write_all(CURSOR_REPLY).is_err() {
                    break;
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let ran_in_node_directory = probe.is_file();
    let _ = child.kill();
    drop(pair);
    let _ = reader.join();
    remove_mapped_batch(&command);
    // Tail only: the console is full of repaint escapes once PowerShell is
    // up, and the error that matters is the last thing it printed.
    let tail: String = captured
        .lock()
        .map(|text| {
            text.chars()
                .rev()
                .take(1500)
                .collect::<String>()
                .chars()
                .rev()
                .collect()
        })
        .unwrap_or_default();
    assert!(
        ran_in_node_directory,
        "the mapped interop spawn did not run the shim in the node directory at {}.\n\
             Console output (tail):\n{tail}",
        directory.path().display()
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("prompt.txt"))
            .unwrap()
            .trim_end(),
        prompt.trim_end()
    );
}

/// The batch path is passed as `cmd.exe /c` command text, which cmd expands
/// before it runs. Unlike the node directory inside the batch, this path
/// cannot rely on doubled percent signs in the file.
#[test]
fn a_batch_path_carrying_percent_or_bang_is_refused() {
    for path in [
        r"C:\Users\%USERNAME%\Temp\launch.cmd",
        r"C:\Users\me!x\Temp\launch.cmd",
        r"C:\Users\R&D\Temp\launch.cmd",
        r"C:\Users\a|b\Temp\launch.cmd",
        r"C:\Users\a(b)\Temp\launch.cmd",
        r"C:\Users\a>b\Temp\launch.cmd",
        r"C:\Users\a^b\Temp\launch.cmd",
        "C:\\Users\\a\"b\\Temp\\launch.cmd",
    ] {
        assert!(
            super::has_cmd_metacharacter(path),
            "{path} reaches cmd.exe as command text and must be refused"
        );
    }
    assert!(
        !super::has_cmd_metacharacter(r"C:\Users\alond\AppData\Local\Temp\launch.cmd"),
        "an ordinary temp path must still be accepted"
    );
}

/// A `Direct` shell establishes no location of its own, so the spawn must
/// carry the node's working directory. Suppressing it makes portable-pty
/// fall back to `USERPROFILE` and opens the agent in the home folder —
/// which is what regressed for every non-mapped arm.
#[test]
fn direct_shell_keeps_the_unc_working_directory() {
    let argv = wrap_argv(
        "claude.exe",
        UNC_SPAWN_PATH,
        EnvType::Windows,
        WindowsShell::Direct,
    );
    assert_eq!(argv.first().map(String::as_str), Some("claude.exe"));

    let command = wrap_command(
        "claude.exe",
        UNC_SPAWN_PATH,
        EnvType::Windows,
        WindowsShell::Direct,
    );
    assert_eq!(
        command
            .get_cwd()
            .map(|cwd| cwd.to_string_lossy().into_owned()),
        Some(UNC_SPAWN_PATH.to_string()),
        "a Direct shell never calls Set-Location or pushd, so it depends entirely on \
             the inherited working directory"
    );
}

/// PowerShell runs a native executable directly and accepts a UNC current
/// directory, so this arm maps nothing — and must therefore still receive
/// the directory.
#[test]
fn powershell_spawn_of_a_native_executable_keeps_the_unc_working_directory() {
    let bin_dir = tempfile::tempdir().unwrap();
    let native = bin_dir.path().join("codex.exe");
    std::fs::write(&native, "MZ").unwrap();

    let command = wrap_command(
        native.to_str().unwrap(),
        UNC_SPAWN_PATH,
        EnvType::Windows,
        WindowsShell::PowerShell,
    );

    assert_eq!(
        command
            .get_cwd()
            .map(|cwd| cwd.to_string_lossy().into_owned()),
        Some(UNC_SPAWN_PATH.to_string()),
        "this arm neither pushds nor Set-Locations, so without the cwd the agent opens \
             in USERPROFILE instead of the node's directory"
    );
}

/// The mapped arm drops the working directory, because `pushd` replaces it
/// with a drive-letter mapping. The arms that must *keep* theirs are
/// covered by the tests above.
#[test]
fn mapped_spawn_drops_the_working_directory() {
    let npm_dir = tempfile::tempdir().unwrap();
    let shim = npm_dir.path().join("codex.cmd");
    std::fs::write(&shim, "@echo off\r\n").unwrap();

    let command = wrap_command(
        shim.to_str().unwrap(),
        UNC_SPAWN_PATH,
        EnvType::Windows,
        WindowsShell::PowerShell,
    );

    assert!(
        command.get_cwd().is_none(),
        "cmd.exe must not be handed the UNC it is about to pushd, or it prints \
             'Defaulting to Windows directory' and the spawn looks broken"
    );
    remove_mapped_batch(&command);
}

/// The interop arms keep the working directory they have always had for a
/// drive-letter directory; only the mapped case drops it.
#[test]
fn interop_drive_letter_spawn_keeps_its_working_directory() {
    let npm_dir = tempfile::tempdir().unwrap();
    let shim = npm_dir.path().join("codex.cmd");
    std::fs::write(&shim, "@echo off\r\n").unwrap();

    let command = wrap_command(
        shim.to_str().unwrap(),
        npm_dir.path().to_str().unwrap(),
        EnvType::WindowsInterop,
        WindowsShell::PowerShell,
    );

    assert!(
        command.get_cwd().is_some(),
        "a drive-letter interop directory is not the shape this change addresses, and \
             the in-script Set-Location must not become the only thing establishing it"
    );
}

/// A generic UNC share, not a WSL one: the behaviour under test is "the
/// spawn directory is UNC", which cmd.exe rejects for any share, so the
/// test stays hermetic and platform-independent.
const UNC_SPAWN_PATH: &str = r"\\server\share\project";

/// The regression, native arm: Codex is the one `WindowsShell::PowerShell`
/// holder, and a Windows node whose directory is a UNC host path is exactly
/// the shape that lost its working directory.
#[test]
#[cfg(windows)]
fn powershell_spawn_in_a_unc_directory_maps_it_through_pushd() {
    let npm_dir = tempfile::tempdir().unwrap();
    let shim = npm_dir.path().join("codex.cmd");
    std::fs::write(&shim, "@echo off\r\n").unwrap();

    let command = wrap_command(
        shim.to_str().unwrap(),
        UNC_SPAWN_PATH,
        EnvType::Windows,
        WindowsShell::PowerShell,
    );
    let script = encoded_script(&command);
    assert_maps_through_pushd(&command, UNC_SPAWN_PATH, &script);
    remove_mapped_batch(&command);
    assert!(
        script.contains(&shim.to_string_lossy().into_owned()),
        "the agent is still the resolved .cmd shim — the fix maps the directory rather \
             than substituting a different executable; got: {script}"
    );
}

/// The same shape on the `WindowsInterop` arm, where Buildmesh runs on the
/// Linux host. A filesystem probe cannot answer there (a Windows path is
/// not on the Linux filesystem), so this arm must be driven by the command
/// itself. `CodexProxy` is the route that puts a `.cmd` here at all, and it
/// is the one that was still broken.
#[test]
#[cfg(windows)]
fn windows_interop_powershell_spawn_in_a_unc_directory_maps_it_through_pushd() {
    let npm_dir = tempfile::tempdir().unwrap();
    let shim = npm_dir.path().join("codex.cmd");
    std::fs::write(&shim, "@echo off\r\n").unwrap();

    let command = wrap_command(
        shim.to_str().unwrap(),
        UNC_SPAWN_PATH,
        EnvType::WindowsInterop,
        WindowsShell::PowerShell,
    );
    let script = encoded_script(&command);
    assert_maps_through_pushd(&command, UNC_SPAWN_PATH, &script);
    remove_mapped_batch(&command);
    assert!(
        script.contains(&shim.to_string_lossy().into_owned()),
        "the interop arm must still launch the resolved shim; got: {script}"
    );
}

/// The control that keeps this narrow: a drive-letter directory is not the
/// shape cmd.exe rejects, so it must keep today's direct launch.
#[test]
fn powershell_spawn_in_a_drive_letter_directory_is_not_mapped() {
    let npm_dir = tempfile::tempdir().unwrap();
    let shim = npm_dir.path().join("codex.cmd");
    std::fs::write(&shim, "@echo off\r\n").unwrap();

    let argv = wrap_argv(
        shim.to_str().unwrap(),
        npm_dir.path().to_str().unwrap(),
        EnvType::Windows,
        WindowsShell::PowerShell,
    );

    assert_eq!(
        argv.first().map(String::as_str),
        Some("powershell.exe"),
        "a drive-letter directory launches through the shim exactly as before; got: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg == "pushd"),
        "no share mapping is needed for a local directory; got: {argv:?}"
    );
}

/// A native executable never reaches cmd.exe — PowerShell runs it directly
/// and accepts a UNC current directory — so it must not be mapped.
#[test]
fn powershell_spawn_of_a_native_executable_is_not_mapped() {
    let bin_dir = tempfile::tempdir().unwrap();
    let native = bin_dir.path().join("codex.exe");
    std::fs::write(&native, "MZ").unwrap();

    let command = wrap_command(
        native.to_str().unwrap(),
        UNC_SPAWN_PATH,
        EnvType::Windows,
        WindowsShell::PowerShell,
    );
    let script = encoded_script(&command);

    assert_eq!(
        command
            .get_argv()
            .first()
            .map(|arg| arg.to_string_lossy().into_owned())
            .as_deref(),
        Some("powershell.exe")
    );
    assert!(
        script.contains(&native.to_string_lossy().into_owned()),
        "a .exe is executed by PowerShell directly; got: {script}"
    );
}

/// A bare stem is resolved by PowerShell itself, which prefers the `.ps1`
/// twin over the `.cmd` shim — so cmd.exe never enters the chain and there
/// is nothing to map. This is the interop non-proxy route.
#[test]
fn powershell_spawn_of_a_bare_stem_is_not_mapped() {
    let argv = wrap_argv(
        "codex",
        UNC_SPAWN_PATH,
        EnvType::WindowsInterop,
        WindowsShell::PowerShell,
    );

    assert_eq!(argv.first().map(String::as_str), Some("powershell.exe"));
    assert!(
        !argv.iter().any(|arg| arg == "pushd"),
        "PowerShell resolves a bare stem without cmd.exe, so the UNC directory is \
             never handed to it; got: {argv:?}"
    );
}

/// Both spellings the code accepts but no test covered: an uppercase
/// batch extension, and the forward-slash UNC form external tools emit.
#[test]
#[cfg(windows)]
fn uppercase_batch_extension_and_forward_slash_unc_still_map() {
    let npm_dir = tempfile::tempdir().unwrap();
    let shim = npm_dir.path().join("codex.CMD");
    std::fs::write(&shim, "@echo off\r\n").unwrap();
    let forward_slash = "//server/share/project";

    let command = wrap_command(
        shim.to_str().unwrap(),
        forward_slash,
        EnvType::Windows,
        WindowsShell::PowerShell,
    );

    assert_maps_through_pushd(&command, forward_slash, &encoded_script(&command));
    remove_mapped_batch(&command);
}

#[test]
fn apply_wsl_env_carries_attention_callback_variables() {
    let mut command = portable_pty::CommandBuilder::new("wsl.exe");
    apply_wsl_env(&mut command, crate::models::EnvType::Wsl, &[], &[]);

    let wslenv = command
        .get_env("WSLENV")
        .expect("WSLENV should be configured for WSL")
        .to_string_lossy();
    assert!(wslenv
        .split(':')
        .any(|entry| entry.split('/').next() == Some("BUILDMESH_PORT")));
    assert!(wslenv
        .split(':')
        .any(|entry| entry.split('/').next() == Some("BUILDMESH_SESSION_ID")));
    command.env("BUILDMESH_HOOK_TOKEN", "test-token");
    apply_wsl_env(&mut command, crate::models::EnvType::Wsl, &[], &[]);
    assert!(command
        .get_env("WSLENV")
        .unwrap()
        .to_string_lossy()
        .split(':')
        .any(|entry| entry.split('/').next() == Some("BUILDMESH_HOOK_TOKEN")));
}

#[test]
fn append_to_wslenv_deduplicates_by_base_name() {
    let mut wslenv = "SSH_AUTH_SOCK/up:CODEX_HOME/u".to_string();
    append_to_wslenv(&mut wslenv, "CODEX_HOME", "/u");
    assert_eq!(wslenv, "SSH_AUTH_SOCK/up:CODEX_HOME/u");
    append_to_wslenv(&mut wslenv, "BUILDMESH_PORT", "/u");
    assert_eq!(wslenv, "SSH_AUTH_SOCK/up:CODEX_HOME/u:BUILDMESH_PORT/u");
}

/// #2034 — the end-to-end fail-closed assertion on the platform that owns
/// the Seatbelt branch. Before this, a profile-write failure logged and
/// returned a *direct* command, so the requested sandbox silently became an
/// unsandboxed launch. `Err` here is what stops that: `launch_process`
/// propagates it before `spawn_child`, so no agent process is created.
///
/// macOS-only because the branch is `cfg!(target_os = "macos")`; the
/// portable half of the contract — profile-write failure yields `Err` — is
/// pinned in `agent::sandbox` on every host.
#[cfg(target_os = "macos")]
#[test]
fn requested_sandbox_without_a_writable_profile_yields_no_command() {
    let session_id = -97_512 - (std::process::id() as i64 % 100_000);
    let profile = std::env::temp_dir().join(format!("buildmesh-sandbox-{session_id}.sb"));
    // RAII: the gate restores the previous env value and the scratch
    // directory is removed even if an assertion below panics.
    let _scratch = crate::sandbox::test_support::block_writes_at(&profile);

    let result = crate::sandbox::test_support::with_dev_gate_result(Some("1"), || {
        let recipe = crate::agent::provider::SpawnRecipe {
            binary: "claude",
            base_args: vec!["--dangerously-skip-permissions".into()],
            trailing_args: vec![],
            windows_shell: crate::agent::provider::WindowsShell::Direct,
        };
        super::wrap(
            recipe,
            EnvType::Windows,
            None,
            None,
            &std::env::temp_dir().to_string_lossy(),
            session_id,
            true,
        )
    });

    let error = match result {
        Ok(_) => panic!("a failed sandbox setup must not yield a launchable command"),
        Err(error) => error,
    };
    assert!(error.contains("sandbox setup failed"), "{error}");
    // The remediation must name the real cause — a temp-directory write
    // failure — not tell the user to re-enable a gate that was already
    // open for this to be reachable.
    assert!(error.contains("temporary directory"), "{error}");
}

/// #2034 — the developer gate is authoritative. Even with the Mesh flag on,
/// a process started without `BUILDMESH_SANDBOX=1` must build the ordinary
/// direct command rather than a `sandbox-exec` wrapper.
#[cfg(target_os = "macos")]
#[test]
fn mesh_flag_alone_does_not_reach_the_seatbelt_wrapper() {
    let command = crate::sandbox::test_support::with_dev_gate_result(None, || {
        let recipe = crate::agent::provider::SpawnRecipe {
            binary: "claude",
            base_args: vec![],
            trailing_args: vec![],
            windows_shell: crate::agent::provider::WindowsShell::Direct,
        };
        super::wrap(
            recipe,
            EnvType::Windows,
            None,
            None,
            &std::env::temp_dir().to_string_lossy(),
            -97_513 - (std::process::id() as i64 % 100_000),
            true,
        )
    })
    .expect("the unsandboxed path always assembles");
    let argv = command.get_argv();
    assert_ne!(
        argv.first().map(|s| s.to_string_lossy().into_owned()),
        Some("sandbox-exec".to_string()),
        "a persisted mesh flag must not open the sandbox in a release build: {argv:?}"
    );
}

fn decode_ps(encoded: &str) -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("valid base64");
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).expect("valid utf-16")
}

/// PowerShell's -EncodedCommand expects Base64 of UTF-16LE bytes with NO BOM.
/// A BOM (or worse, the wrong-endian BOM) prepends a U+FEFF/U+FFFE code unit to
/// the decoded command and breaks every Windows PowerShell spawn.
#[test]
fn encode_for_powershell_produces_no_bom_utf16le() {
    let encoded = encode_for_powershell("echo hi");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .expect("valid base64");

    // First two bytes must be the UTF-16LE encoding of 'e' (0x65 0x00), not a BOM.
    assert_eq!(
        &bytes[..2],
        &[0x65, 0x00],
        "leading bytes should be 'e' as UTF-16LE, not a BOM"
    );

    let decoded = decode_ps(&encoded);
    assert_eq!(decoded, "echo hi");
}

/// The formatter must use PowerShell's call operator `&` with each arg
/// wrapped in single quotes, so multi-line prefill text (e.g. selected text
/// from a Handover containing backticks, brackets, numbered lists) is
/// treated as a single string literal — not parsed as separate PowerShell
/// statements line-by-line.
///
/// Regression originally surfaced when an early issue-spawn prefill body
/// contained backticks (PowerShell's line-continuation character), which
/// PowerShell parsed as the start of a new statement and rejected with
/// `Unexpected token '<word>'`. The issue path now ships just a URL +
/// title — see memory: buildmesh-issue-spawn-url-only — but the handover
/// path still ships arbitrary multi-line text, so this guarantee still
/// matters.
#[test]
fn format_powershell_command_quotes_multiline_prefill_safely() {
    let body = "Currently, when spawning a new agent...\n\
                    1. `default_provider` (per-mesh override)\n\
                    2. Buildmesh-wide default\n\
                    3. Anthropic (hardcoded fallback)";
    let args = vec![
        "--anthropic".to_string(),
        "--prefill".to_string(),
        body.to_string(),
    ];
    let cmd_str = format_powershell_command("claude", &args);

    // Must start with the call operator so PowerShell treats it as command invocation.
    assert!(
        cmd_str.starts_with("& "),
        "command must use PowerShell call operator: {}",
        cmd_str
    );

    // The binary and every arg must be wrapped in single quotes. After the
    // leading `& 'claude' `, there must be no bare newline outside of a quoted
    // string — i.e. every newline in the prefill stays inside the single-quoted
    // arg, not at the top level of the script.
    let after_call = cmd_str.strip_prefix("& ").unwrap();
    assert!(
        after_call.starts_with("'claude'"),
        "binary must be single-quoted: {}",
        cmd_str
    );

    // The prefill body's newlines must appear inside a single-quoted region —
    // i.e. between an odd-numbered ' and the next '. We verify by checking
    // that every newline is preceded by an odd number of single quotes.
    for (i, _) in cmd_str.match_indices('\n') {
        let quotes_before = cmd_str[..i].chars().filter(|c| *c == '\'').count();
        assert!(
                quotes_before % 2 == 1,
                "newline at byte {} is outside a quoted string — PowerShell will parse it as a new statement.\nCommand: {}",
                i, cmd_str
            );
    }
}

/// Single quotes inside an argument must be escaped by doubling them ('')
/// per PowerShell single-quoted string rules.
#[test]
fn format_powershell_command_escapes_embedded_single_quotes() {
    let args = vec!["--prefill".to_string(), "it's a test".to_string()];
    let cmd_str = format_powershell_command("claude", &args);
    assert!(
        cmd_str.contains("'it''s a test'"),
        "expected doubled-quote escaping, got: {}",
        cmd_str
    );
}
