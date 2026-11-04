//! Node Glance: the at-a-glance transcript layer of an Agent Node.
//!
//! A glance answers three questions without opening the terminal: what was the
//! agent last asked, what is it doing about it, and what did it last say. It is
//! built from the same harness-owned [`Parsed`] window every reader already
//! produces, so a harness adopts it by having a transcript reader at all:
//! [`TranscriptReader::glance_window`] chooses the bounded window (a JSONL tail
//! by default, a store query for document and SQLite readers) and
//! [`TranscriptReader::describe_tool_call`] names one tool call (a
//! harness-agnostic description by default).
//!
//! Everything here is pure over the parsed window, so the shaping rules are
//! testable without a transcript on disk.

use serde::Serialize;
use ts_rs::TS;

use super::types::{truncate, Parsed, ToolCall, Turn};

/// Turns retained for a glance: enough to reach back from a run of tool-only
/// turns to the prompt that started them in an ordinary turn, while keeping one
/// read bounded.
pub(crate) const GLANCE_TURNS: usize = 24;
/// The latest assistant message is a preview, not the report.
pub(crate) const GLANCE_MESSAGE_BYTES: usize = 600;
/// The prompt only has to say what the task is.
pub(crate) const GLANCE_PROMPT_BYTES: usize = 280;
/// One activity line: a command, a path or a search.
pub(crate) const GLANCE_TARGET_BYTES: usize = 160;
/// Most recent activity lines kept, newest last.
pub(crate) const GLANCE_ACTIVITY: usize = 3;

/// One thing the agent did: a tool and, when the input names one, what it
/// acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(
    export,
    rename = "NodeGlanceActivity",
    export_to = "NodeGlanceActivity.ts"
)]
pub struct GlanceActivity {
    /// The harness's own tool name (`Bash`, `exec_command`, `edit`, …).
    pub tool: String,
    /// The command, path, pattern or URL the call acted on, truncated.
    pub target: Option<String>,
}

/// The transcript-derived layer of a Node Glance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "TranscriptGlance.ts")]
pub struct TranscriptGlance {
    /// The latest genuine user prompt in the window, truncated. `None` when
    /// the window holds only the agent's own turns (a very long turn).
    pub last_prompt: Option<String>,
    /// The latest assistant text across the window, truncated.
    pub last_message: Option<String>,
    /// Whether `last_message` was written after `last_prompt`, i.e. it is the
    /// agent's answer (so far) to the current task rather than an older one.
    pub message_after_prompt: bool,
    /// The most recent tool calls since the latest prompt, newest last.
    pub activity: Vec<GlanceActivity>,
    /// Tool calls made since the latest prompt (within the window).
    #[ts(as = "i32")]
    pub tool_calls_since_prompt: usize,
    /// When the transcript was last written, in epoch milliseconds. The
    /// harness-agnostic "last sign of life".
    #[ts(type = "number | null")]
    pub updated_at_ms: Option<i64>,
}

/// Describe a tool call without knowing the harness: the first input field
/// that names what the call acted on, in a fixed priority order. Harnesses
/// whose tools carry their target elsewhere override
/// [`TranscriptReader::describe_tool_call`](super::adapter::TranscriptReader::describe_tool_call).
pub(crate) fn describe_tool_call(call: &ToolCall) -> GlanceActivity {
    GlanceActivity {
        tool: call.name.clone(),
        target: tool_target(&call.input)
            .map(|target| truncate(&one_line(&target), GLANCE_TARGET_BYTES)),
    }
}

/// Input keys that name a tool call's subject, most specific first. A command
/// outranks a path because a shell call's working directory is not what it
/// does, and a search pattern outranks the directory it searches.
const TARGET_KEYS: &[&str] = &[
    "command",
    "cmd",
    "file_path",
    "filePath",
    "notebook_path",
    "pattern",
    "query",
    "path",
    "absolute_path",
    "url",
    "description",
    "prompt",
];

fn tool_target(input: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    match input {
        // A raw string input (Codex `apply_patch`, freeform tools) is its own
        // subject; a patch names the files it touches.
        Value::String(text) => patch_files(text).or_else(|| non_empty(text)),
        Value::Object(map) => TARGET_KEYS
            .iter()
            .find_map(|key| match map.get(*key)? {
                Value::String(text) => patch_files(text).or_else(|| non_empty(text)),
                // `["bash", "-lc", "npm test"]`: the script is the last element.
                Value::Array(parts) => {
                    let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
                    match words.as_slice() {
                        [shell, flag, script] if is_shell(shell) && flag.starts_with('-') => {
                            non_empty(script)
                        }
                        _ => non_empty(&words.join(" ")),
                    }
                }
                _ => None,
            })
            // A patch wrapped as `{"input": "*** Begin Patch…"}` names its files;
            // any other `input` is too generic to stand for the call.
            .or_else(|| map.get("input")?.as_str().and_then(patch_files)),
        _ => None,
    }
}

fn is_shell(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    matches!(
        name.trim_end_matches(".exe"),
        "bash" | "sh" | "zsh" | "pwsh" | "powershell" | "cmd"
    )
}

/// The files an `apply_patch` envelope names, or `None` for other text.
fn patch_files(text: &str) -> Option<String> {
    if !text.trim_start().starts_with("*** Begin Patch") {
        return None;
    }
    let files: Vec<&str> = text
        .lines()
        .filter_map(|line| {
            ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .map(str::trim)
        .collect();
    (!files.is_empty()).then(|| files.join(", "))
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Collapse whitespace runs so a multi-line command reads as one line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Shape a parsed window into a glance. `describe` is the reader's own tool
/// describer, so a harness override applies to every activity line.
pub(crate) fn from_parsed(
    parsed: &Parsed,
    updated_at_ms: Option<i64>,
    describe: impl Fn(&ToolCall) -> GlanceActivity,
) -> TranscriptGlance {
    let turns: &[Turn] = &parsed.turns;
    let prompt_index = turns
        .iter()
        .rposition(|turn| turn.role == "user" && !turn.text.trim().is_empty());
    let since_prompt = &turns[prompt_index.map_or(0, |index| index + 1)..];
    let calls: Vec<&ToolCall> = since_prompt
        .iter()
        .filter(|turn| turn.role == "assistant")
        .flat_map(|turn| turn.tool_calls.iter())
        .collect();
    let activity = calls
        .iter()
        .skip(calls.len().saturating_sub(GLANCE_ACTIVITY))
        .map(|call| describe(call))
        .collect();
    let message_after_prompt = since_prompt
        .iter()
        .any(|turn| turn.role == "assistant" && !turn.text.trim().is_empty());
    TranscriptGlance {
        last_prompt: prompt_index
            .map(|index| truncate(turns[index].text.trim(), GLANCE_PROMPT_BYTES)),
        last_message: parsed
            .last_assistant_message
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| truncate(text, GLANCE_MESSAGE_BYTES)),
        // A window that never saw the prompt cannot place the message before it.
        message_after_prompt: message_after_prompt || prompt_index.is_none(),
        activity,
        tool_calls_since_prompt: calls.len(),
        updated_at_ms,
    }
}

#[cfg(test)]
#[path = "glance_tests.rs"]
mod tests;
