//! OpenCode session-ID capture via `opencode session list --format json`.
//!
//! OpenCode self-assigns IDs of the form `ses_<hex+base62>` and does not
//! accept a caller-chosen UUID (`docs/learning/opencode-harness-capabilities.md`).
//! The TUI is not documented to print that ID, and the PTY UUID regex in
//! `session_capture` cannot match `ses_…`, so Buildmesh learns the ID after
//! spawn by listing sessions and picking the newest row whose `directory`
//! matches the node's spawn path.
//!
//! The parse/select helpers are pure so the matching rules are unit-tested
//! against live CLI fixtures without spawning `opencode`.

use std::time::Duration;

use serde::Deserialize;

use crate::models::EnvType;
use crate::process_util::{command_no_window, run_command_with_timeout};

/// One row of `opencode session list --format json` (1.18.3).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ListedSession {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub updated: i64,
    pub created: i64,
    #[serde(default, rename = "projectId")]
    pub project_id: Option<String>,
    pub directory: String,
}

/// Parse the JSON array `opencode session list --format json` prints.
pub fn parse_session_list_json(raw: &str) -> Result<Vec<ListedSession>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(trimmed).map_err(|e| format!("opencode session list JSON: {e}"))
}

/// Pick the session ID to store for a freshly spawned node.
///
/// 1. Restrict to rows whose `directory` matches `spawn_directory`.
/// 2. Prefer rows created at or after `created_not_before_ms` (spawn time
///    minus a small skew so a TUI that minted the row a few ms early still
///    matches).
/// 3. If the time window is empty (list ran before the TUI flushed, or
///    clocks disagree), fall back to the newest matching directory — a
///    worktree directory is unique per node, so the latest row is the
///    current session.
pub fn select_id_for_directory<'a>(
    sessions: &'a [ListedSession],
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<&'a str> {
    let matching: Vec<&ListedSession> = sessions
        .iter()
        .filter(|s| directories_match(&s.directory, spawn_directory))
        .filter(|s| is_opencode_session_id(&s.id))
        .collect();
    if matching.is_empty() {
        return None;
    }
    let in_window: Vec<&ListedSession> = matching
        .iter()
        .copied()
        .filter(|s| s.created >= created_not_before_ms)
        .collect();
    let pool = if in_window.is_empty() {
        matching
    } else {
        in_window
    };
    pool.into_iter()
        .max_by_key(|s| (s.created, s.updated))
        .map(|s| s.id.as_str())
}

/// OpenCode session IDs start with `ses_` (schema `SessionID`).
pub fn is_opencode_session_id(id: &str) -> bool {
    id.starts_with("ses_") && id.len() > 4
}

fn directories_match(listed: &str, spawn: &str) -> bool {
    let a = normalize_directory(listed);
    let b = normalize_directory(spawn);
    if looks_windows_path(&a) || looks_windows_path(&b) {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn normalize_directory(path: &str) -> String {
    let mut s = path.replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

fn looks_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':'
}

/// Wall-clock ms used as the "not before" bound: spawn time minus 10s so a
/// TUI that created the session slightly before we sampled `SystemTime`
/// still matches.
pub const CAPTURE_SKEW_MS: i64 = 10_000;

const LIST_TIMEOUT: Duration = Duration::from_secs(8);

/// Run `opencode session list --format json` in the same environment the
/// agent was spawned into (host cmd shim on Windows, `wsl.exe --exec` for
/// WSL nodes, direct elsewhere).
pub fn list_sessions_json(env_type: EnvType) -> Result<String, String> {
    let command = if env_type == EnvType::Wsl {
        let distro = crate::env::detect_default_wsl_distro()
            .ok_or_else(|| "default WSL distribution is unavailable".to_string())?;
        let mut command = command_no_window("wsl.exe");
        command.args([
            "-d",
            &distro,
            "--exec",
            "opencode",
            "session",
            "list",
            "--format",
            "json",
            "-n",
            "50",
        ]);
        command
    } else if cfg!(target_os = "windows") {
        let mut command = command_no_window("cmd.exe");
        command.args(["/d", "/c", "opencode session list --format json -n 50"]);
        command
    } else {
        let mut command = command_no_window("opencode");
        command.args(["session", "list", "--format", "json", "-n", "50"]);
        command
    };
    let output = run_command_with_timeout(command, "opencode session list", LIST_TIMEOUT)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "opencode session list exited {}: {stderr}",
            output.status
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// One-shot capture: list, parse, select. Returns None on any failure so
/// the poller can retry.
pub fn try_capture_cli_session_id(
    env_type: EnvType,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    let json = match list_sessions_json(env_type) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("opencode session list failed: {e}");
            return None;
        }
    };
    let sessions = match parse_session_list_json(&json) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("opencode session list parse failed: {e}");
            return None;
        }
    };
    select_id_for_directory(&sessions, spawn_directory, created_not_before_ms).map(str::to_string)
}

/// Retry delays after spawn. The TUI needs a beat to mint the session row.
const RETRY_DELAYS_MS: &[u64] = &[400, 800, 1_600, 2_500, 4_000];

/// Background poller: learn the OpenCode `ses_…` ID and write it to
/// `agent_nodes.cli_session_id`. Overwrites a PTY-regex false positive
/// (a labeled UUID that isn't an OpenCode ID) so resume gets `--session ses_…`.
pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType, spawn_epoch_ms: i64) {
    std::thread::Builder::new()
        .name(format!("opencode-session-capture-{node_id}"))
        .spawn(move || {
            let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
            for (i, delay) in RETRY_DELAYS_MS.iter().enumerate() {
                std::thread::sleep(Duration::from_millis(*delay));
                if let Some(id) = try_capture_cli_session_id(env_type, &spawn_directory, not_before) {
                    match crate::db::update_cli_session_id(node_id, &id) {
                        Ok(()) => {
                            tracing::info!(
                                "opencode session capture: stored {id} for node {node_id} (attempt {})",
                                i + 1
                            );
                            return;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "opencode session capture: db write failed for node {node_id}: {e}"
                            );
                            return;
                        }
                    }
                }
            }
            tracing::warn!(
                "opencode session capture: gave up for node {node_id} in {spawn_directory}"
            );
        })
        .ok();
}

pub fn now_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
  {
    "id": "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
    "title": "Buildmesh local dev version numbering scheme",
    "updated": 1787752625959,
    "created": 1787693314118,
    "projectId": "2c4cce34eaf0c0a8f9f9827cd4961a657f8919ab",
    "directory": "F:\\src\\buildmesh\\.claude\\worktrees\\high-crisp-buttercup"
  },
  {
    "id": "ses_fc256eca5ffeGxygWliQQsJGNP",
    "title": "New session - 2026-08-26T10:41:25.850Z",
    "updated": 1787748927801,
    "created": 1787740885850,
    "projectId": "2c4cce34eaf0c0a8f9f9827cd4961a657f8919ab",
    "directory": "F:\\src\\buildmesh"
  },
  {
    "id": "ses_fc5464a2effeYtpofsVCRSVxxe",
    "title": "Buildmesh codebase review with GitHub issues",
    "updated": 1787693652506,
    "created": 1787691644370,
    "projectId": "2c4cce34eaf0c0a8f9f9827cd4961a657f8919ab",
    "directory": "F:\\src\\buildmesh\\.claude\\worktrees\\monetary-insular-pier"
  }
]"#;

    #[test]
    fn parse_live_session_list_fixture() {
        let sessions = parse_session_list_json(FIXTURE).expect("fixture parses");
        assert_eq!(sessions.len(), 3);
        assert_eq!(sessions[0].id, "ses_fc52ccfb9ffek1jl23ZwpRuSP7");
        assert_eq!(
            sessions[0].directory,
            "F:\\src\\buildmesh\\.claude\\worktrees\\high-crisp-buttercup"
        );
        assert_eq!(
            sessions[1].project_id.as_deref(),
            Some("2c4cce34eaf0c0a8f9f9827cd4961a657f8919ab")
        );
    }

    #[test]
    fn parse_empty_list_and_blank_stdout() {
        assert!(parse_session_list_json("[]").unwrap().is_empty());
        assert!(parse_session_list_json("  \n").unwrap().is_empty());
    }

    #[test]
    fn select_matches_windows_directory_slash_and_case() {
        let sessions = parse_session_list_json(FIXTURE).unwrap();
        let id = select_id_for_directory(
            &sessions,
            r"f:/src/buildmesh/.claude/worktrees/high-crisp-buttercup",
            0,
        );
        assert_eq!(id, Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"));
    }

    #[test]
    fn select_prefers_newest_in_time_window_for_same_directory() {
        let sessions = vec![
            ListedSession {
                id: "ses_oldersessionid0000000001".into(),
                title: "old".into(),
                updated: 100,
                created: 100,
                project_id: None,
                directory: "/tmp/worktree".into(),
            },
            ListedSession {
                id: "ses_newersessionid0000000002".into(),
                title: "new".into(),
                updated: 200,
                created: 200,
                project_id: None,
                directory: "/tmp/worktree".into(),
            },
        ];
        assert_eq!(
            select_id_for_directory(&sessions, "/tmp/worktree", 150),
            Some("ses_newersessionid0000000002")
        );
    }

    #[test]
    fn select_falls_back_to_newest_directory_match_outside_window() {
        let sessions = parse_session_list_json(FIXTURE).unwrap();
        // Window is after every created timestamp — still pick the (only)
        // matching directory row rather than returning None.
        let id = select_id_for_directory(
            &sessions,
            r"F:\src\buildmesh",
            9_000_000_000_000,
        );
        assert_eq!(id, Some("ses_fc256eca5ffeGxygWliQQsJGNP"));
    }

    #[test]
    fn select_ignores_other_directories() {
        let sessions = parse_session_list_json(FIXTURE).unwrap();
        assert_eq!(
            select_id_for_directory(&sessions, r"F:\src\other-repo", 0),
            None
        );
    }

    #[test]
    fn select_rejects_non_ses_ids() {
        let sessions = vec![ListedSession {
            id: "01a024d2-7cd6-7ea2-b907-531b0d261be7".into(),
            title: "uuid".into(),
            updated: 1,
            created: 1,
            project_id: None,
            directory: "/tmp/worktree".into(),
        }];
        assert_eq!(
            select_id_for_directory(&sessions, "/tmp/worktree", 0),
            None
        );
    }

    #[test]
    fn unix_directories_are_case_sensitive() {
        let sessions = vec![ListedSession {
            id: "ses_unixpathid00000000000001".into(),
            title: "n".into(),
            updated: 1,
            created: 1,
            project_id: None,
            directory: "/home/Adam/src".into(),
        }];
        assert_eq!(
            select_id_for_directory(&sessions, "/home/adam/src", 0),
            None
        );
        assert_eq!(
            select_id_for_directory(&sessions, "/home/Adam/src/", 0),
            Some("ses_unixpathid00000000000001")
        );
    }
}
