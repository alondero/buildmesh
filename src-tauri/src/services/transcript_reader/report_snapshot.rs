//! A stable report read for Circuit interpretation, not proof of task ownership.

use super::readers::opencode::{
    opencode_resolve, parse_opencode_messages, read_opencode_message_rows, OPENCODE_DIGEST_WINDOW,
};
use super::*;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
};

#[cfg(test)]
pub(crate) mod result_read_test_support {
    use std::{cell::RefCell, path::Path};

    type ReadHook = Box<dyn FnOnce(&Path)>;
    thread_local! {
        pub(super) static AFTER_READ: RefCell<Option<ReadHook>> = const { RefCell::new(None) };
    }

    /// Change the publication inside the real read, without racing a timer or
    /// sharing injection state with other test threads.
    pub(crate) fn with_after_read<T>(
        hook: impl FnOnce(&Path) + 'static,
        observe: impl FnOnce() -> T,
    ) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                AFTER_READ.with(|slot| slot.borrow_mut().take());
            }
        }
        AFTER_READ.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(hook));
        });
        let _reset = Reset;
        observe()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Companion {
    path: PathBuf,
    length: u64,
    modified: std::time::SystemTime,
}

impl Companion {
    fn is_current(&self) -> bool {
        fs::metadata(&self.path).is_ok_and(|metadata| {
            metadata.len() == self.length && metadata.modified().ok() == Some(self.modified)
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ReportSource {
    File {
        path: PathBuf,
        length: u64,
        modified: std::time::SystemTime,
        /// A sibling file the report's clock or turn boundary was read from.
        companion: Option<Companion>,
    },
    OpenCode {
        path: PathBuf,
        session_id: String,
        rows: Vec<(String, serde_json::Value)>,
    },
    ResultFile {
        transcript: Option<Box<ReportSnapshot>>,
        path: PathBuf,
        length: u64,
        modified: std::time::SystemTime,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReportSnapshot {
    pub text: String,
    pub revision: String,
    pub published_at_ms: i64,
    /// Native turn boundary, independent of the displayed agent status.
    /// This says nothing about task correctness or complete owned-work coverage.
    pub turn_finished: bool,
    source: ReportSource,
}

impl ReportSnapshot {
    /// Anchor a result to the already admitted turn. File-backed turns also
    /// retain their transcript guard; receipt-backed turns keep their input fence.
    pub(crate) fn read_result_file(
        path: &Path,
        turn_revision: &str,
        published_at_ms: i64,
        transcript: Option<&Self>,
    ) -> std::io::Result<Option<Self>> {
        use sha2::{Digest, Sha256};
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let result = crate::circuit::handoff::read_result(path);
        #[cfg(test)]
        if let Some(hook) =
            result_read_test_support::AFTER_READ.with(|slot| slot.borrow_mut().take())
        {
            hook(path);
        }
        let Some(text) = result? else {
            return Ok(None);
        };
        let snapshot = Self {
            revision: format!(
                "{}:result:{}",
                turn_revision,
                hex::encode(Sha256::digest(text.as_bytes()))
            ),
            text: crate::secret_scrubber::SecretScrubber::scrub(&text),
            published_at_ms,
            turn_finished: transcript.is_some_and(|report| report.turn_finished),
            source: ReportSource::ResultFile {
                transcript: transcript.cloned().map(Box::new),
                path: path.into(),
                length: metadata.len(),
                modified: metadata.modified()?,
            },
        };
        if snapshot.is_current() {
            Ok(Some(snapshot))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "result file or transcript changed while reading the report",
            ))
        }
    }

    pub(crate) fn is_current(&self) -> bool {
        match &self.source {
            ReportSource::File {
                path,
                length,
                modified,
                companion,
            } => {
                fs::metadata(path).is_ok_and(|metadata| {
                    metadata.len() == *length && metadata.modified().ok() == Some(*modified)
                }) && companion.as_ref().is_none_or(Companion::is_current)
            }
            ReportSource::OpenCode {
                path,
                session_id,
                rows,
            } => {
                read_opencode_message_rows(path, session_id, OPENCODE_DIGEST_WINDOW).as_ref()
                    == Some(rows)
            }
            ReportSource::ResultFile {
                transcript,
                path,
                length,
                modified,
            } => {
                transcript.as_ref().is_none_or(|report| report.is_current())
                    && fs::metadata(path).is_ok_and(|metadata| {
                        metadata.len() == *length && metadata.modified().ok() == Some(*modified)
                    })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportReadError {
    Unsupported,
    NoSession,
    NoTranscript,
    Unreadable,
    PartialPublication,
    MalformedRecord,
    NoReport,
    WorkInProgress,
    NoNativeCompletion,
    NoTimestamp,
    ChangedDuringRead,
}

impl ReportReadError {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Unsupported => "this harness has no readable report adapter",
            Self::NoSession => "the harness session identity has not been captured",
            Self::NoTranscript => "the current session transcript has not been found",
            Self::Unreadable => "the transcript could not be read",
            Self::PartialPublication => "the harness is still publishing a transcript record",
            Self::MalformedRecord => "the transcript contains an unrecognised or malformed record",
            Self::NoReport => "the current turn has not published an assistant report",
            Self::WorkInProgress => "newer input or tool activity follows the assistant report",
            Self::NoNativeCompletion => "the harness has not recorded a current finished turn",
            Self::NoTimestamp => "the report has no usable publication timestamp",
            Self::ChangedDuringRead => "the transcript changed while it was being read",
        }
    }
}

pub(crate) fn read(
    format: TranscriptFormat,
    session_id: &str,
    node_path: &str,
) -> Result<ReportSnapshot, ReportReadError> {
    use ReportReadError as E;
    if format == TranscriptFormat::OpenCode {
        // Resolve the env-aware SQLite store, then read it. The resolve/read
        // split mirrors `read_opencode_tail` and lets the store-backed read be
        // exercised against a temporary DB without touching `$HOME`.
        let (path, session_id) =
            opencode_resolve(Some(session_id), node_path).map_err(|_| E::NoTranscript)?;
        return read_opencode_file(&path, session_id);
    }
    if format == TranscriptFormat::Cline {
        // Issue #1776: Cline *has* a transcript reader (the Node Digest reads
        // `<id>.messages.json`), but that reader is a whole-document parser and
        // this one is line-oriented: it requires a trailing newline, parses
        // each line as a standalone JSON record, and derives the publication
        // time from a per-record `timestamp`. A Cline document is a single JSON
        // object, so routing it here would not merely fail — it would fail
        // *misleadingly*, reporting `PartialPublication` ("still publishing a
        // transcript record") or `NoReport` ("has not published an assistant
        // report") for a node whose document is complete. `Unsupported` is both
        // the truth and the useful answer: `readiness::prepare` admits
        // hook-native evidence for exactly this variant, so a Cline circuit
        // continues on its `agent_end` receipt instead of being walled off.
        return Err(E::Unsupported);
    }
    read_file(
        &locate_transcript(format, session_id, node_path).ok_or(E::NoTranscript)?,
        format,
    )
}

/// Read an OpenCode session's report from its SQLite store. The store is the
/// OpenCode equivalent of a transcript file, so a malformed message record must
/// degrade to `MalformedRecord` exactly like the file-backed readers rather than
/// masquerading as "no report yet".
fn read_opencode_file(path: &Path, session_id: &str) -> Result<ReportSnapshot, ReportReadError> {
    use ReportReadError as E;
    let rows = read_opencode_message_rows(path, session_id, OPENCODE_DIGEST_WINDOW)
        .ok_or(E::Unreadable)?;
    let parsed = parse_opencode_messages(
        &rows
            .iter()
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>(),
        1,
    );
    if parsed.saw_malformed {
        return Err(E::MalformedRecord);
    }
    let (id, message) = rows.last().ok_or(E::NoReport)?;
    if message.pointer("/info/role").and_then(|role| role.as_str()) != Some("assistant") {
        return Err(E::WorkInProgress);
    }
    // Digest parsing drops empty/reasoning-only messages. They must not let
    // an older final answer borrow the newer message's completion timestamp.
    let latest = parse_opencode_messages(std::slice::from_ref(message), 1);
    let last = latest.turns.last().ok_or(E::WorkInProgress)?;
    if last.role != "assistant" || !last.tool_calls.is_empty() {
        return Err(E::WorkInProgress);
    }
    let published_at_ms = message
        .pointer("/info/time/completed")
        .and_then(|time| time.as_i64())
        .ok_or(E::WorkInProgress)?;
    let text = adapters::opencode::parse_opencode_messages_with_text_limit(
        std::slice::from_ref(message),
        1,
        usize::MAX,
    )
    .last_assistant_message
    .ok_or(E::NoReport)?;
    let revision = assistant_revision(id, &last.text, &text);
    let snapshot = ReportSnapshot {
        text: crate::secret_scrubber::SecretScrubber::scrub(&text),
        revision,
        published_at_ms,
        // Message completion is not a native session-idle boundary.
        turn_finished: false,
        source: ReportSource::OpenCode {
            path: path.into(),
            session_id: session_id.into(),
            rows,
        },
    };
    if snapshot.is_current() {
        Ok(snapshot)
    } else {
        Err(E::ChangedDuringRead)
    }
}

#[cfg(test)]
fn from_file(path: &Path, format: TranscriptFormat) -> Option<ReportSnapshot> {
    read_file(path, format).ok()
}

fn read_file(path: &Path, format: TranscriptFormat) -> Result<ReportSnapshot, ReportReadError> {
    use ReportReadError as E;
    let metadata = fs::metadata(path).map_err(|_| E::Unreadable)?;
    let mut file = fs::File::open(path).map_err(|_| E::Unreadable)?;
    if metadata.len() == 0 {
        return Err(E::NoReport);
    }
    file.seek(SeekFrom::End(-1)).map_err(|_| E::Unreadable)?;
    let mut last_byte = [0];
    file.read_exact(&mut last_byte).map_err(|_| E::Unreadable)?;
    if last_byte[0] != b'\n' {
        return Err(E::PartialPublication);
    }
    let start = metadata.len().saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| E::Unreadable)?;
    let mut reader = BufReader::new(file.take(metadata.len() - start));
    if start > 0 {
        reader
            .read_until(b'\n', &mut Vec::new())
            .map_err(|_| E::Unreadable)?;
    }
    let lines = reader
        .lines()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| E::Unreadable)?;
    let mut last = None;
    let mut published_at_ms = None;
    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        let value =
            serde_json::from_str::<serde_json::Value>(line).map_err(|_| E::MalformedRecord)?;
        // Only the last dialogue record decides readiness; full-turn parsing
        // coalesces earlier tool calls with a later final answer.
        let parsed = parse_transcript(format, std::iter::once(line.clone()), 1);
        if parsed.saw_malformed {
            return Err(E::MalformedRecord);
        }
        if let Some(turn) = parsed.turns.into_iter().last() {
            published_at_ms = record_time(format, &value);
            last = Some(turn);
        }
    }
    let last = last.ok_or(E::NoReport)?;
    if last.role != "assistant" || !last.tool_calls.is_empty() {
        return Err(E::WorkInProgress);
    }
    let mut companion = None;
    let turn_finished = match format {
        TranscriptFormat::Grok => {
            // Grok's transcript records carry no clock: the sibling events file
            // supplies both the publication time and the native turn boundary.
            use super::readers::grok::{completed_turn, TurnEndUnavailable};
            let turn = completed_turn(path).map_err(|reason| match reason {
                TurnEndUnavailable::Partial => E::PartialPublication,
                TurnEndUnavailable::Unfinished => E::NoNativeCompletion,
            })?;
            published_at_ms = Some(turn.completed_at_ms);
            companion = Some(Companion {
                path: turn.events,
                length: turn.length,
                modified: turn.modified,
            });
            true
        }
        TranscriptFormat::Codex => {
            let completion = adapter::dispatch("codex")
                .and_then(|adapter| adapter.completed_turn(&lines.join("\n")))
                .ok_or(E::NoNativeCompletion)?;
            published_at_ms = Some(completion.completed_at_ms);
            true
        }
        TranscriptFormat::Muse => {
            if !crate::services::muse_watcher::report_turn_finished(&lines) {
                return Err(E::NoNativeCompletion);
            }
            true
        }
        TranscriptFormat::CommandCode => {
            if !crate::services::commandcode_watcher::report_turn_finished(&lines) {
                return Err(E::NoNativeCompletion);
            }
            true
        }
        _ => false,
    };
    let report = assistant_report_from_file(path, format).ok_or(E::NoReport)?;
    let snapshot = ReportSnapshot {
        text: crate::secret_scrubber::SecretScrubber::scrub(&report.text),
        revision: report.revision,
        published_at_ms: published_at_ms.ok_or(E::NoTimestamp)?,
        turn_finished,
        source: ReportSource::File {
            path: path.into(),
            length: metadata.len(),
            modified: metadata.modified().map_err(|_| E::Unreadable)?,
            companion,
        },
    };
    if snapshot.is_current() {
        Ok(snapshot)
    } else {
        Err(E::ChangedDuringRead)
    }
}

fn record_time(format: TranscriptFormat, value: &serde_json::Value) -> Option<i64> {
    if format == TranscriptFormat::Muse {
        return value
            .get("recorded_at")?
            .as_i64()
            .map(|micros| micros / 1000);
    }
    let timestamp = match format {
        TranscriptFormat::Mcode => value.pointer("/message/timestamp"),
        TranscriptFormat::Agy => value.get("created_at"),
        TranscriptFormat::CommandCode => value
            .get("timestamp")
            .or_else(|| value.pointer("/message/meta/createdAt")),
        _ => value.get("timestamp"),
    }?;
    timestamp.as_i64().or_else(|| {
        chrono::DateTime::parse_from_rfc3339(timestamp.as_str()?)
            .ok()
            .map(|time| time.timestamp_millis())
    })
}

#[cfg(test)]
#[path = "report_snapshot_tests.rs"]
mod tests;
