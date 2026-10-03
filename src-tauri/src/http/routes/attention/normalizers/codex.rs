//! codex hook validation and lifecycle mapping.
use super::super::common::*;
use crate::agent::session_lifecycle::{HookSignalDetail, LifecycleKind};
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
    if payload.event() == "posttoolusefailure" {
        return failure(detail);
    }
    if payload.event() == "posttooluse"
        && payload.tool_name.as_deref() != Some("request_user_input")
    {
        return Classified {
            decision: Decision::CodexToolResult,
            validated: true,
            detail: HookSignalDetail {
                kind: Some(LifecycleKind::WorkResumed),
                ..detail
            },
        };
    }
    if let Some(result) = tool_event(payload, &detail, &["request_user_input"], None) {
        return result;
    }
    match payload.event().as_str() {
        "sessionstart" => ignore(detail),
        "userpromptsubmit" | "permissionresult" => running(detail),
        "permissionrequest" => permission(detail),
        "stopfailure" | "stopcancelled" | "interrupt" => failure(detail),
        "stop" => completion(payload, detail, count_pending),
        _ => unavailable(detail),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "codex",
            include_str!("../../../../../tests/fixtures/attention/codex.json"),
        );
    }
}
