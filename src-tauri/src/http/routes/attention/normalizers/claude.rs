//! claude hook validation and lifecycle mapping.
use super::super::common::*;
use crate::agent::session_lifecycle::HookSignalDetail;
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(value: &Value) -> Option<HookPayload> {
    let fields = [COMPATIBLE_FIELDS, &["agent_id", "notification_type"]].concat();
    HookPayload::parse_for(value, &fields)
}

pub(super) fn classify(
    payload: &HookPayload,
    detail: HookSignalDetail,
    count_pending: &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified {
    if let Some(result) = tool_event(payload, &detail, &["AskUserQuestion"], Some("ExitPlanMode")) {
        return result;
    }
    match payload.event().as_str() {
        "subagentstart" if payload.agent_id.as_deref().is_some_and(|id| !id.is_empty()) => {
            Classified {
                decision: Decision::ChildStarted,
                validated: true,
                detail,
            }
        }
        "subagentstop" if payload.agent_id.as_deref().is_some_and(|id| !id.is_empty()) => {
            Classified {
                decision: Decision::BackgroundTaskCompleted,
                validated: true,
                detail,
            }
        }
        "subagentstart" | "subagentstop" | "sessionstart" => ignore(detail),
        "userpromptsubmit" | "permissionresult" | "elicitationresult" => running(detail),
        "permissionrequest" => permission(detail),
        "elicitation" => question(payload, detail),
        "notification" if payload.notification_type.as_deref() == Some("elicitation_dialog") => {
            question(payload, detail)
        }
        "notification" if payload.notification_type.as_deref() == Some("auth_success") => {
            ignore(detail)
        }
        "notification"
            if payload
                .message
                .as_deref()
                .is_some_and(|m| m.to_ascii_lowercase().contains("needs your permission")) =>
        {
            permission(detail)
        }
        "stopfailure" | "stopcancelled" | "interrupt" => failure(detail),
        "stop" | "notification" => completion(payload, detail, count_pending),
        _ => unavailable(detail),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "claude",
            include_str!("../../../../../tests/fixtures/attention/claude.json"),
        );
    }
}
