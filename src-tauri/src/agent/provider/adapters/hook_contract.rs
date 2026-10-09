//! The Claude-style hook request contract that Claude Code and Codex both
//! deliver to the attention route. It is shared by those two adapters only:
//! each declares its own strategy and calls [`parse`] with its own id, so the
//! recorded provider (and therefore replay) always names the harness that
//! parsed the hook. No other harness reaches this parser.

use crate::circuit::observation::{HumanWaitKind as Kind, ObservedWorkFact as Fact};
use crate::circuit::strategy::{submission_digest, NativeHook};
use std::collections::BTreeSet;

/// Parse a Claude-style lifecycle/ownership hook.
///
/// `submission_echo` is true only for Claude Code, whose `UserPromptSubmit`
/// carries the verbatim prompt that proves which Buildmesh submission a turn
/// acknowledges (issue #1898). Codex never claims one.
pub(super) fn parse(
    harness: &'static str,
    submission_echo: bool,
    value: &serde_json::Value,
) -> Option<NativeHook> {
    let event = value
        .get("hook_event_name")
        .or_else(|| value.get("hookEventName"))
        .or_else(|| value.get("hookName"))?
        .as_str()?;
    let human_fact = human_fact(value);
    // Codex's PermissionRequest contract has no stable request id. Keep
    // the event as an uncorrelated permission wait instead of dropping
    // it or inventing a correlation token.
    if human_fact.is_none()
        && !matches!(
            event,
            "UserPromptSubmit"
                | "Stop"
                | "SubagentStart"
                | "SubagentStop"
                | "PermissionRequest"
                | "StopFailure"
        )
    {
        return None;
    }
    let token = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    // The turn token naming the prompt the harness is processing. Claude
    // Code's documented spelling is `prompt_id`; the other aliases are
    // the shapes sibling harnesses use for the same fact.
    let turn_id = ["prompt_id", "promptId", "turn_id"]
        .iter()
        .find_map(|key| token(key));
    let active_work = (|| {
        let tasks = value.get("background_tasks")?.as_array()?;
        let crons = value.get("session_crons")?.as_array()?;
        let mut active = BTreeSet::new();
        for (prefix, items) in [("task", tasks), ("cron", crons)] {
            for item in items {
                let id = item.get("id")?.as_str()?.trim();
                if id.is_empty() {
                    return None;
                }
                active.insert(format!("{prefix}:{id}"));
            }
        }
        Some(active)
    })();
    Some(NativeHook {
        session_id: token("session_id")
            .or_else(|| token("sessionId"))
            .or_else(|| token("sessionID"))
            .or_else(|| token("conversationId"))
            .or_else(|| token("conversation_id"))
            .or_else(|| token("taskId")),
        turn_id: turn_id.clone(),
        event: event.into(),
        child_id: token("agent_id"),
        human_fact,
        background_busy: false,
        execution_num: None,
        termination_reason: None,
        provider: Some(harness.into()),
        active_work,
        prompt_digest: (submission_echo && event == "UserPromptSubmit" && turn_id.is_some())
            .then(|| {
                value
                    .get("prompt")
                    .and_then(|text| text.as_str())
                    // An empty echo identifies nothing: accepting one
                    // would be a bare turn token with no content proof.
                    .filter(|text| !text.is_empty())
                    .map(submission_digest)
            })
            .flatten(),
        final_report: if event == "Stop" {
            token("last_assistant_message")
                .map(|text| crate::secret_scrubber::SecretScrubber::scrub(&text))
        } else {
            None
        },
    })
}

// Tool names identify request kind, never request identity.
fn human_fact(value: &serde_json::Value) -> Option<Fact> {
    let event = value
        .get("hook_event_name")
        .or_else(|| value.get("hookEventName"))
        .or_else(|| value.get("hookName"))?
        .as_str()?;
    let request_id = [
        "request_id",
        "tool_use_id",
        "toolUseId",
        "requestId",
        "requestID",
        "elicitation_id",
        "toolCallId",
        "tool_call_id",
        "callId",
        "call_id",
        "permissionID",
        "permission_id",
    ]
    .iter()
    .find_map(|key| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
    })?;
    let tool = value
        .get("tool_name")
        .or_else(|| value.get("toolName"))
        .and_then(|v| v.as_str());
    let wait_kind = match tool {
        Some("AskUserQuestion" | "request_user_input" | "ask_user_question") => Kind::Question,
        Some("ExitPlanMode") => Kind::ReviewApproval,
        _ => Kind::Permission,
    };
    match event {
        "PermissionRequest" => Some(Fact::HumanWaitRequested {
            wait_kind: Kind::Permission,
            request_id: request_id.into(),
        }),
        "PreToolUse"
            if matches!(
                tool,
                Some(
                    "AskUserQuestion" | "request_user_input" | "ask_user_question" | "ExitPlanMode"
                )
            ) =>
        {
            Some(Fact::HumanWaitRequested {
                wait_kind,
                request_id: request_id.into(),
            })
        }
        "PostToolUse" => Some(Fact::ToolResponse {
            wait_kind,
            request_id: request_id.into(),
        }),
        "PostToolUseFailure" => Some(Fact::ToolFailed {
            wait_kind,
            request_id: request_id.into(),
        }),
        "PermissionResult" => Some(Fact::HumanResponse {
            wait_kind: Kind::Permission,
            request_id: request_id.into(),
        }),
        _ => None,
    }
}
