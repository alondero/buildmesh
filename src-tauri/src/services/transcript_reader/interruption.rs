//! Explicit failed turns, never inferred from assistant prose or terminal silence.
use super::{locate_transcript, TranscriptFormat};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
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
    read_transcript(format, &path, session, interruption_cache::production())
}

fn read_transcript(
    format: TranscriptFormat,
    path: &Path,
    session: &str,
    cache: &interruption_cache::Cache,
) -> Option<Option<Failure>> {
    match format {
        TranscriptFormat::Codex => read_cached(format, path, session, cache, codex_stamp, || {
            codex_file(path)
        }),
        TranscriptFormat::Mcode => read_cached(format, path, session, cache, mcode_stamp, || {
            mcode_file(path, session)
        }),
        _ => None,
    }
}

fn codex_stamp(path: &Path) -> Option<interruption_cache::Stamp> {
    interruption_cache::Stamp::of_file(path)
}

/// Manifest, the runtime database, and its write-ahead log when one exists.
/// A checkpointed store and a store with an uncheckpointed log are different
/// stamps, so a failure that lands only in the log is not served from cache.
fn mcode_stamp(transcript: &Path) -> Option<interruption_cache::Stamp> {
    let (manifest, database) = mcode_store(transcript)?;
    let mut wal = database.as_os_str().to_owned();
    wal.push("-wal");
    Some(interruption_cache::Stamp::parts(vec![
        Some(interruption_cache::file_part(&manifest)?),
        Some(interruption_cache::file_part(&database)?),
        interruption_cache::file_part(Path::new(&wal)),
    ]))
}

/// Serve a previous parse when the stamp still matches. The file is read only
/// on a miss, and only a stable parse is stored: a change during the read is
/// unavailable and is not cached.
fn read_cached(
    format: TranscriptFormat,
    path: &Path,
    session: &str,
    cache: &interruption_cache::Cache,
    stamp_of: impl Fn(&Path) -> Option<interruption_cache::Stamp>,
    read_file: impl FnOnce() -> Option<Option<Failure>>,
) -> Option<Option<Failure>> {
    let stamp = stamp_of(path)?;
    let key = interruption_cache::Key {
        format,
        path: path.to_path_buf(),
        session_id: session.to_owned(),
    };
    if let Some(parsed) = cache.get(&key, &stamp) {
        return Some(parsed);
    }
    let parsed = read_file()?;
    if stamp_of(path)? != stamp {
        return None;
    }
    cache.put(key, stamp, parsed.clone());
    Some(parsed)
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

fn mcode_store(transcript: &Path) -> Option<(PathBuf, PathBuf)> {
    let manifest = transcript.with_file_name("manifest.json");
    let database = transcript
        .ancestors()
        .find(|candidate| {
            candidate.file_name().is_some_and(|name| name == "sessions")
                && candidate
                    .parent()
                    .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "v2"))
        })?
        .parent()?
        .join("sqlite/runtime-state.sqlite");
    Some((manifest, database))
}

fn mcode_file(transcript: &Path, session: &str) -> Option<Option<Failure>> {
    let (manifest_path, database) = mcode_store(transcript)?;
    let manifest: serde_json::Value =
        serde_json::from_reader(File::open(manifest_path).ok()?).ok()?;
    if manifest["sessionId"].as_str()? != session {
        return None;
    }
    let conn = Connection::open_with_flags(
        database,
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

/// Parsed interruption evidence, keyed by source identity and served only while
/// the source stamp still matches. Unavailable reads are not stored.
mod interruption_cache {
    use super::{Failure, TranscriptFormat};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex};
    use std::time::SystemTime;

    /// Live sessions are the working set. One new session past the cap evicts a
    /// single older entry, so crossing the cap does not re-parse every session.
    pub(super) const CAPACITY: usize = 64;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(super) struct Key {
        pub(super) format: TranscriptFormat,
        pub(super) path: PathBuf,
        pub(super) session_id: String,
    }

    /// Length and modification time of each file the read depends on. A missing
    /// optional sibling (a SQLite `-wal`) is itself part of the stamp.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Stamp {
        parts: Vec<Option<(u64, SystemTime)>>,
    }

    impl Stamp {
        pub(super) fn of_file(path: &Path) -> Option<Self> {
            Some(Self {
                parts: vec![Some(file_part(path)?)],
            })
        }

        pub(super) fn parts(parts: Vec<Option<(u64, SystemTime)>>) -> Self {
            Self { parts }
        }
    }

    pub(super) fn file_part(path: &Path) -> Option<(u64, SystemTime)> {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.len(), meta.modified().ok()?))
    }

    struct Entry {
        stamp: Stamp,
        parsed: Option<Failure>,
    }

    #[derive(Default)]
    struct State {
        entries: HashMap<Key, Entry>,
        misses: u64,
    }

    pub(super) struct Cache {
        state: Mutex<State>,
    }

    impl Default for Cache {
        fn default() -> Self {
            Self {
                state: Mutex::new(State::default()),
            }
        }
    }

    static CACHE: LazyLock<Cache> = LazyLock::new(Cache::default);

    pub(super) fn production() -> &'static Cache {
        &CACHE
    }

    impl Cache {
        /// `None` is a miss. `Some` is the cached parse, including a proven
        /// absence of a failure.
        pub(super) fn get(&self, key: &Key, stamp: &Stamp) -> Option<Option<Failure>> {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mismatch = state
                .entries
                .get(key)
                .is_some_and(|entry| entry.stamp != *stamp);
            if mismatch {
                state.entries.remove(key);
                state.misses += 1;
                return None;
            }
            match state.entries.get(key) {
                Some(entry) => Some(entry.parsed.clone()),
                None => {
                    state.misses += 1;
                    None
                }
            }
        }

        pub(super) fn put(&self, key: Key, stamp: Stamp, parsed: Option<Failure>) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.entries.len() >= CAPACITY && !state.entries.contains_key(&key) {
                if let Some(victim) = state.entries.keys().next().cloned() {
                    state.entries.remove(&victim);
                }
            }
            state.entries.insert(key, Entry { stamp, parsed });
        }

        #[cfg_attr(not(test), expect(dead_code, reason = "Test cache observation"))]
        pub(super) fn misses(&self) -> u64 {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .misses
        }

        #[cfg_attr(not(test), expect(dead_code, reason = "Test cache bounds"))]
        pub(super) fn len(&self) -> usize {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entries
                .len()
        }
    }
}

#[cfg(test)]
#[path = "interruption_tests.rs"]
mod tests;
