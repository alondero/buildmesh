//! Orchestrate a sandboxed agent spawn (GH #498).
//!
//! Ties the pieces together for one spawn: a per-node AppContainer profile, the
//! worktree + user-profile toolchain ACL grants, a curated environment (PATH
//! prefers grant-free `C:\Program Files\Git`; TEMP redirected into the
//! writable worktree), then the owned ConPTY spawn. Returns boxed `Child` /
//! `MasterPty` so `spawn_agent_inner` registers them exactly like the
//! unsandboxed `PtyPair`.
//!
//! The default agent chain (`cwrap → bash → claude.exe`) runs grant-free this
//! way: Git's `bash`/`git` carry the app-package ACE, and `claude.exe` lives in
//! the user profile (granted without elevation). System `node`/msys2 would need
//! admin and are deliberately not on the curated PATH.

#![cfg(target_os = "windows")]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use portable_pty::{Child, CommandBuilder, MasterPty};

use super::acl::{self, Access};
use super::appcontainer::AppContainerProfile;
use super::conpty::spawn_in_appcontainer;

const GIT_CMD: &str = r"C:\Program Files\Git\cmd";
const GIT_BIN: &str = r"C:\Program Files\Git\bin";

/// Per-node cleanup info, consumed on close to delete the profile + revoke ACEs.
struct Cleanup {
    profile_name: String,
    sid: String,
    granted: Vec<PathBuf>,
}

static CLEANUP: Lazy<Mutex<HashMap<i64, Cleanup>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn profile_name(session_id: i64) -> String {
    format!("com.alond.buildmesh.node-{}", session_id)
}

/// Spawn `cmd` for `session_id` inside a per-node AppContainer.
pub fn spawn_sandboxed(
    cmd: &CommandBuilder,
    session_id: i64,
    worktree_host_path: &str,
    rows: u16,
    cols: u16,
) -> Result<(Box<dyn Child + Send + Sync>, Box<dyn MasterPty + Send>), String> {
    let name = profile_name(session_id);
    // `internetClient` so the agent reaches the Anthropic API and pushes over HTTPS.
    let profile = AppContainerProfile::create_or_derive(&name, true)?;
    let sid = profile.sid_string()?;

    // Grants: the worktree (read+write) and the user-profile dirs the container
    // can't reach by default. Failures on the optional toolchain dirs are
    // tolerated (the dir may not exist); the worktree grant is mandatory.
    let mut granted: Vec<PathBuf> = Vec::new();
    let worktree = PathBuf::from(worktree_host_path);
    acl::grant_dir(&worktree, &sid, Access::Full)
        .map_err(|e| format!("grant worktree to sandbox: {}", e))?;
    granted.push(worktree.clone());
    for (dir, access) in optional_grants() {
        if dir.exists() && acl::grant_dir(&dir, &sid, access).is_ok() {
            granted.push(dir);
        }
    }

    // A per-spawn temp dir inside the worktree (already writable for the
    // container) so the agent's TEMP writes don't hit the denied host TEMP.
    let tmp = worktree.join(".bm-sandbox-tmp");
    let _ = std::fs::create_dir_all(&tmp);

    let cmdline = argv_to_cmdline(cmd.get_argv());
    let cwd = cmd.get_cwd().map(|c| c.to_string_lossy().into_owned());
    let env = curated_env(cmd, &tmp.to_string_lossy());

    let spawned = spawn_in_appcontainer(&cmdline, cwd.as_deref(), &env, rows, cols, Some(&profile));

    match spawned {
        Ok((child, pty)) => {
            CLEANUP.lock().unwrap().insert(
                session_id,
                Cleanup { profile_name: name, sid, granted },
            );
            // The SID is no longer needed now the process exists; the on-disk
            // profile persists and is removed in `cleanup`.
            drop(profile);
            Ok((Box::new(child), Box::new(pty)))
        }
        Err(e) => {
            // Spawn failed — undo the grants we just made so nothing leaks.
            for dir in &granted {
                let _ = acl::revoke_dir(dir, &sid);
            }
            let _ = profile.delete();
            Err(e)
        }
    }
}

/// Delete the node's AppContainer profile and revoke its ACEs. Best-effort;
/// called from the close path (`kill_session`).
pub fn cleanup(session_id: i64) {
    let info = CLEANUP.lock().unwrap().remove(&session_id);
    if let Some(info) = info {
        for dir in &info.granted {
            let _ = acl::revoke_dir(dir, &info.sid);
        }
        // Re-derive the profile by name only to delete its on-disk entry.
        if let Ok(p) = AppContainerProfile::create_or_derive(&info.profile_name, false) {
            let _ = p.delete();
        }
    }
}

/// User-profile dirs to grant so the default agent chain can run. `.local\bin`
/// holds `cwrap`/`claude.exe` (run = RX); `.claude` holds config + session
/// state the agent reads and writes (Full).
fn optional_grants() -> Vec<(PathBuf, Access)> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    match home {
        Some(home) => vec![
            (home.join(".local").join("bin"), Access::ReadExecute),
            (home.join(".claude"), Access::Full),
        ],
        None => Vec::new(),
    }
}

/// Quote one argv element for a Windows command line (the standard
/// backslash/quote rules CreateProcessW's parser expects).
fn quote_arg(arg: &OsStr) -> String {
    let s = arg.to_string_lossy();
    if !s.is_empty() && !s.contains([' ', '\t', '\n', '\u{0b}', '"']) {
        return s.into_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                // Escape the run of backslashes (they precede a quote) then the quote.
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('\\');
                out.push('"');
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // Trailing backslashes precede the closing quote — double them.
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

fn argv_to_cmdline(argv: &[std::ffi::OsString]) -> String {
    argv.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the child environment: inherit the command's full env, prepend
/// grant-free Git to PATH, and redirect TEMP/TMP into `tmp` (writable).
fn curated_env(cmd: &CommandBuilder, tmp: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut path_set = false;
    for (k, v) in cmd.iter_full_env_as_str() {
        if k.eq_ignore_ascii_case("path") {
            out.push((k.to_string(), format!("{};{};{}", GIT_CMD, GIT_BIN, v)));
            path_set = true;
        } else if k.eq_ignore_ascii_case("temp") || k.eq_ignore_ascii_case("tmp") {
            // Replaced below with the sandbox temp dir.
        } else {
            out.push((k.to_string(), v.to_string()));
        }
    }
    if !path_set {
        out.push(("PATH".to_string(), format!("{};{}", GIT_CMD, GIT_BIN)));
    }
    out.push(("TEMP".to_string(), tmp.to_string()));
    out.push(("TMP".to_string(), tmp.to_string()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_args_that_need_it() {
        assert_eq!(quote_arg(OsStr::new("-NoProfile")), "-NoProfile");
        assert_eq!(quote_arg(OsStr::new("plain")), "plain");
        assert_eq!(quote_arg(OsStr::new("a b")), "\"a b\"");
        // base64 EncodedCommand payloads never contain spaces/quotes.
        assert_eq!(quote_arg(OsStr::new("AGMAdwByAGEAcA==")), "AGMAdwByAGEAcA==");
    }

    #[test]
    fn cmdline_joins_argv_with_spaces() {
        let cmd = CommandBuilder::from_argv(
            ["powershell.exe", "-NoProfile", "-EncodedCommand", "AAAA"]
                .iter()
                .map(std::ffi::OsString::from)
                .collect(),
        );
        assert_eq!(
            argv_to_cmdline(cmd.get_argv()),
            "powershell.exe -NoProfile -EncodedCommand AAAA"
        );
    }

    #[test]
    fn curated_env_prepends_git_and_redirects_temp() {
        let mut cmd = CommandBuilder::new("x.exe");
        cmd.env("PATH", r"C:\msys64\usr\bin");
        cmd.env("TEMP", r"C:\Users\me\AppData\Local\Temp");
        let env = curated_env(&cmd, r"C:\wt\.bm-sandbox-tmp");

        let path = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("path")).unwrap();
        assert!(path.1.starts_with(GIT_CMD), "Git must win on PATH: {}", path.1);
        assert!(path.1.contains(r"C:\msys64\usr\bin"), "original PATH preserved");

        let temp = env.iter().find(|(k, _)| k == "TEMP").unwrap();
        assert_eq!(temp.1, r"C:\wt\.bm-sandbox-tmp");
        // The host TEMP must not survive.
        assert!(!env.iter().any(|(_, v)| v.contains(r"AppData\Local\Temp")));
    }
}
