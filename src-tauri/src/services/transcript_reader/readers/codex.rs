//! Codex transcript adapter (issue #885, #887).
//!
//! Codex writes `rollout-<timestamp>-<session-id>.jsonl` files under
//! `~/.codex/sessions/YYYY/MM/DD/`. Lines are `{"type": <envelope>,
//! "payload": {...}}` envelopes — `parse_codex_turns` extracts the
//! `message`, `function_call`, and `function_call_output` payload kinds.
//!
//! Issue #1661 step 6: Codex is the **fourth harness migrated end-to-end**.
//! `find_codex_rollout` + the pure walk + `parse_codex_turns` +
//! `is_codex_synthetic` + `codex_concat_text` + `codex_tool_input` all
//! live in this file. The capture poller in `services::codex_session`
//! independently walks the same rollout tree (its `rollout_days_newest_first`);
//! step 6 only collapses the reader's copy — the poller's directory→id
//! mapping is its own concern and stays in `codex_session.rs`.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use crate::env;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, merge_into_with_text_limit, push_bounded, truncate, truncate_json_strings,
    Parsed, ToolCall, Turn, MAX_TOOL_STRING,
};

/// Drop-in [`TranscriptAdapter`] for Codex.
pub(crate) struct CodexAdapter;

impl TranscriptAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        // Session ids are global within the selected runtime home.
        let runtime = env::runtime_for_spawn_path(ctx.node_path);
        let home = env::codex_dir_for_env(runtime, ctx.node_path)?;
        let home = PathBuf::from(env::to_host_path_for_runtime(
            &home.to_string_lossy(),
            runtime,
        ));
        find_codex_rollout_in(&home.join("sessions"), ctx.session_id)
    }

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        parse_codex_turns_with_text_limit(lines, keep, max_text)
    }

    fn completed_turn(&self, lines: &str) -> Option<super::super::NativeTurnCompletion> {
        let mut completion = None;
        let mut started_turn = None;
        for line in lines.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                // Damage before a later complete turn must not poison the
                // whole window. Damage after completion may conceal new work.
                completion = None;
                started_turn = None;
                continue;
            };
            let payload = &value["payload"];
            match (value["type"].as_str(), payload["type"].as_str()) {
                (Some("event_msg"), Some("task_started")) => {
                    started_turn = payload["turn_id"].as_str().map(str::to_owned);
                    completion = None;
                }
                (Some("event_msg"), Some("task_complete")) => {
                    let turn_id = payload["turn_id"].as_str().filter(|id| {
                        !id.is_empty()
                            && started_turn.as_deref().is_none_or(|started| started == *id)
                    });
                    let completed_at_ms = value["timestamp"]
                        .as_str()
                        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
                        .map(|timestamp| timestamp.timestamp_millis());
                    completion = turn_id
                        .zip(completed_at_ms)
                        .map(
                            |(turn_id, completed_at_ms)| super::super::NativeTurnCompletion {
                                turn_id: turn_id.into(),
                                completed_at_ms,
                                final_report: payload["last_agent_message"]
                                    .as_str()
                                    .filter(|text| !text.trim().is_empty())
                                    .map(crate::secret_scrubber::SecretScrubber::scrub),
                            },
                        );
                }
                (Some("event_msg"), Some("token_count")) | (Some("token_usage_record"), _) => {}
                // User input, tool activity, aborts, and unknown records after
                // completion invalidate it. A final-looking message alone is
                // insufficient: Codex can continue working after commentary.
                _ => completion = None,
            }
        }
        completion
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        matches!(
            value.get("type").and_then(|kind| kind.as_str()),
            Some("response_item") | Some("event_msg")
        ) && value.get("payload").is_some_and(|payload| {
            payload.get("type").and_then(|kind| kind.as_str()) == Some("message")
                && payload.get("role").and_then(|role| role.as_str()) == Some("assistant")
                && !codex_concat_text(payload.get("content").unwrap_or(&serde_json::Value::Null))
                    .trim()
                    .is_empty()
        })
    }
}

/// Locate a Codex rollout file `rollout-<timestamp>-<session_id>.jsonl` under
/// `<codex home>/sessions/YYYY/MM/DD/`. Codex cannot relocate its sessions
/// dir per-project (issue #885), so the global one is walked — fixed depth
/// 3, at most a few hundred day dirs, <10ms cold.
/// Walk an explicit runtime sessions root. Tests use temporary directories.
/// Walks newest-first (years, months, days each
/// sorted descending) so the common case — a recent session — terminates
/// after a handful of dirs.
pub(crate) fn find_codex_rollout_in(sessions_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let suffix = format!("-{session_id}.jsonl");
    for year in subdirs_sorted_desc(sessions_dir) {
        for month in subdirs_sorted_desc(&year) {
            for day in subdirs_sorted_desc(&month) {
                let Ok(entries) = fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let matches = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(&suffix));
                    if matches {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

/// Immediate subdirectories of `dir`, sorted by name descending. Date-named
/// dirs (`2026`, `07`, `18`) sort chronologically, so descending = newest
/// first.
fn subdirs_sorted_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    dirs
}

/// True for Codex's injected context messages (`<user_instructions>` /
/// `<environment_context>` wrappers) — session plumbing, not genuine user
/// turns, mirroring [`super::super::is_synthetic_message`] for Claude Code.
fn is_codex_synthetic(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<user_instructions>") || t.starts_with("<environment_context>")
}

/// Pull the text out of a Codex message `content` array. Codex types its
/// blocks `input_text` (user) / `output_text` (assistant); accept both
/// plus a plain `text` for defensive breadth. Multiple blocks join with
/// newlines.
pub(crate) fn codex_concat_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|b| {
                matches!(
                    b.get("type").and_then(|t| t.as_str()),
                    Some("input_text") | Some("output_text") | Some("text")
                )
            })
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A Codex `function_call`'s `arguments` is either a JSON object or a
/// string-encoded JSON blob (the OpenAI wire form). Decode the string
/// form so the Coordinator sees the input's structure, not an escaped
/// blob; a string that isn't valid JSON is delivered as-is.
fn codex_tool_input(arguments: Option<&serde_json::Value>) -> serde_json::Value {
    match arguments {
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.clone()))
        }
        Some(v) => v.clone(),
        None => serde_json::Value::Null,
    }
}

/// Parse Codex rollout JSONL lines into logical turns, honouring the same
/// [`Parsed`] contract as [`super::super::parse_turns`]: rolling
/// `keep`-bounded turn window, whole-stream last-assistant-message
/// tracking, and a malformed flag so a Codex format drift degrades
/// loudly as `ShapeChanged`, never as a quiet `Empty`. Dispatches on
/// the *payload* type rather than the envelope type (`response_item`
/// vs `event_msg`) — Codex has carried `function_call` under both across
/// versions.
#[cfg(test)]
pub(crate) fn parse_codex_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    parse_codex_turns_with_text_limit(lines, keep, super::super::types::MAX_TURN_TEXT)
}

pub(crate) fn parse_codex_turns_with_text_limit(
    lines: impl Iterator<Item = String>,
    keep: usize,
    max_text: usize,
) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
    // The trailing turn is an assistant turn opened by function_call events;
    // the turn's closing assistant message merges into it.
    let mut assistant_open = false;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let outer = val.get("type").and_then(|t| t.as_str());
        if outer != Some("response_item") && outer != Some("event_msg") {
            // session_meta, turn_context, compaction markers, … — not turns.
            continue;
        }
        let Some(payload) = val.get("payload") else {
            saw_malformed = true;
            continue;
        };
        match payload.get("type").and_then(|t| t.as_str()) {
            Some("message") => {
                let role = payload.get("role").and_then(|r| r.as_str());
                // Codex emits context updates during model/mode switches.
                // They are not dialogue, and must not poison a later report.
                if matches!(role, Some("system" | "developer")) {
                    continue;
                }
                if role != Some("user") && role != Some("assistant") {
                    saw_malformed = true;
                    continue;
                }
                let Some(content) = payload.get("content") else {
                    saw_malformed = true;
                    continue;
                };
                let text = codex_concat_text(content);
                if role == Some("user") {
                    assistant_open = false;
                    if is_codex_synthetic(&text) || text.trim().is_empty() {
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
                } else {
                    if text.trim().is_empty() {
                        assistant_open = false;
                        continue;
                    }
                    if assistant_open {
                        if let Some(last) = turns.back_mut() {
                            merge_into_with_text_limit(last, &text, Vec::new(), max_text);
                            if !last.text.is_empty() {
                                last_assistant_message = Some(last.text.clone());
                            }
                        }
                    } else {
                        let turn = Turn {
                            role: "assistant".to_string(),
                            text: truncate(&text, max_text),
                            tool_calls: Vec::new(),
                        };
                        last_assistant_message = Some(turn.text.clone());
                        push_bounded(&mut turns, turn, keep);
                    }
                    // The assistant's text message closes the turn; later
                    // function_calls belong to the next one.
                    assistant_open = false;
                }
            }
            Some("function_call") => {
                let name = payload
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let call = ToolCall {
                    name,
                    input: truncate_json_strings(
                        codex_tool_input(payload.get("arguments")),
                        MAX_TOOL_STRING,
                    ),
                };
                if assistant_open {
                    if let Some(last) = turns.back_mut() {
                        last.tool_calls.push(call);
                        cap_tool_calls(&mut last.tool_calls);
                    }
                } else {
                    push_bounded(
                        &mut turns,
                        Turn {
                            role: "assistant".to_string(),
                            text: String::new(),
                            tool_calls: vec![call],
                        },
                        keep,
                    );
                    assistant_open = true;
                }
            }
            // function_call_output (tool results), reasoning, token_count, …
            // — deliberately skipped, like Claude's tool_result echoes.
            _ => {}
        }
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
    use crate::services::transcript_reader::test_support::fixture;
    use crate::services::transcript_reader::types::empty_or_shape_changed;
    use crate::services::transcript_reader::{
        assistant_report_from_file, read_assistant_report, read_last_assistant_message_from_file,
        read_tail_from_file, TranscriptFormat, TranscriptTail, UnavailableReason,
    };
    use std::fs;

    #[test]
    fn circuit_recovers_a_delayed_codex_parent_report_without_a_new_pty_turn() {
        let root = tempfile::TempDir::new().unwrap();
        let directory = "F:/repo/.claude/worktrees/source";
        let anchor = 1_788_701_729_172;
        let id = "01a076ee-6c95-7c82-9e5f-928e9f43ad7a";
        let recover = || {
            crate::services::codex_session::find_historic_id_for_directory_in(
                root.path(),
                directory,
                anchor,
                true,
            )
        };
        assert!(
            recover().is_none(),
            "initial startup capture sees no rollout yet"
        );
        assert!(read_assistant_report(TranscriptFormat::Codex, None, directory).is_none());
        let day = root.path().join("2026/09/06");
        fs::create_dir_all(&day).unwrap();
        let path = day.join(format!("rollout-{id}.jsonl"));
        let meta = serde_json::json!({"type":"session_meta", "payload":{
            "id":id, "cwd":directory, "timestamp":"2026-09-06T13:35:32.115Z", "source":"cli", "thread_source":"user"}});
        let report = serde_json::json!({"type":"response_item", "payload":{
            "type":"message", "role":"assistant", "phase":"final_answer", "content":[{"type":"output_text", "text":"Implementation and verification finished; changes are uncommitted."}]}});
        fs::write(&path, format!("{meta}\n{report}\n")).unwrap();
        let child = serde_json::json!({"type":"session_meta", "payload":{
            "id":"01a076f5-cae9-7db2-b159-7c4682cd2b7f", "cwd":directory,
            "timestamp":"2026-09-06T13:36:32.115Z", "source":{"subagent":{}}}});
        fs::write(day.join("child.jsonl"), format!("{child}\n")).unwrap();
        assert_eq!(recover().as_deref(), Some(id));
        let actual = assistant_report_from_file(&path, TranscriptFormat::Codex).unwrap();
        assert_eq!(
            actual.text,
            "Implementation and verification finished; changes are uncommitted."
        );
        assert!(!actual.revision.is_empty());
    }

    #[test]
    fn codex_contract_parses_tail_and_last_assistant_message() {
        let tail = read_tail_from_file(
            &fixture("codex", "codex_rollout_transcript.jsonl"),
            10,
            TranscriptFormat::Codex,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = tail
        else {
            panic!("fixture should parse to an available tail, got {tail:?}");
        };
        let roles: Vec<&str> = turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "user", "assistant"],
            "turns: {turns:#?}"
        );
        assert_eq!(turns[0].text, "Search the directory for TypeScript files.");
        // The shell function_call opens the assistant turn; the closing
        // assistant message merges into it.
        assert_eq!(turns[1].tool_calls.len(), 1);
        assert_eq!(turns[1].tool_calls[0].name, "shell");
        assert_eq!(turns[1].tool_calls[0].input["command"], "dir /s /b *.ts");
        assert!(turns[1]
            .text
            .starts_with("I found the following TypeScript files"));
        // call_02's arguments are a string-encoded JSON blob — decoded to
        // structure, not delivered as an escaped string.
        assert_eq!(turns[3].tool_calls[0].name, "read_file");
        assert_eq!(turns[3].tool_calls[0].input["file_path"], "src/login.ts");
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The file looks good. I don't see any bugs.")
        );
    }

    #[test]
    fn codex_cheap_digest_reader_matches_full_reader() {
        let cheap = read_last_assistant_message_from_file(
            &fixture("codex", "codex_rollout_transcript.jsonl"),
            TranscriptFormat::Codex,
        );
        let TranscriptTail::Available {
            turns,
            last_assistant_message,
        } = cheap
        else {
            panic!("expected available, got {cheap:?}");
        };
        assert!(turns.is_empty(), "cheap reader must not return turns");
        assert_eq!(
            last_assistant_message.as_deref(),
            Some("The file looks good. I don't see any bugs.")
        );
    }

    #[test]
    fn codex_context_only_session_degrades_to_empty() {
        let lines = vec![
            r#"{"type":"session_meta","payload":{"id":"x","cwd":"F:\\src","timestamp":"t","cli_version":"0.144.0"}}"#.to_string(),
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_instructions>\nuse tabs\n</user_instructions>"}]}}"#.to_string(),
        ];
        let parsed = parse_codex_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(!parsed.saw_malformed);
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::Empty
        );
    }

    #[test]
    fn codex_renamed_fields_degrade_to_shape_changed() {
        let lines = vec![
            r#"{"type":"response_item","payload":{"type":"message","author":"assistant","blocks":[{"type":"output_text","text":"renamed"}]}}"#.to_string(),
        ];
        let parsed = parse_codex_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(parsed.saw_malformed, "renamed role/content is malformed");
        assert_eq!(
            empty_or_shape_changed(parsed.saw_malformed),
            UnavailableReason::ShapeChanged
        );
    }

    #[test]
    fn codex_function_calls_and_closing_message_form_one_turn() {
        let lines = vec![
            r#"{"type":"event_msg","payload":{"type":"function_call","name":"shell","arguments":{"command":"ls"},"call_id":"c1"}}"#.to_string(),
            r#"{"type":"event_msg","payload":{"type":"function_call","name":"read_file","arguments":{"file_path":"a.rs"},"call_id":"c2"}}"#.to_string(),
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#.to_string(),
        ];
        let parsed = parse_codex_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1, "turns: {:#?}", parsed.turns);
        assert_eq!(parsed.turns[0].tool_calls.len(), 2);
        assert_eq!(parsed.turns[0].text, "Done.");
        assert_eq!(parsed.last_assistant_message.as_deref(), Some("Done."));
    }

    #[test]
    fn codex_function_call_under_response_item_envelope_parses() {
        let lines = vec![
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":{"command":"ls"},"call_id":"c1"}}"#.to_string(),
        ];
        let parsed = parse_codex_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].tool_calls[0].name, "shell");
    }

    #[test]
    fn codex_rollout_walk_finds_session_file() {
        let temp = std::env::temp_dir().join(format!(
            "buildmesh_test_codex_sessions_{}",
            std::process::id()
        ));
        let day = temp.join("2026").join("07").join("18");
        std::fs::create_dir_all(&day).unwrap();
        let file =
            day.join("rollout-2026-07-18T10-00-00-c1234567-89ab-cdef-0123-456789abcdef.jsonl");
        std::fs::write(&file, "{}").unwrap();

        let found = find_codex_rollout_in(&temp, "c1234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(found.as_deref(), Some(file.as_path()));
        assert!(
            find_codex_rollout_in(&temp, "00000000-dead-beef-0000-000000000000").is_none(),
            "unknown session id must not match"
        );
        assert!(
            find_codex_rollout_in(&temp.join("nope"), "x").is_none(),
            "missing sessions dir degrades to None, not an error"
        );
        std::fs::remove_dir_all(&temp).ok();
    }
}

#[cfg(test)]
mod native_completion_tests {
    use crate::services::transcript_reader::native_completion::{
        native_turn_completion_from_file, native_turn_snapshot_from_file,
    };
    use crate::services::transcript_reader::{NativeTurnCompletion, TranscriptFormat};
    use std::fs;

    #[test]
    fn circuit_reconciliation_rejects_transcript_activity_between_observation_and_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let completed = serde_json::json!({"timestamp":"2026-09-24T14:24:36.861Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn"}}).to_string() + "\n";
        fs::write(&path, &completed).unwrap();
        let snapshot = native_turn_snapshot_from_file(&path, TranscriptFormat::Codex).unwrap();
        assert!(snapshot.is_current());
        let guard = crate::circuit::stepper::ObservationInputFence {
            transcript_guard: Some(snapshot),
            report_guard: None,
            agent_node_id: 9,
            input_stamp: "input".into(),
            observed_at_ms: 1,
            session_id: "session".into(),
            session_incarnation: "incarnation".into(),
        };
        fs::write(&path,completed + &serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"next"}}).to_string() + "\n").unwrap();
        let error = crate::db::circuit::evidence::commit_transition(
            0,
            None,
            "{}",
            &[],
            crate::db::circuit::evidence::EvidenceWrite {
                input_guard: Some(&guard),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("Native transcript changed before evidence commit"));
    }

    #[test]
    fn circuit_codex_native_final_report_is_complete_and_secret_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let body = format!(
            "{}\nCredential ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\nFINAL FINDING",
            "Synthetic review finding.\n".repeat(1500)
        );
        let records = [
            serde_json::json!({"ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"turn"}}),
            serde_json::json!({"ordinal":31,"timestamp":"2026-09-24T14:24:36.861Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn","last_agent_message":body}}),
        ];
        fs::write(&path, format!("{}\n{}\n", records[0], records[1])).unwrap();
        let report = native_turn_completion_from_file(&path, TranscriptFormat::Codex)
            .unwrap()
            .final_report
            .unwrap();
        assert_eq!(report, crate::secret_scrubber::SecretScrubber::scrub(&body));
        assert!(report.ends_with("FINAL FINDING"));
        assert!(!report.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"));
    }

    #[test]
    fn codex_completion_replays_missed_hook_and_rejects_later_activity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        // Envelope shape captured from the stalled run 122; no task prose is
        // needed to establish the native lifecycle transition.
        let completed = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn-1\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"phase\":\"final_answer\",\"content\":[]}}\n",
            "{\"timestamp\":\"2026-09-13T18:27:33.252Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\"}}\n",
        );
        fs::write(&path, completed).unwrap();
        assert_eq!(
            native_turn_completion_from_file(&path, TranscriptFormat::Codex),
            Some(NativeTurnCompletion {
                turn_id: "turn-1".into(),
                completed_at_ms: 1789324053252,
                final_report: None,
            })
        );
        for noise in ["\n \t\n", "old damaged record\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"old\"},\"timestamp\":\"bad\"}\n"] {
            fs::write(&path, format!("{noise}{completed}\n \t")).unwrap();
            assert!(native_turn_completion_from_file(&path, TranscriptFormat::Codex).is_some(),
                "historical damage and blank lines must not discard a later explicit completion");
        }
        for suffix in [
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn-2\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\"}}\n",
            "{\"type\":\"event_msg\"",
            "malformed\n",
        ] {
            fs::write(&path, format!("{completed}{suffix}")).unwrap();
            assert_eq!(native_turn_completion_from_file(&path, TranscriptFormat::Codex), None, "{suffix}");
        }
        fs::write(&path, completed.replace("task_complete", "item_completed")).unwrap();
        assert_eq!(
            native_turn_completion_from_file(&path, TranscriptFormat::Codex),
            None
        );
        fs::write(&path, completed.replacen("turn-1", "other-turn", 1)).unwrap();
        assert_eq!(
            native_turn_completion_from_file(&path, TranscriptFormat::Codex),
            None
        );
        fs::write(
            &path,
            completed.replace("2026-09-13T18:27:33.252Z", "invalid"),
        )
        .unwrap();
        assert_eq!(
            native_turn_completion_from_file(&path, TranscriptFormat::Codex),
            None
        );
    }

    #[test]
    fn native_completion_bounds_large_rollouts_and_ignores_unsupported_formats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let tail = "{\"timestamp\":\"2026-09-13T18:27:33.252Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-1\"}}\n";
        fs::write(&path, format!("{}\n{tail}", "é".repeat(200_000))).unwrap();
        assert_eq!(
            native_turn_completion_from_file(&path, TranscriptFormat::Codex)
                .unwrap()
                .turn_id,
            "turn-1"
        );
        assert_eq!(
            native_turn_completion_from_file(&path, TranscriptFormat::ClaudeCode),
            None
        );
        let snapshot = native_turn_snapshot_from_file(&path, TranscriptFormat::Codex).unwrap();
        assert!(snapshot.is_current());
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(
                fs::metadata(&path).unwrap().modified().unwrap()
                    + std::time::Duration::from_secs(1),
            ))
            .unwrap();
        assert!(
            !snapshot.is_current(),
            "same-length rewrites must invalidate the observation too"
        );
        let snapshot = native_turn_snapshot_from_file(&path, TranscriptFormat::Codex).unwrap();
        fs::write(
            &path,
            format!(
                "{tail}{{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call\"}}}}\n"
            ),
        )
        .unwrap();
        assert!(
            !snapshot.is_current(),
            "new transcript activity invalidates the observation without reparsing it"
        );
        assert!(native_turn_snapshot_from_file(&path, TranscriptFormat::Codex).is_none());
        fs::remove_file(&path).unwrap();
        assert!(!snapshot.is_current());
    }
}

#[cfg(test)]
mod file_contract_tests {
    use super::*;
    use crate::services::transcript_reader::test_support::{assert_jsonl_contract, fixture};

    #[test]
    fn reader_handles_tail_digest_malformed_empty_shape_changed_and_unreadable_files() {
        assert_jsonl_contract(
            &CodexAdapter,
            &fixture("codex", "codex_rollout_transcript.jsonl"),
            &fixture("codex", "shape_changed.jsonl"),
            "The file looks good. I don't see any bugs.",
        );
    }
}
