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
    use std::io::Write;
    // PowerShell expects UTF-16LE with a BOM (byte order mark)
    let mut le_bytes = Vec::with_capacity(cmd.len() * 2 + 2);
    // UTF-16LE BOM
    le_bytes.write_all(&[0xFE, 0xFF]).unwrap();
    for c in cmd.encode_utf16() {
        le_bytes.write_all(&c.to_le_bytes()).unwrap();
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
