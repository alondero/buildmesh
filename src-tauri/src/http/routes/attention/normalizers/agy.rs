//! agy hook validation and lifecycle mapping.
use super::super::common::*;
use crate::agent::session_lifecycle::HookSignalDetail;
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(value: &Value) -> Option<HookPayload> {
    let fields = [
        COMPATIBLE_FIELDS,
        &[
            "tool_call",
            "fully_idle",
            "termination_reason",
            "execution_num",
            "workspace_paths",
            "artifact_directory_path",
            "model_name",
            "error",
        ],
    ]
    .concat();
    HookPayload::parse_for(value, &fields)
}

pub(super) fn classify(
    payload: &HookPayload,
    detail: HookSignalDetail,
    count_pending: &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified {
    match payload.event().as_str() {
        "pretooluse" => ignore(detail),
        "stop" | "" if payload.fully_idle == Some(false) => Classified::suppress(detail),
        "stop" => completion(payload, detail, count_pending),
        "" if payload.termination_reason.is_some() || payload.session_id.is_some() => {
            completion(payload, detail, count_pending)
        }
        _ => unavailable(detail),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "agy",
            include_str!("../../../../../tests/fixtures/attention/agy.json"),
        );
    }
}
