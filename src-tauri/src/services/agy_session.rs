//! Antigravity session-ID capture from the brain directory.
//!
//! Antigravity (`agy`) self-assigns a UUIDv4 conversation ID and stores every
//! conversation globally under `~/.gemini/antigravity-cli/brain/
//! <conversation-id>/.system_generated/logs/transcript.jsonl` (issue #1283).
//! The interactive TUI does **not** print that UUID to stdout, so the PTY
//! UUID regex in `session_capture` can never match it (issue #1499). After a
//! fresh spawn we scan the brain directory for the conversation this spawn
//! minted, then persist it with `db::set_cli_session_id_if_missing` so
//! `auto_resume_agent_nodes` and manual "Resume"
//! (`agy --conversation <uuid>`) keep working across app restarts.
//!
//! How a conversation is recognised as ours (all grounded in on-disk shapes
//! observed in real transcripts, not assumed fields):
//!
//! - **Creation time, not mtime.** Step 0 carries `created_at` (the moment
//!   the conversation started). A resumed or long-running conversation keeps
//!   its original step-0 timestamp, so filtering on it — rather than the
//!   transcript's modification time — means a freshly spawned node can never
//!   steal the UUID of another active session that just wrote a step. mtime
//!   is used only as a cheap prefilter before opening a file (creation can
//!   never be newer than modification, so the gate cannot exclude a genuinely
//!   fresh conversation) to avoid parsing hundreds of stale transcripts.
//! - **Workspace anchor when provable.** Model steps carry
//!   `tool_calls[].args.Cwd` (e.g. `"Cwd":"\"F:/src/repo\""` — note the
//!   embedded quotes). When a candidate carries a `Cwd`, it must match the
//!   node's spawn path or the candidate is excluded. Step 0 itself has no
//!   `tool_calls`, so a transcript that has not made a tool call yet has an
//!   *unknown* workspace and is judged on timing alone.
//! - **Single-fresh binding.** A candidate created inside this spawn's time
//!   window is bound only when it is the single viable candidate: an
//!   anchored match wins outright, otherwise exactly one viable (anchored or
//!   unknown) candidate binds. Two viable fresh conversations — two nodes
//!   spawning at once, or the user running `agy` standalone — bind nothing;
//!   the `Stop` attention hook (`conversationId` extraction in
//!   `http/routes/attention.rs`) stays as the secondary capture path for
//!   those cases.
//!
//! Historic recovery lives in `session_recovery`: it requires a matching
//! workspace anchor and a bounded launch window. Unknown workspaces that are
//! eligible for live capture are deliberately ineligible for historic recovery.
//!
//! Select/match helpers are pure so the rules are unit-tested without a live
//! `agy` binary. Filesystem scanning is tested against temp brain roots built
//! with the real step shape (`step_index`/`source`/`type`/`status`/
//! `created_at`/`content`/`tool_calls`).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

use crate::models::EnvType;

pub const CAPTURE_SKEW_MS: i64 = 2_000;
const RETRY_DELAYS_MS: &[u64] = &[500, 1_000, 2_000, 4_000];
/// Transcript lines scanned per candidate. Step 0 carries the creation
/// timestamp and the first tool calls (with `Cwd`) appear within the opening
/// steps; bounding the read keeps a huge resumed transcript from stalling the
/// poller on every retry.
const SCAN_LINE_BUDGET: usize = 25;

/// One Antigravity conversation candidate found under the brain root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    /// Step-0 `created_at` as epoch ms — the conversation's true start.
    pub created_ms: i64,
    /// Workspace from `tool_calls[].args.Cwd` when any scanned step carried
    /// one; `None` means unknown (e.g. no tool call yet), not "matches
    /// anywhere".
    pub workspace: Option<String>,
}

/// Antigravity conversation IDs are UUIDs (the same UUID the `Stop` hook
/// delivers as `conversationId` and `resume_args` feeds to `--conversation`).
/// Reject anything else so temp dirs or partial writes never bind a node.
pub fn is_agy_conversation_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

/// Metadata read from a conversation transcript in a single bounded pass:
/// the step-0 creation timestamp plus the first `tool_calls[].args.Cwd`
/// workspace anchor, if any scanned step carried one.
///
/// Returns `None` when no scanned line yields a parsable creation timestamp —
/// without proof of *when* the conversation started, freshness cannot be
/// established and the candidate is skipped rather than guessed at.
fn read_conversation_meta(path: &Path) -> Option<(i64, Option<String>)> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut created_ms: Option<i64> = None;
    let mut workspace: Option<String> = None;
    let mut scanned = 0usize;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        scanned += 1;
        if scanned > SCAN_LINE_BUDGET {
            break;
        }
        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if created_ms.is_none() {
            created_ms = val
                .get("created_at")
                .or_else(|| val.get("timestamp"))
                .and_then(|v| v.as_str())
                .and_then(parse_rfc3339_ms);
        }
        if workspace.is_none() {
            workspace = extract_cwd_anchor(&val);
        }
        if created_ms.is_some() && workspace.is_some() {
            break;
        }
    }
    created_ms.map(|ms| (ms, workspace))
}

/// Pull the workspace anchor out of a transcript step's tool calls. Observed
/// on real transcripts as `tool_calls: [{name: "run_command", args:
/// {Cwd: "\"F:/src/repo\"", ...}}]` — the value arrives wrapped in an extra
/// layer of quotes, which is stripped here. Only the observed `Cwd` key (plus
/// its lowercase variant) is read; anything else would be guessing at an
/// undocumented schema.
fn extract_cwd_anchor(val: &serde_json::Value) -> Option<String> {
    let calls = val.get("tool_calls")?.as_array()?;
    for call in calls {
        let Some(args) = call.get("args") else {
            continue;
        };
        for key in ["Cwd", "cwd"] {
            if let Some(raw) = args.get(key).and_then(|v| v.as_str()) {
                // Some transcripts JSON-encode the entire argument, including
                // Windows backslashes. JSON decoding already removes the
                // enclosing quotes and escape sequences; plain text is kept as
                // written when decoding is not applicable.
                let cleaned = serde_json::from_str::<String>(raw)
                    .unwrap_or_else(|_| raw.to_owned())
                    .trim()
                    .to_owned();
                if !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        }
    }
    None
}

fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn transcript_mtime_ms(path: &Path) -> Option<i64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// Build a candidate from a conversation directory. Returns `None` for
/// anything that cannot be proven fresh: non-UUID names, missing transcripts,
/// transcripts untouched since before this spawn's window (cheap mtime gate —
/// creation never postdates modification), and transcripts with no parsable
/// creation timestamp.
fn read_conversation_candidate(
    brain_dir: &Path,
    conv_dir: &Path,
    conv_id: &str,
    created_not_before_ms: i64,
) -> Option<Candidate> {
    if !is_agy_conversation_id(conv_id) {
        return None;
    }
    // Shared with the transcript reader so the layout never drifts between
    // the two (`transcript.jsonl` first, `transcript_full.jsonl` fallback).
    let transcript = crate::services::transcript_reader::agy_locator_in(brain_dir, conv_id)?;
    debug_assert!(
        transcript.starts_with(conv_dir),
        "locator resolved outside the scanned conversation dir"
    );
    // Cheap gate first: a transcript last written before the spawn window
    // necessarily started before it too, so it can be skipped without
    // opening or parsing a single line.
    if transcript_mtime_ms(&transcript)? < created_not_before_ms {
        return None;
    }
    let (created_ms, workspace) = read_conversation_meta(&transcript)?;
    if created_ms < created_not_before_ms {
        return None;
    }
    Some(Candidate {
        id: conv_id.to_string(),
        created_ms,
        workspace,
    })
}

/// Pick the conversation ID to store for a freshly spawned node.
///
/// Candidates passed in must already be proven fresh (creation inside the
/// spawn window). A candidate whose `Cwd` anchor provably belongs elsewhere
/// is excluded. An anchored match for `spawn_directory` wins outright;
/// otherwise binding requires exactly one viable candidate — two viable
/// fresh conversations (a sibling spawn, a standalone `agy` run) bind
/// nothing rather than risk cross-wiring sessions.
pub fn select_id_for_directory<'a>(
    candidates: &'a [Candidate],
    spawn_directory: &str,
) -> Option<&'a str> {
    // A candidate with a non-empty, non-matching workspace provably belongs
    // elsewhere and is excluded. Unknown workspaces stay viable.
    let viable: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| is_agy_conversation_id(&c.id))
        .filter(|c| match &c.workspace {
            Some(dir) if !dir.trim().is_empty() => {
                crate::env::directories_match(dir, spawn_directory)
            }
            _ => true,
        })
        .collect();
    // A proven anchor for this directory wins outright — unless two different
    // conversations both claim it, in which case nothing binds.
    let claims: Vec<&Candidate> = viable
        .iter()
        .filter(|c| {
            c.workspace
                .as_deref()
                .is_some_and(|dir| crate::env::directories_match(dir, spawn_directory))
        })
        .copied()
        .collect();
    if claims.len() == 1 {
        return Some(claims[0].id.as_str());
    }
    if !claims.is_empty() {
        return None;
    }
    // No proven anchor: bind only a single viable candidate.
    if viable.len() == 1 {
        return Some(viable[0].id.as_str());
    }
    None
}

pub(crate) fn collect_candidates(brain_dir: &Path, created_not_before_ms: i64) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(brain_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let conv_dir = entry.path();
        if !conv_dir.is_dir() {
            continue;
        }
        let conv_id = entry.file_name().to_string_lossy().to_string();
        if let Some(candidate) =
            read_conversation_candidate(brain_dir, &conv_dir, &conv_id, created_not_before_ms)
        {
            out.push(candidate);
        }
    }
    out
}

/// Historic startup recovery entry point used by the AGY adapter. Workspace
/// matching remains provider-owned because AGY records it inside tool-call
/// metadata rather than in the conversation directory name.
pub(crate) fn find_historic_id_for_directory(
    env_type: EnvType,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let brain_dir = crate::env::agy_brain_dir_for_env(env_type, spawn_directory)?;
    find_historic_id_for_directory_in(&brain_dir, spawn_directory, anchor_ms, recorded_start)
}

pub(crate) fn find_historic_id_for_directory_in(
    brain_dir: &Path,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let cutoff = anchor_ms.saturating_sub(crate::services::session_recovery::CLOCK_SKEW_MS);
    let candidates = collect_candidates(&brain_dir, cutoff)
        .into_iter()
        .filter(|candidate| {
            candidate
                .workspace
                .as_deref()
                .is_some_and(|cwd| crate::env::directories_match(cwd, spawn_directory))
        });
    crate::services::session_recovery::select_recovery_identity(
        candidates.map(|candidate| (candidate.id, candidate.created_ms)),
        anchor_ms,
        recorded_start,
    )
}

/// Scan `brain_dir` for the conversation this spawn minted: proven fresh by
/// step-0 creation time inside the spawn window, bound only when it is the
/// single viable candidate (see `select_id_for_directory`).
pub fn find_fresh_id_for_directory_in(
    brain_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    if !brain_dir.is_dir() {
        return None;
    }
    let candidates = collect_candidates(brain_dir, created_not_before_ms);
    select_id_for_directory(&candidates, spawn_directory).map(str::to_string)
}

enum CaptureAttempt {
    AlreadyStored,
    NotFound,
    Stored(String),
}

/// Poll briefly for Antigravity's brain-directory conversation, then fill an
/// otherwise empty `cli_session_id`. The hook path may win first; the DB
/// predicate keeps this delayed fallback from overwriting it. Creation-time
/// gating (not mtime) makes late retries safe: an old conversation that
/// writes mid-poll can never look fresh.
pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        for (attempt, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                return;
            }
            let Some(brain_dir) = crate::env::agy_brain_dir_for_env(env_type, &spawn_directory)
            else {
                tracing::warn!("agy session capture: no brain directory for env {env_type:?}");
                return;
            };
            let path = brain_dir.clone();
            let directory = spawn_directory.clone();
            // Keep the DB predicate, disk scan, and conditional write in one
            // blocking task. Splitting these into three dispatches on every
            // retry needlessly thrashes the blocking pool and widens the race
            // window between finding a conversation and claiming the row.
            let captured = crate::blocking::run_blocking("agy_capture", move || {
                if crate::db::cli_session_id_present(node_id).map_err(|error| error.to_string())? {
                    return Ok(CaptureAttempt::AlreadyStored);
                }
                let Some(id) = find_fresh_id_for_directory_in(&path, &directory, not_before) else {
                    return Ok(CaptureAttempt::NotFound);
                };
                match crate::db::set_cli_session_id_if_missing(node_id, &id) {
                    Ok(true) => Ok(CaptureAttempt::Stored(id)),
                    Ok(false) => Ok(CaptureAttempt::AlreadyStored),
                    Err(error) => Err(error.to_string()),
                }
            })
            .await;
            let capture = match captured {
                Ok(attempt) => attempt,
                Err(error) => {
                    tracing::warn!(
                        "agy session capture: blocking task failed for node {node_id}: {error}"
                    );
                    return;
                }
            };
            let id = match capture {
                CaptureAttempt::AlreadyStored => return,
                CaptureAttempt::NotFound => continue,
                CaptureAttempt::Stored(id) => id,
            };
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                return;
            }
            tracing::info!(
                "agy session capture: stored {id} for node {node_id} (attempt {})",
                attempt + 1
            );
            return;
        }
        tracing::warn!("agy session capture: gave up for node {node_id} in {spawn_directory}");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const UUID_A: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_B: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";

    fn cand(id: &str, created_ms: i64, workspace: Option<&str>) -> Candidate {
        Candidate {
            id: id.into(),
            created_ms,
            workspace: workspace.map(str::to_string),
        }
    }

    /// Real step shape (see `tests/fixtures/agy_transcript.jsonl`): flat
    /// keys, `created_at` on every step, tool `Cwd` with embedded quotes.
    fn user_step(created_at: &str, content: &str) -> String {
        serde_json::json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": created_at,
            "content": content,
            "thinking": null,
            "tool_calls": [],
            "truncated_fields": [],
        })
        .to_string()
    }

    fn tool_step(created_at: &str, cwd: &str) -> String {
        serde_json::json!({
            "step_index": 3,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": created_at,
            "tool_calls": [{
                "name": "run_command",
                "args": {"CommandLine": "ls", "Cwd": format!("\"{cwd}\"")},
            }],
        })
        .to_string()
    }

    fn write_conv(brain: &Path, conv_id: &str, body: &str) -> std::path::PathBuf {
        let logs = brain.join(conv_id).join(".system_generated").join("logs");
        fs::create_dir_all(&logs).unwrap();
        let path = logs.join("transcript.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
        brain.join(conv_id)
    }

    #[test]
    fn validates_agy_conversation_id_as_uuid() {
        assert!(is_agy_conversation_id(UUID_A));
        assert!(is_agy_conversation_id(UUID_B));
        assert!(!is_agy_conversation_id("conv-aaaa-1111"));
        assert!(!is_agy_conversation_id("sess_01j6xyz890"));
        assert!(!is_agy_conversation_id(""));
    }

    #[test]
    fn single_fresh_candidate_binds_without_workspace_anchor() {
        // Step 0 carries no tool_calls on a just-spawned session; timing
        // alone binds it when nothing else is fresh.
        let candidates = vec![cand(UUID_A, 200, None)];
        assert_eq!(
            select_id_for_directory(&candidates, "/tmp/wt"),
            Some(UUID_A)
        );
    }

    #[test]
    fn anchored_match_wins_over_unknown_candidate() {
        let candidates = vec![cand(UUID_A, 300, None), cand(UUID_B, 200, Some("/tmp/wt"))];
        assert_eq!(
            select_id_for_directory(&candidates, "/tmp/wt"),
            Some(UUID_B)
        );
    }

    #[test]
    fn anchored_foreign_candidate_is_excluded_leaving_single_viable() {
        // Proven to belong elsewhere; the remaining unknown candidate binds.
        let candidates = vec![
            cand(UUID_A, 300, Some("/tmp/other")),
            cand(UUID_B, 200, None),
        ];
        assert_eq!(
            select_id_for_directory(&candidates, "/tmp/wt"),
            Some(UUID_B)
        );
    }

    #[test]
    fn two_viable_fresh_candidates_bind_nothing() {
        // Sibling spawn or standalone `agy` run: never cross-wire.
        let candidates = vec![cand(UUID_A, 200, None), cand(UUID_B, 250, None)];
        assert_eq!(select_id_for_directory(&candidates, "/tmp/wt"), None);
    }

    #[test]
    fn two_claims_on_same_directory_bind_nothing() {
        let candidates = vec![
            cand(UUID_A, 200, Some("/tmp/wt")),
            cand(UUID_B, 250, Some("/tmp/wt")),
        ];
        assert_eq!(select_id_for_directory(&candidates, "/tmp/wt"), None);
    }

    #[test]
    fn rejects_non_uuid_ids() {
        let candidates = vec![cand("conv-aaaa-1111", 200, None)];
        assert_eq!(select_id_for_directory(&candidates, "/tmp/wt"), None);
    }

    #[test]
    fn anchored_match_accepts_windows_slash_and_case() {
        let candidates = vec![cand(
            UUID_A,
            200,
            Some(r"F:\src\buildmesh\.claude\worktrees\agy-test"),
        )];
        assert_eq!(
            select_id_for_directory(&candidates, "f:/src/buildmesh/.claude/worktrees/agy-test"),
            Some(UUID_A)
        );
    }

    #[test]
    fn finds_single_fresh_session_with_real_shape() {
        let temp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let fresh =
            (now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let not_before = (now - chrono::Duration::seconds(30)).timestamp_millis();
        write_conv(
            temp.path(),
            UUID_A,
            &format!(
                "{}\n{}",
                user_step(&fresh, "do the thing"),
                tool_step(&fresh, "/tmp/wt"),
            ),
        );
        assert_eq!(
            find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", not_before),
            Some(UUID_A.to_string())
        );
    }

    #[test]
    fn historic_recovery_uses_json_encoded_windows_workspace_anchor() {
        const CREATED: i64 = 1_787_830_399_000;
        const TIMESTAMP: &str = "2026-08-27T11:33:19.986Z";
        let temp = tempfile::TempDir::new().unwrap();
        let transcript = write_conv(
            temp.path(),
            UUID_A,
            &serde_json::json!({
                "created_at": TIMESTAMP,
                "tool_calls": [{
                    "args": {"Cwd": serde_json::to_string(r"F:\repo").unwrap()}
                }]
            })
            .to_string(),
        )
        .join(".system_generated/logs/transcript.jsonl");

        assert_eq!(
            find_historic_id_for_directory_in(temp.path(), "F:/repo", CREATED, true),
            Some(UUID_A.to_string())
        );

        fs::write(
            transcript,
            serde_json::json!({"created_at": TIMESTAMP}).to_string(),
        )
        .unwrap();
        assert_eq!(
            find_historic_id_for_directory_in(temp.path(), "F:/repo", CREATED, true),
            None,
            "an AGY transcript without a workspace anchor must not be bound during historic recovery",
        );
    }

    #[test]
    fn stale_creation_is_rejected_despite_fresh_mtime() {
        // The steal-a-live-session regression: file written NOW, but step-0
        // creation is old. mtime freshness must not bind it.
        let temp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let not_before = (now - chrono::Duration::seconds(30)).timestamp_millis();
        write_conv(
            temp.path(),
            UUID_A,
            &user_step("2020-01-01T00:00:00Z", "old conversation, touched today"),
        );
        assert_eq!(
            find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", not_before),
            None
        );
    }

    #[test]
    fn untouched_transcript_is_skipped_without_parsing() {
        // mtime gate: transcript last written before the window never binds,
        // however fresh its content claims to be.
        let temp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let fresh =
            (now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        write_conv(temp.path(), UUID_A, &user_step(&fresh, "hi"));
        let future = (now + chrono::Duration::seconds(3600)).timestamp_millis();
        assert_eq!(
            find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", future),
            None
        );
    }

    #[test]
    fn transcript_without_creation_timestamp_is_skipped() {
        // No proof of when it started: skip rather than guess.
        let temp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let not_before = (now - chrono::Duration::seconds(30)).timestamp_millis();
        write_conv(
            temp.path(),
            UUID_A,
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","content":"no clock"}"#,
        );
        assert_eq!(
            find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", not_before),
            None
        );
    }

    #[test]
    fn skips_non_uuid_directories_and_missing_transcripts() {
        let temp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("conv-aaaa-1111")).unwrap();
        fs::create_dir_all(temp.path().join(UUID_A)).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        assert_eq!(
            find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", now - 60_000),
            None
        );
    }

    #[test]
    fn transcript_full_fallback_resolves_when_short_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let fresh =
            (now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let not_before = (now - chrono::Duration::seconds(30)).timestamp_millis();
        let conv = temp.path().join(UUID_A);
        let logs = conv.join(".system_generated").join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("transcript_full.jsonl"), user_step(&fresh, "hi")).unwrap();
        assert_eq!(
            find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", not_before),
            Some(UUID_A.to_string())
        );
    }

    #[test]
    fn embedded_quotes_in_cwd_anchor_are_stripped() {
        // Real `Cwd` values arrive double-wrapped: `"\"F:/repo\""`.
        let val = serde_json::json!({
            "tool_calls": [{"name": "run_command", "args": {"Cwd": "\"F:/src/repo\""}}],
        });
        assert_eq!(extract_cwd_anchor(&val).as_deref(), Some("F:/src/repo"));
    }

    #[test]
    fn wsl_brain_dir_derives_from_spawn_path_user() {
        let dir = crate::env::agy_dir_for_env(crate::models::EnvType::Wsl, "/home/alice/src/repo")
            .expect("derivable from /home/ prefix");
        assert_eq!(
            dir.join("brain"),
            std::path::PathBuf::from("/home/alice/.gemini/antigravity-cli/brain")
        );
    }

    #[test]
    fn wsl_brain_dir_without_home_prefix_is_none() {
        // Never guess a username: an underivable home yields no directory
        // rather than a wrong user's brain.
        assert!(
            crate::env::agy_dir_for_env(crate::models::EnvType::Wsl, "/mnt/c/src/repo").is_none()
        );
    }
}
