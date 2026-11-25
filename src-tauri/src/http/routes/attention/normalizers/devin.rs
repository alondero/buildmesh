use super::super::common::*;
use crate::agent::session_lifecycle::HookSignalDetail;
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(value: &Value) -> Option<HookPayload> {
    HookPayload::parse_for(value, COMPATIBLE_FIELDS)
}

pub(super) fn classify(
    payload: &HookPayload,
    detail: HookSignalDetail,
    count_pending: &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified {
    if let Some(result) = tool_event(
        payload,
        &detail,
        &["ask_user_question"],
        Some("exit_plan_mode"),
    ) {
        return result;
    }
    match payload.event().as_str() {
        "sessionstart" | "sessionend" | "postcompaction" => ignore(detail),
        "userpromptsubmit" => running(detail),
        "permissionrequest" => permission(detail),
        "stop" => completion(payload, detail, count_pending),
        _ => unavailable(detail),
    }
}

#[cfg(test)]
#[path = "devin_tests.rs"]
mod tests;
