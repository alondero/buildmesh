//! opencode hook validation and lifecycle mapping.
use super::super::common::*;
use crate::agent::session_lifecycle::HookSignalDetail;
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(value: &Value) -> Option<HookPayload> {
    let fields = [
        COMPATIBLE_FIELDS,
        &["notification_type", "error", "agent_id"],
    ]
    .concat();
    HookPayload::parse_for(value, &fields)
}

pub(super) fn classify(
    payload: &HookPayload,
    detail: HookSignalDetail,
    count_pending: &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified {
    let child = payload.agent_id.as_deref().is_some_and(|id| !id.is_empty());
    match payload.event().as_str() {
        "session.created" => ignore(detail),
        // A native subagent is a child session (issue #2636). Mapped like
        // Claude's subagent events so the node is not Ready while one is alive.
        "subagentstart" if child => Classified {
            decision: Decision::ChildStarted,
            validated: true,
            detail,
        },
        "subagentstop" if child => Classified {
            decision: Decision::BackgroundTaskCompleted,
            validated: true,
            detail,
        },
        "subagentstart" | "subagentstop" => ignore(detail),
        "session.idle" => Classified::ready(detail),
        "session.busy" | "userpromptsubmit" | "permission.replied" | "question.replied"
        | "question.rejected" => running(detail),
        "permission.asked" => permission(detail),
        "question.asked" => question(payload, detail),
        "session.error" if payload.error.as_deref() == Some("MessageAbortedError") => {
            interrupted(detail)
        }
        "session.error" => failure(detail),
        "notification" if payload.notification_type.as_deref() == Some("permission_prompt") => {
            permission(detail)
        }
        "stop" => completion(payload, detail, count_pending),
        _ => unavailable(detail),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "opencode",
            include_str!("../../../../../tests/fixtures/attention/opencode.json"),
        );
    }
}
