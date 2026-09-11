//! OS-axis seam — wraps a provider's `SpawnRecipe` in the right shell for the
//! runtime environment.
//!
//! - WSL on Windows: `wsl.exe -d <distro> --cd <path> --exec sh -lc ...`
//! - WSL on Linux: direct invocation
//! - macOS, `sandbox` on: `sandbox-exec -f <profile.sb> <binary> <args...>`
//!   (Seatbelt containment to the worktree — see `agent::sandbox`, issue #497)
//! - macOS, `sandbox` off: direct invocation
//! - Windows native + PowerShell shell: `powershell.exe -NoLogo -EncodedCommand <base64>`
//!   (used by Codex so ANSI escapes propagate correctly through ConPTY)
//! - Windows native + Cmd shell: `cmd.exe /c "<binary> <args>"`
//!   (used by node-shim providers whose binary is a `.cmd` batch file)
//! - Windows native + Direct: spawn the binary directly (rare; mainly for tests)

use crate::agent::provider::{SpawnRecipe, WindowsShell};
use crate::models::EnvType;
use crate::pty;
use portable_pty::CommandBuilder;

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

pub fn wrap(
    mut recipe: SpawnRecipe,
    env_type: EnvType,
    wsl_distro: Option<&str>,
    executable_override: Option<&str>,
    spawn_path: &str,
    session_id: i64,
    sandbox: bool,
) -> CommandBuilder {
    recipe.base_args.extend(std::mem::take(&mut recipe.trailing_args));
    let executable = executable_override.unwrap_or(recipe.binary);
    let mut cmd = if env_type == EnvType::WindowsInterop {
        let script = if recipe.windows_shell == WindowsShell::Cmd {
            let mut args = vec!["/d".into(), "/c".into(), "pushd".into(), spawn_path.into(), "&&".into(), executable.into()];
            args.extend(recipe.base_args.clone());
            format_powershell_command("cmd.exe", &args)
        } else {
            format!("Set-Location -LiteralPath {}; {}", ps_single_quote(spawn_path), format_powershell_command(executable, &recipe.base_args))
        };
        let mut command = CommandBuilder::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile", "-EncodedCommand", &encode_for_powershell(&format!("{script}; exit $LASTEXITCODE"))]);
        if let Ok(distro) = std::env::var("WSL_DISTRO_NAME") { command.env("BUILDMESH_WSL_HOST", distro); }
        command
    } else if env_type == EnvType::Wsl && !cfg!(windows) {
        let mut c = CommandBuilder::new(executable);
        c.args(recipe.base_args);
        c
    } else if env_type == EnvType::Wsl {
        tracing::info!("spawn_environment: building WSL command via wsl.exe");
        let mut c = CommandBuilder::new("wsl.exe");
        let default_distro = crate::env::get_default_wsl_distro();
        if let Some(distro) = wsl_distro.or(default_distro.as_deref()) {
            c.args(["-d", distro]);
        }
        // Use the same login environment as discovery. Positional parameters
        // preserve arbitrary prompts without evaluating them as shell code.
        c.args(["--cd", spawn_path, "--exec", "sh", "-lc",
            "export PATH=\"$HOME/.local/bin:$HOME/.npm-global/bin:$PATH\"; exec \"$@\"", "buildmesh", executable]);
        c.args(recipe.base_args);
        c
    } else if cfg!(target_os = "macos") {
        // macOS Seatbelt sandbox (issue #497). When the Mesh has the sandbox
        // toggle on, launch the agent through `sandbox-exec -f <profile>` so it
        // can only read/write the worktree (see `agent::sandbox`). The profile
        // write is the only fallible step; on failure we log loudly and fall
        // back to a direct spawn rather than blocking the user — the toggle is
        // opt-in, so a direct spawn matches the off state, not a silent bypass
        // of an expected guarantee.
        if sandbox {
            match crate::agent::sandbox::seatbelt_command(
                recipe.binary,
                &recipe.base_args,
                spawn_path,
                session_id,
            ) {
                Ok(c) => {
                    tracing::info!(
                        "spawn_environment: building sandboxed macOS command (sandbox-exec) for {}",
                        executable
                    );
                    c
                }
                Err(e) => {
                    tracing::error!(
                        "spawn_environment: failed to write Seatbelt profile for session {} ({}); \
                         falling back to UNSANDBOXED direct spawn for {}",
                        session_id,
                        e,
                        recipe.binary
                    );
                    let mut c = CommandBuilder::new(executable);
                    c.args(recipe.base_args);
                    c
                }
            }
        } else {
            tracing::info!("spawn_environment: building macOS command for {}", executable);
            let mut c = CommandBuilder::new(executable);
            c.args(recipe.base_args);
            c
        }
    } else {
        match recipe.windows_shell {
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
                tracing::info!("spawn_environment: building direct Windows spawn for {}", executable);
                let mut c = CommandBuilder::new(executable);
                c.args(recipe.base_args);
                c
            }
        }
    };

    if (cfg!(windows) && env_type == EnvType::Wsl) || env_type == EnvType::WindowsInterop {
        cmd.cwd(crate::env::to_host_path(spawn_path));
    } else {
        cmd.cwd(spawn_path);
    }
    if crate::env::is_wsl_host() && env_type != EnvType::WindowsInterop {
        if let Some(path) = crate::agent::detection::native_wsl_path() { cmd.env("PATH", path); }
    }
    cmd.env("BUILDMESH_SESSION_ID", session_id.to_string());
    cmd.env("BUILDMESH_PORT", crate::http_server::current_http_port().to_string());
    // Issue #1366 round-2 fix: the runtime hook token is minted
    // lazily by the Grok adapter's `provision_attention_hooks`
    // BEFORE `wrap()` runs (the orchestrator orders: provision →
    // spawn). If the token has been minted AND we are spawning a
    // descendant of that runtime, propagate it as
    // `BUILDMESH_HOOK_TOKEN` so the Grok runner can URL-expand
    // `?token=$BUILDMESH_HOOK_TOKEN` to the right value. Gated on
    // `Some(token)` so non-Grok agents never see the variable
    // (preserves the round-1 fix: Claude / Codex / AGY POST URLs
    // carry no `?token=` and the route's per-provider gate
    // recognises them as legitimate).
    if let Some(token) = crate::agent::runtime_hook_token() {
        cmd.env("BUILDMESH_HOOK_TOKEN", token);
    }
    pty::strip_git_env_vars(&mut cmd);

    cmd
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
    let direction = if env_type == EnvType::WindowsInterop { "/w" } else { "/u" };
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
    if env_type == EnvType::WindowsInterop { set_wslenv_direction(&mut wslenv, "BUILDMESH_WSL_HOST", direction); }
    if !wslenv.is_empty() {
        cmd.env("WSLENV", wslenv);
    }
}

fn set_wslenv_direction(wslenv: &mut String, key: &str, direction: &str) {
    let flags = wslenv.split(':').find(|part| part.split('/').next() == Some(key))
        .and_then(|entry| entry.split_once('/')).map(|(_, flags)| flags.replace(['u', 'w'], "")).unwrap_or_default();
    let mut entries: Vec<_> = wslenv.split(':').filter(|entry| !entry.is_empty() && entry.split('/').next() != Some(key)).map(str::to_string).collect();
    entries.push(format!("{key}/{flags}{}", direction.trim_start_matches('/')));
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
    };
    use base64::Engine;

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "requires WSL with Windows interop enabled"]
    fn live_wsl_host_launches_windows_harnesses() {
        use crate::agent::provider::{SpawnRecipe, WindowsShell};
        use crate::models::EnvType;
        assert!(crate::env::is_wsl_host());
        let windows_temp = crate::env::windows_cli_home("AppData/Local/Temp").unwrap();
        let script_dir = tempfile::tempdir_in(&windows_temp).unwrap();
        let script = script_dir.path().join("probe script.cmd");
        std::fs::write(&script, "@echo off\r\necho %BUILDMESH_SESSION_ID%> probe.txt\r\n").unwrap();
        for root in [std::path::Path::new("/tmp"), windows_temp.as_path()] {
            let directory = tempfile::Builder::new().prefix("buildmesh reverse ").tempdir_in(root).unwrap();
            let spawn_path = crate::env::windows_path_from_wsl(directory.path().to_str().unwrap());
            for shell in [WindowsShell::PowerShell, WindowsShell::Cmd] {
                let (binary, args) = if shell == WindowsShell::Cmd {
                    (crate::env::windows_path_from_wsl(script.to_str().unwrap()), vec![])
                } else {
                    ("powershell.exe".to_string(), vec!["-NoProfile".into(), "-EncodedCommand".into(),
                        encode_for_powershell("[IO.File]::WriteAllText((Join-Path $PWD.ProviderPath 'probe.txt'), $env:BUILDMESH_SESSION_ID)")])
                };
                let recipe = SpawnRecipe { binary: "probe", base_args: args, trailing_args: vec![], windows_shell: shell };
                let mut command = super::wrap(recipe, EnvType::WindowsInterop, None, Some(&binary), &spawn_path, 8125, false);
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
                        if count == 0 { break; }
                        output.extend_from_slice(&buffer[..count]);
                        if output.ends_with(b"\x1b[6n") { writer.write_all(b"\x1b[1;1R").unwrap(); }
                    }
                    output
                });
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let status = loop {
                    if let Some(status) = child.try_wait().unwrap() { break status; }
                    if std::time::Instant::now() >= deadline { child.kill().unwrap(); panic!("Windows PTY timed out: {shell:?} {spawn_path}"); }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                };
                assert!(status.success(), "{shell:?}: {spawn_path}: {}", String::from_utf8_lossy(&drain.join().unwrap()));
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
            base_args: vec!["-c".into(), "printf '%s\\n' \"$PWD\" \"$BUILDMESH_SESSION_ID\" \"$1\" > probe.txt".into(), "probe".into(), payload.into()],
            trailing_args: vec![], windows_shell: crate::agent::provider::WindowsShell::Direct,
        };
        let mut command = super::wrap(recipe, resolved.env_type, None, None, &resolved.spawn_path, 8123, false);
        apply_wsl_env(&mut command, resolved.env_type, &[], &[]);
        let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
        let mut child = crate::agent::spawn::spawn_child(&pair, command).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(directory.path().join("probe.txt")).unwrap(),
            format!("{}\n8123\n{payload}\n", resolved.spawn_path));
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires an installed default WSL distribution"]
    fn live_wsl_directory_runs_windows_cmd_harness() {
        let home = crate::env::wsl_home().unwrap();
        let directory = tempfile::Builder::new().prefix("buildmesh-native-")
            .tempdir_in(crate::env::to_host_path(&home.to_string_lossy())).unwrap();
        let script_dir = tempfile::tempdir().unwrap();
        let script = script_dir.path().join("native probe.cmd");
        std::fs::write(&script, "@echo off\r\necho native-in-guest> probe.txt\r\n").unwrap();
        let mut resolved = crate::env::resolve_raw_path(directory.path().to_str().unwrap());
        crate::env::apply_harness_runtime(&mut resolved, crate::models::EnvType::Windows);
        let recipe = crate::agent::provider::SpawnRecipe {
            binary: "probe", base_args: vec![], trailing_args: vec![],
            windows_shell: crate::agent::provider::WindowsShell::Cmd,
        };
        let command = super::wrap(recipe, resolved.env_type, None, script.to_str(), &resolved.spawn_path, 8124, false);
        let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
        let mut child = crate::agent::spawn::spawn_child(&pair, command).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(directory.path().join("probe.txt")).unwrap().trim(), "native-in-guest");
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
        assert!(command.get_env("WSLENV").unwrap().to_string_lossy().split(':')
            .any(|entry| entry.split('/').next() == Some("BUILDMESH_HOOK_TOKEN")));
    }

    #[test]
    fn append_to_wslenv_deduplicates_by_base_name() {
        let mut wslenv = "SSH_AUTH_SOCK/up:CODEX_HOME/u".to_string();
        append_to_wslenv(&mut wslenv, "CODEX_HOME", "/u");
        assert_eq!(wslenv, "SSH_AUTH_SOCK/up:CODEX_HOME/u");
        append_to_wslenv(&mut wslenv, "BUILDMESH_PORT", "/u");
        assert_eq!(
            wslenv,
            "SSH_AUTH_SOCK/up:CODEX_HOME/u:BUILDMESH_PORT/u"
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
        assert_eq!(&bytes[..2], &[0x65, 0x00], "leading bytes should be 'e' as UTF-16LE, not a BOM");

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
        let args = vec!["--anthropic".to_string(), "--prefill".to_string(), body.to_string()];
        let cmd_str = format_powershell_command("claude", &args);

        // Must start with the call operator so PowerShell treats it as command invocation.
        assert!(cmd_str.starts_with("& "), "command must use PowerShell call operator: {}", cmd_str);

        // The binary and every arg must be wrapped in single quotes. After the
        // leading `& 'claude' `, there must be no bare newline outside of a quoted
        // string — i.e. every newline in the prefill stays inside the single-quoted
        // arg, not at the top level of the script.
        let after_call = cmd_str.strip_prefix("& ").unwrap();
        assert!(after_call.starts_with("'claude'"), "binary must be single-quoted: {}", cmd_str);

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
        assert!(cmd_str.contains("'it''s a test'"), "expected doubled-quote escaping, got: {}", cmd_str);
    }
}
