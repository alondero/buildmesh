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
    let mut payload = HookPayload::parse_for(value, &fields)?;
    // AGY nests the tool as `toolCall: {name, args}`.
    if let Some(call) = payload.tool_call.as_ref() {
        payload.tool_name = payload
            .tool_name
            .take()
            .or_else(|| call.get("name")?.as_str().map(str::to_owned));
        payload.tool_input = payload
            .tool_input
            .take()
            .or_else(|| call.get("args").cloned());
    }
    Some(payload)
}

pub(super) fn classify(
    payload: &HookPayload,
    detail: HookSignalDetail,
    count_pending: &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified {
    // Antigravity's Stop payload names no event (1.3.5). Only Stop is
    // provisioned, so a Stop-shaped payload is that Stop; naming it lets the
    // callback count as delivery evidence.
    let detail = if payload.event().is_empty()
        && (payload.termination_reason.is_some() || payload.fully_idle.is_some())
    {
        HookSignalDetail {
            provider_event: Some("Stop".into()),
            ..detail
        }
    } else {
        detail
    };
    if let Some(result) = tool_event(payload, &detail, &["ask_question"], None) {
        return result;
    }
    match payload.event().as_str() {
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
