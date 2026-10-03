//! grok hook validation and lifecycle mapping.
use super::super::common::*;
use crate::agent::session_lifecycle::HookSignalDetail;
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(value: &Value) -> Option<HookPayload> {
    let fields = [COMPATIBLE_FIELDS, &["notification_type", "reason", "error"]].concat();
    HookPayload::parse_for(value, &fields)
}

pub(super) fn classify(
    payload: &HookPayload,
    detail: HookSignalDetail,
    count_pending: &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified {
    if let Some(result) = tool_event(payload, &detail, &["ask_user_question"], None) {
        return result;
    }
    match payload.event().as_str() {
        "sessionstart" => ignore(detail),
        "userpromptsubmit" => running(detail),
        "stopfailure" | "stopcancelled" => failure(detail),
        "notification" if payload.notification_type.as_deref() == Some("permission_prompt") => {
            permission(detail)
        }
        "notification"
            if matches!(
                payload.notification_type.as_deref(),
                Some("question" | "question_prompt" | "ask_user")
            ) =>
        {
            question(payload, detail)
        }
        "notification" if payload.notification_type.as_deref() == Some("task_complete") => {
            Classified::ready(detail)
        }
        "stop" | "notification" => completion(payload, detail, count_pending),
        _ => unavailable(detail),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "grok",
            include_str!("../../../../../tests/fixtures/attention/grok.json"),
        );
    }
}
