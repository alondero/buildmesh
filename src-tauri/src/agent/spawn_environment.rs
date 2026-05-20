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
/// PowerShell's -Command parses arguments before execution, tripping over special
/// characters like backticks `<>()'"`|;&$#@ etc. -EncodedCommand takes a Base64
/// UTF-16LE string and executes it without any parsing — all characters pass
/// through unchanged, which is essential when prefill text from GitHub issues
/// contains code snippets with these characters.
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
                // Build the command as a single string so we can encode it.
                // Using -EncodedCommand (Base64 UTF-16LE) prevents PowerShell from
                // parsing special characters in arguments (backticks, <>, newlines,
                // quotes, etc.) — critical when prefill text from GitHub issues
                // contains code snippets with special chars.
                let cmd_str = format!("{} {}", recipe.binary, recipe.base_args.join(" "));
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
    cmd.env("BUILDMESH_PORT", crate::http_server::HTTP_PORT_DEFAULT.to_string());
    pty::strip_git_env_vars(&mut cmd);

    cmd
}

#[cfg(test)]
mod tests {
    use super::encode_for_powershell;
    use base64::Engine;

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

        // Round-trip: decode the UTF-16LE bytes back to a string.
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let decoded = String::from_utf16(&units).expect("valid utf-16");
        assert_eq!(decoded, "echo hi");
    }
}
