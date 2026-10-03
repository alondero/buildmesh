//! Stable observation of a reader's explicit native turn completion.
use super::{locate_transcript, reader, TranscriptFormat};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeTurnCompletion {
    pub turn_id: String,
    pub completed_at_ms: i64,
    pub final_report: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeTurnSnapshot {
    pub completion: NativeTurnCompletion,
    path: PathBuf,
    length: u64,
    modified: std::time::SystemTime,
}

impl NativeTurnSnapshot {
    /// This is a short-lived read-stability check, not a durable report ID.
    pub(crate) fn is_current(&self) -> bool {
        fs::metadata(&self.path).is_ok_and(|metadata| {
            metadata.len() == self.length && metadata.modified().ok() == Some(self.modified)
        })
    }
}

pub(crate) fn read_native_turn_completion(
    format: TranscriptFormat,
    session_id: Option<&str>,
    node_path: &str,
) -> Option<NativeTurnSnapshot> {
    let path = locate_transcript(format, session_id?, node_path)?;
    native_turn_snapshot_from_file(&path, format)
}

pub(super) fn native_turn_snapshot_from_file(
    path: &Path,
    format: TranscriptFormat,
) -> Option<NativeTurnSnapshot> {
    let metadata = fs::metadata(path).ok()?;
    let snapshot = NativeTurnSnapshot {
        completion: native_turn_completion_from_file(path, format)?,
        path: path.to_path_buf(),
        length: metadata.len(),
        modified: metadata.modified().ok()?,
    };
    snapshot.is_current().then_some(snapshot)
}

pub(super) fn native_turn_completion_from_file(
    path: &Path,
    format: TranscriptFormat,
) -> Option<NativeTurnCompletion> {
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut input = BufReader::new(file.take(size - start));
    if start > 0 {
        input.read_until(b'\n', &mut Vec::new()).ok()?;
    }
    let mut lines = String::new();
    input.read_to_string(&mut lines).ok()?;
    // A partially published next record may start a new turn.
    if !lines.ends_with('\n') && !lines.rsplit('\n').next()?.trim().is_empty() {
        return None;
    }
    reader(format).completed_turn(&lines)
}
