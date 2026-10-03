//! Provider-neutral hook data and shared Claude-compatible wire helpers.
use crate::agent::session_lifecycle::{
    HookSignalDetail, LifecycleKind, SemanticTurnKind, SignalHealth,
};
use crate::http::request;
use std::path::Path;

#[derive(serde::Deserialize, Default, Debug, Clone, PartialEq, Eq)]
pub(super) struct HookPayload {
    pub(super) agent_id: Option<String>,
    pub(super) cwd: Option<String>,
    #[serde(
        alias = "sessionId",
        alias = "sessionID",
        alias = "conversationId",
        alias = "conversation_id",
        alias = "taskId"
    )]
    pub(super) session_id: Option<String>,
    #[serde(alias = "hookEventName", alias = "hook_event_name", alias = "hookName")]
    pub(super) hook_event_name: Option<String>,
    #[serde(alias = "transcriptPath", alias = "transcript_path")]
    pub(super) transcript_path: Option<String>,
    /// Grok's structured notification type on `Notification` events
    /// (`permission_prompt`, `idle_prompt`, `task_complete`, …). The
    /// Grok docs note the matcher tests this field — we use it as a
    /// belt-and-braces parallel to Claude's `message`-substring check.
    /// Accepts both the wire camelCase (`notificationType`) and the
    /// grok-agent-sdk snake_case (`notification_type`).
    #[serde(alias = "notificationType", alias = "notification_type")]
    pub(super) notification_type: Option<String>,
    /// Notification hooks carry the human-readable notification text, e.g.
    /// "Claude needs your permission to use Bash".
    pub(super) message: Option<String>,
    /// Tool metadata used by Claude/Codex permission callbacks.
    #[serde(alias = "toolName", alias = "tool_name")]
    pub(super) tool_name: Option<String>,
    #[serde(alias = "toolInput", alias = "tool_input")]
    pub(super) tool_input: Option<serde_json::Value>,
    /// AGY's nested pre-tool envelope.
    #[serde(alias = "toolCall", alias = "tool_call")]
    pub(super) tool_call: Option<serde_json::Value>,
    /// Grok Stop callbacks carry the final assistant text inline.
    #[serde(alias = "lastAssistantMessage", alias = "last_assistant_message")]
    pub(super) last_assistant_message: Option<String>,
    /// AGY signals "the turn truly settled" with `fullyIdle: true` and
    /// "the harness is still busy on background work" with `fullyIdle:
    /// false` (issue #1285, #1367). The latter is the false-yield analogue of
    /// Claude Code's background-task detection (issue #878): we
    /// publish the turn (naming / autopilot still fire) but suppress
    /// the attention marking. Missing / unknown defaults to "idle"
    /// so a future harness that omits the field keeps working.
    #[serde(alias = "fullyIdle", alias = "fully_idle", default)]
    pub(super) fully_idle: Option<bool>,
    /// AGY's `terminationReason` (e.g. `"model_stop"`,
    /// `"tool_execution_limit_reached"`, `"error"`, `"max_steps_exceeded"`).
    /// Opaque to Buildmesh today — recorded for future debugging but not a
    /// decision input in `decide()`.
    #[serde(alias = "terminationReason", alias = "termination_reason", default)]
    pub(super) termination_reason: Option<String>,
    /// Present if the hook execution encountered an error.
    #[serde(default)]
    pub(super) error: Option<String>,
    /// AGY execution step index or invocation count.
    #[serde(alias = "executionNum", alias = "execution_num", default)]
    pub(super) execution_num: Option<i64>,
    /// AGY workspace paths.
    #[serde(alias = "workspacePaths", alias = "workspace_paths", default)]
    pub(super) workspace_paths: Option<Vec<String>>,
    /// AGY artifact directory path.
    #[serde(
        alias = "artifactDirectoryPath",
        alias = "artifact_directory_path",
        default
    )]
    pub(super) artifact_directory_path: Option<String>,
    /// AGY model name.
    #[serde(alias = "modelName", alias = "model_name", default)]
    pub(super) model_name: Option<String>,
    /// Grok Stop callbacks carry `reason` (e.g. `"end_turn"`).
    #[serde(alias = "reason", default)]
    pub(super) reason: Option<String>,
    #[serde(alias = "promptId", alias = "prompt_id")]
    pub(super) turn_id: Option<String>,
    #[serde(
        alias = "toolUseId",
        alias = "tool_use_id",
        alias = "requestID",
        alias = "requestId",
        alias = "elicitation_id",
        alias = "toolCallId",
        alias = "tool_call_id",
        alias = "callId",
        alias = "call_id",
        alias = "permissionID",
        alias = "permission_id"
    )]
    pub(super) request_id: Option<String>,
    pub(super) tool_output: Option<String>,
    pub(super) source_kind: Option<String>,
    pub(super) source_id: Option<String>,
}

impl HookPayload {
    #[cfg(test)]
    pub(super) fn parse(body: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(body).ok()?;
        Self::parse_value(&value)
    }

    #[cfg(test)]
    pub(super) fn parse_value(value: &serde_json::Value) -> Option<Self> {
        Self::parse_for(
            value,
            &[
                COMPATIBLE_FIELDS,
                &[
                    "agent_id",
                    "notification_type",
                    "tool_call",
                    "fully_idle",
                    "termination_reason",
                    "error",
                    "execution_num",
                    "workspace_paths",
                    "artifact_directory_path",
                    "model_name",
                    "reason",
                    "tool_output",
                    "source_kind",
                    "source_id",
                ],
            ]
            .concat(),
        )
    }
}

/// Provider-neutral action shown above an awaiting Agent Node's terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SemanticTurn {
    pub(super) kind: SemanticTurnKind,
    pub(super) description: String,
}

const MAX_SEMANTIC_DESCRIPTION: usize = 240;

pub(super) fn clean_description(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let mut chars = normalized.chars();
    let clipped: String = chars.by_ref().take(MAX_SEMANTIC_DESCRIPTION).collect();
    Some(if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    })
}

/// The answer choices a structured question request offered (issue #1966).
///
/// Read only from the `questions[].options[].label` list the question tools
/// already send — the same structured field the question *text* is parsed from.
/// Prose, notification messages, permission decisions, and shapes that do not
/// parse yield `None`, so a client renders an open-to-answer action rather
/// than inventing yes/no semantics.
pub(super) fn question_choices(
    payload: &HookPayload,
) -> Option<crate::agent::session_lifecycle::InputRequest> {
    let questions = payload.tool_input.as_ref()?.get("questions")?.as_array()?;
    let choices: Vec<&str> = questions
        .iter()
        .filter_map(|question| {
            question
                .get("options")
                .and_then(|options| options.as_array())
        })
        .flatten()
        .filter_map(|option| option.get("label").and_then(|label| label.as_str()))
        .collect();
    crate::agent::session_lifecycle::InputRequest::from_choices(choices)
}

pub(super) fn string_field<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_str().filter(|value| !value.trim().is_empty())
}

/// Pull a single key=value pair out of an `&`-delimited URL query
/// string. Lives in `crate::services::transcript_reader::types` so the
/// `services` layer (Grok's adapter, issue #1661) can share the parser
/// without the `http::routes::attention` module reaching back into
/// the services graph. The route uses it via this alias for tests
/// (the production code path is `GrokAdapter::verify_attention_token`,
/// not this one).
#[cfg(test)]
pub(super) fn extract_query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    crate::services::transcript_reader::types::extract_query_value(query, key)
}

/// Normalize known hook shapes without guessing from arbitrary terminal text.
pub(super) fn semantic_turn(payload: &HookPayload) -> Option<SemanticTurn> {
    let nested_name = payload
        .tool_call
        .as_ref()
        .and_then(|value| string_field(value, &["name"]));
    let tool_name = payload.tool_name.as_deref().or(nested_name);
    let command = payload
        .tool_input
        .as_ref()
        .and_then(|value| {
            string_field(value, &["command"]).or_else(|| string_field(value, &["cmd"]))
        })
        .or_else(|| {
            payload.tool_call.as_ref().and_then(|value| {
                string_field(value, &["args", "command"])
                    .or_else(|| string_field(value, &["args", "cmd"]))
            })
        });

    let permission_event = matches!(
        payload.hook_event_name.as_deref(),
        Some("PermissionRequest") | Some("PreToolUse") | Some("permission.asked")
    ) || payload.notification_type.as_deref() == Some("permission_prompt");

    if permission_event {
        if let Some(command) = command {
            return Some(SemanticTurn {
                kind: SemanticTurnKind::CommandConfirmation,
                description: clean_description(&format!("Run: {}", command.trim()))?,
            });
        }

        let path = payload.tool_input.as_ref().and_then(|value| {
            string_field(value, &["file_path"]).or_else(|| string_field(value, &["path"]))
        });
        let description = match (tool_name, path) {
            (Some(name), Some(path)) => {
                let label = if name.eq_ignore_ascii_case("edit") {
                    "edit"
                } else {
                    name.trim()
                };
                format!("Allow {}: {}", label, path.trim())
            }
            (Some(name), None) => format!("Allow: {}", name.trim()),
            (None, Some(path)) => format!("Allow: {}", path.trim()),
            _ => payload
                .message
                .as_deref()
                .map(str::trim)
                .unwrap_or("Allow tool")
                .to_owned(),
        };
        return Some(SemanticTurn {
            kind: SemanticTurnKind::PermissionRequest,
            description: clean_description(&description)?,
        });
    }

    if !payload.hook_event_name.as_deref().is_some_and(|event| {
        event.eq_ignore_ascii_case("stop") || event.eq_ignore_ascii_case("notification")
    }) {
        return None;
    }
    let description = payload
        .last_assistant_message
        .as_deref()
        .or_else(|| {
            matches!(
                payload.notification_type.as_deref(),
                Some("idle_prompt") | Some("task_complete")
            )
            .then_some(payload.message.as_deref())
            .flatten()
        })?
        .trim();
    clean_description(description).map(|description| SemanticTurn {
        kind: SemanticTurnKind::TurnFinished,
        description,
    })
}

/// Extract the provider-owned session id from a structured hook callback.
/// An arbitrary string must never enter `cli_session_id`: resume treats
/// that column as an executable CLI argument. Codex, Claude, AGY, Grok,
/// and Cursor all use UUIDs; the alias on `HookPayload::session_id`
/// makes `conversationId` (AGY) and `conversation_id` (Cursor) parse
/// through the same code path. OpenCode mints `ses_<hex+base62>` ids
/// instead (issue #1294) and MiniMax Code mints `mvs_<hex>` ids (issue
/// #1797), so this helper is **provider-aware** and dispatches to
/// `request::parse_opencode_session_id` for OpenCode (`--session <uuid>` is
/// `Invalid session ID` on the live CLI), to `request::parse_mcode_session_id`
/// for mcode, or to `request::parse_cli_session_id` for every other provider
/// (the issue #1237 UUID validator shared with `import_and_resume`). A harness
/// missing from that dispatcher has its id silently discarded, so its
/// `cli_session_id` capture no-ops.
#[cfg(test)]
pub(super) fn hook_session_id(body: &[u8], provider: &str) -> Option<String> {
    hook_session_id_from_payload(&HookPayload::parse(body)?, provider)
}

pub(super) fn hook_session_id_from_payload(
    payload: &HookPayload,
    provider: &str,
) -> Option<String> {
    let id = payload.session_id.as_deref()?;
    request::parse_session_id_for_provider(provider, id)
}

/// What to do with an incoming attention webhook (issue #1364).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    /// The harness accepted input or resolved a blocking tool request.
    Running,
    /// Codex emitted catch-all `PostToolUse`; the route must verify that an
    /// approval marker was pending before turning this into `Running`.
    CodexToolResult,
    /// The user is needed — publish the Node Turn with attention marking and
    /// land the node in `AwaitingInput`.
    MarkInput,
    /// An ordinary turn finished with no user input needed — land the node in
    /// `Ready` (never the Autopilot-only `Completed`).
    Ready,
    /// Publish the Node Turn without attention marking: the turn ended only
    /// because background tasks are still running and the harness will
    /// re-invoke itself when they finish (issue #878).
    SuppressPendingBackground,
    /// A Kimi background task reached a terminal notification. This is a
    /// correlation event, not a completion by itself: the route resolves it
    /// to `Ready` only after the foreground turn has stopped.
    BackgroundTaskCompleted,
    ChildStarted,
    /// Capture any structured session id, then stop. SessionStart (and similar
    /// boot events) must not look like a turn completion.
    Ignore,
}

impl Decision {
    pub(super) fn lifecycle_kind(self, detail: &HookSignalDetail) -> Option<LifecycleKind> {
        match self {
            Self::Running => Some(LifecycleKind::WorkResumed),
            Self::Ready => Some(LifecycleKind::TurnCompleted),
            Self::SuppressPendingBackground => Some(LifecycleKind::BackgroundRunning),
            Self::MarkInput => Some(detail.kind.unwrap_or(LifecycleKind::InputRequired)),
            _ => None,
        }
    }
}

/// The result of classifying a hook POST body: the [`Decision`] plus the
/// provider envelope that survives into the `agent-lifecycle` event.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Classified {
    pub(super) decision: Decision,
    pub(super) validated: bool,
    pub(super) detail: crate::agent::session_lifecycle::HookSignalDetail,
}

impl Classified {
    pub(super) fn mark_input(detail: crate::agent::session_lifecycle::HookSignalDetail) -> Self {
        Self {
            decision: Decision::MarkInput,
            validated: true,
            detail,
        }
    }
    pub(super) fn ready(detail: crate::agent::session_lifecycle::HookSignalDetail) -> Self {
        Self {
            decision: Decision::Ready,
            validated: true,
            detail: HookSignalDetail {
                kind: Some(LifecycleKind::TurnCompleted),
                ..detail
            },
        }
    }
    pub(super) fn suppress(detail: crate::agent::session_lifecycle::HookSignalDetail) -> Self {
        Self {
            decision: Decision::SuppressPendingBackground,
            validated: true,
            detail: HookSignalDetail {
                kind: Some(LifecycleKind::BackgroundRunning),
                ..detail
            },
        }
    }
}

pub(super) const COMPATIBLE_FIELDS: &[&str] = &[
    "session_id",
    "hook_event_name",
    "cwd",
    "transcript_path",
    "message",
    "tool_name",
    "tool_input",
    "last_assistant_message",
    "turn_id",
    "request_id",
];

impl HookPayload {
    /// Project only fields owned by this harness before validating their types.
    /// An unrelated harness's metadata cannot invalidate this envelope.
    pub(super) fn parse_for(value: &serde_json::Value, fields: &[&str]) -> Option<Self> {
        let object = value.as_object()?;
        let mut selected = serde_json::Map::new();
        for &field in fields {
            let aliases: &[&str] = match field {
                "session_id" => &[
                    "sessionId",
                    "sessionID",
                    "conversationId",
                    "conversation_id",
                    "taskId",
                ],
                "hook_event_name" => &["hookEventName", "hookName", "hook_name"],
                "transcript_path" => &["transcriptPath"],
                "notification_type" => &["notificationType"],
                "tool_name" => &["toolName"],
                "tool_input" => &["toolInput"],
                "tool_call" => &["toolCall"],
                "last_assistant_message" => &["lastAssistantMessage"],
                "fully_idle" => &["fullyIdle"],
                "termination_reason" => &["terminationReason"],
                "execution_num" => &["executionNum"],
                "workspace_paths" => &["workspacePaths"],
                "artifact_directory_path" => &["artifactDirectoryPath"],
                "model_name" => &["modelName"],
                "turn_id" => &["promptId", "prompt_id"],
                "request_id" => &[
                    "toolUseId",
                    "tool_use_id",
                    "requestID",
                    "requestId",
                    "elicitation_id",
                    "toolCallId",
                    "tool_call_id",
                    "callId",
                    "call_id",
                    "permissionID",
                    "permission_id",
                ],
                _ => &[],
            };
            if let Some(value) = object
                .get(field)
                .or_else(|| aliases.iter().find_map(|alias| object.get(*alias)))
            {
                selected.insert(field.to_owned(), value.clone());
            }
        }
        serde_json::from_value(serde_json::Value::Object(selected)).ok()
    }

    pub(super) fn event(&self) -> String {
        self.hook_event_name
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .replace('_', "")
    }
}

pub(super) fn detail(payload: &HookPayload, provider: &str) -> HookSignalDetail {
    HookSignalDetail {
        provider_event: payload.hook_event_name.clone(),
        provider_session_id: hook_session_id_from_payload(payload, provider),
        completion_reason: payload
            .termination_reason
            .clone()
            .or_else(|| payload.reason.clone()),
        transcript_path: payload.transcript_path.clone(),
        signal_health: SignalHealth::Ok,
        message: payload.message.clone(),
        notification_type: payload.notification_type.clone(),
        ..Default::default()
    }
}

pub(super) fn unavailable(detail: HookSignalDetail) -> Classified {
    Classified {
        validated: false,
        ..Classified::mark_input(HookSignalDetail {
            kind: Some(LifecycleKind::SignalUnavailable),
            signal_health: SignalHealth::Degraded,
            ..detail
        })
    }
}

pub(super) fn running(detail: HookSignalDetail) -> Classified {
    Classified {
        decision: Decision::Running,
        validated: true,
        detail: HookSignalDetail {
            kind: Some(LifecycleKind::WorkResumed),
            ..detail
        },
    }
}

pub(super) fn ignore(detail: HookSignalDetail) -> Classified {
    Classified {
        decision: Decision::Ignore,
        validated: true,
        detail,
    }
}

pub(super) fn permission(detail: HookSignalDetail) -> Classified {
    Classified::mark_input(HookSignalDetail {
        kind: Some(LifecycleKind::PermissionRequested),
        ..detail
    })
}

pub(super) fn question(payload: &HookPayload, detail: HookSignalDetail) -> Classified {
    Classified::mark_input(HookSignalDetail {
        kind: Some(LifecycleKind::QuestionRequested),
        request: question_choices(payload),
        message: payload.message.clone().or_else(|| {
            let questions = payload.tool_input.as_ref()?.get("questions")?.as_array()?;
            clean_description(
                &questions
                    .iter()
                    .filter_map(|q| q.get("question")?.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }),
        semantic_turn: None,
        ..detail
    })
}

pub(super) fn failure(detail: HookSignalDetail) -> Classified {
    Classified::mark_input(HookSignalDetail {
        kind: Some(LifecycleKind::Error),
        signal_health: SignalHealth::Degraded,
        semantic_turn: None,
        ..detail
    })
}

/// Shared mechanics for harnesses that explicitly use the same tool-hook contract.
pub(super) fn tool_event(
    payload: &HookPayload,
    detail: &HookSignalDetail,
    question_tools: &[&str],
    plan_tool: Option<&str>,
) -> Option<Classified> {
    let tool = payload.tool_name.as_deref();
    let is_question = tool.is_some_and(|name| question_tools.contains(&name));
    let is_plan = plan_tool.is_some() && tool == plan_tool;
    match payload.event().as_str() {
        "pretooluse" if is_question => Some(question(payload, detail.clone())),
        "pretooluse" if is_plan => Some(permission(detail.clone())),
        "posttooluse" | "posttoolusefailure" if is_question || is_plan => {
            Some(running(detail.clone()))
        }
        "pretooluse" | "posttooluse" => Some(ignore(detail.clone())),
        _ => None,
    }
}

pub(super) fn completion(
    payload: &HookPayload,
    detail: HookSignalDetail,
    count_pending: impl FnOnce(&Path) -> Option<usize>,
) -> Classified {
    let Some(path) = payload
        .transcript_path
        .as_deref()
        .filter(|path| !path.is_empty())
    else {
        return Classified::ready(detail);
    };
    let host_path = crate::env::to_host_path(path);
    match count_pending(Path::new(&host_path)) {
        Some(n) if n > 0 => Classified::suppress(detail),
        Some(_) => Classified::ready(detail),
        None => Classified {
            validated: true,
            ..unavailable(detail)
        },
    }
}
