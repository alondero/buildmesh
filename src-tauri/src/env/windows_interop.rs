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
        super::is_windows_path(&home)
            .then(|| std::path::PathBuf::from(super::to_host_path(&home)))
    });
    CODEX_HOME.clone()
}

pub(crate) fn powershell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Explicitly enter the owning Linux distribution for callbacks. Windows
/// localhost may belong to a different Buildmesh instance under WSL NAT.
pub(crate) fn windows_attention_command(url: Option<&str>) -> Option<String> {
    if !is_wsl_host() {
        return None;
    }
    let distro = powershell_literal(&std::env::var("WSL_DISTRO_NAME").ok()?);
    let url = url.map(powershell_literal).unwrap_or_else(|| "('http://localhost:' + $env:BUILDMESH_PORT + '/api/attention/' + $env:BUILDMESH_SESSION_ID)".into());
    let script = format!("$OutputEncoding = [System.Text.UTF8Encoding]::new($false); $input | & wsl.exe -d {distro} --exec curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST --data-binary '@-' {url}; exit $LASTEXITCODE");
    Some(format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        encode_powershell(&script)
    ))
}

pub(crate) fn unix_attention_curl() -> &'static str {
    if cfg!(windows) {
        "$(command -v curl.exe || command -v curl)"
    } else {
        "buildmesh_curl() { if [ -n \"$BUILDMESH_WSL_HOST\" ]; then wsl.exe -d \"$BUILDMESH_WSL_HOST\" --exec curl \"$@\"; else curl \"$@\"; fi; }; buildmesh_curl"
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
