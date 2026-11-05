//! Explicit failed turns, never inferred from assistant prose or terminal silence.
use super::{locate_transcript, TranscriptFormat};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureKind {
    Transient,
    Quota,
    Policy,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub turn: String,
    pub at_ms: i64,
    pub code: String,
    pub kind: FailureKind,
}

impl Failure {
    pub fn delay_ms(&self, attempt: usize) -> Option<i64> {
        match self.kind {
            FailureKind::Transient => [30_000, 120_000, 300_000].get(attempt).copied(),
            FailureKind::Quota => [900_000, 1_800_000, 3_600_000].get(attempt).copied(),
            FailureKind::Policy | FailureKind::Other => None,
        }
    }
    pub fn description(&self) -> &'static str {
        match self.kind {
            FailureKind::Transient => "The harness stopped after a temporary capacity or connection failure.",
            FailureKind::Quota => "The provider's token-plan limit was reached.",
            FailureKind::Policy => "The provider rejected this turn under its safety policy (Daybreak access may be required). Review the provider message; automatic retry is disabled.",
            FailureKind::Other => "The harness reported a failed turn that needs inspection; automatic retry is disabled.",
        }
    }
}

/// Outer None is unavailable evidence; inner None proves no current failure.
pub(crate) fn read(node: &crate::models::AgentNode) -> Option<Option<Failure>> {
    let format = crate::coordinator::enrichment::native_completion_format(node)?;
    if !matches!(format, TranscriptFormat::Codex | TranscriptFormat::Mcode) {
        return None;
    }
    let session = node.cli_session_id.as_deref()?;
    let path = locate_transcript(
        format,
        session,
        &crate::coordinator::enrichment::transcript_dir(node),
    )?;
    match format {
        TranscriptFormat::Codex => codex_file(&path),
        TranscriptFormat::Mcode => mcode_file(&path, session),
        _ => None,
    }
}

fn codex_file(path: &Path) -> Option<Option<Failure>> {
    let mut file = File::open(path).ok()?;
    let before = file.metadata().ok()?;
    let start = before.len().saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut input = BufReader::new(file.take(before.len() - start));
    if start > 0 {
        input.read_until(b'\n', &mut Vec::new()).ok()?;
    }
    let mut text = String::new();
    input.read_to_string(&mut text).ok()?;
    let after = std::fs::metadata(path).ok()?;
    if before.len() != after.len()
        || before.modified().ok()? != after.modified().ok()?
        || !text.ends_with('\n')
    {
        return None;
    }
    // A malformed trailing record could conceal a newer turn.
    serde_json::from_str::<serde_json::Value>(text.lines().rfind(|l| !l.trim().is_empty())?)
        .ok()?;
    Some(codex(&text))
}

fn codex(text: &str) -> Option<Failure> {
    let mut failure = None;
    let mut started = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            failure = None;
            started = None;
            continue;
        };
        let p = &v["payload"];
        match (v["type"].as_str(), p["type"].as_str()) {
            (Some("event_msg"), Some("task_started")) => {
                started = p["turn_id"].as_str().map(str::to_owned);
                failure = None;
            }
            (Some("event_msg"), Some("task_complete")) => {
                failure = (|| {
                    let turn = p["turn_id"].as_str().filter(|s| !s.is_empty())?;
                    if started.as_deref().is_some_and(|s| s != turn) {
                        return None;
                    }
                    let error = p.get("error").filter(|e| !e.is_null())?;
                    let code = error["codex_error_info"].as_str().unwrap_or("unknown");
                    let kind = match code {
                        "server_overloaded" => FailureKind::Transient,
                        "cyber_policy" => FailureKind::Policy,
                        _ => FailureKind::Other,
                    };
                    Some(Failure {
                        turn: turn.into(),
                        at_ms: chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str()?)
                            .ok()?
                            .timestamp_millis(),
                        code: code.into(),
                        kind,
                    })
                })();
            }
            (Some("event_msg"), Some("token_count")) | (Some("token_usage_record"), _) => {}
            _ => failure = None,
        }
    }
    failure
}

fn mcode_file(transcript: &Path, session: &str) -> Option<Option<Failure>> {
    let manifest: serde_json::Value =
        serde_json::from_reader(File::open(transcript.with_file_name("manifest.json")).ok()?)
            .ok()?;
    if manifest["sessionId"].as_str()? != session {
        return None;
    }
    let root = transcript
        .ancestors()
        .find(|p| {
            p.file_name().is_some_and(|n| n == "sessions")
                && p.parent()
                    .is_some_and(|p| p.file_name().is_some_and(|n| n == "v2"))
        })?
        .parent()?;
    let conn = Connection::open_with_flags(
        root.join("sqlite/runtime-state.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(100))
        .ok()?;
    mcode(&conn, session)
}

// These are the native schema's existing indexes; never modify the provider DB.
const MCODE_FAILURE_SQL: &str = "SELECT a.turn_id,a.updated_at_ms,s.error_code,s.error_message,i.accepted_sequence
    FROM local_runtime_sessions s JOIN local_runtime_session_agent_state a USING(session_id)
    JOIN local_runtime_turn_ingress i ON i.turn_id=a.turn_id AND i.session_id=s.session_id
    WHERE s.session_id=?1 AND s.status='error' AND a.terminal_outcome='failed'
      AND a.updated_at_ms>=s.updated_at_ms AND i.status='failed' AND i.accepted_sequence IS NOT NULL";
const MCODE_ACTIVE_SQL: &str = "SELECT 1 FROM local_runtime_turn_ingress
    INDEXED BY idx_local_runtime_turn_ingress_session_accepted_sequence
    WHERE session_id=?1 AND status='accepted' LIMIT 1";
const MCODE_NEWER_SQL: &str = "SELECT session_id FROM local_runtime_turn_ingress
    INDEXED BY idx_local_runtime_turn_ingress_sequence
    WHERE accepted_sequence IS NOT NULL AND accepted_sequence>?1
    ORDER BY accepted_sequence LIMIT 1025";
const NEWER_TURN_LIMIT: usize = 1024;

fn mcode(conn: &Connection, session: &str) -> Option<Option<Failure>> {
    // A single read transaction binds projections and ingress. The sequence
    // watermark avoids scanning all historical ingress for every session.
    let tx = conn.unchecked_transaction().ok()?;
    let candidate = tx
        .query_row(MCODE_FAILURE_SQL, [session], |row| {
            let code: Option<i64> = row.get(2)?;
            let message: Option<String> = row.get(3)?;
            let kind = match (code, message.as_deref()) {
                (
                    Some(50113),
                    Some(
                        "Connection error."
                        | "Internal server error. Please retry your request. (500)",
                    ),
                ) => FailureKind::Transient,
                (Some(42212), Some(m))
                    if m.starts_with("The Token Plan usage limit has been reached.") =>
                {
                    FailureKind::Quota
                }
                _ => FailureKind::Other,
            };
            Ok((
                Failure {
                    turn: row.get(0)?,
                    at_ms: row.get(1)?,
                    code: code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                    kind,
                },
                row.get::<_, i64>(4)?,
            ))
        })
        .optional()
        .ok()?;
    let Some((failure, sequence)) = candidate else {
        return Some(None);
    };
    if tx
        .query_row(MCODE_ACTIVE_SQL, [session], |_| Ok(()))
        .optional()
        .ok()?
        .is_some()
    {
        return Some(None);
    }
    let mut stmt = tx.prepare(MCODE_NEWER_SQL).ok()?;
    let newer = stmt
        .query_map([sequence], |row| row.get::<_, String>(0))
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if newer.iter().any(|id| id == session) {
        return Some(None);
    }
    // An old failure beyond the bounded evidence window is unavailable, not
    // permission to guess that no newer work exists. Missing indexes also fail closed.
    if newer.len() > NEWER_TURN_LIMIT {
        return None;
    }
    Some((!failure.turn.is_empty()).then_some(failure))
}

#[cfg(test)]
#[path = "interruption_tests.rs"]
mod tests;
