//! MiniMax Code session-ID capture from the manifest scan.
//!
//! `mcode` self-assigns its session ids and writes them to
//! `<dataDir>/v2/sessions/**/manifest.json` (`sessionId`, `createdAtMs`)
//! beside the canonical `messages.jsonl` — the same scan
//! `TranscriptFormat::Mcode` uses to locate a transcript for a *known* id
//! (issue #1798). No PTY banner shape is verified for mcode, so PTY capture
//! stays off (`captures_session_id_from_pty = false`) and both capture paths
//! below reuse the manifest scan instead:
//!
//! - a bounded post-spawn poller (`after_fresh_spawn`) binds the fresh id
//!   through the shared [`crate::services::session_recovery`] service, so
//!   circuits can attach to already-silent sources;
//! - historic startup recovery (`recover_suspended_session_id`) rebinds an
//!   archived node to its transcript after a restart.
//!
//! The manifest carries no workspace anchor Buildmesh has verified (its
//! `paths` field shape is unconfirmed against a live CLI), so matching is
//! time-window only: a candidate binds only when it is the single session
//! created in the spawn window. Two fresh manifests — a sibling spawn, a
//! standalone `mcode` run — bind nothing rather than risk cross-wiring
//! sessions. A scan that finds nothing returns `None` and changes nothing:
//! the node keeps its previous (empty) identity exactly as before.
//!
//! The scan runs on the poller's retry ticks, so traversal is pruned by the
//! same cutoff the selection applies: dated directories older than the
//! window are never descended into, and manifests whose mtime already
//! proves they predate the window are never opened (creation never
//! postdates modification). Unparseable directory names are never pruned.
//!
//! Pure helpers stay fixture-driven so the rules are unit-tested without a
//! live `mcode` binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Datelike;

use crate::models::EnvType;

const RETRY_DELAYS_MS: &[u64] = &[400, 800, 1_600, 2_500, 4_000, 6_000];

/// One MiniMax session candidate found under `v2/sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub created_ms: i64,
}

/// Read the `(sessionId, createdAtMs)` pair from one `manifest.json`.
/// Returns `None` when the file is not a session manifest (bad JSON,
/// missing/empty id, unparseable timestamp), when its mtime already proves
/// it predates `created_not_before_ms`, or when the sibling `messages.jsonl`
/// is absent — a manifest without history cannot back a transcript and is
/// skipped, never guessed at.
fn read_manifest_candidate(manifest_path: &Path, created_not_before_ms: i64) -> Option<Candidate> {
    if manifest_mtime_ms(manifest_path).is_some_and(|mtime| mtime < created_not_before_ms) {
        return None;
    }
    let text = fs::read_to_string(manifest_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let manifest = value.as_object()?;
    let id = manifest
        .get("sessionId")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())?
        .to_string();
    let created_ms = match manifest.get("createdAtMs") {
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_u64().and_then(|u| i64::try_from(u).ok()))?,
        Some(serde_json::Value::String(s)) => s.parse::<i64>().ok()?,
        _ => return None,
    };
    if !manifest_path
        .parent()
        .is_some_and(|dir| dir.join("messages.jsonl").is_file())
    {
        return None;
    }
    Some(Candidate { id, created_ms })
}

fn manifest_mtime_ms(path: &Path) -> Option<i64> {
    let modified = fs::symlink_metadata(path).ok()?.modified().ok()?;
    let millis = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    i64::try_from(millis).ok()
}

/// True when a dated directory prefix is entirely older than the cutoff, so
/// the walk can skip the whole subtree. `level` is the depth below the
/// sessions root (`1 = YYYY`, `2 = MM`, `3 = DD`, `4 = session dir`);
/// anything unparseable (legacy layouts, unexpected names) is never pruned.
fn dir_prefix_before_cutoff(dir: &Path, level: usize, cutoff_ms: i64) -> bool {
    let cutoff = chrono::DateTime::from_timestamp_millis(cutoff_ms);
    let Some(cutoff) = cutoff else {
        return false;
    };
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    match level {
        1 => name
            .parse::<i32>()
            .is_ok_and(|year| year < cutoff.year()),
        2 => {
            let (Some(parent), Ok(month)) = (dir.parent(), name.parse::<u32>()) else {
                return false;
            };
            let year = parent
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<i32>().ok());
            year.is_some_and(|year| (year, month) < (cutoff.year(), cutoff.month()))
        }
        3 => {
            // Walk up two levels to rebuild YYYY/MM/DD.
            let month_dir = dir.parent();
            let year_dir = month_dir.and_then(Path::parent);
            let (Some(month_dir), Some(year_dir)) = (month_dir, year_dir) else {
                return false;
            };
            let month = month_dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<u32>().ok());
            let year = year_dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<i32>().ok());
            let day = name.parse::<u32>().ok();
            match (year, month, day) {
                (Some(year), Some(month), Some(day)) => {
                    chrono::NaiveDate::from_ymd_opt(year, month, day)
                        .is_some_and(|date| date < cutoff.date_naive())
                }
                _ => false,
            }
        }
        4 => {
            // Session dirs open with `<HH-MM-SS-mmm>-session_…`; combine the
            // time-of-day prefix with the parent date and prune whole
            // sessions older than the cutoff.
            let date = dir
                .parent()
                .and_then(|day_dir| {
                    day_dir.parent().and_then(|month_dir| {
                        month_dir.parent().map(|year_dir| (year_dir, month_dir, day_dir))
                    })
                })
                .and_then(|(year_dir, month_dir, day_dir)| {
                    let year = year_dir
                        .file_name()
                        .and_then(|name| name.to_str())?
                        .parse::<i32>()
                        .ok()?;
                    let month = month_dir
                        .file_name()
                        .and_then(|name| name.to_str())?
                        .parse::<u32>()
                        .ok()?;
                    let day = day_dir
                        .file_name()
                        .and_then(|name| name.to_str())?
                        .parse::<u32>()
                        .ok()?;
                    chrono::NaiveDate::from_ymd_opt(year, month, day)
                });
            let time = name
                .get(..12)
                .and_then(|prefix| {
                    let mut fields = prefix.split('-');
                    let (hour, min, sec, milli) = (
                        fields.next()?.parse::<u32>().ok()?,
                        fields.next()?.parse::<u32>().ok()?,
                        fields.next()?.parse::<u32>().ok()?,
                        fields.next()?.parse::<u32>().ok()?,
                    );
                    chrono::NaiveTime::from_hms_milli_opt(hour, min, sec, milli)
                });
            match (date, time) {
                (Some(date), Some(time)) => {
                    date.and_time(time).and_utc().timestamp_millis() < cutoff_ms
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// Collect session candidates under `sessions_root` (`<dataDir>/v2/sessions`)
/// created at or after `created_not_before_ms`. Same walk rules as the
/// transcript locator: a directory carrying its own `manifest.json` IS a
/// session directory and is never descended into (`snapshots/`, `reports/`
/// are artifacts, never sessions); depth 5 covers
/// `<root>/<YYYY>/<MM>/<DD>/<session-dir>/` plus one spare level. Dated
/// subtrees older than the cutoff are pruned before descent, so a poll tick
/// over a large archive only touches the fresh fringe.
pub(crate) fn collect_candidates(
    sessions_root: &Path,
    created_not_before_ms: i64,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    if !sessions_root.is_dir() {
        return out;
    }
    let mut stack = vec![(sessions_root.to_path_buf(), 0usize)];
    while let Some((dir, level)) = stack.pop() {
        if level > 0 && dir_prefix_before_cutoff(&dir, level, created_not_before_ms) {
            continue;
        }
        let manifest = dir.join("manifest.json");
        if manifest.is_file() {
            if let Some(candidate) = read_manifest_candidate(&manifest, created_not_before_ms) {
                if candidate.created_ms >= created_not_before_ms {
                    out.push(candidate);
                }
            }
            continue;
        }
        if level >= 5 {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                fs::symlink_metadata(path)
                    .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
            })
            .collect();
        subdirs.sort();
        for sub in subdirs {
            stack.push((sub, level + 1));
        }
    }
    out
}

/// Resolve the `v2/sessions` root for the environment that ran (or will run)
/// the CLI. Reuses the transcript adapter's data-dir resolution so the
/// capture poller and the reader can never disagree on where sessions live.
fn sessions_root_for_spawn(spawn_path: &str) -> Option<PathBuf> {
    let data_dir =
        crate::services::transcript_reader::adapters::mcode::minimax_data_dir_for_spawn(
            spawn_path,
        )?;
    Some(data_dir.join("v2").join("sessions"))
}

/// Historic startup recovery entry point used by the mcode adapter, and the
/// production boundary behind the fresh-spawn poller (which reaches it via
/// the shared live recovery with the spawn generation as anchor). Time
/// window and ambiguity policy stay in the shared recovery service; the
/// manifest scan stays here.
pub(crate) fn find_historic_id_for_directory(
    _env_type: EnvType,
    spawn_path: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let sessions_root = sessions_root_for_spawn(spawn_path)?;
    find_historic_id_for_sessions_root_in(&sessions_root, anchor_ms, recorded_start)
}

pub(crate) fn find_historic_id_for_sessions_root_in(
    sessions_root: &Path,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let cutoff = anchor_ms.saturating_sub(crate::services::session_recovery::CLOCK_SKEW_MS);
    let candidates = collect_candidates(sessions_root, cutoff)
        .into_iter()
        .filter(|candidate| !candidate.id.is_empty())
        .map(|candidate| (candidate.id, candidate.created_ms));
    crate::services::session_recovery::select_recovery_identity(
        candidates,
        anchor_ms,
        recorded_start,
    )
}

enum CaptureAttempt {
    AlreadyStored,
    NotFound,
    Regenerated,
    Stored(String),
}

/// Background poller: read the manifest scan until the fresh session created
/// in this node's spawn window appears, then bind it via the shared live
/// recovery (which owns the conditional write, so this delayed fallback can
/// never overwrite an identity captured earlier). Every tick's DB predicate,
/// disk scan, and conditional write run in one blocking task — never as
/// bare synchronous queries on the async worker (agy poller pattern).
pub fn start_capture_poller(node_id: i64, spawn_directory: String, _env_type: EnvType) {
    let generation = crate::db::session_started_at_ms(node_id).ok().flatten();
    tauri::async_runtime::spawn(async move {
        let Some(generation) = generation else { return; };
        // The manifest can be minted at launch but only flushed with the
        // first model commit. Slow first responses outlive the fast polls.
        for (attempt, delay) in RETRY_DELAYS_MS
            .iter()
            .copied()
            .chain(std::iter::repeat_n(10_000, 60))
            .enumerate()
        {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
                tracing::debug!("mcode session capture: node {node_id} gone, stop");
                return;
            }
            let captured = crate::blocking::run_blocking("mcode_capture", move || {
                if crate::db::session_started_at_ms(node_id).ok().flatten() != Some(generation) {
                    return Ok(CaptureAttempt::Regenerated);
                }
                // Lean predicate: avoids hydrating the full 30+-column row
                // on every tick.
                if crate::db::cli_session_id_present(node_id).map_err(|error| error.to_string())? {
                    return Ok(CaptureAttempt::AlreadyStored);
                }
                match crate::services::session_recovery::recover_live_node(node_id)
                    .map_err(|error| error.to_string())?
                {
                    Some(id) => Ok(CaptureAttempt::Stored(id)),
                    None => Ok(CaptureAttempt::NotFound),
                }
            })
            .await;
            let attempt_result = match captured {
                Ok(attempt_result) => attempt_result,
                Err(error) => {
                    tracing::warn!(
                        "mcode session capture: blocking task failed for node {node_id}: {error}"
                    );
                    return;
                }
            };
            match attempt_result {
                CaptureAttempt::AlreadyStored | CaptureAttempt::Regenerated => return,
                CaptureAttempt::NotFound => continue,
                CaptureAttempt::Stored(id) => {
                    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
                        return;
                    }
                    tracing::info!(
                        "mcode session capture: stored {id} for node {node_id} (attempt {})",
                        attempt + 1
                    );
                    return;
                }
            }
        }
        tracing::warn!("mcode session capture: gave up for node {node_id} in {spawn_directory}");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_ID: &str = "mcode-session-001";

    /// 2026-09-19T10:00:00Z — matches the dated fixture layout below, so
    /// day-level pruning can be asserted against real calendar dates.
    fn created_ms() -> i64 {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 19)
            .unwrap()
            .and_hms_milli_opt(10, 0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    fn write_session(root: &Path, dir_name: &str, session_id: &str, created_ms: i64) -> PathBuf {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::json!({
                "schemaVersion": 1,
                "sessionId": session_id,
                "createdAtMs": created_ms,
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("messages.jsonl"), "{\"message_id\":\"msg-1\"}\n").unwrap();
        dir
    }

    #[test]
    fn fresh_manifest_binds_within_spawn_window() {
        let root = tempfile::tempdir().unwrap();
        write_session(
            root.path(),
            "2026/09/19/10-00-00-000-session_abc",
            SESSION_ID,
            created_ms(),
        );
        // A stale manifest from an earlier conversation must not shadow it.
        write_session(
            root.path(),
            "2026/09/18/09-00-00-000-session_old",
            "mcode-session-stale",
            created_ms() - 90_000,
        );

        // The production boundary: spawn generation as anchor, recorded start.
        assert_eq!(
            find_historic_id_for_sessions_root_in(root.path(), created_ms(), true),
            Some(SESSION_ID.to_string())
        );
    }

    #[test]
    fn nothing_binds_when_manifest_not_yet_flushed() {
        let root = tempfile::tempdir().unwrap();
        // Only a stale conversation exists: an empty scan binds nothing, so
        // the node keeps its previous (empty) identity exactly as before.
        write_session(
            root.path(),
            "2026/09/18/09-00-00-000-session_old",
            "mcode-session-stale",
            created_ms() - 90_000,
        );
        assert_eq!(
            find_historic_id_for_sessions_root_in(root.path(), created_ms(), true),
            None
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            find_historic_id_for_sessions_root_in(empty.path(), created_ms(), true),
            None
        );
    }

    #[test]
    fn ambiguous_fresh_manifests_bind_nothing() {
        let root = tempfile::tempdir().unwrap();
        write_session(
            root.path(),
            "2026/09/19/10-00-00-000-session_a",
            "mcode-session-a",
            created_ms(),
        );
        write_session(
            root.path(),
            "2026/09/19/10-00-01-000-session_b",
            "mcode-session-b",
            created_ms() + 1_000,
        );

        assert_eq!(
            find_historic_id_for_sessions_root_in(root.path(), created_ms(), true),
            None
        );
    }

    #[test]
    fn recovery_binds_archived_node_to_its_transcript() {
        let root = tempfile::tempdir().unwrap();
        let dir = write_session(
            root.path(),
            "2026/09/19/10-00-00-000-session_abc",
            SESSION_ID,
            created_ms(),
        );
        let messages = dir.join("messages.jsonl");

        // The archived node's launch generation rebinds through the same scan.
        let recovered = find_historic_id_for_sessions_root_in(root.path(), created_ms(), true);
        assert_eq!(recovered.as_deref(), Some(SESSION_ID));
        // And the recovered id locates the transcript the reader parses.
        assert_eq!(
            crate::services::transcript_reader::adapters::mcode::find_mcode_transcript_in(
                root.path(),
                recovered.as_deref().unwrap()
            ),
            Some(messages)
        );
    }

    #[test]
    fn manifests_without_history_or_shape_are_skipped() {
        let root = tempfile::tempdir().unwrap();
        // Manifest without a sibling messages.jsonl: skipped.
        let bare = root.path().join("2026/09/19/10-00-00-000-session_bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(
            bare.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "mcode-bare", "createdAtMs": created_ms()}).to_string(),
        )
        .unwrap();
        // Malformed manifest: skipped.
        let broken = root.path().join("2026/09/19/10-00-01-000-session_broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("manifest.json"), "not json").unwrap();
        std::fs::write(broken.join("messages.jsonl"), "noise").unwrap();
        // Manifest without a timestamp proves nothing about freshness: skipped.
        let undated = root.path().join("2026/09/19/10-00-02-000-session_undated");
        std::fs::create_dir_all(&undated).unwrap();
        std::fs::write(
            undated.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "mcode-undated"}).to_string(),
        )
        .unwrap();
        std::fs::write(undated.join("messages.jsonl"), "noise").unwrap();

        let cutoff = created_ms().saturating_sub(crate::services::session_recovery::CLOCK_SKEW_MS);
        assert!(collect_candidates(root.path(), cutoff).is_empty());
        assert_eq!(
            find_historic_id_for_sessions_root_in(root.path(), created_ms(), true),
            None
        );
    }

    #[test]
    fn dated_subtrees_older_than_cutoff_are_pruned() {
        let root = tempfile::tempdir().unwrap();
        write_session(
            root.path(),
            "2026/09/19/10-00-00-000-session_fresh",
            SESSION_ID,
            created_ms(),
        );
        // A whole history of older days collapses to nothing under a fresh
        // cutoff: the walk prunes them without opening a single manifest.
        for day in ["15", "16", "17", "18"] {
            write_session(
                root.path(),
                &format!("2026/09/{day}/10-00-00-000-session_old"),
                "mcode-session-stale",
                created_ms() - 90_000,
            );
        }
        let cutoff = created_ms().saturating_sub(crate::services::session_recovery::CLOCK_SKEW_MS);
        let pruned = collect_candidates(root.path(), cutoff);
        assert_eq!(
            pruned,
            vec![Candidate {
                id: SESSION_ID.to_string(),
                created_ms: created_ms(),
            }]
        );
        // An unparseable (legacy) layout is never pruned: unknown names are
        // still scanned.
        write_session(root.path(), "legacy-session-dir", "mcode-legacy", created_ms());
        let with_legacy = collect_candidates(root.path(), cutoff);
        assert_eq!(with_legacy.len(), 2);
    }
}