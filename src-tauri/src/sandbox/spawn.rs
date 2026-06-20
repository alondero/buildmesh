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
    // claude.exe reads and writes its top-level config/state in `~/.claude.json`
    // — a file in the home *root*, a sibling of the granted `~/.claude` dir, not
    // inside it. Without access claude stalls during startup config load (it
    // never renders its TUI). Granted as a single file (no dir inheritance).
    for file in optional_file_grants() {
        if file.exists() && acl::grant_file(&file, &sid, Access::Full).is_ok() {
            granted.push(file);
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

/// Home-root files (not inside any granted directory) the default agent chain
/// needs. `~/.claude.json` holds claude.exe's config, project history, and OAuth
/// state — read and written every session, so granted Full.
fn optional_file_grants() -> Vec<PathBuf> {
    match std::env::var_os("USERPROFILE").map(PathBuf::from) {
        Some(home) => vec![home.join(".claude.json")],
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

    /// LIVE DIAGNOSTIC (ignored by default) — reproduce the "cwrap exits ~500ms
    /// after spawning inside the AppContainer" blocker (GH #498 handoff). The
    /// production reader thread streams the child's ConPTY bytes to the UI but
    /// never to `buildmesh.log`, so we know the agent dies but not why. This
    /// exercises the *real* `spawn_sandboxed` (real per-node AppContainer profile,
    /// real worktree/.claude/.local-bin grants, real owned ConPTY) with the exact
    /// argv the post-PowerShell-fix path builds for Anthropic on Windows
    /// (`cmd.exe /c cwrap --anthropic`), captures every byte the child emits, and
    /// prints it with the child's exit code.
    ///
    /// Run manually:
    /// ```text
    /// cargo test -p buildmesh sandbox::spawn::tests::repro_cwrap_exit_in_sandbox -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live: spawns real cwrap+claude into an AppContainer; needs the dev host toolchain"]
    fn repro_cwrap_exit_in_sandbox() {
        use std::io::Read;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        // A throwaway dir stands in for the node's worktree — granted Full to the
        // container SID exactly like a real node, so the grant surface matches.
        let worktree =
            std::env::temp_dir().join(format!("bm-repro-cwrap-{}", std::process::id()));
        std::fs::create_dir_all(&worktree).unwrap();

        // Default: exactly what `spawn_environment::wrap` emits for Anthropic on
        // Windows once the sandbox PowerShell→cmd translation has run. Override
        // the args (everything after `cmd.exe /c`) via BM_REPRO_ARGS to probe
        // individual links of the cmd→bash→claude chain without recompiling.
        let args_str =
            std::env::var("BM_REPRO_ARGS").unwrap_or_else(|_| "cwrap --anthropic".to_string());
        let extra: Vec<String> = args_str.split_whitespace().map(String::from).collect();
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.arg("/c");
        for a in &extra {
            cmd.arg(a);
        }
        cmd.cwd(&worktree);
        // BM_REPRO_FRESH_CONFIG=1 points claude at an empty CLAUDE_CONFIG_DIR
        // inside the (granted) worktree — isolates "is the early-init hang driven
        // by the user's ~/.claude config (MCP servers, statusline) or by the
        // runtime/OS?".
        if std::env::var("BM_REPRO_FRESH_CONFIG").is_ok() {
            let cfg = worktree.join(".cfg");
            let _ = std::fs::create_dir_all(&cfg);
            cmd.env("CLAUDE_CONFIG_DIR", cfg.to_string_lossy().into_owned());
            eprintln!("REPRO using fresh CLAUDE_CONFIG_DIR={}", cfg.display());
        }
        eprintln!("REPRO running: cmd.exe /c {}", args_str);

        // Negative id so it can never collide with a real node row.
        let session_id: i64 = -987_654;
        let (mut child, pty) = spawn_sandboxed(
            &cmd,
            session_id,
            &worktree.to_string_lossy(),
            40,
            120,
        )
        .expect("spawn_sandboxed");

        let mut reader = pty.try_clone_reader().expect("reader");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let secs: u64 = std::env::var("BM_REPRO_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(12);
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut exit_code: Option<u32> = None;
        while Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                out.extend_from_slice(&chunk);
            }
            if let Ok(Some(status)) = child.try_wait() {
                exit_code = Some(status.exit_code());
                // The child has exited; drain whatever ConPTY still has buffered
                // (it does not EOF the pipe on exit) for a short grace window.
                let drain = Instant::now() + Duration::from_millis(800);
                while Instant::now() < drain {
                    if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                        out.extend_from_slice(&chunk);
                    }
                }
                break;
            }
        }

        child.kill().ok();
        drop(pty);
        let _ = child.wait();
        reader_thread.join().ok();

        // Revoke the ACEs + delete the profile, then remove the throwaway dir.
        cleanup(session_id);
        let _ = std::fs::remove_dir_all(&worktree);

        let text = String::from_utf8_lossy(&out);
        eprintln!(
            "\n===== CWRAP SANDBOX REPRO — exit_code={:?}, {} bytes =====",
            exit_code,
            out.len()
        );
        eprintln!("{}", text);
        eprintln!("===== END CWRAP SANDBOX REPRO =====\n");
    }

    /// LIVE VERIFICATION (ignored by default) — drive the *real* production path
    /// (`build_spawn_command` → `spawn_sandboxed`) for a sandboxed Anthropic node
    /// and confirm claude.exe boots inside the AppContainer instead of dying in
    /// the loader the way `cwrap → bash` did. A booted claude streams its TUI and
    /// stays alive (exit_code stays None); a failure exits fast with an error in
    /// the captured bytes.
    ///
    /// Run manually:
    /// ```text
    /// cargo test -p buildmesh sandbox::spawn::tests::repro_anthropic_sandbox_direct_boots -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live: spawns real claude.exe into an AppContainer; needs the dev host toolchain"]
    fn repro_anthropic_sandbox_direct_boots() {
        use crate::agent::spawn::{build_spawn_command, SessionIdMode};
        use crate::env::ResolvedPath;
        use crate::models::{EnvType, Provider};
        use std::io::Read;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let worktree =
            std::env::temp_dir().join(format!("bm-repro-direct-{}", std::process::id()));
        std::fs::create_dir_all(&worktree).unwrap();
        let wt = worktree.to_string_lossy().into_owned();

        let resolved = ResolvedPath {
            host_path: wt.clone(),
            spawn_path: wt.clone(),
            raw_path: wt.clone(),
            env_type: EnvType::Windows,
        };
        let session_id: i64 = -987_655;
        let cmd = build_spawn_command(
            &resolved,
            Provider::Anthropic,
            &SessionIdMode::None,
            session_id,
            None,
            None,
            None,
            true, // sandbox
        );
        eprintln!(
            "REPRO direct argv: {:?}",
            cmd.get_argv()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        );

        let (mut child, pty) =
            spawn_sandboxed(&cmd, session_id, &wt, 40, 120).expect("spawn_sandboxed");
        eprintln!("REPRO_CLAUDE_PID={:?}", child.process_id());

        let mut reader = pty.try_clone_reader().expect("reader");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut out = Vec::new();
        // BM_REPRO_SECS widens the observation window so an external probe
        // (e.g. `Get-NetTCPConnection -OwningProcess <pid>`) can inspect the hung
        // process's socket state before the test tears it down (#528 loopback probe).
        let secs: u64 = std::env::var("BM_REPRO_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut exit_code: Option<u32> = None;
        while Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                out.extend_from_slice(&chunk);
            }
            if let Ok(Some(status)) = child.try_wait() {
                exit_code = Some(status.exit_code());
                let drain = Instant::now() + Duration::from_millis(800);
                while Instant::now() < drain {
                    if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                        out.extend_from_slice(&chunk);
                    }
                }
                break;
            }
        }
        let still_alive = exit_code.is_none();

        child.kill().ok();
        drop(pty);
        let _ = child.wait();
        reader_thread.join().ok();
        cleanup(session_id);
        let _ = std::fs::remove_dir_all(&worktree);

        let text = String::from_utf8_lossy(&out);
        eprintln!(
            "\n===== ANTHROPIC SANDBOX-DIRECT — still_alive={}, exit_code={:?}, {} bytes =====",
            still_alive,
            exit_code,
            out.len()
        );
        eprintln!("{}", text);
        eprintln!("===== END ANTHROPIC SANDBOX-DIRECT =====\n");
    }

    /// CONTROL (ignored) — spawn claude.exe the SAME no-stdin way but through a
    /// normal PTY with NO AppContainer. Distinguishes a real container blocker
    /// from a reproduction artifact: if claude renders here but hangs in the
    /// sandboxed repro, the AppContainer is the cause; if it ALSO hangs here, the
    /// repro's no-input ConPTY is the artifact (claude needs terminal I/O the
    /// dumb reader doesn't provide) and production may behave differently.
    ///
    /// Run: cargo test -p buildmesh sandbox::spawn::tests::control_claude_no_sandbox -- --ignored --nocapture
    #[test]
    #[ignore = "live: spawns real claude.exe (no sandbox) through a normal PTY"]
    fn control_claude_no_sandbox() {
        use portable_pty::{native_pty_system, PtySize};
        use std::io::Read;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let worktree =
            std::env::temp_dir().join(format!("bm-control-{}", std::process::id()));
        std::fs::create_dir_all(&worktree).unwrap();

        let pty = native_pty_system()
            .openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/c", "claude.exe", "--dangerously-skip-permissions"]);
        cmd.cwd(&worktree);
        let mut child = pty.slave.spawn_command(cmd).expect("spawn");
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().expect("reader");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let secs: u64 = std::env::var("BM_REPRO_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(15);
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut exit_code: Option<u32> = None;
        while Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                out.extend_from_slice(&chunk);
            }
            if let Ok(Some(s)) = child.try_wait() {
                exit_code = Some(s.exit_code());
                break;
            }
        }
        let still_alive = exit_code.is_none();
        child.kill().ok();
        drop(pty.master);
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&worktree);

        let text = String::from_utf8_lossy(&out);
        eprintln!(
            "\n===== CONTROL (no sandbox) — still_alive={}, exit_code={:?}, {} bytes =====",
            still_alive, exit_code, out.len()
        );
        eprintln!("{}", text);
        eprintln!("===== END CONTROL =====\n");
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
