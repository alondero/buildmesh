//! opencode hook validation and lifecycle mapping.
use super::super::common::*;
use crate::agent::session_lifecycle::HookSignalDetail;
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(value: &Value) -> Option<HookPayload> {
    let fields = [COMPATIBLE_FIELDS, &["notification_type"]].concat();
    HookPayload::parse_for(value, &fields)
}

pub(super) fn classify(
    payload: &HookPayload,
    detail: HookSignalDetail,
    count_pending: &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified {
    match payload.event().as_str() {
        "session.created" => ignore(detail),
        "session.idle" => Classified::ready(detail),
        "session.busy" | "userpromptsubmit" | "permission.replied" | "question.replied"
        | "question.rejected" => running(detail),
        "permission.asked" => permission(detail),
        "question.asked" => question(payload, detail),
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
