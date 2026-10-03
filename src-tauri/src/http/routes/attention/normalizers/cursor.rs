//! cursor hook validation and lifecycle mapping.
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
    match payload.event().as_str() {
        "stop" => completion(payload, detail, count_pending),
        _ => unavailable(detail),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "cursor",
            include_str!("../../../../../tests/fixtures/attention/cursor.json"),
        );
    }
}
