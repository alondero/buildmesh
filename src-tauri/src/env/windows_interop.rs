//! Windows process access from a Linux Buildmesh host inside WSL.
use once_cell::sync::Lazy;

pub(crate) fn is_wsl_host() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WSL_DISTRO_NAME").is_some()
}

pub(crate) fn encode_powershell(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(crate) fn decode_powershell_command(command: &str) -> Option<String> {
    use base64::Engine;
    let encoded = command
        .strip_prefix("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ")?
        .split_whitespace()
        .next()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

pub(crate) fn powershell_command(script: &str) -> std::process::Command {
    let mut command = crate::process_util::command_no_window("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &encode_powershell(script),
    ]);
    command
}

pub(crate) fn windows_home() -> Option<String> {
    static HOME: Lazy<Option<String>> = Lazy::new(|| {
        if !is_wsl_host() {
            return None;
        }
        let command = powershell_command("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::Write($env:USERPROFILE)");
        let output = crate::process_util::run_command_with_timeout(
            command,
            "Windows home",
            std::time::Duration::from_secs(10),
        )
        .ok()?;
        if !output.status.success() {
            return None;
        }
        let home = String::from_utf8(output.stdout).ok()?;
        super::is_windows_path(&home).then_some(home)
    });
    HOME.clone()
}

pub(crate) fn windows_cli_home(relative: &str) -> Option<std::path::PathBuf> {
    let home = windows_home()?;
    Some(std::path::PathBuf::from(super::to_host_path(&format!(
        "{home}/{}",
        relative
    ))))
}

/// Resolve Windows Codex's effective state directory from the Windows
/// environment. A Linux host's `CODEX_HOME` is intentionally never consulted.
pub(crate) fn windows_codex_home() -> Option<std::path::PathBuf> {
    if !is_wsl_host() {
        return None;
    }
    static CODEX_HOME: Lazy<Option<std::path::PathBuf>> = Lazy::new(|| {
        let command = powershell_command(
            "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $path = if ([string]::IsNullOrWhiteSpace($env:CODEX_HOME)) { Join-Path $env:USERPROFILE '.codex' } else { $env:CODEX_HOME }; [Console]::Write($path)",
        );
        let output = crate::process_util::run_command_with_timeout(
            command,
            "Windows Codex home",
            std::time::Duration::from_secs(10),
        )
        .ok()?;
        if !output.status.success() {
            return None;
        }
        let home = String::from_utf8(output.stdout).ok()?.trim().to_string();
        super::is_windows_path(&home).then(|| std::path::PathBuf::from(super::to_host_path(&home)))
    });
    CODEX_HOME.clone()
}

pub(crate) fn powershell_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('\'');
    for character in value.chars() {
        // PowerShell recognizes smart apostrophes as string delimiters too.
        if matches!(
            character,
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}'
        ) {
            literal.push(character);
        }
        literal.push(character);
    }
    literal.push('\'');
    literal
}

/// Explicitly enter the owning Linux distribution for callbacks. Windows
/// localhost may belong to a different Buildmesh instance under WSL NAT.
pub(crate) fn windows_attention_command(url: Option<&str>) -> Option<String> {
    windows_attention_command_impl(
        url.map(|one| vec![one]).as_deref(),
        HookShell::PropagateExit,
    )
}

/// [`windows_attention_command`] for a caller that has no `$BUILDMESH_PORT` to
/// expand and must name each Buildmesh instance instead. curl reads the piped
/// body once and POSTs it to each URL, so one hook still reaches whichever
/// instance owns the node (issue #2343).
pub(crate) fn windows_attention_command_for_urls(urls: &[String]) -> Option<String> {
    let targets: Vec<&str> = urls.iter().map(String::as_str).collect();
    windows_attention_command_impl(Some(&targets), HookShell::Silent)
}

/// The one-line command a Windows-side hook shell runs to POST its stdin to
/// Buildmesh's loopback attention endpoint.
///
/// Codex runs a Windows hook command through `powershell.exe -NoProfile
/// -Command` (`codex_core::spawn`), so this value is *PowerShell statement
/// text*, not a nested `powershell.exe` invocation. It must therefore stay
/// free of double quotes (PowerShell 5.1 drops embedded double quotes on the
/// way to a native program), single quotes (which would force the TOML value
/// carrying it into an escaped, double-quoted string) and newlines, because it
/// rides a `-c` launch argument through Buildmesh's own PowerShell script and
/// then through the `cmd.exe` that runs an npm `codex.CMD` shim.
///
/// Those same constraints are why this used to be a nested
/// `powershell.exe -EncodedCommand <base64>` payload: the base64 hid the
/// quotes, at a cost of ~2.7 command-line characters per character of script.
/// Codex carries one callback per attention event on the launch command line,
/// so the encoded form alone filled all but a few hundred characters of the
/// 8,191-character `cmd.exe` limit and any real prompt overflowed it (#2184).
///
/// On a WSL host the callback has to reach Buildmesh on the Linux side, so
/// curl runs inside the owning distribution; everywhere else the host's own
/// `Invoke-WebRequest` posts it. The body is sent as a UTF-8 byte array
/// rather than a string: a string body is encoded with the request's charset,
/// and a charset cannot be named here because the `;` separating it would end
/// the PowerShell statement.
pub(crate) fn windows_hook_callback_command(url: &str) -> String {
    windows_hook_callback_command_for_runtime(url, wsl_host_distro().as_deref())
}

/// The owning Linux distribution when Buildmesh itself runs under WSL. A
/// native Windows host has none: its own `localhost` is Buildmesh's.
fn wsl_host_distro() -> Option<String> {
    if !is_wsl_host() {
        return None;
    }
    std::env::var("WSL_DISTRO_NAME")
        .ok()
        .filter(|d| !d.is_empty())
}

/// Host-independent core of [`windows_hook_callback_command`], so a caller can
/// build either runtime's command and tests need not run on that host.
pub(crate) fn windows_hook_callback_command_for_runtime(url: &str, distro: Option<&str>) -> String {
    let Some(distro) = distro else {
        return format!(
            "[Console]::InputEncoding=[Text.Encoding]::UTF8; \
             try {{ Invoke-WebRequest -UseBasicParsing -Method Post \
             -ContentType application/json \
             -Body ([Text.Encoding]::UTF8.GetBytes([Console]::In.ReadToEnd())) \
             -TimeoutSec 10 -Uri {url} | Out-Null }} catch {{ }}; \
             echo ([char]123+[char]125); exit 0"
        );
    };
    if is_shell_safe_token(distro) {
        return format!(
            "wsl.exe -d {distro} --exec curl -fsS --connect-timeout 2 \
             --max-time 10 -o /dev/null -X POST --data-binary `@- {url}; \
             echo ([char]123+[char]125); exit 0"
        );
    }
    // A distribution name PowerShell would re-parse cannot travel unquoted
    // (`wsl.exe -d My Distro`), so that name keeps the encoded payload that
    // quotes it. Rare enough that its command-line cost is acceptable.
    windows_attention_command_for_distro(distro, Some(&[url]), HookShell::CodexJson)
}

/// Whether a value can appear unquoted in a PowerShell command line. WSL
/// distribution names are short identifiers; anything holding whitespace or a
/// shell metacharacter is rejected so the callback never depends on a shell's
/// re-parse of an unquoted token.
fn is_shell_safe_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn windows_attention_command_impl(urls: Option<&[&str]>, hook: HookShell) -> Option<String> {
    if !is_wsl_host() {
        return None;
    }
    let distro = std::env::var("WSL_DISTRO_NAME").ok()?;
    Some(windows_attention_command_for_distro(&distro, urls, hook))
}

/// What the Windows-side hook shell does after the POST, per harness.
///
/// A callback observes lifecycle; failing to reach Buildmesh is not the
/// harness's failure, so `CodexJson` and `Silent` suppress curl's stderr and
/// exit 0. `PropagateExit` exists for the adapters that wrap this command in
/// their own line (Kimi, Cursor, Agy) and own the exit contract there; it keeps
/// their behaviour unchanged.
#[derive(Clone, Copy, PartialEq)]
enum HookShell {
    /// Codex lifecycle callback: emit `{}` so the hook runner sees valid JSON.
    CodexJson,
    /// MiniMax callback: no stdout contract, and any number of baked targets
    /// can be unreachable (issue #2343), so curl's status must not escape.
    Silent,
    /// Leave the exit code to the caller that wraps this command.
    PropagateExit,
}

/// `None` keeps the `$BUILDMESH_*` expansion for harnesses that still have the
/// environment; a list names the targets to bake instead (issue #2343). The
/// script's single `--data-binary '@-'` reads the piped body once, and curl
/// sends it to each URL that follows.
fn windows_attention_command_for_distro(
    distro_name: &str,
    urls: Option<&[&str]>,
    hook: HookShell,
) -> String {
    let distro = powershell_literal(distro_name);
    let url = urls
        .map(|list| {
            list.iter()
                .map(|one| powershell_literal(one))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "('http://localhost:' + $env:BUILDMESH_PORT + '/api/attention/' + $env:BUILDMESH_SESSION_ID)".into());
    let stdout = match hook {
        HookShell::CodexJson => "; [Console]::Out.WriteLine('{}')".to_string(),
        HookShell::Silent | HookShell::PropagateExit => String::new(),
    };
    let (stderr, exit) = match hook {
        // Codex callbacks observe lifecycle and must not gate the harness.
        HookShell::CodexJson | HookShell::Silent => (" 2>$null", "exit 0"),
        HookShell::PropagateExit => ("", "exit $LASTEXITCODE"),
    };
    let script = format!("$OutputEncoding = [System.Text.UTF8Encoding]::new($false); $input | & wsl.exe -d {distro} --exec curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST --data-binary '@-' {url}{stderr}{stdout}; {exit}");
    format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        encode_powershell(&script)
    )
}

pub(crate) fn unix_attention_curl() -> &'static str {
    if cfg!(windows) {
        "$(command -v curl.exe || command -v curl)"
    } else {
        "buildmesh_curl() { if [ -n \"$BUILDMESH_WSL_HOST\" ]; then wsl.exe -d \"$BUILDMESH_WSL_HOST\" --exec curl \"$@\"; else curl \"$@\"; fi; }; buildmesh_curl"
    }
}

#[cfg(test)]
mod command_tests {
    #[test]
    fn powershell_literal_doubles_ascii_and_smart_apostrophes() {
        for (input, expected) in [
            ("'", "''''"),
            ("\u{2018}", "'\u{2018}\u{2018}'"),
            ("\u{2019}", "'\u{2019}\u{2019}'"),
            ("\u{201a}", "'\u{201a}\u{201a}'"),
            ("\u{201b}", "'\u{201b}\u{201b}'"),
            ("don't", "'don''t'"),
            ("don\u{2018}t", "'don\u{2018}\u{2018}t'"),
            ("don\u{2019}t", "'don\u{2019}\u{2019}t'"),
            ("don\u{201a}t", "'don\u{201a}\u{201a}t'"),
            ("don\u{201b}t", "'don\u{201b}\u{201b}t'"),
            (
                "'\u{2018}\u{2019}\u{201a}\u{201b}",
                "'''\u{2018}\u{2018}\u{2019}\u{2019}\u{201a}\u{201a}\u{201b}\u{201b}'",
            ),
        ] {
            assert_eq!(super::powershell_literal(input), expected, "input: {input}");
        }
    }

    #[test]
    fn hook_callback_is_quote_and_newline_free_in_every_runtime() {
        for command in [
            super::windows_hook_callback_command_for_runtime(
                "http://localhost:1992/api/attention/42",
                None,
            ),
            super::windows_hook_callback_command_for_runtime(
                "http://localhost:1992/api/attention/42",
                Some("Ubuntu"),
            ),
        ] {
            assert!(!command.contains('"'), "{command}");
            assert!(!command.contains('\''), "{command}");
            assert!(!command.contains('\n'), "{command}");
            assert!(!command.contains('\r'), "{command}");
            assert!(
                command.contains("http://localhost:1992/api/attention/42"),
                "{command}"
            );
            // The `{}` Codex's Stop hook requires on stdout, spelled without a
            // quote character (a bare `{}` parses as a script block in
            // PowerShell and prints nothing).
            assert!(command.contains("echo ([char]123+[char]125)"), "{command}");
            assert!(command.ends_with("exit 0"), "{command}");
        }
    }

    #[test]
    fn native_host_callback_posts_the_hook_stdin_with_its_own_http_client() {
        let command = super::windows_hook_callback_command_for_runtime(
            "http://localhost:1992/api/attention/42",
            None,
        );
        // PowerShell statement text, not a nested powershell.exe: the value is
        // handed to a PowerShell that Codex already started.
        assert!(!command.contains("EncodedCommand"), "{command}");
        assert!(command.contains("[Console]::In.ReadToEnd()"), "{command}");
        assert!(command.contains("Invoke-WebRequest"), "{command}");
        // A byte-array body: a string body is encoded with the request's
        // charset, and the `;` that would name one ends the statement.
        assert!(
            command.contains("-Body ([Text.Encoding]::UTF8.GetBytes([Console]::In.ReadToEnd()))"),
            "{command}"
        );
        assert!(
            command.contains("[Console]::InputEncoding=[Text.Encoding]::UTF8"),
            "{command}"
        );
    }

    #[test]
    fn wsl_host_callback_curls_from_inside_the_owning_distribution() {
        let command = super::windows_hook_callback_command_for_runtime(
            "http://localhost:1992/api/attention/42",
            Some("Ubuntu"),
        );
        assert!(
            command.starts_with("wsl.exe -d Ubuntu --exec curl"),
            "{command}"
        );
        // A Linux Codex resolves the distro's own localhost; the Windows-side
        // URL would not reach a Buildmesh running under WSL NAT.
        assert!(command.contains("--data-binary `@-"), "{command}");
    }

    #[test]
    fn a_distribution_name_a_shell_would_reparse_keeps_the_encoded_payload() {
        let command = super::windows_hook_callback_command_for_runtime(
            "http://localhost:1992/api/attention/42",
            Some("My Distro"),
        );
        let script = super::decode_powershell_command(&command).expect("encoded payload");
        assert!(script.contains("wsl.exe -d 'My Distro'"), "{script}");
        assert!(
            script.contains("[Console]::Out.WriteLine('{}')"),
            "{script}"
        );
    }

    #[test]
    fn codex_windows_callback_script_fails_open_and_emits_json() {
        let url = "http://localhost:1992/api/attention/42";
        let command = super::windows_attention_command_for_distro(
            "Ubuntu",
            Some(&[url]),
            super::HookShell::CodexJson,
        );
        let script = super::decode_powershell_command(&command).expect("decode PowerShell command");

        assert!(script.contains(" 2>$null"), "{script}");
        assert!(
            script.contains("[Console]::Out.WriteLine('{}')"),
            "{script}"
        );
        assert!(script.ends_with("; exit 0"), "{script}");
        assert!(!script.contains("exit $LASTEXITCODE"), "{script}");
    }

    /// Issue #2343 review: the MiniMax relay names every live instance, and any
    /// of them can be gone by the time the hook runs. curl's status — and its
    /// stderr — must not escape into mcode's hook runner, and the relay has no
    /// stdout contract to emit.
    #[test]
    fn mcode_relay_swallows_every_target_failure() {
        let urls = [
            "http://localhost:1992/api/attention/mcode",
            "http://localhost:2992/api/attention/mcode",
        ];
        let command = super::windows_attention_command_for_distro(
            "Ubuntu",
            Some(&urls),
            super::HookShell::Silent,
        );
        let script = super::decode_powershell_command(&command).expect("decode PowerShell command");

        for url in urls {
            assert!(
                script.contains(&format!("'{url}'")),
                "missing {url}: {script}"
            );
        }
        assert_eq!(
            script.matches("--data-binary '@-'").count(),
            1,
            "one stdin read must serve every target: {script}"
        );
        assert!(
            script.contains(" 2>$null"),
            "stderr must not leak: {script}"
        );
        assert!(script.ends_with("; exit 0"), "{script}");
        assert!(
            !script.contains("exit $LASTEXITCODE"),
            "a dead sibling must not fail the hook: {script}"
        );
        assert!(
            !script.contains("[Console]::Out.WriteLine"),
            "mcode's hook protocol has no stdout contract: {script}"
        );
    }

    /// The adapters that wrap [`super::windows_attention_command`] own their own
    /// exit contract, so the shared shape must keep passing curl's code through.
    /// A change here would silently alter Kimi, Cursor and Agy.
    #[test]
    fn wrapping_adapters_keep_the_propagated_exit_code() {
        let command = super::windows_attention_command_for_distro(
            "Ubuntu",
            None,
            super::HookShell::PropagateExit,
        );
        let script = super::decode_powershell_command(&command).expect("decode PowerShell command");

        assert!(script.contains("$env:BUILDMESH_PORT"), "{script}");
        assert!(script.ends_with("; exit $LASTEXITCODE"), "{script}");
        assert!(!script.contains(" 2>$null"), "{script}");
    }

    #[cfg(windows)]
    #[test]
    fn codex_windows_callback_returns_success_after_wsl_failure() {
        let url = "http://localhost:1992/api/attention/42";
        let command = super::windows_attention_command_for_distro(
            "Ubuntu",
            Some(&[url]),
            super::HookShell::CodexJson,
        );
        let script = super::decode_powershell_command(&command).expect("decode PowerShell command");
        let script = format!(
            "function wsl.exe {{ Write-Error 'simulated callback failure'; $global:LASTEXITCODE = 22 }}; {script}"
        );
        let output = super::powershell_command(&script)
            .output()
            .expect("run decoded Codex callback script");

        assert!(
            output.status.success(),
            "hook failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("simulated callback failure"),
            "callback error escaped stderr suppression: {stderr}"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    #[ignore = "requires WSL with Windows interop enabled"]
    fn live_windows_callback_returns_to_owning_linux_host() {
        use std::io::{Read, Seek, Write};
        use std::time::{Duration, Instant};
        assert!(super::is_wsl_host());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://127.0.0.1:{}/api/attention/8125",
            listener.local_addr().unwrap().port()
        );
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(25);
            let mut connection = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(error) => panic!("callback never arrived: {error}"),
                }
            };
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                connection.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let headers = String::from_utf8(request).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            connection.read_exact(&mut body).unwrap();
            connection
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            String::from_utf8(body).unwrap()
        });
        let payload = "{\"message\":\"WSL caf? ??\"}";
        let mut input = tempfile::tempfile().unwrap();
        writeln!(input, "{payload}").unwrap();
        input.rewind().unwrap();
        let command = super::windows_attention_command(Some(&url)).unwrap();
        let mut process = crate::process_util::command_no_window("sh");
        process
            .args(["-c", &command])
            .stdin(std::process::Stdio::from(input));
        let output = crate::process_util::run_command_with_timeout(
            process,
            "reverse callback",
            Duration::from_secs(20),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(server.join().unwrap().trim(), payload);
    }
}
