//! macOS Seatbelt sandbox for agent PTY processes (issue #497).
//!
//! When a Mesh has its `sandbox` toggle enabled, agent processes on macOS are
//! launched through `sandbox-exec -f <profile.sb> <binary> <args...>` instead
//! of spawning the binary directly. The profile is generated per spawn and is
//! **deny-by-default**: the only filesystem the agent can read *and* write is
//! the node's Git worktree. The user's home folder — and with it `~/.ssh`,
//! `~/.aws`, and every other credential store — is therefore denied implicitly,
//! satisfying the containment goal in PRD #494.
//!
//! Git still needs to authenticate to its remote. Rather than copying private
//! keys into the sandbox we forward the **SSH agent socket** (`SSH_AUTH_SOCK`):
//! the profile grants access to that one socket path, so `git fetch` / `git
//! push` can ask the host's `ssh-agent` to sign challenges without the private
//! key material ever being readable inside the sandbox.
//!
//! ## Platform note
//! Nothing in this module is `#[cfg(target_os = "macos")]`-gated. It uses only
//! cross-platform `std` APIs (`std::env::temp_dir`, `fs::write`,
//! `CommandBuilder`) so the profile generator and command assembler are fully
//! type-checked and unit-tested on every CI host — including the Windows boxes
//! this project is primarily developed on. The *decision* to call into it is
//! made by `spawn_environment::wrap`'s `cfg!(target_os = "macos")` branch.
//!
//! ## Known follow-up (live macOS verification)
//! The exact set of system read paths a given toolchain needs is fiddly and can
//! only be confirmed on real hardware (see issue #497's acceptance criteria 1–3,
//! which require a Mac to exercise). The profile below is a principled baseline:
//! it deliberately errs toward *denying* rather than risk leaking a credential
//! path. Narrow read-only allowances (e.g. a tool that insists on reading a
//! specific dotfile) should be added here as they are discovered on-device, not
//! by widening the home-folder grant.

use portable_pty::CommandBuilder;
use std::io;
use std::path::PathBuf;

/// The binary used to launch a process inside a Seatbelt sandbox on macOS.
const SANDBOX_EXEC: &str = "sandbox-exec";

/// Escape a path (or any string) for use inside a Seatbelt Profile Language
/// (SBPL) double-quoted string literal. SBPL strings are TinyScheme strings:
/// a literal backslash or double-quote must be backslash-escaped, otherwise the
/// profile fails to compile and `sandbox-exec` refuses to launch. Paths with
/// these characters are pathological but a sandbox that silently fails open on
/// them would be worse than one that escapes them.
fn sbpl_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Build the SBPL profile text for an agent confined to `worktree_path`.
///
/// `home_dir` (when known) lets git read its user-level config so commit
/// identity and remote config resolve; the rest of `$HOME` stays denied.
/// `ssh_auth_sock` (when set) is the host `ssh-agent` socket forwarded in for
/// authenticated git operations.
///
/// Pure and deterministic — no environment or filesystem access — so it can be
/// asserted on directly in unit tests. Side effects (resolving `$HOME` /
/// `$SSH_AUTH_SOCK`, writing the file) live in [`seatbelt_command`].
pub fn seatbelt_profile(
    worktree_path: &str,
    home_dir: Option<&str>,
    ssh_auth_sock: Option<&str>,
) -> String {
    let mut p = String::new();
    p.push_str("(version 1)\n\n");
    p.push_str(";; Buildmesh agent sandbox (issue #497) — deny-by-default.\n");
    p.push_str("(deny default)\n\n");

    // The agent spawns child tools (git, node, the language toolchain), needs
    // to signal itself, and reads kernel state via sysctl. mach-lookup is
    // required for a great many system frameworks to initialise at all.
    p.push_str(";; Process & system basics.\n");
    p.push_str("(allow process-fork)\n");
    p.push_str("(allow process-exec)\n");
    p.push_str("(allow signal (target self))\n");
    p.push_str("(allow sysctl-read)\n");
    p.push_str("(allow mach-lookup)\n");
    p.push_str("(allow system-socket)\n\n");

    // Read-only access to the OS and toolchain. These are world-readable
    // executable/library locations, never credential stores.
    p.push_str(";; Read-only system & toolchain locations.\n");
    p.push_str("(allow file-read*\n");
    for sub in [
        "/usr",
        "/bin",
        "/sbin",
        "/System",
        "/Library",
        "/opt",
        "/etc",
        "/private/etc",
        "/private/var/db",
        "/var/db",
        "/Applications",
        "/dev",
    ] {
        p.push_str(&format!("    (subpath {})\n", sbpl_quote(sub)));
    }
    p.push_str("    (literal \"/\"))\n\n");

    // A PTY child needs the usual character devices read-write.
    p.push_str(";; Character devices a PTY child expects.\n");
    p.push_str("(allow file-read* file-write-data\n");
    p.push_str("    (literal \"/dev/null\")\n");
    p.push_str("    (literal \"/dev/zero\")\n");
    p.push_str("    (literal \"/dev/dtracehelper\")\n");
    p.push_str("    (subpath \"/dev/tty\"))\n");
    p.push_str("(allow file-ioctl (subpath \"/dev\"))\n\n");

    // The one read-write region: the node's Git worktree. Everything the agent
    // edits, builds, and commits lives here.
    p.push_str(";; Read-write ONLY inside the node's Git worktree.\n");
    p.push_str(&format!(
        "(allow file* (subpath {}))\n\n",
        sbpl_quote(worktree_path)
    ));

    // Transient scratch space. macOS hands every process a per-user temp dir
    // under /private/var/folders; many tools (git, compilers) write there.
    p.push_str(";; Temporary scratch space.\n");
    p.push_str("(allow file*\n");
    p.push_str("    (subpath \"/private/tmp\")\n");
    p.push_str("    (subpath \"/private/var/folders\")\n");
    p.push_str("    (subpath \"/private/var/tmp\"))\n\n");

    // Git's user-level config (identity, includes, remote rewrites) — read-only.
    // Deliberately scoped to config, NOT the whole home folder: `~/.ssh`,
    // `~/.aws`, and the rest of $HOME remain denied by the deny-default above.
    if let Some(home) = home_dir.filter(|h| !h.is_empty()) {
        let home = home.trim_end_matches('/');
        p.push_str(";; Git user-level config (read-only). $HOME is otherwise denied.\n");
        p.push_str("(allow file-read*\n");
        p.push_str(&format!(
            "    (literal {})\n",
            sbpl_quote(&format!("{home}/.gitconfig"))
        ));
        p.push_str(&format!(
            "    (subpath {}))\n\n",
            sbpl_quote(&format!("{home}/.config/git"))
        ));
    }

    // SSH agent socket forwarding. Granting the socket lets git authenticate
    // over SSH via the host ssh-agent without the private key ever being
    // readable inside the sandbox (the key never leaves the agent process).
    if let Some(sock) = ssh_auth_sock.filter(|s| !s.is_empty()) {
        p.push_str(";; Forwarded SSH agent socket (SSH_AUTH_SOCK) for git auth.\n");
        p.push_str(&format!("(allow file* (literal {}))\n", sbpl_quote(sock)));
        // The socket usually lives under a per-user /private/tmp dir already
        // granted above, but allow its parent dir's lookup explicitly so a
        // socket placed elsewhere (e.g. a forwarded path) still resolves.
        if let Some(parent) = PathBuf::from(sock).parent().and_then(|p| p.to_str()) {
            if !parent.is_empty() {
                p.push_str(&format!(
                    "(allow file-read* (literal {}))\n",
                    sbpl_quote(parent)
                ));
            }
        }
        p.push('\n');
    }

    // Network egress so git can reach its remote over HTTPS or SSH.
    p.push_str(";; Network egress for git fetch/push (HTTPS + SSH).\n");
    p.push_str("(allow network*)\n");

    p
}

/// Path the per-session sandbox profile is written to.
fn profile_path(session_id: i64) -> PathBuf {
    std::env::temp_dir().join(format!("buildmesh-sandbox-{session_id}.sb"))
}

/// Assemble a [`CommandBuilder`] that launches `binary` + `base_args` inside a
/// macOS Seatbelt sandbox confined to `worktree_path`.
///
/// Writes the generated profile to a per-session temp file and returns
/// `sandbox-exec -f <profile> <binary> <args...>`. `$HOME` and `$SSH_AUTH_SOCK`
/// are resolved from the current process environment (which the agent would
/// inherit anyway). The returned builder has no `cwd`/`env` set — the caller
/// (`spawn_environment::wrap`) applies those uniformly for every spawn path.
///
/// Returns `Err` only if the profile file cannot be written; the caller decides
/// how to handle that (today: log and fall back to an unsandboxed direct spawn).
pub fn seatbelt_command(
    binary: &str,
    base_args: &[String],
    worktree_path: &str,
    session_id: i64,
) -> io::Result<CommandBuilder> {
    let home = std::env::var("HOME").ok();
    let ssh_sock = std::env::var("SSH_AUTH_SOCK").ok();
    let profile = seatbelt_profile(worktree_path, home.as_deref(), ssh_sock.as_deref());

    let path = profile_path(session_id);
    std::fs::write(&path, profile)?;

    let mut cmd = CommandBuilder::new(SANDBOX_EXEC);
    cmd.arg("-f");
    cmd.arg(path.as_os_str());
    cmd.arg(binary);
    cmd.args(base_args);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKTREE: &str = "/Users/dev/repo/.claude/worktrees/wt-1";

    #[test]
    fn profile_opens_with_version_and_denies_default() {
        let p = seatbelt_profile(WORKTREE, None, None);
        assert!(
            p.starts_with("(version 1)"),
            "profile must start with the SBPL version header:\n{p}"
        );
        assert!(
            p.contains("(deny default)"),
            "profile must deny by default:\n{p}"
        );
    }

    #[test]
    fn profile_grants_read_write_to_the_worktree() {
        let p = seatbelt_profile(WORKTREE, None, None);
        assert!(
            p.contains(&format!("(allow file* (subpath \"{WORKTREE}\"))")),
            "profile must grant read-write to the worktree subpath:\n{p}"
        );
    }

    #[test]
    fn profile_allows_network_for_git() {
        let p = seatbelt_profile(WORKTREE, None, None);
        assert!(
            p.contains("(allow network*)"),
            "profile must allow network egress for git:\n{p}"
        );
    }

    #[test]
    fn profile_forwards_ssh_auth_sock_when_set() {
        let sock = "/private/tmp/com.apple.launchd.AbC/Listeners";
        let p = seatbelt_profile(WORKTREE, None, Some(sock));
        assert!(
            p.contains(&format!("(allow file* (literal \"{sock}\"))")),
            "profile must forward the SSH_AUTH_SOCK socket when set:\n{p}"
        );
    }

    #[test]
    fn profile_omits_ssh_block_when_sock_unset() {
        let p = seatbelt_profile(WORKTREE, None, None);
        assert!(
            !p.contains("SSH_AUTH_SOCK"),
            "profile must not mention the SSH socket block when no socket is set:\n{p}"
        );
        // An empty string is treated as "unset" — a blank env var must not
        // produce an `(allow file* (literal ""))` that grants nothing or, worse,
        // matches unexpectedly.
        let p_empty = seatbelt_profile(WORKTREE, None, Some(""));
        assert!(
            !p_empty.contains("(literal \"\")"),
            "empty sock must not emit a literal rule:\n{p_empty}"
        );
    }

    /// The whole point of the sandbox: the home folder's credential stores are
    /// never granted. They are denied implicitly by `(deny default)`, so the
    /// guarantee is simply that we never emit an `allow` rule naming them.
    #[test]
    fn profile_never_grants_home_credential_stores() {
        let home = "/Users/dev";
        let sock = "/private/tmp/agent.sock";
        let p = seatbelt_profile(WORKTREE, Some(home), Some(sock));
        assert!(!p.contains(".ssh"), "profile must never grant ~/.ssh:\n{p}");
        assert!(!p.contains(".aws"), "profile must never grant ~/.aws:\n{p}");
        // We DO allow git's user config — that's read-only and not a secret.
        assert!(
            p.contains(&format!("(literal \"{home}/.gitconfig\"))"))
                || p.contains(&format!("{home}/.gitconfig")),
            "profile should allow read of ~/.gitconfig when HOME is known:\n{p}"
        );
    }

    #[test]
    fn profile_escapes_quotes_and_backslashes_in_path() {
        let nasty = "/Users/dev/we\"ird\\path";
        let p = seatbelt_profile(nasty, None, None);
        assert!(
            p.contains("/Users/dev/we\\\"ird\\\\path"),
            "path quotes/backslashes must be SBPL-escaped:\n{p}"
        );
    }

    fn argv(cmd: &CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn seatbelt_command_wraps_binary_with_sandbox_exec() {
        let session_id = -97_001; // negative => unlikely to collide with a real session temp file
        let args = vec![
            "--dangerously-skip-permissions".to_string(),
            "--session-id".to_string(),
            "abc".to_string(),
        ];
        let cmd = seatbelt_command("claude", &args, WORKTREE, session_id).expect("write profile");

        let got = argv(&cmd);
        assert_eq!(got[0], SANDBOX_EXEC, "must invoke sandbox-exec: {got:?}");
        assert_eq!(got[1], "-f", "must pass a profile file: {got:?}");
        let profile_file = got[2].clone();
        assert!(
            profile_file.ends_with(".sb"),
            "profile path must be a .sb file: {got:?}"
        );
        assert_eq!(
            &got[3..],
            &[
                "claude",
                "--dangerously-skip-permissions",
                "--session-id",
                "abc"
            ],
            "inner binary + args must follow the profile, unchanged: {got:?}"
        );

        // The profile file must actually exist and confine to the worktree.
        let written = std::fs::read_to_string(&profile_file).expect("profile written to disk");
        assert!(written.contains("(deny default)"));
        assert!(written.contains(WORKTREE));
        let _ = std::fs::remove_file(&profile_file);
    }
}
