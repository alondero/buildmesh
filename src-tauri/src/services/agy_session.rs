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
//! - **Launch workspace when available.** The harness's read-only
//!   `conversation_summaries.db` records `workspace_uris` by conversation ID.
//!   A single distinct decodable file URI can anchor the conversation, even
//!   when review commands run in another worktree. Unsupported URI members are
//!   ignored; multiple distinct file roots are ambiguous and fall back to
//!   transcript Cwd. Matching still uses the existing directory comparison,
//!   so WSL guest paths and host UNC paths are not reconciled here. Missing or
//!   unreadable summary metadata, or a row with no unambiguous file root, falls
//!   back to transcript `tool_calls[].args.Cwd`
//!   (e.g. `"Cwd":"\"F:/src/repo\""`); malformed row encoding for a matched
//!   conversation remains unverified. Without either source, workspace is
//!   unknown and only timing can qualify a fresh candidate.
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

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
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
    /// One unambiguous decoded launch workspace, or transcript Cwd when
    /// summary metadata is absent, unreadable, blank, or ambiguous. Empty
    /// means unknown, not a match anywhere.
    pub workspaces: Vec<String>,
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
        let Some(args) = call.get("args") else { continue; };
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkspaceSummary {
    Blank,
    Encoded(String),
    Invalid(String),
}

type WorkspaceSummaries = HashMap<String, WorkspaceSummary>;

const SUMMARY_BUSY_TIMEOUT: Duration = Duration::from_millis(200);
const SUMMARY_QUERY_CHUNK_SIZE: usize = 500;

/// Fetch only summaries for transcripts that passed the cheap mtime gate.
/// The connection and statement are dropped before the caller reads files.
fn read_workspace_summaries(
    summary_db_path: &Path,
    candidate_ids: &[String],
) -> Result<Option<WorkspaceSummaries>, String> {
    if candidate_ids.is_empty() {
        return Ok(None);
    }
    match fs::symlink_metadata(summary_db_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = summary_db_path
                .parent()
                .ok_or("summary database path has no parent")?
                .metadata()
                .map_err(|error| error.to_string())?;
            return if parent.is_dir() {
                Ok(None)
            } else {
                Err("summary database parent is not a directory".into())
            };
        }
        Err(error) => return Err(error.to_string()),
    }
    let conn = rusqlite::Connection::open_with_flags(
        summary_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    conn.busy_timeout(SUMMARY_BUSY_TIMEOUT)
        .map_err(|error| error.to_string())?;

    let mut summaries = HashMap::new();
    for id_chunk in candidate_ids.chunks(SUMMARY_QUERY_CHUNK_SIZE) {
        let placeholders = vec!["?"; id_chunk.len()].join(", ");
        let query = format!(
            "SELECT conversation_id, workspace_uris FROM conversation_summaries \
             WHERE conversation_id IN ({placeholders})"
        );
        let mut statement = conn
            .prepare(&query)
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(id_chunk.iter()), |row| {
                let conversation_id: String = row.get(0)?;
                let workspace = match row.get_ref(1)? {
                    rusqlite::types::ValueRef::Null => WorkspaceSummary::Blank,
                    rusqlite::types::ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
                        Ok(raw) if raw.trim().is_empty() => WorkspaceSummary::Blank,
                        Ok(raw) => WorkspaceSummary::Encoded(raw.to_owned()),
                        Err(error) => WorkspaceSummary::Invalid(format!(
                            "workspace_uris is not UTF-8: {error}"
                        )),
                    },
                    value => WorkspaceSummary::Invalid(format!(
                        "workspace_uris has unexpected SQLite type {:?}",
                        value.data_type()
                    )),
                };
                Ok((conversation_id, workspace))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (conversation_id, workspace) = row.map_err(|error| error.to_string())?;
            summaries.insert(conversation_id, workspace);
        }
    }
    Ok(Some(summaries))
}

fn workspace_uri_paths(raw: &str) -> Result<Vec<String>, String> {
    let uris: Vec<String> = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    // Unsupported members cannot provide an anchor. The observed schema does
    // not establish that multiple file roots are all launch-scoped, so use
    // transcript Cwd instead of allowing an unverified root to claim a node.
    let mut roots = Vec::new();
    for uri in &uris {
        if let Ok(path) = workspace_uri_path(uri) {
            if !roots.contains(&path) {
                roots.push(path);
            }
        }
    }
    if roots.len() > 1 {
        return Ok(Vec::new());
    }
    Ok(roots)
}

fn workspace_uri_path(uri: &str) -> Result<String, ()> {
    if !valid_percent_escapes(uri) {
        return Err(());
    }
    let url = reqwest::Url::parse(uri).map_err(|_| ())?;
    if url.scheme() != "file" || url.query().is_some() || url.fragment().is_some() {
        return Err(());
    }
    // Keep CLI path syntax: host-native to_file_path rejects WSL /home URIs
    // on Windows. Decode escapes without treating literal '+' as a space.
    let mut bytes = url.path().bytes();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let hi = char::from(bytes.next().ok_or(())?).to_digit(16).ok_or(())?;
            let lo = char::from(bytes.next().ok_or(())?).to_digit(16).ok_or(())?;
            (hi * 16 + lo) as u8
        } else {
            byte
        });
    }
    let path = String::from_utf8(decoded).map_err(|_| ())?;
    if path.contains('\0') {
        return Err(());
    }
    if let Some(host) = url.host_str() {
        return Ok(format!("//{host}{path}"));
    }
    let path_bytes = path.as_bytes();
    if path_bytes.len() >= 4 && path_bytes[1].is_ascii_alphabetic() && path_bytes[2..4] == *b":/" {
        Ok(path[1..].to_owned())
    } else {
        Ok(path)
    }
}

fn valid_percent_escapes(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

/// Find a transcript that could have been created in this scan's time window.
/// The mtime gate is deliberately before database lookup and transcript parse.
fn candidate_transcript_path(
    brain_dir: &Path,
    conv_dir: &Path,
    conv_id: &str,
    created_not_before_ms: i64,
) -> Option<PathBuf> {
    if !is_agy_conversation_id(conv_id) {
        return None;
    }
    // Shared with the transcript reader so the layout never drifts between
    // the two (`transcript.jsonl` first, `transcript_full.jsonl` fallback).
    let transcript =
        crate::services::transcript_reader::adapters::agy::agy_locator_in(brain_dir, conv_id)?;
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
    Some(transcript)
}

/// Build a candidate after the mtime gate. A valid native launch workspace
/// set takes precedence over transcript Cwd; missing or unreadable database
/// metadata uses Cwd, while malformed metadata for this row rejects it.
fn read_conversation_candidate(
    conv_id: &str,
    transcript: &Path,
    created_not_before_ms: i64,
    summaries: Option<&WorkspaceSummaries>,
) -> Option<Candidate> {
    let (created_ms, transcript_workspace) = read_conversation_meta(transcript)?;
    if created_ms < created_not_before_ms {
        return None;
    }
    let workspaces = match summaries.and_then(|summaries| summaries.get(conv_id)) {
        Some(WorkspaceSummary::Encoded(raw)) => match workspace_uri_paths(raw) {
            Ok(workspaces) if !workspaces.is_empty() => workspaces,
            Ok(_) => transcript_workspace.into_iter().collect(),
            Err(error) => {
                tracing::warn!("agy session discovery: unusable workspace metadata for conversation {conv_id}: {error}");
                return None;
            }
        },
        Some(WorkspaceSummary::Invalid(error)) => {
            tracing::warn!("agy session discovery: unusable workspace metadata for conversation {conv_id}: {error}");
            return None;
        }
        Some(WorkspaceSummary::Blank) | None => transcript_workspace.into_iter().collect(),
    };
    Some(Candidate {
        id: conv_id.to_string(),
        created_ms,
        workspaces,
    })
}

/// Pick the conversation ID to store for a freshly spawned node.
///
/// Candidates passed in must already be proven fresh (creation inside the
/// spawn window). A candidate whose recorded roots exclude this directory
/// is excluded. An anchored match for `spawn_directory` wins outright;
/// otherwise binding requires exactly one viable candidate — two viable
/// fresh conversations (a sibling spawn, a standalone `agy` run) bind
/// nothing rather than risk cross-wiring sessions.
pub fn select_id_for_directory<'a>(
    candidates: &'a [Candidate],
    spawn_directory: &str,
) -> Option<&'a str> {
    // Candidates with more than one root are ambiguous and cannot bind. A
    // summary with that shape should already have fallen back to transcript
    // Cwd; keep this guard so a future caller cannot bypass that rule. Known
    // single roots that do not include this directory are excluded.
    let viable: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| is_agy_conversation_id(&c.id))
        .filter(|candidate| candidate.workspaces.len() <= 1)
        .filter(|candidate| {
            candidate.workspaces.is_empty()
                || candidate
                    .workspaces
                    .iter()
                    .any(|dir| crate::env::directories_match(dir, spawn_directory))
        })
        .collect();
    // A proven root match for this directory wins outright — unless two
    // different conversations both claim it, in which case nothing binds.
    let claims: Vec<&Candidate> = viable
        .iter()
        .filter(|c| {
            c.workspaces
                .iter()
                .any(|dir| crate::env::directories_match(dir, spawn_directory))
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

pub(crate) fn collect_candidates(
    brain_dir: &Path,
    summary_db_path: &Path,
    created_not_before_ms: i64,
) -> Vec<Candidate> {
    if !brain_dir.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(brain_dir) else {
        return Vec::new();
    };
    let mut transcript_candidates = Vec::new();
    for entry in entries.flatten() {
        let conv_dir = entry.path();
        if !conv_dir.is_dir() {
            continue;
        }
        let conv_id = entry.file_name().to_string_lossy().to_string();
        if let Some(transcript) = candidate_transcript_path(
            brain_dir,
            &conv_dir,
            &conv_id,
            created_not_before_ms,
        ) {
            transcript_candidates.push((conv_id, transcript));
        }
    }
    let candidate_ids = transcript_candidates
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let summaries = match read_workspace_summaries(summary_db_path, &candidate_ids) {
        Ok(summaries) => summaries,
        Err(error) => {
            tracing::warn!(
                summary_db_path = %summary_db_path.display(),
                candidate_count = candidate_ids.len(),
                "agy session discovery: summary lookup failed; falling back to transcript Cwd: {error}"
            );
            None
        }
    };
    tracing::debug!(
        summary_db_path = %summary_db_path.display(),
        candidate_count = candidate_ids.len(),
        summary_row_count = summaries.as_ref().map_or(0, HashMap::len),
        "agy session discovery: queried launch workspace summaries"
    );
    let mut out = Vec::new();
    for (conv_id, transcript) in transcript_candidates {
        if let Some(candidate) = read_conversation_candidate(
            &conv_id,
            &transcript,
            created_not_before_ms,
            summaries.as_ref(),
        ) {
            out.push(candidate);
        }
    }
    out
}

/// Historic startup recovery entry point used by the AGY adapter. Launch roots
/// come from the summaries database when readable, with transcript Cwd as the
/// legacy fallback; historic recovery still requires a matching known root.
pub(crate) fn find_historic_id_for_directory(
    env_type: EnvType,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let brain_dir = crate::env::agy_brain_dir_for_env(env_type, spawn_directory)?;
    let summary_db_path = crate::env::agy_summaries_db_for_env(env_type, spawn_directory)?;
    find_historic_id_for_directory_in(
        &brain_dir,
        &summary_db_path,
        spawn_directory,
        anchor_ms,
        recorded_start,
    )
}

pub(crate) fn find_historic_id_for_directory_in(
    brain_dir: &Path,
    summary_db_path: &Path,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let cutoff = anchor_ms.saturating_sub(crate::services::session_recovery::CLOCK_SKEW_MS);
    let candidates = collect_candidates(brain_dir, summary_db_path, cutoff)
        .into_iter()
        .filter(|candidate| {
            candidate
                .workspaces
                .iter()
                .any(|root| crate::env::directories_match(root, spawn_directory))
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
    summary_db_path: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    if !brain_dir.is_dir() {
        return None;
    }
    let candidates = collect_candidates(brain_dir, summary_db_path, created_not_before_ms);
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
            let Some(brain_dir) =
                crate::env::agy_brain_dir_for_env(env_type, &spawn_directory)
            else {
                tracing::warn!("agy session capture: no brain directory for env {env_type:?}");
                return;
            };
            let Some(summary_db_path) =
                crate::env::agy_summaries_db_for_env(env_type, &spawn_directory)
            else {
                tracing::warn!("agy session capture: no summaries database path for env {env_type:?}");
                return;
            };
            let path = brain_dir.clone();
            let summary_path = summary_db_path.clone();
            let directory = spawn_directory.clone();
            // Keep the DB predicate, disk scan, and conditional write in one
            // blocking task. Splitting these into three dispatches on every
            // retry needlessly thrashes the blocking pool and widens the race
            // window between finding a conversation and claiming the row.
            let captured = crate::blocking::run_blocking("agy_capture", move || {
                if crate::db::cli_session_id_present(node_id)
                    .map_err(|error| error.to_string())?
                {
                    return Ok(CaptureAttempt::AlreadyStored);
                }
                let Some(id) = find_fresh_id_for_directory_in(
                    &path,
                    &summary_path,
                    &directory,
                    not_before,
                )
                else {
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
    use std::time::Instant;

    const UUID_A: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_B: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";

    fn cand(id: &str, created_ms: i64, workspace: Option<&str>) -> Candidate {
        cand_with_workspaces(id, created_ms, workspace.into_iter().collect())
    }

    fn cand_with_workspaces(id: &str, created_ms: i64, roots: Vec<&str>) -> Candidate {
        Candidate {
            id: id.into(),
            created_ms,
            workspaces: roots.into_iter().map(str::to_string).collect(),
        }
    }

    fn test_summaries_db_path(temp_root: &Path) -> PathBuf {
        temp_root.join("conversation_summaries.db")
    }

    fn find_fresh_id_in_test_root(
        temp_root: &Path,
        brain_dir: &Path,
        spawn_directory: &str,
        created_not_before_ms: i64,
    ) -> Option<String> {
        let summary_db_path = test_summaries_db_path(temp_root);
        find_fresh_id_for_directory_in(
            brain_dir,
            &summary_db_path,
            spawn_directory,
            created_not_before_ms,
        )
    }

    fn find_historic_id_in_test_root(
        temp_root: &Path,
        brain_dir: &Path,
        spawn_directory: &str,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        let summary_db_path = test_summaries_db_path(temp_root);
        find_historic_id_for_directory_in(
            brain_dir,
            &summary_db_path,
            spawn_directory,
            anchor_ms,
            recorded_start,
        )
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
        let logs = brain
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&logs).unwrap();
        let path = logs.join("transcript.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
        brain.join(conv_id)
    }

    #[test]
    fn reviewer_recovery_uses_launch_workspace_not_tool_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let brain = temp.path().join("brain");
        let reviewer = temp.path().join("reviewer");
        let summary_db_path = test_summaries_db_path(temp.path());
        let start = "2026-09-29T06:19:46Z";
        write_conv(
            &brain,
            UUID_A,
            &format!(
                "{}\n{}\n",
                user_step(start, "Review the source worktree"),
                tool_step(start, "/repo/source")
            ),
        );
        let anchor = parse_rfc3339_ms(start).unwrap() - 12_000;
        assert_eq!(
            find_historic_id_in_test_root(
                temp.path(),
                &brain,
                "/repo/source",
                anchor,
                true
            ),
            Some(UUID_A.into()),
            "a missing summary database retains legacy transcript matching"
        );
        let conn = rusqlite::Connection::open(&summary_db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, workspace_uris TEXT);",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO conversation_summaries VALUES ('d9428888-122b-11e1-b85c-61cd3cbb3210', X'FF');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2)",
            rusqlite::params![
                UUID_A,
                serde_json::json!([
                    "vscode-remote://ssh-remote+host/repo",
                    reqwest::Url::from_directory_path(&reviewer).unwrap().as_str()
                ])
                .to_string()
            ],
        )
        .unwrap();
        assert_eq!(
            find_historic_id_in_test_root(
                temp.path(),
                &brain,
                &reviewer.to_string_lossy(),
                anchor,
                true
            ),
            Some(UUID_A.into())
        );
        assert_eq!(
            find_historic_id_in_test_root(temp.path(), &brain, "/repo/source", anchor, true),
            None
        );
        assert_eq!(
            find_fresh_id_in_test_root(
                temp.path(),
                &brain,
                &reviewer.to_string_lossy(),
                anchor
            ),
            Some(UUID_A.into())
        );
        for (uri, directory) in [
            (
                "file:///home/user/reviewer%20space+plus",
                "/home/user/reviewer space+plus",
            ),
            ("file:///F:/src/reviewer%20space", "F:/src/reviewer space"),
        ] {
            conn.execute(
                "UPDATE conversation_summaries SET workspace_uris = ?1",
                [serde_json::json!([uri]).to_string()],
            )
            .unwrap();
            assert_eq!(
                find_historic_id_in_test_root(temp.path(), &brain, directory, anchor, true),
                Some(UUID_A.into())
            );
        }
        conn.execute(
            "UPDATE conversation_summaries SET workspace_uris = ?1",
            ["not json"],
        )
        .unwrap();
        assert_eq!(
            find_historic_id_in_test_root(temp.path(), &brain, "/repo/source", anchor, true),
            None,
            "unparsable summary JSON must not fall back to tool Cwd"
        );
        assert_eq!(
            find_fresh_id_in_test_root(temp.path(), &brain, "/repo/source", anchor),
            None
        );
        conn.execute(
            "UPDATE conversation_summaries SET workspace_uris = ?1",
            [r#"["https://example.com"]"#],
        )
        .unwrap();
        assert_eq!(
            find_historic_id_in_test_root(temp.path(), &brain, "/repo/source", anchor, true),
            Some(UUID_A.into()),
            "an unsupported-only root list falls back to transcript Cwd"
        );

        // Multiple file roots are ambiguous, so the transcript Cwd remains
        // the only anchor. An unrelated null row falls back only for itself.
        write_conv(&brain, UUID_B, &tool_step(start, "/other/root"));
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, NULL)",
            [UUID_B],
        )
        .unwrap();
        conn.execute(
            "UPDATE conversation_summaries SET workspace_uris = ?1 WHERE conversation_id = ?2",
            rusqlite::params![
                serde_json::json!([
                    reqwest::Url::from_directory_path(&reviewer)
                        .unwrap()
                        .as_str(),
                    "file:///repo/source"
                ])
                .to_string(),
                UUID_A,
            ],
        )
        .unwrap();
        assert_eq!(
            find_historic_id_in_test_root(
                temp.path(),
                &brain,
                &reviewer.to_string_lossy(),
                anchor,
                true
            ),
            None,
            "ambiguous file roots must not claim the reviewer worktree"
        );
        assert_eq!(
            find_historic_id_in_test_root(temp.path(), &brain, "/repo/source", anchor, true),
            Some(UUID_A.into())
        );
        assert_eq!(
            find_fresh_id_in_test_root(temp.path(), &brain, "/other/root", anchor),
            Some(UUID_B.into()),
            "a null workspace row falls back to that transcript's Cwd"
        );
        conn.execute(
            "UPDATE conversation_summaries SET workspace_uris = ?1 WHERE conversation_id = ?2",
            rusqlite::params!["not json", UUID_B],
        )
        .unwrap();
        assert_eq!(
            find_fresh_id_in_test_root(temp.path(), &brain, "/repo/source", anchor),
            Some(UUID_A.into()),
            "malformed metadata for another conversation does not veto this candidate"
        );

        conn.execute("UPDATE conversation_summaries SET workspace_uris = ''", [])
            .unwrap();
        assert_eq!(
            find_historic_id_in_test_root(temp.path(), &brain, "/repo/source", anchor, true),
            Some(UUID_A.into()),
            "blank workspace metadata uses the transcript Cwd anchor"
        );
        conn.execute("DELETE FROM conversation_summaries", []).unwrap();
        assert_eq!(
            find_historic_id_in_test_root(temp.path(), &brain, "/repo/source", anchor, true),
            Some(UUID_A.into())
        );
    }

    #[test]
    fn inaccessible_summary_path_is_reported_to_the_scan() {
        let temp = tempfile::tempdir().unwrap();
        let not_a_directory = temp.path().join("not-a-directory");
        fs::write(&not_a_directory, "file").unwrap();
        let summary_db_path = not_a_directory.join("conversation_summaries.db");
        assert!(read_workspace_summaries(&summary_db_path, &[UUID_A.into()]).is_err());
    }

    #[test]
    fn summary_schema_failure_falls_back_to_transcript_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let brain = temp.path().join("brain");
        let now = chrono::Utc::now();
        let created = (now - chrono::Duration::seconds(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let not_before = (now - chrono::Duration::seconds(30)).timestamp_millis();
        write_conv(
            &brain,
            UUID_A,
            &format!("{}\n{}", user_step(&created, "legacy fallback"), tool_step(&created, "/repo/source")),
        );
        let conn = rusqlite::Connection::open(test_summaries_db_path(temp.path())).unwrap();
        conn.execute_batch("CREATE TABLE different_schema (id TEXT);")
            .unwrap();
        drop(conn);

        assert_eq!(
            find_fresh_id_in_test_root(temp.path(), &brain, "/repo/source", not_before),
            Some(UUID_A.into()),
            "an unreadable external summary store cannot veto proven transcript Cwd"
        );
    }

    #[test]
    fn summary_lock_uses_short_timeout_then_falls_back_to_transcript_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let brain = temp.path().join("brain");
        let now = chrono::Utc::now();
        let created = (now - chrono::Duration::seconds(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let not_before = (now - chrono::Duration::seconds(30)).timestamp_millis();
        write_conv(
            &brain,
            UUID_A,
            &format!("{}\n{}", user_step(&created, "lock fallback"), tool_step(&created, "/repo/source")),
        );
        let summary_db_path = test_summaries_db_path(temp.path());
        let writer = rusqlite::Connection::open(&summary_db_path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, workspace_uris TEXT); BEGIN EXCLUSIVE;",
            )
            .unwrap();

        let started = Instant::now();
        let captured =
            find_fresh_id_in_test_root(temp.path(), &brain, "/repo/source", not_before);
        let elapsed = started.elapsed();
        writer.execute_batch("ROLLBACK").unwrap();

        assert_eq!(captured, Some(UUID_A.into()));
        assert!(
            elapsed < Duration::from_secs(2),
            "summary DB lock stalled capture for {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_summary_symlink_falls_back_to_transcript_cwd() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let brain = temp.path().join("brain");
        let now = chrono::Utc::now();
        let created = (now - chrono::Duration::seconds(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let not_before = (now - chrono::Duration::seconds(30)).timestamp_millis();
        write_conv(
            &brain,
            UUID_A,
            &format!("{}\n{}", user_step(&created, "symlink fallback"), tool_step(&created, "/repo/source")),
        );
        symlink(
            temp.path().join("missing-target.db"),
            temp.path().join("conversation_summaries.db"),
        )
        .unwrap();

        assert_eq!(
            find_fresh_id_in_test_root(temp.path(), &brain, "/repo/source", not_before),
            Some(UUID_A.into())
        );
    }

    #[test]
    fn workspace_uris_reject_invalid_percent_escapes() {
        for uri in [
            "file:///repo/%",
            "file:///repo/%2",
            "file:///repo/%GG",
            "file:///repo/%FF",
            "file:///repo/%00",
            "https:///repo",
            "file:///repo?query",
            "file:///repo#fragment",
        ] {
            assert!(
                workspace_uri_path(uri).is_err(),
                "accepted invalid workspace URI: {uri}"
            );
        }
    }

    #[test]
    fn workspace_uri_paths_reject_ambiguous_roots_and_empty_sets() {
        assert!(
            workspace_uri_paths(r#"["file:///repo/primary", "file:///repo/shared"]"#)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            workspace_uri_paths(r#"["file:///repo/primary", "file:///repo/primary"]"#)
                .unwrap(),
            vec!["/repo/primary"]
        );
        assert_eq!(
            workspace_uri_paths(
                r#"["file:///repo/primary", "vscode-remote://ssh-remote+host/repo"]"#
            )
            .unwrap(),
            vec!["/repo/primary"]
        );
        assert!(workspace_uri_paths("[]").unwrap().is_empty());
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
        assert_eq!(select_id_for_directory(&candidates, "/tmp/wt"), Some(UUID_A));
    }

    #[test]
    fn anchored_match_wins_over_unknown_candidate() {
        let candidates = vec![
            cand(UUID_A, 300, None),
            cand(UUID_B, 200, Some("/tmp/wt")),
        ];
        assert_eq!(select_id_for_directory(&candidates, "/tmp/wt"), Some(UUID_B));
    }

    #[test]
    fn anchored_foreign_candidate_is_excluded_leaving_single_viable() {
        // Proven to belong elsewhere; the remaining unknown candidate binds.
        let candidates = vec![
            cand(UUID_A, 300, Some("/tmp/other")),
            cand(UUID_B, 200, None),
        ];
        assert_eq!(select_id_for_directory(&candidates, "/tmp/wt"), Some(UUID_B));
    }

    #[test]
    fn two_viable_fresh_candidates_bind_nothing() {
        // Sibling spawn or standalone `agy` run: never cross-wire.
        let candidates = vec![
            cand(UUID_A, 200, None),
            cand(UUID_B, 250, None),
        ];
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
            select_id_for_directory(
                &candidates,
                "f:/src/buildmesh/.claude/worktrees/agy-test"
            ),
            Some(UUID_A)
        );
    }

    #[test]
    fn multi_root_workspace_falls_back_to_transcript_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let brain = temp.path().join("brain");
        let timestamp = "2026-09-29T00:00:00Z";
        write_conv(&brain, UUID_A, &tool_step(timestamp, "/repo/cwd"));
        let transcript = crate::services::transcript_reader::adapters::agy::agy_locator_in(
            &brain, UUID_A,
        )
        .unwrap();
        let summaries = HashMap::from([(
            UUID_A.to_string(),
            WorkspaceSummary::Encoded(
                serde_json::json!(["file:///repo/primary", "file:///repo/shared"]).to_string(),
            ),
        )]);
        let candidate = read_conversation_candidate(
            UUID_A,
            &transcript,
            0,
            Some(&summaries),
        )
        .unwrap();
        assert_eq!(candidate.workspaces, vec!["/repo/cwd"]);

        assert_eq!(
            select_id_for_directory(std::slice::from_ref(&candidate), "/repo/primary"),
            None
        );
        assert_eq!(
            select_id_for_directory(std::slice::from_ref(&candidate), "/repo/shared"),
            None
        );
        assert_eq!(
            select_id_for_directory(std::slice::from_ref(&candidate), "/repo/cwd"),
            Some(UUID_A)
        );

        let unverified = cand_with_workspaces(
            UUID_A,
            candidate.created_ms,
            vec!["/repo/primary", "/repo/shared"],
        );
        assert_eq!(
            select_id_for_directory(&[unverified], "/repo/primary"),
            None,
            "the selection seam must reject unverified multi-root candidates"
        );
    }

    #[test]
    fn finds_single_fresh_session_with_real_shape() {
        let temp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let fresh = (now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
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
            find_fresh_id_in_test_root(temp.path(), temp.path(), "/tmp/wt", not_before),
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
            find_historic_id_in_test_root(temp.path(), temp.path(), "F:/repo", CREATED, true),
            Some(UUID_A.to_string())
        );

        fs::write(
            transcript,
            serde_json::json!({"created_at": TIMESTAMP}).to_string(),
        )
        .unwrap();
        assert_eq!(
            find_historic_id_in_test_root(temp.path(), temp.path(), "F:/repo", CREATED, true),
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
            find_fresh_id_in_test_root(temp.path(), temp.path(), "/tmp/wt", not_before),
            None
        );
    }

    #[test]
    fn untouched_transcript_is_skipped_without_parsing() {
        // mtime gate: transcript last written before the window never binds,
        // however fresh its content claims to be.
        let temp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let fresh = (now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        write_conv(temp.path(), UUID_A, &user_step(&fresh, "hi"));
        let future = (now + chrono::Duration::seconds(3600)).timestamp_millis();
        assert_eq!(
            find_fresh_id_in_test_root(temp.path(), temp.path(), "/tmp/wt", future),
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
            find_fresh_id_in_test_root(temp.path(), temp.path(), "/tmp/wt", not_before),
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
            find_fresh_id_in_test_root(temp.path(), temp.path(), "/tmp/wt", now - 60_000),
            None
        );
    }

    #[test]
    fn transcript_full_fallback_resolves_when_short_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let fresh = (now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let not_before = (now - chrono::Duration::seconds(30)).timestamp_millis();
        let conv = temp.path().join(UUID_A);
        let logs = conv.join(".system_generated").join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("transcript_full.jsonl"), user_step(&fresh, "hi")).unwrap();
        assert_eq!(
            find_fresh_id_in_test_root(temp.path(), temp.path(), "/tmp/wt", not_before),
            Some(UUID_A.to_string())
        );
    }

    #[test]
    fn embedded_quotes_in_cwd_anchor_are_stripped() {
        // Real `Cwd` values arrive double-wrapped: `"\"F:/repo\""`.
        let val = serde_json::json!({
            "tool_calls": [{"name": "run_command", "args": {"Cwd": "\"F:/src/repo\""}}],
        });
        assert_eq!(
            extract_cwd_anchor(&val).as_deref(),
            Some("F:/src/repo")
        );
    }

    #[test]
    fn wsl_brain_home_does_not_depend_on_workspace_location() {
        let _env_guard = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // A Windows checkout and another user's checkout must resolve the
        // same CLI account. This fails the old /home/<workspace-owner> guess,
        // whether the runtime-home probe succeeds or is unavailable.
        assert_eq!(
            crate::env::agy_dir_for_env(crate::models::EnvType::Wsl, "/home/other-user/src/repo"),
            crate::env::agy_dir_for_env(crate::models::EnvType::Wsl, "/mnt/c/src/repo"),
        );
    }

}
