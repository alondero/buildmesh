//! Command Code transcript adapter (issues #1407, #1500).
//!
//! Command Code writes structured per-session JSONL under
//! `~/.commandcode/projects/<encoded-cwd>/<session-id>.jsonl` — one
//! `{type: "message", message: {role, content}}` event per line.
//!
//! Issue #1661 step 3: Command Code is the **second harness migrated
//! end-to-end**. The reader's `commandcode_sessions_dir` path
//! composition + `commandcode_transcript_path_in` +
//! `find_commandcode_transcript` wrapper + `commandcode_message_activity`
//! (used by `commandcode_watcher.rs`) + `parse_commandcode_turns` all
//! live in this file. The capture poller in
//! `services::commandcode_session` already delegates to
//! `commandcode_sessions_dir`, so the seam-inversion is one import
//! rewrite in step 10.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::env;
use crate::models::EnvType;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, merge_into_with_text_limit, push_bounded, truncate, Parsed, Turn,
};
// Command Code's wire shape reuses Claude Code's content
// primitives (same `tool_use` blocks, same `local-command-caveat`
// synthetic wrappers). Reach the shared primitives from
// `transcript_paths` — the legacy indirection through mod.rs ended
// with step 5 of #1661.
use super::claude_code::extract_tool_calls as extract_claude_tool_calls;
use crate::services::transcript_paths::{concat_text_blocks, is_synthetic_message};

/// Drop-in [`TranscriptAdapter`] for Command Code.
pub(crate) struct CommandCodeAdapter;

impl TranscriptAdapter for CommandCodeAdapter {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        let env_type = env::runtime_for_spawn_path(ctx.node_path);
        let sessions_dir = commandcode_sessions_dir(env_type, ctx.node_path)?;
        let path = commandcode_transcript_path_in(&sessions_dir, ctx.session_id);
        path.exists().then_some(path)
    }

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        parse_commandcode_turns_with_text_limit(lines, keep, max_text)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("type").and_then(|kind| kind.as_str()) == Some("message")
            && value.get("message").is_some_and(|message| {
                message.get("role").and_then(|role| role.as_str()) == Some("assistant")
                    && !concat_text_blocks(message.get("content")).trim().is_empty()
            })
    }
}

/// Encode a filesystem path the way Command Code does for its
/// `~/.commandcode/projects/<slug>` directory names: lowercase, replace every
/// non-alphanumeric character with `-`, collapse consecutive `-` runs, and
/// trim leading/trailing `-` (issue #1500).
///
/// For example `F:\src\buildmesh\.claude\worktrees\foo` becomes
/// `f-src-buildmesh-claude-worktrees-foo`, and `/home/user/project` becomes
/// `home-user-project`. This matches the on-disk layout observed in Command
/// Code v1.43.0 and the `c-users-user` / `home-...` slugs reported upstream.
/// Pass the CLI cwd form; for raw paths that may be WSL UNC, normalize with
/// `env::normalize_unc_to_wsl` first.
///
/// Kept `pub(crate)` because `services::commandcode_session::find_*` and
/// `agent_node_discovery` (until step 10 of #1661) both need this; it
/// moves with the adapter.
pub(crate) fn commandcode_project_slug(path: &str) -> String {
    let mut slug = String::with_capacity(path.len());
    let mut last_was_dash = false;
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// The host-accessible Command Code session directory for an agent
/// environment: `<home>/.commandcode/projects/<encoded-cwd>/` (issue #1500).
/// Mirrors [`super::super::transcript_path`] (Claude Code): the transcript-
/// format module composes the slug, while `env` owns the host-accessible
/// projects base (including WSL translation). `spawn_path` is the CLI cwd
/// form; raw WSL UNC paths are normalized first via `env::normalize_unc_to_wsl`
/// so the in-WSL CLI's home-user-repo layout resolves to the same slug.
pub(crate) fn commandcode_sessions_dir(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    let normalized = if env_type == EnvType::Wsl {
        env::normalize_unc_to_wsl(spawn_path)
    } else {
        std::borrow::Cow::Borrowed(spawn_path)
    };
    let projects = env::commandcode_projects_dir(env_type, &normalized)?;
    let slug = commandcode_project_slug(&normalized);
    if slug.is_empty() {
        return None;
    }
    Some(projects.join(slug))
}

/// Pure Command Code locator used by the contract test and kept separate
/// from process-global home/environment discovery.
pub(crate) fn commandcode_transcript_path_in(sessions_root: &Path, session_id: &str) -> PathBuf {
    sessions_root.join(format!("{session_id}.jsonl"))
}

/// Classify a Command Code message payload using the canonical text and
/// tool-extraction rules. Empty, synthetic, tool-result, thinking, and
/// reasoning records are classified separately so the watcher can clear
/// pending tool calls, while the digest parser still omits them from
/// normalized turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandCodeMessageActivity {
    UserTurn,
    ToolUse,
    ToolResult,
    AssistantResponse,
}

/// Classify a Command Code message payload.
pub(crate) fn commandcode_message_activity(
    message: &serde_json::Value,
) -> Option<CommandCodeMessageActivity> {
    let role = message.get("role")?.as_str()?;
    let content = message.get("content")?;
    let text = concat_text_blocks(Some(content));
    let tool_calls = extract_claude_tool_calls(Some(content));

    match role {
        "user" if contains_tool_result(content) => Some(CommandCodeMessageActivity::ToolResult),
        "user" if is_synthetic_message(&text) || text.trim().is_empty() => None,
        "user" => Some(CommandCodeMessageActivity::UserTurn),
        "assistant" if !tool_calls.is_empty() => Some(CommandCodeMessageActivity::ToolUse),
        "assistant" if !text.trim().is_empty() => {
            Some(CommandCodeMessageActivity::AssistantResponse)
        }
        _ => None,
    }
}

fn contains_tool_result(content: &serde_json::Value) -> bool {
    content.as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.get("type").and_then(|kind| kind.as_str()) == Some("tool_result"))
    })
}

/// Parse Command Code JSONL lines into normalized turns. The rolling
/// buffer, assistant digest, malformed-shape signal, and assistant-id
/// coalescing all follow the shared transcript-reader contract used by
/// Claude Code/Cursor.
#[cfg(test)]
pub(crate) fn parse_commandcode_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    parse_commandcode_turns_with_text_limit(lines, keep, super::super::types::MAX_TURN_TEXT)
}

pub(crate) fn parse_commandcode_turns_with_text_limit(
    lines: impl Iterator<Item = String>,
    keep: usize,
    max_text: usize,
) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
    let mut open_assistant_id: Option<String> = None;

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        // Metadata and future event kinds are deliberately ignored. A
        // recognized message envelope with a broken payload is different:
        // it should degrade to ShapeChanged when no usable turns remain.
        if value.get("type").and_then(|kind| kind.as_str()) != Some("message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            saw_malformed = true;
            continue;
        };

        let Some(role) = message.get("role").and_then(|role| role.as_str()) else {
            saw_malformed = true;
            continue;
        };
        if role != "user" && role != "assistant" {
            saw_malformed = true;
            continue;
        }
        let Some(raw_content) = message.get("content") else {
            saw_malformed = true;
            continue;
        };
        if !matches!(
            raw_content,
            serde_json::Value::String(_) | serde_json::Value::Array(_) | serde_json::Value::Null
        ) {
            saw_malformed = true;
            continue;
        }
        let text = concat_text_blocks(Some(raw_content));
        let mut tool_calls = extract_claude_tool_calls(Some(raw_content));

        if role == "user" {
            open_assistant_id = None;
            if is_synthetic_message(&text) || text.trim().is_empty() {
                continue;
            }
            push_bounded(
                &mut turns,
                Turn {
                    role: "user".to_string(),
                    text: truncate(&text, max_text),
                    tool_calls: Vec::new(),
                },
                keep,
            );
            continue;
        }

        // An assistant message containing only internal blocks has no
        // user-facing normalized content. Keep the open id so a split
        // assistant message can still merge a following continuation.
        if text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }

        let id = value
            .get("id")
            .or_else(|| message.get("id"))
            .and_then(|id| id.as_str())
            .map(str::to_string);
        if let (Some(id), Some(open)) = (&id, &open_assistant_id) {
            if id == open {
                if let Some(last) = turns.back_mut() {
                    merge_into_with_text_limit(last, &text, tool_calls, max_text);
                    if !last.text.trim().is_empty() {
                        last_assistant_message = Some(last.text.clone());
                    }
                    continue;
                }
            }
        }

        open_assistant_id = id;
        cap_tool_calls(&mut tool_calls);
        let turn = Turn {
            role: "assistant".to_string(),
            text: truncate(&text, max_text),
            tool_calls,
        };
        if !turn.text.trim().is_empty() {
            last_assistant_message = Some(turn.text.clone());
        }
        push_bounded(&mut turns, turn, keep);
    }

    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed,
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::models::EnvType;
    use crate::services::transcript_reader::test_support::{fixture, write_fixture};
    use crate::services::transcript_reader::types::empty_or_shape_changed;
    use crate::services::transcript_reader::types::MAX_TOOL_STRING;
    use crate::services::transcript_reader::{
        read_last_assistant_message_from_file, read_tail_from_file, TranscriptFormat,
        TranscriptTail, UnavailableReason,
    };
    use std::path::Path;

    #[test]
    fn commandcode_slug_matches_v143_layout() {
        // Observed on-disk layout in Command Code v1.43.0 (issue #1500).
        assert_eq!(
            commandcode_project_slug(r"F:\src\buildmesh\.claude\worktrees\saucy-thunderous-cove"),
            "f-src-buildmesh-claude-worktrees-saucy-thunderous-cove"
        );
        assert_eq!(
            commandcode_project_slug(
                r"F:\src\buildmesh\.claude\worktrees\gh1377-mobile-mobile-companion-quick-action-triage"
            ),
            "f-src-buildmesh-claude-worktrees-gh1377-mobile-mobile-companion-quick-action-triage"
        );
        assert_eq!(commandcode_project_slug(r"C:\Users\User"), "c-users-user");
        assert_eq!(
            commandcode_project_slug("/home/user/project"),
            "home-user-project"
        );
        assert_eq!(
            commandcode_project_slug(
                r"F:\src\buildmesh\.claude\worktrees\gh1376-ui-design-system--surface-elevation-typogra"
            ),
            "f-src-buildmesh-claude-worktrees-gh1376-ui-design-system-surface-elevation-typogra"
        );
        assert_eq!(commandcode_project_slug(""), "");
        assert_eq!(commandcode_project_slug("///"), "");
    }

    #[test]
    fn commandcode_sessions_dir_resolves_under_projects() {
        let dir = commandcode_sessions_dir(
            EnvType::Windows,
            r"F:\src\buildmesh\.claude\worktrees\saucy-thunderous-cove",
        )
        .expect("windows sessions dir should resolve");
        let dir_str = dir.to_string_lossy().replace('\\', "/");
        assert!(
            dir_str.ends_with("projects/f-src-buildmesh-claude-worktrees-saucy-thunderous-cove"),
            "sessions dir should be projects/<slug>, got {dir_str}"
        );
        assert!(
            !dir_str.contains("sessions"),
            "must not use the legacy sessions dir, got {dir_str}"
        );
        assert!(
            commandcode_sessions_dir(EnvType::Windows, "").is_none(),
            "empty slug must not resolve"
        );
    }

    #[test]
    fn commandcode_transcript_path_uses_session_id_under_sessions_root() {
        // Issue #1500: the sessions root is the per-project
        // `projects/<encoded-cwd>/` dir; the pure locator just joins the id.
        let sessions_root = Path::new(
            r"C:\Users\adam\.commandcode\projects\f-src-buildmesh-claude-worktrees-saucy-thunderous-cove",
        );
        let path =
            commandcode_transcript_path_in(sessions_root, "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9");
        assert_eq!(path.parent(), Some(sessions_root));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("3fadada6-e0a3-44a2-ab68-ce1ecf7207a9.jsonl")
        );
    }

    #[test]
    fn commandcode_contract_parses_nested_messages_and_drops_internal_blocks_and_tool_results() {
        let tail = read_tail_from_file(
            &fixture("commandcode", "commandcode_transcript.jsonl"),
            10,
            TranscriptFormat::CommandCode,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!("Command Code fixture should parse to an available tail, got {tail:?}");
        };

        let roles: Vec<&str> = turns.iter().map(|turn| turn.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "assistant", "user", "assistant"],
            "ordinary tool_result echoes and non-message events should be skipped; turns: {turns:#?}"
        );
        assert_eq!(turns[0].text, "Inspect src/login.ts for the redirect bug.");
        assert_eq!(turns[1].text, "I'll inspect the file first.");
        assert_eq!(turns[1].tool_calls.len(), 1);
        assert_eq!(turns[1].tool_calls[0].name, "read_file");
        assert_eq!(turns[1].tool_calls[0].input["file_path"], "src/login.ts");
        assert_eq!(turns[2].text, "I have prepared the redirect fix.");
        assert_eq!(
            turns[2].tool_calls[0].input["diff"],
            "@@ -1 +1 @@\n-const redirect = nextUrl;\n+const redirect = new URL(nextUrl, window.location.origin);"
        );
        assert_eq!(
            turns[4].text,
            "The redirect now preserves the query string."
        );
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The redirect now preserves the query string.")
        );
    }

    #[test]
    fn commandcode_cheap_digest_reader_matches_full_reader() {
        let digest = read_last_assistant_message_from_file(
            &fixture("commandcode", "commandcode_transcript.jsonl"),
            TranscriptFormat::CommandCode,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = digest
        else {
            panic!("expected Command Code digest to be available");
        };
        assert!(turns.is_empty(), "cheap reader must not return turns");
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The redirect now preserves the query string.")
        );
    }

    #[test]
    fn commandcode_thinking_only_turn_is_skipped() {
        let lines = [
            r#"{"type":"message","id":"user-1","message":{"role":"user","content":"Inspect the redirect."}}"#,
            r#"{"type":"message","id":"assistant-1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"I am still inspecting the redirect."}]}}"#,
        ];
        let parsed = parse_commandcode_turns(lines.into_iter().map(str::to_string), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "Inspect the redirect.");
        assert_eq!(parsed.last_assistant_message, None);
    }

    #[test]
    fn commandcode_renamed_fields_degrade_to_shape_changed() {
        let cases = [
            r#"{"type":"message","id":"assistant-1","message":{"author":"assistant","content":[{"type":"text","text":"renamed"}]}}"#,
            r#"{"type":"message","id":"assistant-1","message":{"role":"assistant","blocks":[{"type":"text","text":"renamed"}]}}"#,
        ];

        for line in cases {
            let parsed = parse_commandcode_turns(std::iter::once(line.to_string()), 10);
            assert!(
                parsed.turns.is_empty(),
                "renamed shape should not produce turns"
            );
            assert!(
                parsed.saw_malformed,
                "renamed fields must be marked malformed"
            );
            assert_eq!(
                empty_or_shape_changed(parsed.saw_malformed),
                UnavailableReason::ShapeChanged
            );
        }
    }

    #[test]
    fn commandcode_non_message_stream_degrades_to_empty() {
        let path = write_fixture(
            "commandcode_empty",
            r#"{"type":"session","id":"sess-commandcode-empty"}
{"type":"model_change","model":"commandcode-default"}
{"type":"telemetry","event":"heartbeat"}
"#,
        );
        let tail = read_tail_from_file(&path, 10, TranscriptFormat::CommandCode);
        std::fs::remove_file(path).ok();

        assert_eq!(
            tail,
            TranscriptTail::unavailable(UnavailableReason::Empty),
            "metadata-only streams are quiet, not malformed"
        );
    }

    #[test]
    fn commandcode_rolling_buffer_evicts_old_turns_but_keeps_digest() {
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!(
                r#"{{"type":"message","id":"user-{i}","message":{{"role":"user","content":"prompt {i}"}}}}"#
            ));
            lines.push(format!(
                r#"{{"type":"message","id":"assistant-{i}","message":{{"role":"assistant","content":[{{"type":"text","text":"reply {i}"}}]}}}}"#
            ));
        }

        let parsed = parse_commandcode_turns(lines.into_iter(), 3);
        assert_eq!(parsed.turns.len(), 3, "the rolling buffer must honor keep");
        assert_eq!(
            parsed.turns.last().map(|turn| turn.text.as_str()),
            Some("reply 49")
        );
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("reply 49"));
    }

    #[test]
    fn commandcode_tool_call_input_truncates_large_string_leaves() {
        let big = "x".repeat(100 * 1024);
        let line = serde_json::json!({
            "type": "message",
            "id": "assistant-big-input",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "name": "write_file",
                    "input": {"path": "src/main.rs", "content": big}
                }]
            }
        })
        .to_string();

        let parsed = parse_commandcode_turns(std::iter::once(line), 10);
        let content = parsed.turns[0].tool_calls[0].input["content"]
            .as_str()
            .expect("tool input content should remain a string");
        assert!(content.ends_with('…'));
        assert!(content.chars().count() <= MAX_TOOL_STRING + 1);
    }

    #[test]
    fn commandcode_whitespace_tool_turn_does_not_update_digest() {
        let line = serde_json::json!({
            "type": "message",
            "id": "assistant-whitespace",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "   "},
                    {"type": "tool_use", "name": "Read", "input": {}}
                ]
            }
        })
        .to_string();

        let parsed = parse_commandcode_turns(std::iter::once(line), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.last_assistant_message, None);
    }
}

#[cfg(test)]
mod file_contract_tests {
    use super::*;
    use crate::services::transcript_reader::test_support::{assert_jsonl_contract, fixture};

    #[test]
    fn reader_handles_tail_digest_malformed_empty_shape_changed_and_unreadable_files() {
        assert_jsonl_contract(
            &CommandCodeAdapter,
            &fixture("commandcode", "commandcode_transcript.jsonl"),
            &fixture("commandcode", "shape_changed.jsonl"),
            "The redirect now preserves the query string.",
        );
    }
}
