//! kimi hook validation and lifecycle mapping.
use super::super::common::*;
use crate::agent::session_lifecycle::HookSignalDetail;
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(value: &Value) -> Option<HookPayload> {
    let fields = [
        COMPATIBLE_FIELDS,
        &[
            "notification_type",
            "tool_output",
            "source_kind",
            "source_id",
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
    if payload.event() == "notification" {
        let terminal = payload.source_kind.as_deref() == Some("background_task")
            && matches!(
                payload.notification_type.as_deref(),
                Some(
                    "task.completed"
                        | "task.failed"
                        | "task.killed"
                        | "task.timed_out"
                        | "task.lost"
                )
            );
        return if terminal {
            Classified {
                decision: Decision::BackgroundTaskCompleted,
                validated: true,
                detail,
            }
        } else {
            ignore(detail)
        };
    }
    if matches!(
        payload.event().as_str(),
        "posttooluse" | "posttoolusefailure"
    ) && payload.tool_name.as_deref() == Some("AskUserQuestion")
        && payload
            .tool_input
            .as_ref()
            .and_then(|input| input.get("background"))
            .and_then(Value::as_bool)
            == Some(true)
        && payload.tool_output.as_deref().is_some_and(|output| {
            output.lines().any(|line| {
                line.strip_prefix("task_id: ")
                    .is_some_and(|id| !id.is_empty())
            })
        })
    {
        return ignore(detail);
    }
    if let Some(result) = tool_event(payload, &detail, &["AskUserQuestion"], Some("ExitPlanMode")) {
        return result;
    }
    match payload.event().as_str() {
        "sessionstart" => ignore(detail),
        "userpromptsubmit" | "permissionresult" | "elicitationresult" => running(detail),
        "permissionrequest" => permission(detail),
        "stopfailure" | "interrupt" => failure(detail),
        "stop" => completion(payload, detail, count_pending),
        _ => unavailable(detail),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "kimi",
            include_str!("../../../../../tests/fixtures/attention/kimi.json"),
        );
    }
}
