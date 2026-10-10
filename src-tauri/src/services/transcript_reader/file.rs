//! Shared streaming and bounded byte-window I/O, independent of harness shape.
use super::{
    adapter::TranscriptReader,
    types::{Parsed, MAX_TURN_TEXT},
    AssistantReport, UnavailableReason,
};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

fn parse<R: TranscriptReader + ?Sized>(
    reader: &R,
    lines: impl Iterator<Item = String>,
    keep: usize,
) -> Parsed {
    reader.parse(Box::new(lines), keep, MAX_TURN_TEXT)
}

pub(super) fn read_tail<R: TranscriptReader + ?Sized>(
    adapter: &R,
    path: &Path,
    keep: usize,
) -> Result<Parsed, UnavailableReason> {
    let file = fs::File::open(path).map_err(|_| UnavailableReason::Unreadable)?;
    parse_lines(adapter, BufReader::new(file), keep)
}

fn parse_lines<R: TranscriptReader + ?Sized>(
    adapter: &R,
    input: impl BufRead,
    keep: usize,
) -> Result<Parsed, UnavailableReason> {
    let mut unreadable = false;
    let lines = input.lines().map_while(|line| match line {
        Ok(line) => Some(line),
        Err(_) => {
            unreadable = true;
            None
        }
    });
    let parsed = parse(adapter, lines, keep);
    if unreadable {
        Err(UnavailableReason::Unreadable)
    } else {
        Ok(parsed)
    }
}

pub(super) fn last_assistant_message<R: TranscriptReader + ?Sized>(
    adapter: &R,
    path: &Path,
) -> Result<Parsed, UnavailableReason> {
    const TAIL_BYTES: u64 = 256 * 1024;
    let size = fs::metadata(path)
        .map_err(|_| UnavailableReason::Unreadable)?
        .len();
    if size > TAIL_BYTES {
        let window = parse_byte_window(adapter, path, TAIL_BYTES)?;
        if window.last_assistant_message.is_some() {
            return Ok(window);
        }
    }
    // A tool-only byte window must still recover an older assistant answer.
    read_tail(adapter, path, 1)
}

fn parse_byte_window<R: TranscriptReader + ?Sized>(
    adapter: &R,
    path: &Path,
    tail_bytes: u64,
) -> Result<Parsed, UnavailableReason> {
    let mut file = fs::File::open(path).map_err(|_| UnavailableReason::Unreadable)?;
    file.seek(SeekFrom::End(-(tail_bytes as i64)))
        .map_err(|_| UnavailableReason::Unreadable)?;
    let mut input = BufReader::new(file.take(tail_bytes));
    // Drop the partial record before decoding UTF-8; the seek may split a character.
    input
        .read_until(b'\n', &mut Vec::new())
        .map_err(|_| UnavailableReason::Unreadable)?;
    parse_lines(adapter, input, 1)
}

pub(super) fn assistant_report<R: TranscriptReader + ?Sized>(
    adapter: &R,
    path: &Path,
) -> Option<AssistantReport> {
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(file.take(size - start));
    let mut offset = start;
    let mut line = String::new();
    if start > 0 {
        // The window may begin in the middle of a UTF-8 character.
        offset += reader.read_until(b'\n', &mut Vec::new()).ok()? as u64;
    }
    let mut lines = Vec::new();
    let mut assistant_line_offset = None;
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).ok()?;
        if bytes == 0 {
            break;
        }
        offset += bytes as u64;
        // Ignore a record the writer has not finished publishing yet.
        if !line.ends_with('\n') {
            break;
        }
        if adapter.line_has_assistant_text(&line) {
            assistant_line_offset = Some(offset);
        }
        lines.push(std::mem::take(&mut line));
    }
    let preview = parse(adapter, lines.iter().cloned(), 1).last_assistant_message?;
    let text = adapter
        .parse(Box::new(lines.into_iter()), 1, usize::MAX)
        .last_assistant_message?;
    let offset = assistant_line_offset?;
    Some(AssistantReport {
        // Revisions identify the assistant content plus its position. Hashing
        // the normalized text keeps file-backed providers consistent with the
        // OpenCode report reader; the offset still distinguishes identical
        // responses emitted at different points in one transcript.
        revision: assistant_revision(&offset.to_string(), &preview, &text),
        text,
    })
}

/// Read the tail of a transcript as raw records, bounded so a long session
/// costs a fixed read rather than its whole history.
///
/// The wake-up probe needs both the launch record and the session's last
/// assistant message, and the launch can be far back in the file, so a window
/// that is merely "recent" is not sufficient. The bound is what keeps this
/// honest: a probe that would miss its evidence is reported as truncated, so
/// the caller can fall back rather than silently decide from a partial view.
pub(super) fn tail_lines(path: &Path, max_bytes: u64) -> Option<(String, bool)> {
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(max_bytes);
    let truncated = start > 0;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(file.take(size - start));
    let mut body = String::new();
    if truncated {
        // The window may begin mid-record (and mid UTF-8 character); drop it.
        reader.read_until(b'\n', &mut Vec::new()).ok()?;
    }
    reader.read_to_string(&mut body).ok()?;
    Some((body, truncated))
}

pub(super) fn assistant_revision(position: &str, preview: &str, text: &str) -> String {
    use sha2::{Digest, Sha256};
    let legacy = format!("{position}:{:x}", Sha256::digest(preview.as_bytes()));
    if preview == text {
        legacy
    } else {
        format!("{legacy}:{:x}", Sha256::digest(text.as_bytes()))
    }
}

pub(crate) fn same_assistant_revision(current: &str, previous: &str) -> bool {
    current == previous
        || (current.split(':').count() == 3
            && current
                .rsplit_once(':')
                .is_some_and(|(legacy, _)| legacy == previous))
}
