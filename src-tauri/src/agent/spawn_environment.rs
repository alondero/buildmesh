//! OS-axis seam — wraps a provider's `SpawnRecipe` in the right shell for the
//! runtime environment.
//!
//! - WSL (regardless of host): `wsl.exe --cd <path> -- <binary> <args...>`
//! - macOS: direct invocation
//! - Windows native + PowerShell shell: `powershell.exe -NoLogo -EncodedCommand <base64>`
//!   (used by cwrap providers so ANSI escapes propagate correctly through ConPTY)
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
    // PowerShell -EncodedCommand expects Base64 of raw UTF-16LE bytes, no BOM.
    // A BOM gets decoded as a leading U+FEFF/U+FFFE code unit and breaks parsing.
    let mut le_bytes = Vec::with_capacity(cmd.len() * 2);
    for c in cmd.encode_utf16() {
        le_bytes.extend_from_slice(&c.to_le_bytes());
    }
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(&le_bytes)
}

/// Quote a single PowerShell argument as a single-quoted string literal.
/// Single quotes inside the string must be doubled (`'` → `''`); everything else
/// (newlines, backticks, brackets, `$`, etc.) is preserved verbatim because
/// single-quoted PowerShell strings perform no interpolation or escapes.
fn ps_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
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
    recipe: SpawnRecipe,
    env_type: EnvType,
    spawn_path: &str,
    session_id: i64,
) -> CommandBuilder {
    let mut cmd = if env_type == EnvType::Wsl {
        tracing::info!("spawn_environment: building WSL command via wsl.exe");
        let mut c = CommandBuilder::new("wsl.exe");
        c.args(["--cd", spawn_path, "--", recipe.binary]);
        c.args(recipe.base_args);
        c
    } else if cfg!(target_os = "macos") {
        tracing::info!("spawn_environment: building macOS command for {}", recipe.binary);
        let mut c = CommandBuilder::new(recipe.binary);
        c.args(recipe.base_args);
        c
    } else {
        match recipe.windows_shell {
            WindowsShell::PowerShell => {
                tracing::info!(
                    "spawn_environment: building Windows powershell.exe for {}",
                    recipe.binary
                );
                // Build a PowerShell script that calls the binary with each arg
                // single-quoted, then Base64/UTF-16LE encode it for
                // -EncodedCommand. Quoting matters: multi-line prefill text
                // (e.g. GitHub issue bodies with `1. ...` numbered lists or
                // backticks) would otherwise be parsed as separate PowerShell
                // statements after newline boundaries.
                let cmd_str = format_powershell_command(recipe.binary, &recipe.base_args);
                let encoded = encode_for_powershell(&cmd_str);
                let mut c = CommandBuilder::new("powershell.exe");
                c.args(["-NoLogo", "-EncodedCommand", &encoded]);
                c
            }
            WindowsShell::Cmd => {
                tracing::info!(
                    "spawn_environment: building Windows cmd.exe /c for {}",
                    recipe.binary
                );
                let mut c = CommandBuilder::new("cmd.exe");
                c.args(["/c", recipe.binary]);
                c.args(recipe.base_args);
                c
            }
            WindowsShell::Direct => {
                tracing::info!("spawn_environment: building direct Windows spawn for {}", recipe.binary);
                let mut c = CommandBuilder::new(recipe.binary);
                c.args(recipe.base_args);
                c
            }
        }
    };

    cmd.cwd(spawn_path);
    cmd.env("BUILDMESH_SESSION_ID", session_id.to_string());
    cmd.env("BUILDMESH_PORT", crate::http_server::current_http_port().to_string());
    pty::strip_git_env_vars(&mut cmd);

    cmd
}

#[cfg(test)]
mod tests {
    use super::{encode_for_powershell, format_powershell_command};
    use base64::Engine;

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
    /// wrapped in single quotes, so multi-line prefill text (GitHub issue bodies
    /// with backticks, brackets, numbered lists) is treated as a single string
    /// literal — not parsed as separate PowerShell statements line-by-line.
    ///
    /// Regression for: spawning an agent from a GH issue whose body contained
    /// markdown caused PowerShell to error with "Unexpected token '`mesh.toml'"
    /// because each line of the body was being parsed as a new statement.
    #[test]
    fn format_powershell_command_quotes_multiline_prefill_safely() {
        let body = "Currently, when spawning a new agent...\n\
                    1. `mesh.toml [agent] default_provider` (per-mesh override)\n\
                    2. Buildmesh-wide default\n\
                    3. Anthropic (hardcoded fallback)";
        let args = vec!["--anthropic".to_string(), "--prefill".to_string(), body.to_string()];
        let cmd_str = format_powershell_command("cwrap", &args);

        // Must start with the call operator so PowerShell treats it as command invocation.
        assert!(cmd_str.starts_with("& "), "command must use PowerShell call operator: {}", cmd_str);

        // The binary and every arg must be wrapped in single quotes. After the
        // leading `& 'cwrap' `, there must be no bare newline outside of a quoted
        // string — i.e. every newline in the prefill stays inside the single-quoted
        // arg, not at the top level of the script.
        let after_call = cmd_str.strip_prefix("& ").unwrap();
        assert!(after_call.starts_with("'cwrap'"), "binary must be single-quoted: {}", cmd_str);

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
        let cmd_str = format_powershell_command("cwrap", &args);
        assert!(cmd_str.contains("'it''s a test'"), "expected doubled-quote escaping, got: {}", cmd_str);
    }
}
