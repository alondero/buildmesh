//! Harness-owned transcript readers for Node Digest and Circuit enrichment.
//!
//! The public signatures stay stable while each reader owns its location,
//! format parsing and contract fixtures. Shared file windows and result shaping
//! keep unavailable enrichment explicit for every harness.

use adapter::{LocateCtx, TranscriptReader};
use std::path::{Path, PathBuf};
use types::{build_tail, effective_tail, Parsed};

pub(crate) mod adapter;
pub(crate) mod readers;
// Existing lifecycle and session-discovery consumers retain their import paths.
pub(crate) use readers as adapters;
mod file;
mod native_completion;
pub(crate) mod report_snapshot;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod types;

use file::assistant_revision;
pub(crate) use file::same_assistant_revision;
pub(crate) use native_completion::{
    read_native_turn_completion, NativeTurnCompletion, NativeTurnSnapshot,
};
pub(crate) use types::AssistantReport;
#[allow(unused_imports)]
pub use types::{ToolCall, TranscriptTail, Turn, UnavailableReason};

/// A wired on-disk transcript format, selected from the resolved harness id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    ClaudeCode,
    Cursor,
    Codex,
    CommandCode,
    Agy,
    Grok,
    Muse,
    Mcode,
    OpenCode,
    Cline,
}

impl TranscriptFormat {
    /// Claude-compatible profiles are explicit; unknown ids have no reader.
    pub fn for_harness(harness_id: &str) -> Option<Self> {
        match harness_id {
            "anthropic" | "claude" => Some(Self::ClaudeCode),
            "cursor" => Some(Self::Cursor),
            "codex" => Some(Self::Codex),
            "commandcode" => Some(Self::CommandCode),
            "agy" => Some(Self::Agy),
            "grok" => Some(Self::Grok),
            "muse" => Some(Self::Muse),
            "mcode" => Some(Self::Mcode),
            "opencode" => Some(Self::OpenCode),
            "cline" => Some(Self::Cline),
            _ => None,
        }
    }
}

fn reader(format: TranscriptFormat) -> &'static dyn TranscriptReader {
    use readers::*;
    match format {
        TranscriptFormat::ClaudeCode => &ClaudeCodeAdapter,
        TranscriptFormat::Cursor => &CursorAdapter,
        TranscriptFormat::Codex => &CodexAdapter,
        TranscriptFormat::CommandCode => &CommandCodeAdapter,
        TranscriptFormat::Agy => &AgyAdapter,
        TranscriptFormat::Grok => &GrokAdapter,
        TranscriptFormat::Muse => &MuseAdapter,
        TranscriptFormat::Mcode => &McodeAdapter,
        TranscriptFormat::OpenCode => &OpenCodeAdapter,
        TranscriptFormat::Cline => &ClineAdapter,
    }
}

fn locate_transcript(
    format: TranscriptFormat,
    session_id: &str,
    node_path: &str,
) -> Option<PathBuf> {
    reader(format).locate(LocateCtx {
        session_id,
        node_path,
    })
}

fn resolve<'a>(
    format: TranscriptFormat,
    session_id: Option<&'a str>,
    node_path: &str,
) -> Result<(PathBuf, &'a str), UnavailableReason> {
    let session_id = session_id
        .filter(|id| !id.is_empty())
        .ok_or(UnavailableReason::NoSession)?;
    let path = locate_transcript(format, session_id, node_path)
        .filter(|path| path.exists())
        .ok_or(UnavailableReason::NoTranscript)?;
    Ok((path, session_id))
}

fn result(parsed: Result<Parsed, UnavailableReason>, digest: bool) -> TranscriptTail {
    let mut tail = match parsed {
        Ok(parsed) => build_tail(parsed),
        Err(reason) => TranscriptTail::unavailable(reason),
    };
    if digest {
        if let TranscriptTail::Available { turns, .. } = &mut tail {
            turns.clear();
        }
    }
    tail
}

/// Locate and read recent turns, clamping `tail` to the shared limits.
pub fn read_tail(
    format: TranscriptFormat,
    session_id: Option<&str>,
    node_path: &str,
    tail: usize,
) -> TranscriptTail {
    result(
        resolve(format, session_id, node_path).and_then(|(path, session_id)| {
            reader(format).read_tail(&path, session_id, effective_tail(tail))
        }),
        false,
    )
}

/// Read an explicit transcript store through the same reader as production.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Preserved public fixture entrypoint")
)]
pub fn read_tail_from_file(path: &Path, tail: usize, format: TranscriptFormat) -> TranscriptTail {
    result(
        reader(format).read_tail(path, "", effective_tail(tail)),
        false,
    )
}

/// Read only the latest assistant text. Digest results never include turns.
pub fn read_last_assistant_message(
    format: TranscriptFormat,
    session_id: Option<&str>,
    node_path: &str,
) -> TranscriptTail {
    result(
        resolve(format, session_id, node_path).and_then(|(path, session_id)| {
            reader(format).last_assistant_message(&path, session_id)
        }),
        true,
    )
}

/// Read the digest from an explicit transcript store.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Preserved public fixture entrypoint")
)]
pub fn read_last_assistant_message_from_file(
    path: &Path,
    format: TranscriptFormat,
) -> TranscriptTail {
    result(reader(format).last_assistant_message(path, ""), true)
}

/// Circuit reports retain full text and a content-and-position revision.
pub(crate) fn read_assistant_report(
    format: TranscriptFormat,
    session_id: Option<&str>,
    node_path: &str,
) -> Option<AssistantReport> {
    let (path, session_id) = resolve(format, session_id, node_path).ok()?;
    reader(format).assistant_report(&path, session_id)
}

fn assistant_report_from_file(path: &Path, format: TranscriptFormat) -> Option<AssistantReport> {
    reader(format).assistant_report(path, "")
}

fn parse_transcript(
    format: TranscriptFormat,
    lines: impl Iterator<Item = String>,
    keep: usize,
) -> Parsed {
    reader(format).parse(Box::new(lines), keep, types::MAX_TURN_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transcript_format_for_harness_routes_each_format() {
        assert_eq!(
            TranscriptFormat::for_harness("codex"),
            Some(TranscriptFormat::Codex)
        );
        assert_eq!(
            TranscriptFormat::for_harness("commandcode"),
            Some(TranscriptFormat::CommandCode)
        );
        assert_eq!(
            TranscriptFormat::for_harness("cursor"),
            Some(TranscriptFormat::Cursor)
        );
        assert_eq!(
            TranscriptFormat::for_harness("agy"),
            Some(TranscriptFormat::Agy)
        );
        assert_eq!(
            TranscriptFormat::for_harness("grok"),
            Some(TranscriptFormat::Grok)
        );
        assert_eq!(
            TranscriptFormat::for_harness("mcode"),
            Some(TranscriptFormat::Mcode)
        );
        assert_eq!(
            TranscriptFormat::for_harness("muse"),
            Some(TranscriptFormat::Muse)
        );
        assert_eq!(
            TranscriptFormat::for_harness("opencode"),
            Some(TranscriptFormat::OpenCode)
        );
        assert_eq!(
            TranscriptFormat::for_harness("cline"),
            Some(TranscriptFormat::Cline)
        );
        for id in ["anthropic", "claude"] {
            assert_eq!(
                TranscriptFormat::for_harness(id),
                Some(TranscriptFormat::ClaudeCode),
                "{id} is Claude-backed and must stay on Claude Code"
            );
        }
    }

    #[test]
    fn transcript_format_for_harness_rejects_unwired_harnesses() {
        for id in ["kimi", "dsh", "freebuff", "terminal", "", "totally-unknown"] {
            assert_eq!(
                TranscriptFormat::for_harness(id),
                None,
                "{id} has no wired reader and must not resolve to ClaudeCode"
            );
        }
    }

    #[test]
    fn transcript_format_for_harness_matches_capability_catalog() {
        let catalog: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../src/types/generated/HarnessCapabilitiesTable.json"
        ))
        .expect("generated HarnessCapabilitiesTable.json must parse");
        let ids = catalog
            .get("ids")
            .and_then(|v| v.as_array())
            .expect("catalog JSON must include ids in Provider::all() order");
        let capabilities = catalog
            .get("capabilities")
            .and_then(|v| v.as_object())
            .expect("catalog JSON must include capabilities");
        assert!(
            !ids.is_empty(),
            "catalog ids must not be empty or the consistency check is vacuous"
        );
        for id in ids {
            let id = id.as_str().expect("catalog ids must be strings");
            let produces = capabilities
                .get(id)
                .and_then(|caps| caps.get("produces_readable_transcript"))
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| {
                    panic!("catalog capabilities for {id} must carry produces_readable_transcript")
                });
            match TranscriptFormat::for_harness(id) {
                Some(format) => assert!(
                    produces,
                    "{id} has a wired {format:?} reader but advertises produces_readable_transcript=false"
                ),
                None => assert!(
                    !produces,
                    "{id} advertises produces_readable_transcript=true but has no for_harness arm"
                ),
            }
        }
        // The `claude` profile alias is not a catalog id (it maps to
        // `anthropic` via HARNESS_PROFILE_ALIASES) but must keep resolving.
        assert_eq!(
            TranscriptFormat::for_harness("claude"),
            Some(TranscriptFormat::ClaudeCode)
        );
    }
}
