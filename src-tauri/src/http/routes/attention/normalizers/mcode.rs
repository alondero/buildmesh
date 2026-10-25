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
        "userpromptsubmit" => running(detail),
        "permissionrequest" => permission(detail),
        // Native reconciliation is bound to the resolved node/session by the
        // route. Its compatible transcript path is optional and often expires;
        // it must not bypass the lookup. Supply cwd as the native context hint.
        "stop" => completion_from_pending(
            detail,
            count_pending(Path::new(payload.cwd.as_deref().unwrap_or_default())),
        ),
        _ => unavailable(detail),
    }
}

pub(in crate::http::routes::attention) fn resolve_attention_node(
    addressed: Option<crate::models::AgentNode>,
    payload: Option<&HookPayload>,
    generic_mcode: bool,
    resolve_mcode: impl FnOnce(&str, &str) -> Result<Option<crate::models::AgentNode>, rusqlite::Error>,
) -> Result<Option<crate::models::AgentNode>, rusqlite::Error> {
    let mcode_session_id =
        payload.and_then(|payload| hook_session_id_from_payload(payload, "mcode"));
    if generic_mcode
        || mcode_session_id.is_some()
        || addressed
            .as_ref()
            .is_some_and(|node| super::provider_for(&node.provider) == "mcode")
    {
        // A payload without a session id or a cwd names no node: that is
        // absence, and it stays `None`. A *failed read* is unknown and
        // travels out as an error (issue #1533).
        let Some(session_id) = mcode_session_id else {
            return Ok(None);
        };
        let Some(cwd) = payload.and_then(|payload| payload.cwd.as_deref()) else {
            return Ok(None);
        };
        resolve_mcode(&session_id, cwd)
    } else {
        Ok(addressed)
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
