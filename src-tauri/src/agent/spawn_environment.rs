//! OS-axis seam — wraps a provider's `SpawnRecipe` in the right shell for the
//! runtime environment.
//!
//! - WSL on Windows: `wsl.exe -d <distro> --cd <path> --exec sh -lc ...`
//! - WSL on Linux: direct invocation
//! - macOS, sandbox requested: `sandbox-exec -f <profile.sb> <binary> <args...>`
//!   (Seatbelt containment to the worktree — see `agent::sandbox`, issue #497)
//! - macOS, sandbox not requested: direct invocation
//! - Windows native + PowerShell shell: `powershell.exe -NoLogo -EncodedCommand <base64>`
//!   (used by Codex so ANSI escapes propagate correctly through ConPTY)
//! - Windows native + Cmd shell: `cmd.exe /c "<binary> <args>"`
//!   (used by node-shim providers whose binary is a `.cmd` batch file)
//! - Windows native + Direct: spawn the binary directly (rare; mainly for tests)
//!
//! "Sandbox requested" means the Mesh flag is on *and* the developer gate is
//! open ([`crate::sandbox::sandbox_requested`]); the feature is experimental
//! and inert in shipped builds (#2034). Because a requested sandbox is a
//! promise, [`wrap`] is fallible: the macOS Seatbelt profile is written during
//! command assembly, so a write failure returns `Err` rather than degrading to
//! an unsandboxed spawn.
//!
//! The module also owns spawn-environment hygiene that is not a harness's to
//! decide: [`CLAUDE_SESSION_MARKER_ENV_VARS`] and the scrub that drops them
//! from both the PTY spawn ([`wrap`]) and the pipe-based background spawn
//! ([`background_command`]), so an agent Buildmesh launched from inside a
//! Claude Code session does not inherit that session's markers (#2136).

use crate::agent::provider::{SpawnRecipe, WindowsShell};
use crate::models::EnvType;
use crate::pty;
use portable_pty::CommandBuilder;

/// Environment a launching Claude Code session hands to every process it
/// spawns, so each can tell it belongs to *that* session.
///
/// Buildmesh inherits the whole set whenever the app itself was started from a
/// Claude Code session's shell — an agent running `scripts\run-dev.ps1` for
/// `/use`, `/verify` or `/verify-ui` — and then hands it straight back to the
/// agents it spawns. `CLAUDE_CODE_CHILD_SESSION` is the damaging one: Claude
/// Code reads it and starts with "Transcript saving is off — inherited
/// CLAUDE_CODE_CHILD_SESSION marker", writes no
/// `~/.claude/projects/<dir>/<session>.jsonl`, and every transcript consumer
/// downstream (a Circuit's first gate, session recovery) parks on a transcript
/// that will never exist. The rest leak the launching session's identity and,
/// for the messaging token and socket, its credentials into another agent
/// process. Issue #2136.
///
/// This is an explicit list, never a `CLAUDE*` prefix rule. Buildmesh sets
/// `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` and
/// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` on purpose for the MiniMax naming
/// side-channel (`provider_conf::minimax_backend_env`), and `CLAUDE_CONFIG_DIR`
/// for the Windows sandbox; a prefix rule would silently undo them. A
/// deliberately-set value also survives by ordering rather than by exclusion:
/// the harness environment policy and per-profile backend env are layered on
/// top in `agent::spawn::command` *after* this scrub has run.
pub const CLAUDE_SESSION_MARKER_ENV_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// Drop the launching session's markers from a PTY agent spawn.
///
/// `portable_pty::CommandBuilder` seeds itself from the live process
/// environment, so `env_remove` clears an *inherited* value, not just one this
/// process set — that is the whole mechanism, and the reason a caller cannot
/// fix this by not setting the variables.
fn strip_claude_session_markers(cmd: &mut CommandBuilder) {
    for key in CLAUDE_SESSION_MARKER_ENV_VARS {
        cmd.env_remove(key);
    }
}

/// The same scrub for the pipe-based background launches (`agent::background`:
/// session naming, circuit classifiers). Those build a `std::process::Command`
/// rather than a `CommandBuilder`, so they cannot share the loop above, and
/// they are a second instance of the same leak — a backgrounded
/// `claude --print` inherits the same markers the interactive agent must not.
fn strip_claude_session_markers_from_command(cmd: &mut std::process::Command) {
    for key in CLAUDE_SESSION_MARKER_ENV_VARS {
        cmd.env_remove(key);
    }
}

/// Encode a command string for PowerShell's -EncodedCommand parameter.
/// -EncodedCommand decodes the Base64 payload into a PowerShell *script* and
/// runs it. That avoids PowerShell's argument tokenizer (which would mangle
/// `<>()` and backticks), but the decoded text is still parsed as PowerShell,
/// so newlines and special tokens at the script's top level become statements.
/// Always pair this with [`format_powershell_command`] to keep prefill text
/// inside a single-quoted string literal.
fn encode_for_powershell(cmd: &str) -> String {
    crate::env::encode_powershell(cmd)
}

/// Quote a single PowerShell argument as a single-quoted string literal.
/// Single quotes inside the string must be doubled (`'` → `''`); everything else
/// (newlines, backticks, brackets, `$`, etc.) is preserved verbatim because
/// single-quoted PowerShell strings perform no interpolation or escapes.
fn ps_single_quote(s: &str) -> String {
    crate::env::powershell_literal(s)
}

/// Build a PowerShell script that invokes `binary` with `args`, with every
/// token wrapped in single quotes and dispatched via the call operator (`&`).
/// This ensures arguments containing newlines or PowerShell-significant chars
/// (backticks, `<>`, parentheses, `|`, `;`, `$`, `#`, `&`, brackets, list
/// markers like `1.`) are passed through as literal strings rather than being
/// parsed as new statements when the script is decoded by `-EncodedCommand`.
fn format_powershell_command(binary: &str, args: &[String]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(args.len() + 1);
    parts.push(ps_single_quote(binary));
    for a in args {
        parts.push(ps_single_quote(a));
    }
    format!("& {}", parts.join(" "))
}

/// Pipe-based background tasks still need the adapter's Windows shell for npm shims.
pub(crate) fn background_command(
    recipe: &SpawnRecipe,
    executable: Option<&std::path::Path>,
) -> std::process::Command {
    let executable = executable
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| recipe.binary.into());
    let args: Vec<String> = recipe.argv().map(str::to_owned).collect();
    let mut cmd = if cfg!(windows) && recipe.windows_shell == WindowsShell::PowerShell {
        let script = format!(
            "{}; exit $LASTEXITCODE",
            format_powershell_command(&executable, &args)
        );
        let mut cmd = crate::process_util::command_no_window("powershell.exe");
        cmd.args([
            "-NoLogo",
            "-NoProfile",
            "-EncodedCommand",
            &encode_for_powershell(&script),
        ]);
        cmd
    } else {
        let mut cmd = if cfg!(windows) && recipe.windows_shell == WindowsShell::Cmd {
            let mut cmd = crate::process_util::command_no_window("cmd.exe");
            cmd.args(["/d", "/c", &executable]);
            cmd
        } else {
            crate::process_util::command_no_window(executable)
        };
        cmd.args(args);
        cmd
    };
    // The recipe's own deliberate env is applied by the caller afterwards, so
    // scrubbing here clears only what the launching session contributed.
    strip_claude_session_markers_from_command(&mut cmd);
    // The guard signals this process's group (`kill(-pid)`). A child that
    // merely inherited our group has no group whose id is its pid, so that
    // signal is ESRCH and cancellation would leave descendants running.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

pub fn wrap(
    mut recipe: SpawnRecipe,
    env_type: EnvType,
    wsl_distro: Option<&str>,
    executable_override: Option<&str>,
    spawn_path: &str,
    session_id: i64,
    sandbox: bool,
) -> Result<CommandBuilder, String> {
    recipe
        .base_args
        .extend(std::mem::take(&mut recipe.trailing_args));
    let executable = executable_override.unwrap_or(recipe.binary);
    let mut cmd = if env_type == EnvType::WindowsInterop {
        let script = if recipe.windows_shell == WindowsShell::Cmd {
            let mut args = vec![
                "/d".into(),
                "/c".into(),
                "pushd".into(),
                spawn_path.into(),
                "&&".into(),
                executable.into(),
            ];
            args.extend(recipe.base_args.clone());
            format_powershell_command("cmd.exe", &args)
        } else {
            format!(
                "Set-Location -LiteralPath {}; {}",
                ps_single_quote(spawn_path),
                format_powershell_command(executable, &recipe.base_args)
            )
        };
        let mut command = CommandBuilder::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-EncodedCommand",
            &encode_for_powershell(&format!("{script}; exit $LASTEXITCODE")),
        ]);
        if let Ok(distro) = std::env::var("WSL_DISTRO_NAME") {
            command.env("BUILDMESH_WSL_HOST", distro);
        }
        Ok(command)
    } else if env_type == EnvType::Wsl && !cfg!(windows) {
        let mut c = CommandBuilder::new(executable);
        c.args(recipe.base_args);
        Ok(c)
    } else if env_type == EnvType::Wsl {
        tracing::info!("spawn_environment: building WSL command via wsl.exe");
        let mut c = CommandBuilder::new("wsl.exe");
        let default_distro = crate::env::get_default_wsl_distro();
        if let Some(distro) = wsl_distro.or(default_distro.as_deref()) {
            c.args(["-d", distro]);
        }
        // Use the same login environment as discovery. Positional parameters
        // preserve arbitrary prompts without evaluating them as shell code.
        c.args([
            "--cd",
            spawn_path,
            "--exec",
            "sh",
            "-lc",
            "export PATH=\"$HOME/.local/bin:$HOME/.npm-global/bin:$PATH\"; exec \"$@\"",
            "buildmesh",
            executable,
        ]);
        c.args(recipe.base_args);
        Ok(c)
    } else if cfg!(target_os = "macos") {
        // macOS Seatbelt sandbox (issue #497). When the Mesh has the sandbox
        // toggle on AND the developer gate is open, launch the agent through
        // `sandbox-exec -f <profile>` so it can only read/write the worktree
        // (see `agent::sandbox`).
        //
        // Writing the profile is the only fallible step, and this path **fails
        // closed** (#2034): a requested sandbox that cannot be set up must not
        // quietly become an unsandboxed launch. The caller turns this `Err`
        // into a spawn error the user sees, which is the only outcome that
        // keeps "sandboxed" and "sandboxing was requested" the same statement.
        if crate::sandbox::sandbox_requested(sandbox) {
            // `executable`, not `recipe.binary`: every other branch above runs
            // the resolved override, and passing the raw recipe name here
            // would confine and launch a *different* program than the one
            // routing selected. Identical when no override is present.
            crate::agent::sandbox::seatbelt_command(
                executable,
                &recipe.base_args,
                spawn_path,
                session_id,
            )
            .inspect(|_| {
                tracing::info!(
                    "spawn_environment: building sandboxed macOS command (sandbox-exec) for {}",
                    executable
                );
            })
            .map_err(|e| {
                tracing::error!(
                    "spawn_environment: failed to write Seatbelt profile for session {} ({}); \
                     refusing to launch {} unsandboxed",
                    session_id,
                    e,
                    recipe.binary
                );
                crate::agent::sandbox::setup_failure_message(session_id, recipe.binary, &e)
            })
        } else {
            tracing::info!(
                "spawn_environment: building macOS command for {}",
                executable
            );
            let mut c = CommandBuilder::new(executable);
            c.args(recipe.base_args);
            Ok(c)
        }
    } else {
        Ok(match recipe.windows_shell {
            WindowsShell::PowerShell => {
                tracing::info!(
                    "spawn_environment: building Windows powershell.exe for {}",
                    executable
                );
                // Build a PowerShell script that calls the binary with each arg
                // single-quoted, then Base64/UTF-16LE encode it for
                // -EncodedCommand. Quoting matters: multi-line prefill text
                // (e.g. handover prefills containing `1. ...` numbered lists or
                // backticks) would otherwise be parsed as separate PowerShell
                // statements after newline boundaries.
                let cmd_str = format_powershell_command(executable, &recipe.base_args);
                let encoded = encode_for_powershell(&cmd_str);
                let mut c = CommandBuilder::new("powershell.exe");
                // -NoProfile skips loading the user's PowerShell profile, which
                // can add hundreds of ms (modules, prompt frameworks) to *every*
                // agent spawn. This shell only needs to relay the agent CLI's
                // ANSI output through ConPTY — it never touches profile state,
                // and the agent binary is resolved from the process
                // environment's PATH, not the profile.
                c.args(["-NoLogo", "-NoProfile", "-EncodedCommand", &encoded]);
                c
            }
            WindowsShell::Cmd => {
                tracing::info!(
                    "spawn_environment: building Windows cmd.exe /c for {}",
                    executable
                );
                let mut c = CommandBuilder::new("cmd.exe");
                if spawn_path.starts_with("\\\\") || spawn_path.starts_with("//") {
                    // cmd cannot use a UNC current directory. Its own pushd
                    // maps the share for the lifetime of this shell.
                    c.args(["/d", "/c", "pushd", spawn_path, "&&", executable]);
                } else {
                    c.args(["/c", executable]);
                }
                c.args(recipe.base_args);
                c
            }
            WindowsShell::Direct => {
                tracing::info!(
                    "spawn_environment: building direct Windows spawn for {}",
                    executable
                );
                let mut c = CommandBuilder::new(executable);
                c.args(recipe.base_args);
                c
            }
        })
    }?;

    if (cfg!(windows) && env_type == EnvType::Wsl) || env_type == EnvType::WindowsInterop {
        cmd.cwd(crate::env::to_host_path(spawn_path));
    } else {
        cmd.cwd(spawn_path);
    }
    if crate::env::is_wsl_host() && env_type != EnvType::WindowsInterop {
        if let Some(path) = crate::agent::detection::native_wsl_path() {
            cmd.env("PATH", path);
        }
    }
    cmd.env("BUILDMESH_SESSION_ID", session_id.to_string());
    cmd.env(
        "BUILDMESH_PORT",
        crate::http_server::current_http_port().to_string(),
    );
    // Issue #1366 round-2 fix: the runtime hook token is minted
    // lazily by the Grok adapter's `provision_attention_hooks`
    // BEFORE `wrap()` runs (the orchestrator orders: provision →
    // spawn). If the token has been minted AND we are spawning a
    // descendant of that runtime, propagate it as
    // `BUILDMESH_HOOK_TOKEN` so the Grok command hook can expand it
    // into the callback request. Gated on
    // `Some(token)` so non-Grok agents never see the variable
    // (preserves the round-1 fix: Claude / Codex / AGY POST URLs
    // carry no `?token=` and the route's per-provider gate
    // recognises them as legitimate).
    if let Some(token) = crate::agent::runtime_hook_token() {
        cmd.env("BUILDMESH_HOOK_TOKEN", token);
    }
    pty::strip_git_env_vars(&mut cmd);
    // Issue #2136: a Buildmesh launched from inside a Claude Code session
    // hands that session's markers to every agent it spawns, and an inherited
    // `CLAUDE_CODE_CHILD_SESSION` turns the agent's transcript off — which
    // silently parks every downstream transcript reader. Runs before the
    // harness environment policy, so a value Buildmesh sets on purpose is
    // layered back on afterwards.
    strip_claude_session_markers(&mut cmd);
    pty::apply_interactive_tty_env(&mut cmd);

    Ok(cmd)
}

/// Carry command-defined and adapter-declared environment variables across a
/// WSL boundary. The adapter owns the inherited-variable declaration; this
/// module owns the platform-specific `WSLENV` representation.
pub(crate) fn apply_wsl_env(
    cmd: &mut CommandBuilder,
    env_type: EnvType,
    command_variables: &[&str],
    inherited_variables: &[&str],
) {
    if !matches!(env_type, EnvType::Wsl | EnvType::WindowsInterop) {
        return;
    }
    let direction = if env_type == EnvType::WindowsInterop {
        "/w"
    } else {
        "/u"
    };
    let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
    // `wrap` installs these callback values on the outer `wsl.exe` command.
    // They must also be listed in WSLENV or the guest hook process cannot see
    // the port/session that identifies its attention callback (issue #1366).
    //
    for key in ["BUILDMESH_PORT", "BUILDMESH_SESSION_ID"] {
        set_wslenv_direction(&mut wslenv, key, direction);
    }
    if cmd.get_env("BUILDMESH_HOOK_TOKEN").is_some() {
        set_wslenv_direction(&mut wslenv, "BUILDMESH_HOOK_TOKEN", direction);
    }
    for key in command_variables {
        set_wslenv_direction(&mut wslenv, key, direction);
    }
    for key in inherited_variables {
        if std::env::var_os(key).is_some() {
            set_wslenv_direction(&mut wslenv, key, direction);
        }
    }
    if env_type == EnvType::WindowsInterop {
        set_wslenv_direction(&mut wslenv, "BUILDMESH_WSL_HOST", direction);
    }
    if !wslenv.is_empty() {
        cmd.env("WSLENV", wslenv);
    }
}

fn set_wslenv_direction(wslenv: &mut String, key: &str, direction: &str) {
    let flags = wslenv
        .split(':')
        .find(|part| part.split('/').next() == Some(key))
        .and_then(|entry| entry.split_once('/'))
        .map(|(_, flags)| flags.replace(['u', 'w'], ""))
        .unwrap_or_default();
    let mut entries: Vec<_> = wslenv
        .split(':')
        .filter(|entry| !entry.is_empty() && entry.split('/').next() != Some(key))
        .map(str::to_string)
        .collect();
    entries.push(format!(
        "{key}/{flags}{}",
        direction.trim_start_matches('/')
    ));
    *wslenv = entries.join(":");
}

/// Append a WSLENV entry by base name, preserving any existing suffix flags.
pub(crate) fn append_to_wslenv(wslenv: &mut String, key: &str, suffix: &str) {
    if wslenv
        .split(':')
        .any(|part| part.split('/').next() == Some(key))
    {
        return;
    }
    let entry = format!("{key}{suffix}");
    if wslenv.is_empty() {
        wslenv.push_str(&entry);
    } else {
        wslenv.push(':');
        wslenv.push_str(&entry);
    }
}

#[cfg(test)]
mod tests {
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
        let cmd = super::background_command(&recipe, None);
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
        let mut command = super::background_command(&recipe, None);
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
            assert!(!CLAUDE_SESSION_MARKER_ENV_VARS.contains(&deliberate), "{deliberate} is set by buildmesh on purpose and must survive the session-marker scrub");
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
}
