//! Cursor CLI transcript adapter.
//!
//! Cursor writes per-session JSONL under
//! `~/.cursor/projects/<workspace-slug>/agent-transcripts/<session>/
//! <session>.jsonl` with the **same message shape** as Claude Code, but
//! the workspace-scoped path. Issue #1661 step 4: Cursor's adapter owns
//! the workspace slug + path composition; the parser delegates to
//! Claude Code's `parse_turns` because the wire is byte-identical.

use std::path::{Path, PathBuf};

use crate::env;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::readers::claude_code::parse_turns_with_text_limit;
use crate::services::transcript_reader::types::Parsed;

use super::claude_code::ClaudeCodeAdapter;

/// Drop-in [`TranscriptAdapter`] for Cursor.
pub(crate) struct CursorAdapter;

impl TranscriptAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        // Cursor's path layout always builds a path; the file-existence
        // check happens at the reader's call site (matches the legacy
        // `locate_transcript` Cursor arm).
        Some(cursor_transcript_path_in(
            &env::cli_dir_for_spawn(env::cursor_dir(), ".cursor", ctx.node_path)?,
            ctx.session_id,
            ctx.node_path,
        ))
    }

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        // Cursor's JSONL shape matches Claude Code's — delegate to the
        // Claude Code parser rather than duplicate `parse_turns`.
        parse_turns_with_text_limit(lines, keep, max_text)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        // Same predicate Claude Code uses (Cursor's envelope matches).
        ClaudeCodeAdapter.line_has_assistant_text(line)
    }
}

/// Pure path builder for Cursor transcripts, split from the environment
/// lookup so the workspace layout can be tested without process-global
/// state. `pub(crate)` so the reader's contract test still drives the
/// resolve against a temp cursor_home.
pub(crate) fn cursor_transcript_path_in(
    cursor_home: &Path,
    session_id: &str,
    node_path: &str,
) -> PathBuf {
    cursor_home
        .join("projects")
        .join(cursor_workspace_slug(node_path))
        .join("agent-transcripts")
        .join(session_id)
        .join(format!("{session_id}.jsonl"))
}

/// Convert a workspace path into Cursor's lossy project directory slug.
/// Cursor drops a leading separator, removes a Windows drive colon, and
/// uses dashes for path separators and other non-alphanumeric characters.
///
/// Kept `pub(crate)` because `agent_node_discovery` (until step 10 of
/// #1661) reads it to detect worktree-prefixed session directories.
pub(crate) fn cursor_workspace_slug(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut parts = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    if let Some(first) = parts.first_mut() {
        if first.len() == 2 && first.as_bytes()[1] == b':' {
            first.truncate(1);
            first.make_ascii_lowercase();
        }
    }

    parts
        .into_iter()
        .map(|part| {
            part.chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    use crate::services::transcript_reader::test_support::write_fixture;

    use crate::services::transcript_reader::{
        read_tail_from_file, TranscriptFormat, TranscriptTail,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn cursor_workspace_slug_matches_cursor_storage_names() {
        assert_eq!(
            cursor_workspace_slug("/Users/adam/src/buildmesh"),
            "Users-adam-src-buildmesh"
        );
        assert_eq!(
            cursor_workspace_slug("C:\\Users\\adam\\src\\buildmesh"),
            "c-Users-adam-src-buildmesh"
        );
        assert_eq!(
            cursor_workspace_slug(
                "C:\\Users\\adam\\src\\buildmesh\\.claude\\worktrees\\fancy-name"
            ),
            "c-Users-adam-src-buildmesh--claude-worktrees-fancy-name"
        );
    }

    #[test]
    fn cursor_transcript_path_uses_workspace_scoped_session_directory() {
        let path = cursor_transcript_path_in(
            Path::new("/home/adam/.cursor"),
            "session-123",
            "C:\\Users\\adam\\src\\buildmesh",
        );
        assert_eq!(
            path,
            PathBuf::from(
                r#"/home/adam/.cursor/projects/c-Users-adam-src-buildmesh/agent-transcripts/session-123/session-123.jsonl"#
            )
        );
    }

    #[test]
    fn cursor_jsonl_reuses_the_shared_message_parser() {
        let path = write_fixture(
            "cursor_transcript",
            r#"{"type":"user","message":{"role":"user","content":"Inspect the cursor path"}}
{"type":"assistant","message":{"role":"assistant","id":"msg-1","content":[{"type":"tool_use","name":"Read","input":{"file":"src/main.rs"}}]}}
{"type":"assistant","message":{"role":"assistant","id":"msg-1","content":[{"type":"text","text":"The path is wired."}]}}
"#,
        );
        let tail = read_tail_from_file(&path, 10, TranscriptFormat::Cursor);
        std::fs::remove_file(path).ok();

        let TranscriptTail::Available { turns, .. } = tail else {
            panic!("Cursor's compatible JSONL should be readable");
        };
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].text, "The path is wired.");
        assert_eq!(turns[1].tool_calls[0].name, "Read");
    }
}

#[cfg(test)]
mod file_contract_tests {
    use super::*;
    use crate::services::transcript_reader::test_support::{assert_jsonl_contract, fixture};

    #[test]
    fn reader_handles_tail_digest_malformed_empty_shape_changed_and_unreadable_files() {
        assert_jsonl_contract(
            &CursorAdapter,
            &fixture("cursor", "transcript.jsonl"),
            &fixture("cursor", "shape_changed.jsonl"),
            "Cursor blocking question?",
        );
    }
}
