use super::{assistant_revision, ReportReadError as E, ReportSnapshot, ReportSource};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct HeadGuard {
    path: PathBuf,
    session_id: String,
    fingerprint: (i64, [u8; 32]),
}

impl HeadGuard {
    pub(super) fn is_current(&self) -> bool {
        read_head(&self.path, &self.session_id)
            .is_ok_and(|(node_id, message)| fingerprint(node_id, &message) == self.fingerprint)
    }
}

fn fingerprint(node_id: i64, message: &str) -> (i64, [u8; 32]) {
    (node_id, Sha256::digest(message.as_bytes()).into())
}

fn read_head(path: &Path, session_id: &str) -> Result<(i64, String), E> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| E::Unreadable)?;
    conn.busy_timeout(Duration::from_millis(100))
        .map_err(|_| E::Unreadable)?;
    conn.query_row(
        "SELECT n.node_id, n.chat_message
         FROM sessions s CROSS JOIN message_nodes n
         WHERE s.id = ?1 AND n.session_id = s.id AND n.node_id = s.main_chain_id",
        [session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => E::NoReport,
        _ => E::Unreadable,
    })
}

pub(crate) fn read(path: &Path, session_id: &str) -> Result<ReportSnapshot, E> {
    let (node_id, raw) = read_head(path, session_id)?;
    let message: serde_json::Value = serde_json::from_str(&raw).map_err(|_| E::MalformedRecord)?;
    let role = message["role"].as_str().ok_or(E::MalformedRecord)?;
    let metadata = &message["metadata"];
    if role != "assistant"
        || metadata["finish_reason"] != "stop"
        || message
            .get("tool_calls")
            .is_some_and(|calls| !calls.as_array().is_some_and(Vec::is_empty))
    {
        return Err(E::WorkInProgress);
    }
    let text = message["content"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or(E::NoReport)?;
    let published_at_ms = metadata["created_at"]
        .as_str()
        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
        .ok_or(E::NoTimestamp)?
        .timestamp_millis();
    let message_id = message["message_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(E::MalformedRecord)?;
    let snapshot = ReportSnapshot {
        text: crate::secret_scrubber::SecretScrubber::scrub(text),
        revision: assistant_revision(message_id, text, text),
        published_at_ms,
        turn_finished: false,
        source: ReportSource::Devin(HeadGuard {
            path: path.into(),
            session_id: session_id.into(),
            fingerprint: fingerprint(node_id, &raw),
        }),
    };
    if snapshot.is_current() {
        Ok(snapshot)
    } else {
        Err(E::ChangedDuringRead)
    }
}

#[cfg(test)]
mod tests;
