//! MiniMax Code transcript adapter.
//!
//! `mcode` (shipped as `@minimax-ai/code`, verified against 0.4.12)
//! persists canonical history under
//! `<dataDir>/v2/sessions/<YYYY>/<MM>/<DD>/<HH-MM-SS-mmm>-session_<base64url(sessionId)>/`
//! carrying `manifest.json` (schema v1: `sessionId`, `createdAtMs`, …) plus
//! `messages.jsonl` — one record per line shaped
//! `{message_id, turn_id, message: {role, timestamp, content}}` where `role`
//! is `user` | `assistant` | `toolResult` | `compactionSummary` and `content`
//! is an array of typed items (`{type: "text", text}`, `{type: "thinking",
//! thinking}`, `{type: "toolCall", id, name, arguments}`, `{type: "image",
//! …}`).
//!
//! Data dir resolution mirrors the CLI: `$MINIMAX_DATA_DIR` →
//! `$MAVIS_DATA_DIR` → `~/.minimax` (the `~/.minimax-code` install dir is a
//! separate choice and never holds sessions).
//!
//! The locator scans `v2/sessions/**/manifest.json` for a `sessionId` match
//! (the directory name is a timestamp + base64url id, never the raw session
//! id, so the manifest is the only reliable key) and reads the sibling
//! `messages.jsonl`. `toolResult` records are tool echoes, not turns, and
//! `compactionSummary` is bookkeeping — both are skipped the way the Grok
//! adapter drops `tool`/`system` lines.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

mod task_store;

use crate::env;
use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::{
    cap_tool_calls, push_bounded, truncate, truncate_json_strings, Parsed, ToolCall, Turn,
    MAX_TOOL_STRING,
};

/// A background task that finished without another assistant turn.
///
/// MiniMax Code records task completion out of band, and its own
/// `background_task_cadence_reminder` is injected **at the start of the next
/// turn**. A runtime completion notice can also arrive without resuming the
/// assistant. An idle session then never reads the finished result — the
/// stall in issue #2105. A detector driven by that reminder would never fire
/// during the stall it exists to break, so this uses native terminal tool
/// receipts, the host's `<background-task-finished>` notice and the CLI's
/// on-disk terminal summaries instead. See [`detect_background_task_stall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackgroundTaskStall {
    /// Newest finished task that has not been read in this session.
    pub task_id: String,
    /// When the harness recorded the task's terminal state.
    pub ended_at_ms: i64,
    /// When the session last produced an assistant message. A turn that
    /// started after the task ended would have consumed it.
    pub last_assistant_at_ms: Option<i64>,
}

/// Timestamp of the newest assistant message in the stream, if any.
fn newest_assistant_at(lines: &str) -> Option<i64> {
    lines
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|record| {
            let message = record_message(&record)?;
            if message_role(message) != Some("assistant") {
                return None;
            }
            message.get("timestamp").and_then(|t| t.as_i64())
        })
        .max()
}

fn has_pending_tool_call(lines: &str) -> Option<bool> {
    let mut pending = std::collections::HashSet::new();
    for line in lines.lines().filter(|line| !line.trim().is_empty()) {
        let record: serde_json::Value = serde_json::from_str(line).ok()?;
        let message = record_message(&record)?;
        match message_role(message) {
            Some("assistant") => {
                if let Some(content) = message.get("content").and_then(|value| value.as_array()) {
                    for item in content {
                        if item.get("type").and_then(|value| value.as_str()) == Some("toolCall") {
                            pending.insert(item.get("id")?.as_str()?.to_owned());
                        }
                    }
                }
            }
            Some("toolResult") => {
                if let Some(id) = message.get("toolCallId").and_then(|value| value.as_str()) {
                    pending.remove(id);
                }
            }
            _ => {}
        }
    }
    Some(!pending.is_empty())
}

/// A background task this session launched, from the `bash_background`
/// acknowledgement the CLI writes **during the turn that starts it**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchedTask {
    pub task_id: String,
    pub started_at_ms: i64,
}

/// What the CLI's own on-disk task store says about a launched task.
///
/// This is the load-bearing input. Unlike the cadence reminder, the store is
/// written when the task actually ends, so it is readable during exactly the
/// idle window the reminder cannot reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskStoreFact {
    pub task_id: String,
    /// Terminal receipt/notice/summary timestamp, or the latest output write for
    /// a task whose terminal state is still unknown. `None` when the task is
    /// provably over but no source dated it: the pending count may settle, but
    /// the stall detector has no ordering to compare against the assistant.
    pub settled_at_ms: Option<i64>,
    /// A native terminal receipt, host notice or summary proves the task is over.
    pub finished: bool,
}

fn valid_background_task_id(id: &str) -> bool {
    id.starts_with("bg_")
        && id.len() <= 128
        && id
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-')
}

fn is_terminal_task_status(status: &str) -> bool {
    matches!(
        status,
        "succeeded" | "failed" | "canceled" | "cancelled" | "lost"
    )
}

fn background_task_id(details: &serde_json::Value) -> Option<&str> {
    let id = details.get("task_id")?.as_str()?;
    valid_background_task_id(id).then_some(id)
}

/// Tasks the host itself reports terminal in a `<background-task-finished>`
/// notice, with the time each one ended when the record says so: the element's
/// `ended_at_ms`, else the record's own timestamp, else `None`.
///
/// This is the only terminal evidence for a task that failed or was cancelled
/// while the agent's last `task_output` poll raced the exit (that poll still
/// says `running`, and the CLI writes `summary.txt` for successes only). Only
/// the host's structured `hostMetadata.backgroundTaskOrigin` counts: the notice
/// text alone is forgeable by an agent quoting it or a user pasting it.
fn host_terminal_tasks(message: &serde_json::Value) -> Vec<(String, Option<i64>)> {
    let Some(origin) = message
        .get("hostMetadata")
        .and_then(|metadata| metadata.get("backgroundTaskOrigin"))
    else {
        return Vec::new();
    };
    if origin.get("kind").and_then(|kind| kind.as_str()) != Some("background-task-terminal") {
        return Vec::new();
    }
    let text = message_text(message);
    let notice_at = message.get("timestamp").and_then(|t| t.as_i64());
    origin
        .get("taskIds")
        .and_then(|ids| ids.as_array())
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str())
        .filter(|id| valid_background_task_id(id))
        .map(|id| (id.to_owned(), notice_ended_at_ms(&text, id).or(notice_at)))
        .collect()
}

/// `ended_at_ms` from the `<task task_id="…" … ended_at_ms="…"/>` element for `id`.
fn notice_ended_at_ms(text: &str, id: &str) -> Option<i64> {
    let element = &text[text.find(&format!("task_id=\"{id}\""))?..];
    let element = &element[..element.find("/>")?];
    let value = &element[element.find("ended_at_ms=\"")? + "ended_at_ms=\"".len()..];
    value[..value.find('"')?].parse().ok()
}

/// Pull every background task this session launched out of the transcript.
///
/// Only the launch acknowledgement counts: it carries `details.task_id` and
/// `details.status == "started" | "auto_promoted"`, and it is written while the turn that
/// started the task is still running — so it is present during the stall.
pub(crate) fn launched_background_tasks(lines: &str) -> Vec<LaunchedTask> {
    let mut tasks: Vec<LaunchedTask> = Vec::new();
    for line in lines.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(message) = record_message(&record) else {
            continue;
        };
        if message_role(message) != Some("toolResult") {
            continue;
        }
        let Some(details) = message.get("details") else {
            continue;
        };
        if !matches!(
            details.get("status").and_then(|s| s.as_str()),
            Some("started" | "auto_promoted")
        ) {
            continue;
        }
        let Some(task_id) = background_task_id(details) else {
            continue;
        };
        if tasks.iter().any(|task| task.task_id == task_id) {
            continue;
        }
        tasks.push(LaunchedTask {
            task_id: task_id.to_string(),
            started_at_ms: message
                .get("timestamp")
                .and_then(|t| t.as_i64())
                .unwrap_or_default(),
        });
    }
    tasks
}

/// Pure detector: has this session launched a background task that finished
/// without the session ever reading it?
///
/// A task counts as stalled when the CLI's store says it is over **and** its
/// last write is newer than the session's last assistant message. A turn that
/// began after the task ended would have seen the cadence reminder and could
/// have read the result, so that ordering is what separates "the agent is
/// working on it" from "the agent will never learn of it". Terminal silence on
/// its own is never evidence.
pub(crate) fn detect_background_task_stall(
    launched: &[LaunchedTask],
    facts: &[TaskStoreFact],
    last_assistant_at_ms: Option<i64>,
    _now_ms: i64,
) -> Option<BackgroundTaskStall> {
    // Another owned task may still be running (or its state unreadable).
    // Quiet output alone never establishes that it has finished.
    if launched.iter().any(|task| {
        !facts
            .iter()
            .any(|fact| fact.task_id == task.task_id && fact.finished)
    }) {
        return None;
    }
    let mut stall: Option<BackgroundTaskStall> = None;
    for fact in facts {
        if !launched.iter().any(|task| task.task_id == fact.task_id) {
            continue;
        }
        if !fact.finished {
            continue;
        }
        // An undated terminal fact cannot be ordered against the assistant, so
        // it never proves the session missed it.
        let Some(settled_at_ms) = fact.settled_at_ms else {
            continue;
        };
        if let Some(last) = last_assistant_at_ms {
            if last >= settled_at_ms {
                // The session has spoken since the task finished.
                continue;
            }
        }
        let newer = stall
            .as_ref()
            .is_none_or(|current| settled_at_ms > current.ended_at_ms);
        if newer {
            stall = Some(BackgroundTaskStall {
                task_id: fact.task_id.clone(),
                ended_at_ms: settled_at_ms,
                last_assistant_at_ms,
            });
        }
    }
    stall
}

/// Read the CLI's own completion facts for these tasks from its task store.
///
/// `summary.txt` in `<dataDir>/background-tasks/<taskId>/` appears once the task
/// has finished. An output log without that summary records activity, but its
/// modification time cannot prove termination.
fn task_store_facts(tasks_dir: &Path, launched: &[LaunchedTask]) -> Vec<TaskStoreFact> {
    launched
        .iter()
        .filter_map(|task| {
            let dir = tasks_dir.join(&task.task_id);
            let summary = dir.join("summary.txt");
            let output = dir.join("output.log");
            // A missing or unreadable store entry is not evidence: another
            // session may own the task, or the CLI may not have written yet.
            let finished = summary.is_file();
            let settled_at_ms = if finished {
                mtime_ms(&summary)
            } else {
                mtime_ms(&output)
            }?;
            Some(TaskStoreFact {
                task_id: task.task_id.clone(),
                settled_at_ms: Some(settled_at_ms),
                finished,
            })
        })
        .collect()
}

/// Record `task_id` as finished, replacing any earlier fact for it. An undated
/// update keeps the date of an earlier terminal fact: learning the task is over
/// again must not discard when it was known to have ended.
fn settle(facts: &mut Vec<TaskStoreFact>, task_id: &str, settled_at_ms: Option<i64>) {
    let settled_at_ms = settled_at_ms.or_else(|| {
        facts
            .iter()
            .find(|fact| fact.task_id == task_id && fact.finished)
            .and_then(|fact| fact.settled_at_ms)
    });
    facts.retain(|fact| fact.task_id != task_id);
    facts.push(TaskStoreFact {
        task_id: task_id.to_owned(),
        settled_at_ms,
        finished: true,
    });
}

/// Combine the three terminal sources for a launched task: native terminal tool
/// receipts, the host's `<background-task-finished>` notice, and the CLI's
/// terminal summaries. Missing or partial canonical history stays unknown
/// rather than clearing a veto.
fn background_facts(
    lines: &str,
    tasks_dir: &Path,
    launched: &[LaunchedTask],
) -> Option<Vec<TaskStoreFact>> {
    let mut facts = task_store_facts(tasks_dir, launched);
    for line in lines.lines().filter(|line| !line.trim().is_empty()) {
        let record: serde_json::Value = serde_json::from_str(line).ok()?;
        let message = record_message(&record)?;
        let role = message_role(message)?;
        if !matches!(
            role,
            "user" | "assistant" | "toolResult" | "custom" | "compactionSummary"
        ) {
            return None;
        }
        if role == "user" {
            for (task_id, settled_at_ms) in host_terminal_tasks(message) {
                if launched.iter().any(|task| task.task_id == task_id) {
                    settle(&mut facts, &task_id, settled_at_ms);
                }
            }
            continue;
        }
        if role != "toolResult" {
            continue;
        }
        let Some(details) = message.get("details") else {
            continue;
        };
        if matches!(
            details.get("status").and_then(|s| s.as_str()),
            Some("started" | "auto_promoted")
        ) {
            background_task_id(details)?;
            continue;
        }
        if !details
            .get("status")
            .and_then(|s| s.as_str())
            .is_some_and(is_terminal_task_status)
        {
            continue;
        }
        let Some(task_id) = details.get("task_id").and_then(|id| id.as_str()) else {
            continue;
        };
        if !launched.iter().any(|task| task.task_id == task_id) {
            continue;
        }
        let Some(settled_at_ms) = message.get("timestamp").and_then(|t| t.as_i64()) else {
            continue;
        };
        settle(&mut facts, task_id, Some(settled_at_ms));
    }
    Some(facts)
}

/// Stop and wake detection use the same terminal evidence. A pending/unknown
/// registry row can veto a summary; a foreign row cannot replace its evidence.
fn reconciled_background_facts(
    lines: &str,
    transcript: &Path,
    tasks_dir: &Path,
    database: &Path,
    launched: &[LaunchedTask],
) -> Option<Vec<TaskStoreFact>> {
    let mut facts = background_facts(lines, tasks_dir, launched)?;
    task_store::reconcile(transcript, database, launched, &mut facts)?;
    Some(facts)
}

/// The question an `ask_user` questionnaire is still waiting on, if any.
///
/// MiniMax's `ask_user` tool returns immediately with
/// `details.waiting_for_user: true` and the model ends its turn, so the Stop
/// that follows is not a finished turn: the user owes an answer. Any later
/// `user` record (the reply, or a new prompt) closes the wait. Returns the
/// first step's question text, or an empty string when it has none.
pub(crate) fn waiting_user_question(lines: &str) -> Option<String> {
    let mut asks = std::collections::HashMap::new();
    let mut waiting: Option<String> = None;
    for line in lines.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(message) = record_message(&record) else {
            continue;
        };
        match message_role(message) {
            Some("assistant") => {
                for call in message
                    .get("content")
                    .and_then(|content| content.as_array())
                    .into_iter()
                    .flatten()
                    .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("toolCall"))
                {
                    if let Some(id) = call.get("id").and_then(|id| id.as_str()) {
                        let question = call
                            .pointer("/arguments/steps/0/question")
                            .and_then(|q| q.as_str())
                            .unwrap_or_default();
                        asks.insert(id.to_owned(), question.to_owned());
                    }
                }
            }
            Some("toolResult")
                if message.pointer("/details/waiting_for_user")
                    == Some(&serde_json::Value::Bool(true)) =>
            {
                let call = message.get("toolCallId").and_then(|id| id.as_str());
                waiting = Some(
                    call.and_then(|id| asks.get(id).cloned())
                        .unwrap_or_default(),
                );
            }
            Some("user") => waiting = None,
            _ => {}
        }
    }
    waiting
}

/// [`waiting_user_question`] for a node's live MiniMax session.
pub(crate) fn pending_user_question(session_id: &str, spawn_path: &str) -> Option<String> {
    let data_dir = minimax_data_dir_for_spawn(spawn_path, env::minimax_data_dir())?;
    let transcript = find_mcode_transcript_in(&data_dir.join("v2").join("sessions"), session_id)?;
    waiting_user_question(&std::fs::read_to_string(transcript).ok()?)
}

/// Count outstanding work in the caller-resolved runtime data directory.
/// `None` means completion evidence cannot be established for this session.
pub(crate) fn count_pending_background_tasks(session_id: &str, data_dir: &Path) -> Option<usize> {
    let transcript = find_mcode_transcript_in(&data_dir.join("v2").join("sessions"), session_id)?;
    let paths = task_store::StorePaths::for_transcript(&transcript)?;
    let lines = std::fs::read_to_string(&transcript).ok()?;
    if lines.trim().is_empty() {
        return None;
    }
    // A settled background task cannot establish that the foreground is done.
    if has_pending_tool_call(&lines)? {
        return None;
    }
    let launched = launched_background_tasks(&lines);
    let facts = reconciled_background_facts(
        &lines,
        &transcript,
        &paths.tasks_dir,
        &paths.database,
        &launched,
    )?;
    Some(
        launched
            .iter()
            .filter(|task| {
                !facts
                    .iter()
                    .any(|fact| fact.task_id == task.task_id && fact.finished)
            })
            .count(),
    )
}

/// Resolve the spawned runtime's store before reading session completion.
pub(crate) fn count_pending_background_tasks_for_spawn(
    session_id: &str,
    spawn_path: &str,
    native_data_dir: PathBuf,
) -> Option<usize> {
    let data_dir = minimax_data_dir_for_spawn(spawn_path, native_data_dir)?;
    count_pending_background_tasks(session_id, &data_dir)
}

/// Modification time as epoch milliseconds.
fn mtime_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let millis = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    i64::try_from(millis).ok()
}

/// Drop-in [`TranscriptAdapter`] for MiniMax Code.
pub(crate) struct McodeAdapter;

impl TranscriptAdapter for McodeAdapter {
    fn id(&self) -> &'static str {
        "mcode"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        let data_dir = minimax_data_dir_for_spawn(ctx.node_path, env::minimax_data_dir())?;
        find_mcode_transcript_in(&data_dir.join("v2").join("sessions"), ctx.session_id)
    }

    /// Esc fires no hook. An interrupted tool leaves an aborted tool result
    /// last; an interrupted model reply leaves the prompt unanswered. The
    /// caller's write-after-key-press fence tells the latter from a prompt
    /// still waiting for its first reply.
    fn interrupted_turn(&self, lines: &str) -> bool {
        let last = lines
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|record| record_message(&record).cloned())
            .next_back();
        let Some(message) = last else {
            return false;
        };
        match message_role(&message) {
            Some("user") => true,
            Some("toolResult") => {
                message.get("isError").and_then(|e| e.as_bool()) == Some(true)
                    && matches!(
                        message.pointer("/content/0/text").and_then(|t| t.as_str()),
                        Some("This operation was aborted" | "Operation aborted")
                    )
            }
            _ => false,
        }
    }

    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed {
        parse_mcode_turns_with_text_limit(lines, keep, max_text)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        record_message(&value).is_some_and(|message| {
            message_role(message) == Some("assistant") && !message_text(message).trim().is_empty()
        })
    }

    fn stalled_background_task(
        &self,
        lines: &str,
        transcript: &Path,
        now_ms: i64,
    ) -> Option<String> {
        if has_pending_tool_call(lines)? {
            return None;
        }
        let launched = launched_background_tasks(lines);
        if launched.is_empty() {
            return None;
        }
        let paths = task_store::StorePaths::for_transcript(transcript)?;
        let facts = reconciled_background_facts(
            lines,
            transcript,
            &paths.tasks_dir,
            &paths.database,
            &launched,
        )?;
        detect_background_task_stall(&launched, &facts, newest_assistant_at(lines), now_ms)
            .map(|stall| stall.task_id)
    }
}

/// Resolve the MiniMax data dir in the environment that will execute (or
/// executed) the CLI for `spawn_path`: `$MINIMAX_DATA_DIR` → `$MAVIS_DATA_DIR`
/// → `~/.minimax`, translated through the WSL/host boundary the same way the
/// Muse adapter resolves its session store. `env` owns host-path composition.
pub(crate) fn minimax_data_dir_for_spawn(
    spawn_path: &str,
    native_data_dir: PathBuf,
) -> Option<PathBuf> {
    env::cli_dir_for_spawn(native_data_dir, ".minimax", spawn_path)
}

/// Pure MiniMax locator: walk `sessions_root` newest-first and return the
/// `messages.jsonl` beside the first `manifest.json` whose `sessionId`
/// matches. Split from the env lookup so tests drive it against a temp
/// dir. A directory carrying its own `manifest.json` IS a session
/// directory, so the walk never descends into it (`snapshots/` and
/// `reports/` are session artifacts, never sessions) and returns on the
/// first valid match instead of crawling all of history — `locate` runs on
/// every Coordinator poll tick, so an exhaustive scan would thrash disk
/// I/O. A match without a `messages.jsonl` beside it is skipped, not
/// returned (→ `NoTranscript` degrade upstream).
pub(crate) fn find_mcode_transcript_in(sessions_root: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() || !sessions_root.is_dir() {
        return None;
    }
    // Iterative DFS so a deep tree cannot overflow the stack; depth 5
    // covers `<root>/<YYYY>/<MM>/<DD>/<session-dir>/` plus one spare
    // level for legacy layouts.
    let mut stack = vec![(sessions_root.to_path_buf(), 0usize)];
    while let Some((dir, level)) = stack.pop() {
        let manifest = dir.join("manifest.json");
        if manifest.is_file() {
            if manifest_session_id(&manifest).as_deref() == Some(session_id) {
                let messages = dir.join("messages.jsonl");
                if messages.is_file() {
                    return Some(messages);
                }
            }
            continue;
        }
        if level >= 5 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                std::fs::symlink_metadata(path)
                    .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
            })
            .collect();
        // Ascending push + LIFO pop visits the newest dated directories
        // first, so active sessions short-circuit before history is read.
        subdirs.sort();
        for sub in subdirs {
            stack.push((sub, level + 1));
        }
    }
    None
}

/// Read a `manifest.json` and return its `sessionId` when the file parses.
/// Anything else (missing file, bad JSON, non-string id) is `None` — the
/// scan simply moves on to the next manifest.
fn manifest_session_id(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    // The manifest must be an object; `sessionId` must be a string. Both
    // are load-bearing (the CLI validates the same fields before binding
    // history), so anything else degrades to "no match" rather than a
    // best-effort guess.
    value
        .as_object()?
        .get("sessionId")?
        .as_str()
        .map(str::to_string)
}

/// The `message` object of one `messages.jsonl` record, or `None` when the
/// line is not a history record at all.
fn record_message(record: &serde_json::Value) -> Option<&serde_json::Value> {
    record.as_object()?.get("message")
}

fn message_role(message: &serde_json::Value) -> Option<&str> {
    message.as_object()?.get("role")?.as_str()
}

/// Concatenate the `text` of every `{type: "text"}` content item. `content`
/// may also arrive as a bare string (defensive: same convention as the
/// Grok/Claude parsers); `thinking`, `toolCall`, and `image` items are not
/// text and never contribute.
fn message_text(message: &serde_json::Value) -> String {
    match message.as_object().and_then(|m| m.get("content")) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// MiniMax prefixes real user input with reminder blocks in the same text
/// item. Strip complete leading wrappers before truncation, keeping the request.
fn user_text(text: &str) -> &str {
    let mut text = text.trim();
    while text.starts_with("<system-reminder>") {
        let Some(end) = text.find("</system-reminder>") else {
            // A partial injected wrapper is not a user request.
            return "";
        };
        text = text[end + "</system-reminder>".len()..].trim_start();
    }
    text
}

/// Pull tool calls out of an assistant message's `{type: "toolCall"}`
/// content items. `arguments` is natively an object but may arrive as JSON
/// text; both are honoured under the shared `MAX_TOOL_STRING` truncation so
/// a call carrying a whole file body can't dominate the payload.
fn extract_mcode_tool_calls(message: &serde_json::Value) -> Vec<ToolCall> {
    let Some(serde_json::Value::Array(items)) = message.as_object().and_then(|m| m.get("content"))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("toolCall"))
        .filter_map(|item| {
            let obj = item.as_object()?;
            let name = obj
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let input = obj
                .get("arguments")
                .map(|value| match value {
                    serde_json::Value::String(text) => {
                        serde_json::from_str(text).unwrap_or_else(|_| value.clone())
                    }
                    _ => value.clone(),
                })
                .unwrap_or(serde_json::Value::Null);
            Some(ToolCall {
                name,
                input: truncate_json_strings(input, MAX_TOOL_STRING),
            })
        })
        .collect()
}

/// Parse MiniMax `messages.jsonl` lines into logical turns under the shared
/// [`Parsed`] contract: rolling `keep`-bounded window, whole-stream
/// last-assistant tracking, malformed flag so a renamed shape degrades loudly
/// as `ShapeChanged`. `toolResult` echoes and `compactionSummary`
/// bookkeeping are silently skipped — never flagged, never turns.
#[cfg(test)]
pub(crate) fn parse_mcode_turns(lines: impl Iterator<Item = String>, keep: usize) -> Parsed {
    parse_mcode_turns_with_text_limit(lines, keep, super::super::types::MAX_TURN_TEXT)
}

pub(crate) fn parse_mcode_turns_with_text_limit(
    lines: impl Iterator<Item = String>,
    keep: usize,
    max_text: usize,
) -> Parsed {
    let keep = keep.max(1);
    let mut turns: VecDeque<Turn> = VecDeque::new();
    let mut last_assistant_message: Option<String> = None;
    let mut saw_malformed = false;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(message) = record_message(&record) else {
            // Not a history record (ledger/snapshot envelope, partial write)
            // — skip without flagging; the malformed flag is reserved for
            // records that claim the shape but break it.
            continue;
        };
        let role = match message_role(message) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            // `toolResult` echoes, `compactionSummary` bookkeeping, and
            // every unknown role — silently dropped, never flagged.
            _ => continue,
        };
        if message
            .as_object()
            .is_none_or(|m| !m.contains_key("content"))
        {
            saw_malformed = true;
            continue;
        }
        let raw_text = message_text(message);
        let text = if role == "user" {
            user_text(&raw_text)
        } else {
            &raw_text
        };
        let mut tool_calls = if role == "assistant" {
            extract_mcode_tool_calls(message)
        } else {
            Vec::new()
        };
        // A record with no text and no tool calls is a no-op (a
        // thinking/image-only assistant message, an image-only user
        // submission) — skip without flagging so empty placeholders never
        // consume a slot in the `keep`-bounded turn window.
        if text.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }
        if role == "assistant" {
            cap_tool_calls(&mut tool_calls);
            let turn = Turn {
                role: "assistant".to_string(),
                text: truncate(text, max_text),
                tool_calls,
            };
            if !turn.text.trim().is_empty() {
                last_assistant_message = Some(turn.text.clone());
            }
            push_bounded(&mut turns, turn, keep);
        } else {
            push_bounded(
                &mut turns,
                Turn {
                    role: "user".to_string(),
                    text: truncate(text, max_text),
                    tool_calls: Vec::new(),
                },
                keep,
            );
        }
    }
    Parsed {
        turns: turns.into(),
        last_assistant_message,
        saw_malformed,
    }
}

#[cfg(test)]
mod stall_tests;

#[cfg(test)]
mod interrupt_tests {
    use super::*;

    const PROMPT: &str = r#"{"message_id":"msg-user-v1-4b2w","turn_id":"turn_mv88fotx_sf3lun","message":{"role":"user","content":[{"type":"text","text":"Write a 900-word essay about the history of rivers."}],"timestamp":1791989762742}}"#;
    const TOOL_CALL: &str = r#"{"message_id":"msg-VrbM","turn_id":"turn_mv880gow_oc6b01","message":{"role":"assistant","content":[{"type":"text","text":"Running the 90-second sleep now."},{"type":"toolCall","id":"call_01a13adf02b7723d8fc531fa","name":"bash","arguments":{"command":"Start-Sleep -Seconds 90"}}],"stopReason":"toolUse","timestamp":1791989052000}}"#;
    const ABORTED_TOOL: &str = r#"{"message_id":"msg-cYa2","turn_id":"turn_mv880gow_oc6b01","message":{"role":"toolResult","toolCallId":"call_01a13adf02b7723d8fc531fa","toolName":"bash","content":[{"type":"text","text":"This operation was aborted"}],"details":{},"isError":true,"timestamp":1791989088384}}"#;
    const FAILED_TOOL: &str = r#"{"message_id":"msg-f","turn_id":"t","message":{"role":"toolResult","toolCallId":"call_01a13adf02b7723d8fc531fa","toolName":"bash","content":[{"type":"text","text":"exit code 1"}],"details":{},"isError":true,"timestamp":1791989088384}}"#;
    const REPLY: &str = r#"{"message_id":"msg-z8CV","turn_id":"turn_mv87ovpq_2l59p0","message":{"role":"assistant","content":[{"type":"text","text":"OK"}],"stopReason":"stop","timestamp":1791988512678}}"#;

    /// Esc fires no MiniMax hook (captured live on 2026-10-14). Interrupting a
    /// running tool appends an aborted tool result; interrupting the model
    /// leaves the prompt unanswered. Either way nothing follows.
    #[test]
    fn an_aborted_tool_or_an_unanswered_prompt_ends_an_interrupted_turn() {
        let adapter = McodeAdapter;
        assert!(adapter.interrupted_turn(&format!("{PROMPT}\n{TOOL_CALL}\n{ABORTED_TOOL}\n")));
        assert!(adapter.interrupted_turn(&format!("{REPLY}\n{PROMPT}\n")));
        assert!(!adapter.interrupted_turn(&format!("{PROMPT}\n{TOOL_CALL}\n")));
        assert!(!adapter.interrupted_turn(&format!("{PROMPT}\n{TOOL_CALL}\n{FAILED_TOOL}\n")));
        assert!(!adapter.interrupted_turn(&format!("{PROMPT}\n{REPLY}\n")));
        assert!(!adapter.interrupted_turn(""));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ask_user` as MiniMax Code records it (captured live 2026-10-14): the
    /// tool returns at once with `details.waiting_for_user` and the turn
    /// ends. Until a user record follows, the session is waiting on the user.
    #[test]
    fn unanswered_ask_user_questionnaire_is_a_waiting_question() {
        let ask = r#"{"message_id":"m1","turn_id":"t1","message":{"role":"assistant","timestamp":1791981974675,"content":[{"type":"toolCall","id":"call_01a13a72","name":"ask_user","arguments":{"mode":"questionnaire","title":"Colour preference","steps":[{"id":"color","header":"Colour","question":"Which colour do you prefer: red or blue?","selectionMode":"single","options":[{"id":"red","label":"Red","recommended":true},{"id":"blue","label":"Blue"}]}]}}]}}"#;
        let waiting = r#"{"message_id":"m2","turn_id":"t1","message":{"role":"toolResult","toolCallId":"call_01a13a72","toolName":"ask_user","content":[{"type":"text","text":"Questionnaire ask_7d1b is waiting for the local user. Stop this turn until the user replies."}],"details":{"request_id":"ask_7d1b","schema_version":2,"step_count":1,"waiting_for_user":true},"isError":false,"timestamp":1791981977144}}"#;
        let note = record(
            "m3",
            "t1",
            serde_json::json!({"role":"assistant","timestamp":1791981978000u64,
                "content":[{"type":"text","text":"Waiting for your answer."}]}),
        );
        let lines = format!("{ask}\n{waiting}\n{note}\n");
        assert_eq!(
            waiting_user_question(&lines).as_deref(),
            Some("Which colour do you prefer: red or blue?")
        );

        let reply = record("m4", "t2", user_msg("Red"));
        assert_eq!(waiting_user_question(&format!("{lines}{reply}\n")), None);
        assert_eq!(waiting_user_question(&format!("{note}\n")), None);
    }

    fn record(message_id: &str, turn_id: &str, message: serde_json::Value) -> String {
        serde_json::json!({
            "message_id": message_id,
            "turn_id": turn_id,
            "message": message,
        })
        .to_string()
    }

    fn user_msg(text: &str) -> serde_json::Value {
        serde_json::json!({
            "role": "user",
            "timestamp": 1_788_000_000_000u64,
            "content": [{"type": "text", "text": text}],
        })
    }

    fn assistant_msg(text: &str, calls: serde_json::Value) -> serde_json::Value {
        let mut content = vec![serde_json::json!({"type": "text", "text": text})];
        if let serde_json::Value::Array(mut extra) = calls {
            content.append(&mut extra);
        }
        serde_json::json!({
            "role": "assistant",
            "timestamp": 1_788_000_001_000u64,
            "content": content,
        })
    }

    #[test]
    fn parses_user_and_assistant_records_with_typed_content() {
        let lines = [
            record("msg-1", "turn-1", user_msg("Fix the login bug")),
            record(
                "msg-2",
                "turn-1",
                assistant_msg(
                    "Reading the file.",
                    serde_json::json!([{"type": "toolCall", "id": "c1",
                        "name": "read_file", "arguments": {"path": "src/auth.rs"}}]),
                ),
            ),
            record(
                "msg-3",
                "turn-1",
                serde_json::json!({"role": "toolResult", "timestamp": 1_788_000_002_000u64,
                    "content": [{"type": "text", "text": "file contents"}]}),
            ),
        ];
        let parsed = parse_mcode_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].role, "user");
        assert_eq!(parsed.turns[0].text, "Fix the login bug");
        assert!(parsed.turns[0].tool_calls.is_empty());
        assert_eq!(parsed.turns[1].role, "assistant");
        assert_eq!(parsed.turns[1].text, "Reading the file.");
        assert_eq!(parsed.turns[1].tool_calls.len(), 1);
        assert_eq!(parsed.turns[1].tool_calls[0].name, "read_file");
        assert_eq!(
            parsed.turns[1].tool_calls[0].input,
            serde_json::json!({"path": "src/auth.rs"})
        );
        assert_eq!(
            parsed.last_assistant_message.as_deref(),
            Some("Reading the file.")
        );
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn string_arguments_decode_and_thinking_only_messages_drop() {
        let lines = [
            record(
                "msg-1",
                "turn-1",
                assistant_msg(
                    "",
                    serde_json::json!([
                        {"type": "thinking", "thinking": "private plan"},
                        {"type": "toolCall", "id": "c1", "name": "run",
                         "arguments": "{\"cmd\":\"cargo test\"}"},
                    ]),
                ),
            ),
            record(
                "msg-2",
                "turn-2",
                serde_json::json!({"role": "compactionSummary",
                    "summary": "older work compacted"}),
            ),
        ];
        let parsed = parse_mcode_turns(lines.into_iter(), 10);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].text, "");
        assert_eq!(
            parsed.turns[0].tool_calls[0].input,
            serde_json::json!({"cmd": "cargo test"})
        );
        // No assistant text anywhere: no last message, but no malformed flag
        // either — thinking-only and compaction records are known shapes.
        assert_eq!(parsed.last_assistant_message, None);
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn missing_content_flags_malformed_while_unknown_lines_skip_quietly() {
        let lines = [
            "not json at all".to_string(),
            record(
                "msg-1",
                "turn-1",
                serde_json::json!({"role": "assistant", "timestamp": 1u64}),
            ),
        ];
        let parsed = parse_mcode_turns(lines.into_iter(), 10);
        assert!(parsed.turns.is_empty());
        assert!(parsed.saw_malformed);
    }

    #[test]
    fn locator_finds_messages_via_manifest_scan_not_dir_name() {
        let root = tempfile::tempdir().unwrap();
        // Date-shaped nesting with an opaque (non-id) directory name, exactly
        // like the CLI's `<YYYY>/<MM>/<DD>/<time>-session_<base64url>` layout.
        let session_dir = root.path().join("2026/09/19/10-00-00-000-session_abc");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-123",
                "createdAtMs": 1_788_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        let messages = session_dir.join("messages.jsonl");
        std::fs::write(&messages, record("msg-1", "turn-1", user_msg("hi"))).unwrap();
        // A decoy manifest for another session must not shadow the match.
        let decoy = root.path().join("2026/09/18/09-00-00-000-session_zzz");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(
            decoy.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-999",
                "createdAtMs": 1_787_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        std::fs::write(decoy.join("messages.jsonl"), "noise").unwrap();

        assert_eq!(
            find_mcode_transcript_in(root.path(), "ses-123"),
            Some(messages)
        );
        assert_eq!(find_mcode_transcript_in(root.path(), "ses-absent"), None);
        assert_eq!(find_mcode_transcript_in(root.path(), ""), None);
    }

    #[test]
    fn empty_user_records_skip_without_consuming_window() {
        // An image-only user submission carries no text and no tool calls:
        // it must not evict a meaningful turn from a small window.
        let lines = [
            record("msg-1", "turn-1", user_msg("first")),
            record(
                "msg-2",
                "turn-2",
                serde_json::json!({"role": "user", "timestamp": 1u64,
                    "content": [{"type": "image", "data": "…"}]}),
            ),
            record(
                "msg-3",
                "turn-2",
                assistant_msg("answer", serde_json::json!([])),
            ),
        ];
        let parsed = parse_mcode_turns(lines.into_iter(), 2);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].text, "first");
        assert_eq!(parsed.turns[1].text, "answer");
        assert!(!parsed.saw_malformed);
    }

    #[test]
    fn locator_never_descends_into_session_artifact_dirs() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("2026/09/19/10-00-00-000-session_abc");
        std::fs::create_dir_all(session_dir.join("snapshots")).unwrap();
        std::fs::write(
            session_dir.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-123",
                "createdAtMs": 1_788_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        // A manifest buried in snapshots/ is a session artifact, never a
        // session — the walk must not reach it even though it matches.
        std::fs::write(
            session_dir.join("snapshots/manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-buried",
                "createdAtMs": 1_788_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            session_dir.join("snapshots/messages.jsonl"),
            record("msg-9", "turn-9", user_msg("buried")),
        )
        .unwrap();
        assert_eq!(find_mcode_transcript_in(root.path(), "ses-buried"), None);
    }

    #[test]
    fn locator_prefers_newest_duplicate_manifest() {
        let root = tempfile::tempdir().unwrap();
        for (day, tag) in [("18", "old"), ("19", "new")] {
            let dir = root
                .path()
                .join(format!("2026/09/{day}/10-00-00-000-session_abc"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("manifest.json"),
                serde_json::json!({"schemaVersion": 1, "sessionId": "ses-dup",
                    "createdAtMs": 1_788_000_000_000u64})
                .to_string(),
            )
            .unwrap();
            std::fs::write(
                dir.join("messages.jsonl"),
                record("msg-1", "turn-1", user_msg(tag)),
            )
            .unwrap();
        }
        let found = find_mcode_transcript_in(root.path(), "ses-dup").unwrap();
        let text = std::fs::read_to_string(found).unwrap();
        assert!(
            text.contains("\"new\""),
            "expected newest duplicate, got {text}"
        );
    }

    #[test]
    fn locator_ignores_manifest_without_messages() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("2026/09/19/10-00-00-000-session_abc");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("manifest.json"),
            serde_json::json!({"schemaVersion": 1, "sessionId": "ses-123",
                "createdAtMs": 1_788_000_000_000u64})
            .to_string(),
        )
        .unwrap();
        assert_eq!(find_mcode_transcript_in(root.path(), "ses-123"), None);
    }

    #[test]
    fn line_predicate_matches_assistant_text_only() {
        let text = record(
            "msg-1",
            "turn-1",
            assistant_msg("Blocking question?", serde_json::json!([])),
        );
        assert!(McodeAdapter.line_has_assistant_text(&text));
        let tools_only = record(
            "msg-2",
            "turn-1",
            assistant_msg(
                "",
                serde_json::json!([{"type": "toolCall", "id": "c1",
                "name": "run", "arguments": {}}]),
            ),
        );
        assert!(!McodeAdapter.line_has_assistant_text(&tools_only));
        let user = record("msg-3", "turn-2", user_msg("go on"));
        assert!(!McodeAdapter.line_has_assistant_text(&user));
        assert!(!McodeAdapter.line_has_assistant_text("garbage"));
    }
}

#[cfg(test)]
mod contract_tests {

    use crate::services::transcript_reader::{assistant_report_from_file, TranscriptFormat};

    #[test]
    fn mcode_native_transcript_recovers_circuit_report() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("2026/09/19/10-00-00-000-session_abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("manifest.json"),
            r#"{"schemaVersion":1,"sessionId":"ses-123","createdAtMs":1788000000000}"#,
        )
        .unwrap();
        let file = session.join("messages.jsonl");
        std::fs::write(&file, concat!(
            "{\"message_id\":\"m1\",\"turn_id\":\"t1\",\"message\":{\"role\":\"user\",\"timestamp\":1788000000000,\"content\":[{\"type\":\"text\",\"text\":\"Fix the parser\"}]}}\n",
            "{\"message_id\":\"m2\",\"turn_id\":\"t1\",\"message\":{\"role\":\"assistant\",\"timestamp\":1788000001000,\"content\":[{\"type\":\"text\",\"text\":\"Parser fixed. Tests pass.\"}]}}\n",
        )).unwrap();

        let path = crate::services::transcript_reader::readers::mcode::find_mcode_transcript_in(
            temp.path(),
            "ses-123",
        )
        .expect("manifest scan must resolve the session transcript");
        assert_eq!(path, file);
        let report = assistant_report_from_file(&path, TranscriptFormat::Mcode)
            .expect("a completed mcode turn must be readable by the circuit");
        assert_eq!(report.text, "Parser fixed. Tests pass.");

        use std::io::Write;
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        writeln!(writer, "{{\"message_id\":\"m3\",\"turn_id\":\"t1\",\"message\":{{\"role\":\"toolResult\",\"timestamp\":1788000002000,\"content\":[{{\"type\":\"text\",\"text\":\"tool echo\"}}]}}}}").unwrap();
        assert_eq!(
            assistant_report_from_file(&path, TranscriptFormat::Mcode)
                .unwrap()
                .revision,
            report.revision,
            "a toolResult echo must not advance the assistant revision"
        );
        writeln!(writer, "{{\"message_id\":\"m4\",\"turn_id\":\"t2\",\"message\":{{\"role\":\"assistant\",\"timestamp\":1788000003000,\"content\":[{{\"type\":\"text\",\"text\":\"Next task done.\"}}]}}}}").unwrap();
        assert_ne!(
            assistant_report_from_file(&path, TranscriptFormat::Mcode)
                .unwrap()
                .revision,
            report.revision,
            "a fresh assistant response must advance the revision"
        );
    }
}

#[cfg(test)]
mod file_contract_tests {
    use super::*;
    use crate::services::transcript_reader::test_support::{assert_jsonl_contract, fixture};

    #[test]
    fn reader_handles_tail_digest_malformed_empty_shape_changed_and_unreadable_files() {
        assert_jsonl_contract(
            &McodeAdapter,
            &fixture("mcode", "transcript.jsonl"),
            &fixture("mcode", "shape_changed.jsonl"),
            "MiniMax blocking question?",
        );
    }
}

#[cfg(test)]
mod background_task_tests;
