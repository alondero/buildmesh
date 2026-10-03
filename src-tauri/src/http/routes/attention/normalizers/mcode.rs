//! mcode hook validation and lifecycle mapping.
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
        "sessionstart" => ignore(detail),
        "permissionrequest" => permission(detail),
        "stop" => completion(payload, detail, count_pending),
        _ => unavailable(detail),
    }
}

pub(in crate::http::routes::attention) fn resolve_attention_node(
    addressed: Option<crate::models::AgentNode>,
    payload: Option<&HookPayload>,
    generic_mcode: bool,
    resolve_mcode: impl FnOnce(&str, &str) -> Option<crate::models::AgentNode>,
) -> Option<crate::models::AgentNode> {
    let mcode_session_id =
        payload.and_then(|payload| hook_session_id_from_payload(payload, "mcode"));
    if generic_mcode
        || mcode_session_id.is_some()
        || addressed
            .as_ref()
            .is_some_and(|node| super::provider_for(&node.provider) == "mcode")
    {
        resolve_mcode(&mcode_session_id?, payload?.cwd.as_deref()?)
    } else {
        addressed
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "mcode",
            include_str!("../../../../../tests/fixtures/attention/mcode.json"),
        );
    }
}
