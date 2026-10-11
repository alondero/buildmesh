//! Naming consumes dialogue, never the terminal rendering of that dialogue.
use crate::models::AgentNode;
use crate::services::transcript_reader::{self, TranscriptTail};

pub(super) fn read(node: &AgentNode) -> Option<String> {
    let format = crate::coordinator::enrichment::native_completion_format(node)?;
    let path = crate::env::node_working_path(node).spawn_path;
    let tail =
        transcript_reader::read_naming_dialogue(format, node.cli_session_id.as_deref(), &path);
    from_tail(tail)
}

pub(super) fn from_tail(tail: TranscriptTail) -> Option<String> {
    let TranscriptTail::Available { turns, .. } = tail else {
        return None;
    };
    // Anchor the name in an actual request. Tool-only turns and later status
    // chatter must not displace it or become a task when no request is known.
    let start = turns
        .iter()
        .position(|turn| turn.role == "user" && !turn.text.trim().is_empty())?;
    let mut messages = Vec::new();
    for turn in turns
        .iter()
        .skip(start)
        .filter(|turn| !turn.text.trim().is_empty())
        .take(4)
    {
        messages.push(serde_json::json!({
            "role": turn.role,
            "text": crate::secret_scrubber::SecretScrubber::scrub(&turn.text),
        }));
    }
    serde_json::to_string(&messages).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use transcript_reader::TranscriptFormat;

    fn read_fixture(format: TranscriptFormat, lines: Vec<serde_json::Value>) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        from_tail(transcript_reader::read_naming_from_file(
            &path, "session", format,
        ))
        .unwrap()
    }

    #[test]
    fn codex_naming_uses_request_not_agents_injection_or_tool_output() {
        let message = |role: &str, text: &str| {
            json!({"type":"response_item", "payload":{
                "type":"message", "role":role, "content":[{"type":"input_text","text":text}]
            }})
        };
        let text = read_fixture(TranscriptFormat::Codex, vec![
            message("user", "# AGENTS.md instructions for /repo\n<INSTRUCTIONS>\nBuildmesh attention callback instructions\n</INSTRUCTIONS>"),
            message("user", "Find the session about Bitwarden access"),
            json!({"type":"response_item","payload":{"type":"function_call_output","output":"Hook failed: buildmesh attention callback"}}),
            message("assistant", "I found the Bitwarden session."),
        ]);
        let actual: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            actual,
            json!([
                {"role":"user","text":"Find the session about Bitwarden access"},
                {"role":"assistant","text":"I found the Bitwarden session."}
            ])
        );
    }

    #[test]
    fn mcode_naming_strips_reminders_before_text_truncation() {
        let text = read_fixture(
            TranscriptFormat::Mcode,
            vec![
                json!({"message":{"role":"user","content":[{"type":"text","text":format!("<system-reminder>\n{}\n</system-reminder>\n\nHelp set up my first Grok bot", "agent instructions ".repeat(500))}]}}),
                json!({"message":{"role":"toolResult","content":[{"type":"text","text":"Loading spinner session"}]}}),
                json!({"message":{"role":"assistant","content":[{"type":"thinking","thinking":"private reasoning"},{"type":"text","text":"I will check Grok access."}]}}),
            ],
        );
        let actual: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            actual,
            json!([
                {"role":"user","text":"Help set up my first Grok bot"},
                {"role":"assistant","text":"I will check Grok access."}
            ])
        );
    }

    #[test]
    fn naming_readers_preserve_initial_requests_across_formats() {
        use transcript_reader::test_support::fixture;
        let cases = [
            (
                TranscriptFormat::ClaudeCode,
                "claude_code",
                "claude_code_transcript.jsonl",
                "Fix the login redirect bug",
            ),
            (
                TranscriptFormat::Codex,
                "codex",
                "codex_rollout_transcript.jsonl",
                "Search the directory for TypeScript files.",
            ),
            (
                TranscriptFormat::CommandCode,
                "commandcode",
                "commandcode_transcript.jsonl",
                "Inspect src/login.ts for the redirect bug.",
            ),
            (
                TranscriptFormat::Agy,
                "agy",
                "agy_transcript.jsonl",
                "Inspect src/login.ts for the redirect bug.",
            ),
            (
                TranscriptFormat::Grok,
                "grok",
                "grok_chat_history.jsonl",
                "Fix the login redirect bug",
            ),
            (
                TranscriptFormat::Muse,
                "muse",
                "muse_transcript.jsonl",
                "<REDACTED>",
            ),
            (
                TranscriptFormat::Cline,
                "cline",
                "cline_messages.json",
                "Fix the failing login test",
            ),
            (
                TranscriptFormat::Cursor,
                "claude_code",
                "claude_code_transcript.jsonl",
                "Fix the login redirect bug",
            ),
        ];
        for (format, dir, file, expected) in cases {
            let text = from_tail(transcript_reader::read_naming_from_file(
                &fixture(dir, file),
                "session",
                format,
            ))
            .unwrap();
            let messages: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                messages[0],
                json!({"role":"user","text":expected}),
                "{format:?}"
            );
            assert!(messages.as_array().unwrap().len() <= 4);
            assert!(!text.contains("tool_calls"), "{format:?}");
        }
    }

    #[test]
    fn tool_heavy_session_keeps_request_and_assistant_context() {
        let mut lines = vec![
            json!({"type":"user","message":{"role":"user","content":"Fix the attention callback error"}}),
        ];
        for n in 0..500 {
            lines.push(json!({"type":"assistant","message":{"id":format!("msg-{n}"),"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"path":"irrelevant.rs"}}]}}));
        }
        lines.push(json!({"type":"assistant","message":{"id":"answer","role":"assistant","content":[{"type":"text","text":"The hook command uses the wrong shell."}]}}));
        let text = read_fixture(TranscriptFormat::ClaudeCode, lines);
        let messages: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            messages,
            json!([
                {"role":"user","text":"Fix the attention callback error"},
                {"role":"assistant","text":"The hook command uses the wrong shell."}
            ])
        );
    }

    #[test]
    fn split_assistant_message_does_not_displace_user_clarification() {
        let mut lines = vec![json!({"type":"user","message":{"role":"user","content":"Continue"}})];
        for text in ["Which task?", "Login", "or", "the callback?"] {
            lines.push(json!({"type":"assistant","message":{"id":"same-message","role":"assistant","content":[{"type":"text","text":text}]}}));
        }
        lines
            .push(json!({"type":"user","message":{"role":"user","content":"Fix login redirects"}}));
        let text = read_fixture(TranscriptFormat::ClaudeCode, lines);
        let messages: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(messages.as_array().unwrap().len(), 3);
        assert_eq!(messages[1]["text"], "Which task?\nLogin\nor\nthe callback?");
        assert_eq!(
            messages[2],
            json!({"role":"user","text":"Fix login redirects"})
        );
    }

    #[test]
    fn naming_payload_is_bounded_unicode_dialogue_without_secrets() {
        let lines = (0..6).map(|n| json!({"type":"user","message":{"role":"user","content":format!("Task {n}: {}", "日本語".repeat(2000))}})).collect();
        let text = read_fixture(TranscriptFormat::ClaudeCode, lines);
        let messages: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(messages.as_array().unwrap().len(), 4);
        for message in messages.as_array().unwrap() {
            let text = message["text"].as_str().unwrap();
            // Shared preview truncation appends the three-byte ellipsis.
            assert!(text.len() <= transcript_reader::types::MAX_TURN_TEXT + 3);
            assert!(text.ends_with('…'));
        }
        let text = read_fixture(
            TranscriptFormat::ClaudeCode,
            vec![
                json!({"type":"user","message":{"role":"user","content":"Fix authentication: Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789"}}),
            ],
        );
        assert!(!text.contains("abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(text.contains("Fix authentication"));
    }

    #[test]
    fn byte_cap_inside_multibyte_tool_record_preserves_initial_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let user =
            json!({"type":"user","message":{"role":"user","content":"Fix login"}}).to_string();
        let mut bytes = format!("{user}\n{{\"type\":\"tool\",\"output\":\"").into_bytes();
        bytes.resize(1024 * 1024 - 1, b'x');
        bytes.extend_from_slice("日本語\"}\n".as_bytes());
        std::fs::write(&path, bytes).unwrap();
        let text = from_tail(transcript_reader::read_naming_from_file(
            &path,
            "session",
            TranscriptFormat::ClaudeCode,
        ))
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            json!([{"role":"user","text":"Fix login"}])
        );
    }

    #[tokio::test]
    #[ignore = "requires an explicitly selected local transcript and authenticated naming harness"]
    async fn live_transcript_naming() {
        let path = std::env::var("BUILDMESH_NAMING_TRANSCRIPT").unwrap();
        let harness = std::env::var("BUILDMESH_NAMING_FORMAT").unwrap();
        let expected = std::env::var("BUILDMESH_NAMING_EXPECTED").unwrap();
        let format = TranscriptFormat::for_harness(&harness).unwrap();
        let dialogue = from_tail(transcript_reader::read_naming_from_file(
            std::path::Path::new(&path),
            "session",
            format,
        ))
        .unwrap();
        let backend = std::env::var("BUILDMESH_BACKGROUND_HARNESS").unwrap();
        let prefs = crate::preferences::AppPreferences::default();
        let plan = crate::preferences::launch_configurations::capture(
            &prefs,
            &backend,
            &crate::preferences::launch_configurations::LaunchOverrides::default(),
        )
        .unwrap();
        let launch = super::super::engine::naming_backend_env_from_plan(plan, &prefs).unwrap();
        let slug = super::super::engine::summarize_and_rename_with(0, &dialogue, launch)
            .await
            .unwrap();
        println!("Live naming result: {slug}");
        assert!(
            slug.contains(&expected),
            "expected task topic {expected}, got {slug}"
        );
    }

    #[test]
    fn opencode_naming_reads_first_messages_of_the_selected_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);").unwrap();
        for (id, session, text) in [
            ("first", "ses_task", "Fix login"),
            ("other", "ses_other", "Unrelated task"),
        ] {
            conn.execute(
                "INSERT INTO message VALUES (?1, ?2, 0, ?3)",
                rusqlite::params![id, session, json!({"role":"user"}).to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part VALUES (?1, ?1, ?2, ?3)",
                rusqlite::params![id, session, json!({"type":"text","text":text}).to_string()],
            )
            .unwrap();
        }
        for n in 1..250 {
            let id = format!("later-{n}");
            conn.execute(
                "INSERT INTO message VALUES (?1, 'ses_task', ?2, ?3)",
                rusqlite::params![id, n, json!({"role":"assistant"}).to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part VALUES (?1, ?1, 'ses_task', ?2)",
                rusqlite::params![
                    id,
                    json!({"type":"text","text":"Working on login"}).to_string()
                ],
            )
            .unwrap();
        }
        let text = from_tail(transcript_reader::read_naming_from_file(
            &path,
            "ses_task",
            TranscriptFormat::OpenCode,
        ))
        .unwrap();
        let messages: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(messages[0], json!({"role":"user","text":"Fix login"}));
        assert_eq!(messages.as_array().unwrap().len(), 4);
        assert!(!text.contains("Unrelated"));
    }

    #[test]
    fn unavailable_or_assistant_only_conversation_defers_naming() {
        use transcript_reader::{Turn, UnavailableReason};
        for tail in [
            TranscriptTail::unavailable(UnavailableReason::NoSession),
            TranscriptTail::unavailable(UnavailableReason::Unsupported),
            TranscriptTail::Available {
                turns: vec![],
                last_assistant_message: None,
            },
            TranscriptTail::Available {
                turns: vec![Turn {
                    role: "assistant".into(),
                    text: "Loading spinner".into(),
                    tool_calls: vec![],
                }],
                last_assistant_message: None,
            },
        ] {
            assert!(from_tail(tail).is_none());
        }
    }

    #[test]
    fn mcode_reminder_only_and_partial_reminders_are_not_requests() {
        use transcript_reader::adapter::dispatch;
        for text in [
            "<system-reminder>internal</system-reminder>",
            "<system-reminder>partial",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("messages.jsonl");
            std::fs::write(
                &path,
                json!({"message":{"role":"user","content":text}}).to_string(),
            )
            .unwrap();
            assert!(dispatch("mcode")
                .unwrap()
                .naming_dialogue(&path, "session")
                .unwrap()
                .is_empty());
        }
    }
}
