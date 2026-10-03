//! Per-node ordering and outstanding-work fences over normalized hooks.
use super::common::{Classified, Decision, HookPayload};

/// Result of one attention callback. `ask_already_resolved` is true only for
/// this callback: an ask whose reply already arrived. It is not stored on
/// the node, so a later empty or unparseable body cannot inherit it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Accept {
    pub(super) accepted: bool,
    pub(super) ask_already_resolved: bool,
    /// A reply was recorded while some other prompt is still open. Do not
    /// publish `Running` for the whole node.
    pub(super) preserve_attention: bool,
}

impl Accept {
    pub(super) fn rejected() -> Self {
        Self {
            accepted: false,
            ask_already_resolved: false,
            preserve_attention: false,
        }
    }

    pub(super) fn passthrough() -> Self {
        Self {
            accepted: true,
            ask_already_resolved: false,
            preserve_attention: false,
        }
    }
}

pub(super) fn accept_hook(
    state: &mut crate::agent::hook_state::HookState,
    payload: &HookPayload,
    classified: &Classified,
) -> Accept {
    let event = payload
        .hook_event_name
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace('_', "");
    let resets_session_state = matches!(event.as_str(), "sessionstart" | "session.created")
        && payload
            .session_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty());
    // Child work belongs to the session and can outlive its launching turn.
    // The route already checked the session identity before entering here.
    if event == "subagentstop" && classified.decision == Decision::BackgroundTaskCompleted {
        let completed = payload
            .agent_id
            .as_deref()
            .is_some_and(|id| state.finish_child(id));
        return Accept {
            accepted: completed && !state.has_children() && !state.has_questions(),
            ..Accept::passthrough()
        };
    }
    // A boot event with no session identity is not safe to use as a
    // generation boundary: it could be a delayed callback from an older
    // process. The route's session-id fence handles identified events;
    // absent identities leave the existing ordering state intact. Defer the
    // reset until the acceptance fence below so a dropped callback cannot
    // mutate ordering state.
    if event == "notification"
        && payload.source_kind.as_deref() == Some("background_task")
        && classified.decision == Decision::BackgroundTaskCompleted
    {
        let accepted = payload
            .source_id
            .as_deref()
            .is_some_and(|task| state.finish_background_task(task))
            && !state.has_questions();
        return Accept {
            accepted,
            ask_already_resolved: false,
            preserve_attention: false,
        };
    }
    let starts_turn = event == "userpromptsubmit";
    if !state.accepts(payload.turn_id.as_deref(), starts_turn) {
        return Accept::rejected();
    }
    if event == "subagentstart" {
        if let Some(id) = payload.agent_id.as_deref().filter(|id| !id.is_empty()) {
            state.start_child(id);
        }
    }
    let key = payload
        .request_id
        .as_deref()
        .or(payload.tool_name.as_deref())
        .unwrap_or("question");
    let tracks_input_request = classified.detail.kind
        == Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested)
        || matches!(event.as_str(), "permissionrequest" | "permission.asked")
        || (event == "pretooluse" && payload.tool_name.as_deref() == Some("ExitPlanMode"));
    // Peek only. The id is consumed after this callback is accepted, so a
    // rejected ask cannot eat a reply that a later callback still needs.
    let ask_already_resolved =
        tracks_input_request && event != "notification" && state.has_early_reply(key);
    if tracks_input_request && event != "notification" && !ask_already_resolved {
        if matches!(event.as_str(), "permissionrequest" | "permission.asked") {
            state.permission_request(key);
        } else {
            state.question(key, crate::agent::hook_state::QuestionKind::Foreground);
        }
    } else if matches!(event.as_str(), "posttooluse" | "posttoolusefailure")
        && payload.tool_name.as_deref() == Some("AskUserQuestion")
        && payload
            .tool_input
            .as_ref()
            .and_then(|input| input.get("background"))
            .and_then(|value| value.as_bool())
            == Some(true)
    {
        if let Some(task) = payload
            .tool_output
            .as_deref()
            .and_then(|output| {
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("task_id: "))
            })
            .filter(|task| !task.is_empty())
        {
            state.background_question(key, task);
        }
    } else if matches!(
        event.as_str(),
        "permissionresult"
            | "permission.replied"
            | "elicitationresult"
            | "question.replied"
            | "question.rejected"
    ) {
        state.resolve_reply(
            payload
                .request_id
                .as_deref()
                .or(payload.tool_name.as_deref()),
        );
    } else if matches!(event.as_str(), "posttooluse" | "posttoolusefailure") {
        state.resolve_question(
            payload
                .request_id
                .as_deref()
                .or(payload.tool_name.as_deref()),
        );
    }
    let ends_turn = matches!(event.as_str(), "stop" | "session.idle");
    // An identified Stop is Codex's only denied-permission fallback. It is an
    // explicit turn fence, so clear that permission marker before deciding
    // whether other foreground questions still block the callback. An
    // unidentified Stop remains fenced and cannot clear anything.
    if event == "stop" && payload.turn_id.is_some() {
        state.clear_permission_requests();
    }
    // Detached/background questions do not keep the foreground model turn
    // active. End it before the final question fence so a valid Kimi Stop can
    // still authorize a later task-completion notification. Foreground
    // questions, including unresolved permissions, keep the turn active until
    // their explicit reply arrives; a dropped callback never mutates it.
    if ends_turn && !state.has_foreground_questions() {
        state.note_background_snapshot(classified.decision == Decision::SuppressPendingBackground);
        state.end_turn();
    }
    // A real prompt is an explicit user action and must never be fenced by a
    // detached/background question. The background request remains tracked
    // and can still resolve later, but dropping this callback would leave the
    // node frozen in its previous attention state (review finding).
    // A reply resolves its own key. Publishing it as `Running` while another
    // question is open would resume the whole node, so the gate used to
    // reject it after the id had already been stored. Accept the reply and
    // tell the route not to resume.
    let keyed_reply = matches!(
        event.as_str(),
        "permissionresult"
            | "permission.replied"
            | "elicitationresult"
            | "question.replied"
            | "question.rejected"
    );
    let preserve_attention = keyed_reply && state.has_foreground_questions();
    let accepted = preserve_attention
        || starts_turn
        || !(state.has_questions()
            && matches!(
                classified.decision,
                Decision::Ready
                    | Decision::SuppressPendingBackground
                    | Decision::Running
                    | Decision::CodexToolResult
                    | Decision::ChildStarted
            ));
    if !accepted {
        return Accept::rejected();
    }
    if ask_already_resolved {
        state.take_early_reply(key);
    }

    if resets_session_state {
        *state = Default::default();
    }
    if event == "session.busy" {
        state.mark_turn_active();
    }
    Accept {
        accepted: true,
        ask_already_resolved,
        preserve_attention,
    }
}

pub(super) fn effective_decision(
    decision: Decision,
    state: &crate::agent::hook_state::HookState,
) -> Decision {
    if decision == Decision::ChildStarted {
        if !state.has_children() {
            Decision::Ignore
        } else if state.is_turn_active() {
            Decision::Running
        } else {
            Decision::SuppressPendingBackground
        }
    } else if decision == Decision::Ready && state.has_children() {
        Decision::SuppressPendingBackground
    } else if decision == Decision::BackgroundTaskCompleted
        && (state.is_turn_active() || state.has_children() || state.has_other_background_work())
    {
        Decision::Ignore
    } else if decision == Decision::BackgroundTaskCompleted {
        Decision::Ready
    } else {
        decision
    }
}

pub(super) fn normalize_decision(
    decision: Decision,
    state: &crate::agent::hook_state::HookState,
    codex_permission_pending: bool,
) -> Decision {
    if decision == Decision::CodexToolResult {
        if codex_permission_pending {
            Decision::Running
        } else {
            Decision::Ignore
        }
    } else {
        effective_decision(decision, state)
    }
}

/// Decision the route actually publishes. An ask whose reply already arrived
/// (`--auto` answers in the same turn, and the HTTP callbacks can reorder)
/// must not raise a banner and must not move the node. The reply's own
/// callback is what resumes work. Publishing `Running` here would undo a
/// later `session.idle` that landed before this late ask.
pub(super) fn lifecycle_decision(
    decision: Decision,
    state: &crate::agent::hook_state::HookState,
    codex_permission_pending: bool,
    accept: Accept,
) -> Decision {
    let decision = normalize_decision(decision, state, codex_permission_pending);
    if accept.ask_already_resolved || accept.preserve_attention {
        Decision::Ignore
    } else {
        decision
    }
}

use super::Applied;

/// Apply the turn fence before the route can update the node's lifecycle
/// projection. A stale native event is retained as rejected Circuit history,
/// then returned to the caller so it can take the early `StaleDropped` path.
pub(super) fn apply_hook_after_turn_fence(
    session_id: i64,
    state: &mut crate::agent::hook_state::HookState,
    payload: Option<&HookPayload>,
    classified: &Classified,
    native_hook: Option<&crate::services::circuit_worker::native_hooks::NativeHook>,
    on_accepted: impl FnOnce(
        &mut crate::agent::hook_state::HookState,
        Option<&crate::services::circuit_worker::native_hooks::NativeHook>,
        Accept,
    ) -> Result<Applied, String>,
) -> Result<Applied, String> {
    // No payload means an empty or unparseable body. That callback did not
    // resolve an ask, so the degraded mark-attention path must not inherit
    // a previous callback's answer.
    let accept = if let Some(payload) = payload {
        let accept = accept_hook(state, payload, classified);
        if !accept.accepted {
            // Preserve an explicitly old-turn native lifecycle event for
            // Circuit history while keeping it out of the active session
            // projection. A missing optional turn ID is not an explicit
            // mismatch and creates no native receipt on this rejected path.
            if let Some(hook) =
                native_hook.filter(|hook| state.mismatches_turn(hook.turn_id.as_deref()))
            {
                crate::services::circuit_worker::native_hooks::receive(
                    session_id,
                    hook.clone(),
                    false,
                    true,
                )?;
            }
            return Ok(Applied::StaleDropped);
        }
        accept
    } else {
        Accept::passthrough()
    };
    on_accepted(state, native_hook, accept)
}
