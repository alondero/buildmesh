//! Orchestrate a sandboxed agent spawn (GH #498).
//!
//! Ties the pieces together for one spawn: a per-node AppContainer profile, the
//! worktree + user-profile toolchain ACL grants, a curated environment (PATH
//! prefers grant-free `C:\Program Files\Git`; TEMP redirected into the
//! writable worktree), then the owned ConPTY spawn. Returns boxed `Child` /
//! `MasterPty` so `spawn_agent_inner` registers them exactly like the
//! unsandboxed `PtyPair`.
//!
//! The default agent chain runs `claude.exe` directly (post-#531 cwrap
//! absorption), so the chain that needs to stay grant-free inside the
//! AppContainer is just `claude.exe` itself (and anything it shells out to,
//! like `git`). `claude.exe` lives in the user profile (granted without
//! elevation); Git's `bash`/`git` carry the app-package ACE. System `node` /
//! msys2 would need admin and are deliberately not on the curated PATH.

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
///
/// **Superseded (ADR-0014).** The AppContainer hung `claude.exe` (#528); the
/// production seam (`agent::spawn::sandbox_spawn`) now uses
/// [`spawn_sandboxed_restricted`]. This fn + [`cleanup`] are retained only as the
/// record exercised by the ignored `repro_*` diagnostic tests — do not re-wire
/// them into production without restoring the matching [`cleanup`] call.
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

// ---------------------------------------------------------------------------
// ADR-0014 restricted-token spawn (GH #528) — the pivot off AppContainer.
//
// Same orchestration as `spawn_sandboxed` (grant the worktree + the home paths
// the default agent chain needs, curate env, owned ConPTY spawn) but the
// confinement primitive is a restricted token of the *same user* instead of an
// AppContainer SID. Grants target the token's `grant_sid()` (`RESTRICTED`,
// S-1-5-12) — that SID is in the token's restricting set, so granting it opens
// exactly the granted object (see `restricted_token` docs). The token stays at
// Medium integrity: the §4 spike found Low IL breaks msys/Bun named objects.
//
// `include_user_sid` is the unresolved §4 trade-off (see `RestrictedToken::new`):
// `true` lets msys `bash` / claude boot but re-opens home reads; `false` denies
// home reads but breaks `bash`. **This is the live production spawn path** —
// `agent::spawn::sandbox_spawn` calls it with `(grant_home=false,
// include_user_sid=true)`, fixing the #528 hang and #533 loopback. Read
// confinement (the strict `include_user_sid=false` mode) is deferred to #542;
// `spawn_sandboxed` above (AppContainer) is the superseded predecessor.
// ---------------------------------------------------------------------------

/// Per-node cleanup for the restricted-token path: revoke the grants.
struct RestrictedCleanup {
    sid: String,
    granted: Vec<PathBuf>,
}

static RESTRICTED_CLEANUP: Lazy<Mutex<HashMap<i64, RestrictedCleanup>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Spawn `cmd` for `session_id` confined by a restricted token (ADR-0014 / #528).
/// When `grant_home` is set, also grants the home paths the default Anthropic
/// agent chain needs (`~/.claude`, `~/.local/bin`, `~/.claude.json`). See the
/// module comment for `include_user_sid`.
pub fn spawn_sandboxed_restricted(
    cmd: &CommandBuilder,
    session_id: i64,
    worktree_host_path: &str,
    rows: u16,
    cols: u16,
    grant_home: bool,
    include_user_sid: bool,
) -> Result<(Box<dyn Child + Send + Sync>, Box<dyn MasterPty + Send>), String> {
    use super::conpty::spawn_with_restricted_token;
    use super::restricted_token::RestrictedToken;

    let token = RestrictedToken::new(include_user_sid)?;
    let sid = token.grant_sid().to_string();

    let mut granted: Vec<PathBuf> = Vec::new();

    // The worktree: grant the sandbox SID full control. No integrity relabel —
    // the token is Medium, same as the worktree, so writes work as-is.
    let worktree = PathBuf::from(worktree_host_path);
    acl::grant_dir(&worktree, &sid, Access::Full)
        .map_err(|e| format!("grant worktree to sandbox: {}", e))?;
    granted.push(worktree.clone());

    if grant_home {
        for (dir, access) in optional_grants() {
            if dir.exists() && acl::grant_dir(&dir, &sid, access).is_ok() {
                granted.push(dir);
            }
        }
        for file in optional_file_grants() {
            if file.exists() && acl::grant_file(&file, &sid, Access::Full).is_ok() {
                granted.push(file);
            }
        }
    }

    let tmp = worktree.join(".bm-sandbox-tmp");
    let _ = std::fs::create_dir_all(&tmp);

    let cmdline = argv_to_cmdline(cmd.get_argv());
    let cwd = cmd.get_cwd().map(|c| c.to_string_lossy().into_owned());
    let env = curated_env(cmd, &tmp.to_string_lossy());

    match spawn_with_restricted_token(&cmdline, cwd.as_deref(), &env, rows, cols, &token) {
        Ok((child, pty)) => {
            RESTRICTED_CLEANUP
                .lock()
                .unwrap()
                .insert(session_id, RestrictedCleanup { sid, granted });
            Ok((Box::new(child), Box::new(pty)))
        }
        Err(e) => {
            for path in &granted {
                let _ = acl::revoke_dir(path, &sid);
            }
            Err(e)
        }
    }
}

/// Revoke the restricted-token node's grants. Best-effort; close path.
pub fn cleanup_restricted(session_id: i64) {
    let info = RESTRICTED_CLEANUP.lock().unwrap().remove(&session_id);
    if let Some(info) = info {
        for path in &info.granted {
            let _ = acl::revoke_dir(path, &info.sid);
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
/// grant-free Git to PATH, and redirect the temp vars into `tmp` (writable).
///
/// `TMPDIR` is dropped alongside `TEMP`/`TMP`: a Git-Bash / MSYS host sets it to
/// the host temp, and the sandbox child shells out to `bash` (git hooks), which
/// honours `TMPDIR` — so leaving the host value would both leak the host path
/// and point writes at a directory the restricted token can't write to.
fn curated_env(cmd: &CommandBuilder, tmp: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut path_set = false;
    for (k, v) in cmd.iter_full_env_as_str() {
        if k.eq_ignore_ascii_case("path") {
            out.push((k.to_string(), format!("{};{};{}", GIT_CMD, GIT_BIN, v)));
            path_set = true;
        } else if k.eq_ignore_ascii_case("temp")
            || k.eq_ignore_ascii_case("tmp")
            || k.eq_ignore_ascii_case("tmpdir")
        {
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
    out.push(("TMPDIR".to_string(), tmp.to_string()));
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

    /// LIVE DIAGNOSTIC (ignored by default) — historical repro for the
    /// "cwrap exits ~500ms after spawning inside the AppContainer" blocker
    /// (GH #498 handoff). The production reader thread streams the child's
    /// ConPTY bytes to the UI but never to `buildmesh.log`, so we know the
    /// agent dies but not why. This exercises the *real* `spawn_sandboxed`
    /// (real per-node AppContainer profile, real worktree/.claude/.local-bin
    /// grants, real owned ConPTY) and captures every byte the child emits,
    /// then prints the captured output together with the child's exit code.
    ///
    /// Default argv exercises the current production path (`claude.exe`,
    /// post-#531 cwrap absorption). The original #498 reproduction targeted
    /// the now-archived `cwrap.cmd` launcher, which is no longer shipped
    /// with the dev host toolchain; the original diagnostic lives in git
    /// history (PR #531 merge commit and earlier).
    ///
    /// Run manually (default):
    /// ```text
    /// cargo test -p buildmesh sandbox::spawn::tests::repro_claude_exit_in_sandbox -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live: spawns real claude.exe into an AppContainer; needs the dev host toolchain"]
    fn repro_claude_exit_in_sandbox() {
        use std::io::Read;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        // A throwaway dir stands in for the node's worktree — granted Full to the
        // container SID exactly like a real node, so the grant surface matches.
        let worktree =
            std::env::temp_dir().join(format!("bm-repro-spawn-{}", std::process::id()));
        std::fs::create_dir_all(&worktree).unwrap();

        // Default: the current production claude.exe direct path (post-#531).
        // Override via BM_REPRO_ARGS to probe other launchers on PATH. The
        // original #498 reproduction targeted `cwrap.cmd`, which was archived
        // when cwrap was absorbed into buildmesh — see the doc above.
        let args_str =
            std::env::var("BM_REPRO_ARGS").unwrap_or_else(|_| "claude.exe --dangerously-skip-permissions".to_string());
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
            "\n===== SANDBOX SPAWN REPRO — exit_code={:?}, {} bytes =====",
            exit_code,
            out.len()
        );
        eprintln!("{}", text);
        eprintln!("===== END SANDBOX SPAWN REPRO =====\n");
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
            &[], // no per-profile backend env (built-in Anthropic subscription)
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

    /// Read from `pty` until `stop_marker` appears or `secs` elapse (or the
    /// child exits), then kill + tear down. Returns (still_alive, exit_code,
    /// captured_text). Mirrors the read-thread pattern the conpty tests use
    /// (ConPTY never EOFs the output pipe, so we drop the pty to end the reader).
    fn observe(
        mut child: Box<dyn Child + Send + Sync>,
        pty: Box<dyn MasterPty + Send>,
        secs: u64,
        stop_marker: &str,
    ) -> (bool, Option<u32>, String) {
        use std::io::Read;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

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
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut exit_code: Option<u32> = None;
        while Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(150)) {
                out.extend_from_slice(&chunk);
            }
            if String::from_utf8_lossy(&out).contains(stop_marker) {
                break;
            }
            if let Ok(Some(s)) = child.try_wait() {
                exit_code = Some(s.exit_code());
                break;
            }
        }
        let still_alive = exit_code.is_none();
        child.kill().ok();
        drop(pty);
        let _ = child.wait();
        reader_thread.join().ok();
        (still_alive, exit_code, String::from_utf8_lossy(&out).into_owned())
    }

    /// §4 SPIKE (ignored) — the decisive feasibility test. It proves that the
    /// ADR-0014 restricted token **fixes #528's breakage** (child spawn, bash,
    /// network all work — none of the AppContainer object-namespace failures) but
    /// that the read-confinement goal (criteria d/e) and bash-init (criterion c)
    /// are **mutually exclusive** under a same-user restricted token.
    ///
    /// The pivot — `RestrictedToken::new(include_user_sid)` — controls the only
    /// free variable. The test runs the same probes under both settings:
    ///   * STRICT (`false`): home reads/writes DENIED (d/e ✓), but `bash` dies at
    ///     its user-SID-keyed `CreateFileMapping` (c ✗) — msys can't init.
    ///   * PERMISSIVE (`true`): `bash` inits (c ✓) and the network reaches the API
    ///     + loopback (f ✓), but the user SID re-opens home reads (d ✗ — leak).
    /// Child spawn (b) works in both. Criterion (a) — named-pipe creation for
    /// child stdio, the #528 operation — is proven by the live-claude sibling
    /// test (libuv); cmd/bash can't exercise it directly.
    ///
    /// The asserted EXCLUSION is the spike's verdict: a SID-based restricted token
    /// cannot tell a user-private *file* apart from a user-keyed *kernel object*,
    /// so it can't deny one while admitting the other. See ADR-0014 §Spike result.
    ///
    /// Hermetic (temp worktree + a temp secret in `%USERPROFILE%`, both removed).
    /// Run: cargo test -p buildmesh sandbox::spawn::tests::spike_restricted_token_tradeoff -- --ignored --nocapture
    #[test]
    #[ignore = "live: builds restricted tokens + grants a temp worktree; dev host only"]
    fn spike_restricted_token_tradeoff() {
        use crate::sandbox::conpty::spawn_with_restricted_token;
        use crate::sandbox::restricted_token::RestrictedToken;

        let pid = std::process::id();
        let worktree = std::env::temp_dir().join(format!("bm-spike-rt-{}", pid));
        std::fs::create_dir_all(&worktree).unwrap();
        let tmp = worktree.join(".bm-sandbox-tmp");
        std::fs::create_dir_all(&tmp).unwrap();

        let strict = RestrictedToken::new(false).expect("strict token");
        let permissive = RestrictedToken::new(true).expect("permissive token");
        let sid = strict.grant_sid().to_string(); // same RESTRICTED SID for both
        acl::grant_dir(&worktree, &sid, Access::Full).expect("grant worktree");

        // Curated env: Git on PATH (bash/curl), TEMP into the writable worktree.
        let mut env: Vec<(String, String)> = std::env::vars().collect();
        for (k, v) in env.iter_mut() {
            if k.eq_ignore_ascii_case("path") {
                *v = format!("{};{};{}", GIT_CMD, GIT_BIN, v);
            }
        }
        env.retain(|(k, _)| !k.eq_ignore_ascii_case("temp") && !k.eq_ignore_ascii_case("tmp"));
        let tmp_s = tmp.to_string_lossy().into_owned();
        env.push(("TEMP".into(), tmp_s.clone()));
        env.push(("TMP".into(), tmp_s));

        let wt = worktree.to_string_lossy().into_owned();
        let run = |token: &RestrictedToken, cmdline: &str, secs: u64, marker: &str| -> String {
            let (child, pty) =
                spawn_with_restricted_token(cmdline, Some(&wt), &env, 30, 120, token)
                    .expect("spawn under restricted token");
            let (_alive, _ec, text) = observe(Box::new(child), Box::new(pty), secs, marker);
            text
        };

        let cmd = r"C:\Windows\System32\cmd.exe";
        let up = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
        let secret = up.join(format!("bm-spike-secret-{}.txt", pid));
        std::fs::write(&secret, "TOPSECRET_MARKER\n").unwrap();
        let read_cmd = format!("{cmd} /c \"type {} & echo READ_DONE & pause\"", secret.display());
        let bash_cmd = format!("{cmd} /c \"bash --version & echo BASH_DONE & pause\"");

        // (b) child spawn works regardless of the SID set.
        let b = run(&strict, &format!("{cmd} /c \"echo CHILD_OK & pause\""), 8, "CHILD_OK");
        eprintln!("[b child-spawn]\n{b}\n");
        assert!(b.contains("CHILD_OK"), "(b) child spawn failed: {b:?}");

        // ---- STRICT (no user SID): reads denied, but bash can't init ----
        let d_strict = run(&strict, &read_cmd, 8, "READ_DONE");
        eprintln!("[strict d read-deny]\n{d_strict}\n");
        assert!(!d_strict.contains("TOPSECRET_MARKER"), "(d) strict: home read LEAKED: {d_strict:?}");
        assert!(d_strict.to_lowercase().contains("denied"), "(d) strict: expected access-denied: {d_strict:?}");

        let c_strict = run(&strict, &bash_cmd, 12, "BASH_DONE");
        eprintln!("[strict c bash-init]\n{c_strict}\n");
        assert!(!c_strict.contains("GNU bash"), "(c) strict: bash UNEXPECTEDLY inited — tension may be resolvable: {c_strict:?}");

        // ---- PERMISSIVE (user SID): bash inits, but reads leak ----
        let c_perm = run(&permissive, &bash_cmd, 12, "BASH_DONE");
        eprintln!("[permissive c bash-init]\n{c_perm}\n");
        assert!(c_perm.contains("GNU bash"), "(c) permissive: bash should init with the user SID: {c_perm:?}");

        let d_perm = run(&permissive, &read_cmd, 8, "READ_DONE");
        eprintln!("[permissive d read-deny]\n{d_perm}\n");
        assert!(d_perm.contains("TOPSECRET_MARKER"), "(d) permissive: home read should leak (the trade-off): {d_perm:?}");

        let _ = std::fs::remove_file(&secret);

        // (f) network under the permissive token (the only one claude could use).
        // /v:on so `!errorlevel!` re-evaluates after each curl (delayed expansion).
        let port = std::env::var("BUILDMESH_PORT").unwrap_or_else(|_| "1992".into());
        let f = run(
            &permissive,
            &format!(
                "{cmd} /v:on /c \"curl -s --max-time 15 -o NUL https://api.anthropic.com/v1/messages & echo API_EXIT=!errorlevel! & curl -s --max-time 6 -o NUL http://127.0.0.1:{port}/ & echo LB_EXIT=!errorlevel! & echo F_DONE & pause\""
            ),
            30,
            "F_DONE",
        );
        eprintln!("[f network]\n{f}\n");
        assert!(f.contains("API_EXIT=0"), "(f) outbound api.anthropic.com unreachable: {f:?}");
        // Loopback: any curl exit but 28 (timeout = SYN dropped) proves loopback
        // isn't WFP-blocked. 0 = hub answered, 7 = refused (works, nothing up).
        assert!(!f.contains("LB_EXIT=28"), "(f) loopback to 127.0.0.1:{port} BLOCKED (timeout): {f:?}");

        acl::revoke_dir(&worktree, &sid).ok();
        let _ = std::fs::remove_dir_all(&worktree);
        eprintln!(
            "===== SPIKE VERDICT: restricted token FIXES #528 (b/c/f work, no AppContainer hang) \
             but read-confinement (d/e) and bash (c) are MUTUALLY EXCLUSIVE under a same-user token ====="
        );
    }

    /// §4 SPIKE (ignored) — the capstone: a real `claude.exe` launched under the
    /// restricted token (permissive: user SID included, so msys/Bun init) must
    /// render its prompt beyond the 16-byte ConPTY handshake and stay alive. This
    /// is criterion (a)+(g): claude's startup spawns a child (the shell-snapshot)
    /// via libuv's `uv__create_stdio_pipe_pair` — the exact `CreateNamedPipe`
    /// operation that spun forever inside an AppContainer (#528). If claude renders
    /// here, the pivot has FIXED #528. (Read-confinement is the separate, still-open
    /// question the trade-off test characterises — this permissive token does NOT
    /// confine home reads.)
    ///
    /// NOTE: grants + lowers the integrity of the real `~/.claude` and
    /// `~/.claude.json` so claude can read its OAuth + write session state;
    /// `cleanup_restricted` revokes the grants and restores the labels to Medium.
    ///
    /// Run: cargo test -p buildmesh sandbox::spawn::tests::spike_restricted_token_claude_boots -- --ignored --nocapture
    #[test]
    #[ignore = "live: spawns real claude.exe under a restricted token; needs the dev host toolchain"]
    fn spike_restricted_token_claude_boots() {
        use crate::agent::spawn::{build_spawn_command, SessionIdMode};
        use crate::env::ResolvedPath;
        use crate::models::{EnvType, Provider};

        let worktree = std::env::temp_dir().join(format!("bm-spike-claude-{}", std::process::id()));
        std::fs::create_dir_all(&worktree).unwrap();
        let wt = worktree.to_string_lossy().into_owned();

        let resolved = ResolvedPath {
            host_path: wt.clone(),
            spawn_path: wt.clone(),
            raw_path: wt.clone(),
            env_type: EnvType::Windows,
        };
        let session_id: i64 = -987_644;
        let cmd = build_spawn_command(
            &resolved,
            Provider::Anthropic,
            &[], // no per-profile backend env (built-in Anthropic subscription)
            &SessionIdMode::None,
            session_id,
            None,
            None,
            None,
            true, // sandbox
        );
        eprintln!(
            "SPIKE claude argv: {:?}",
            cmd.get_argv().iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>()
        );

        let (child, pty) = spawn_sandboxed_restricted(&cmd, session_id, &wt, 40, 120, true, true)
            .expect("spawn_sandboxed_restricted");
        eprintln!("SPIKE_CLAUDE_PID={:?}", child.process_id());

        let secs: u64 = std::env::var("BM_REPRO_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
        let (still_alive, exit_code, text) = observe(child, pty, secs, "\u{0}NEVER\u{0}");
        cleanup_restricted(session_id);
        let _ = std::fs::remove_dir_all(&worktree);

        eprintln!(
            "\n===== SPIKE CLAUDE (restricted token) — still_alive={}, exit_code={:?}, {} bytes =====",
            still_alive, exit_code, text.len()
        );
        eprintln!("{text}");
        eprintln!("===== END SPIKE CLAUDE =====\n");

        // (a)+(g): claude survived its early child-spawn (named pipe created) and
        // rendered beyond the bare ConPTY handshake (16 bytes).
        assert!(still_alive, "(g) claude exited early (exit={exit_code:?}); should stay alive rendering its prompt");
        assert!(text.len() > 16, "(a)+(g) claude rendered only the {}-byte handshake — likely hung in early init", text.len());
    }

    #[test]
    fn curated_env_prepends_git_and_redirects_temp() {
        let mut cmd = CommandBuilder::new("x.exe");
        cmd.env("PATH", r"C:\msys64\usr\bin");
        cmd.env("TEMP", r"C:\Users\me\AppData\Local\Temp");
        cmd.env("TMPDIR", r"C:\Users\me\AppData\Local\Temp");
        let env = curated_env(&cmd, r"C:\wt\.bm-sandbox-tmp");

        let path = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("path")).unwrap();
        assert!(path.1.starts_with(GIT_CMD), "Git must win on PATH: {}", path.1);
        assert!(path.1.contains(r"C:\msys64\usr\bin"), "original PATH preserved");

        // Every temp var points at the sandbox dir.
        for key in ["TEMP", "TMP", "TMPDIR"] {
            let var = env.iter().find(|(k, _)| k == key)
                .unwrap_or_else(|| panic!("{key} must be set"));
            assert_eq!(var.1, r"C:\wt\.bm-sandbox-tmp", "{key} redirected to sandbox tmp");
        }
        // The host temp must not survive under any var (TEMP/TMP/TMPDIR or other).
        assert!(!env.iter().any(|(_, v)| v.contains(r"AppData\Local\Temp")));
    }
}
