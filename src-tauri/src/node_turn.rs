//! Node Turn — the point at which an Agent Node yields control back to the user
//! (see CONTEXT.md "Node Turn"). Claude Code surfaces this as several hooks (the
//! Stop hook = awaiting input, plus the catch-all Notification hook = idle or
//! permission prompt); Buildmesh treats them as one undifferentiated signal,
//! because all are yields.
//!
//! A Node Turn is the single inbound fact that fans out to two *independent*
//! reactions: marking the node for attention (`commands::attention`) and
//! considering an AI rename (`session_naming`). This module is the seam where
//! that fan-out lives, so neither reaction has to know about the other — before
//! this, `mark_attention` reached directly into `session_naming::on_turn`,
//! coupling two orthogonal concerns through one call site.
//!
//! Deliberately NOT an event bus: the two reactions are fixed, and the only
//! other reader of node state — the Coordinator — reads via the HTTP control
//! API, not this in-process seam. A subscriber registry would be speculative
//! generality (one adapter, not two varying ones).

use tauri::AppHandle;

/// Publish a Node Turn for `node_id`: its agent has yielded control back to the
/// user. Fans out to the three independent consumers (attention marking, AI
/// rename, and — for Autopilot-managed nodes only — the wrap-up pipeline's
/// state evaluation, issues #483-#485).
pub fn publish(node_id: i64, app: &AppHandle) {
    crate::commands::attention::mark_attention(node_id, app);
    publish_passive(node_id, app);
}

pub fn publish_with_signal(
    node_id: i64,
    app: &AppHandle,
    semantic_turn: Option<crate::agent::session_lifecycle::SemanticTurnPayload>,
    detail: crate::agent::session_lifecycle::HookSignalDetail,
) {
    let detail = crate::agent::session_lifecycle::HookSignalDetail {
        semantic_turn,
        ..detail
    };
    publish_hook(
        node_id,
        app,
        detail
            .kind
            .unwrap_or(match detail.semantic_turn.as_ref().map(|turn| turn.kind) {
                Some(
                    crate::agent::session_lifecycle::SemanticTurnKind::PermissionRequest
                    | crate::agent::session_lifecycle::SemanticTurnKind::CommandConfirmation,
                ) => crate::agent::session_lifecycle::LifecycleKind::PermissionRequested,
                _ => crate::agent::session_lifecycle::LifecycleKind::InputRequired,
            }),
        detail,
    );
}

/// Publish a Node Turn that is a clean turn completion (issue #1364): the
/// harness signalled the turn finished with no user input needed and no
/// background work pending. The node lands in `Ready` (never `Completed`).
/// Naming and the autopilot pipeline still see the turn via
/// [`publish_passive`]; the lifecycle writes `Ready` and emits
/// `agent-lifecycle` on both transports.
pub fn publish_ready(
    node_id: i64,
    app: &AppHandle,
    detail: crate::agent::session_lifecycle::HookSignalDetail,
) {
    publish_hook(
        node_id,
        app,
        crate::agent::session_lifecycle::LifecycleKind::TurnCompleted,
        detail,
    );
}

pub(crate) fn recover_ready(
    node_id: i64,
    app: &AppHandle,
    detail: crate::agent::session_lifecycle::HookSignalDetail,
    recovery: &crate::agent::session_lifecycle::CircuitTurnRecovery<'_>,
) {
    match crate::agent::session_lifecycle::recover_turn_completed(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node_id,
        &detail,
        recovery,
    ) {
        Ok(true) => publish_passive(node_id, app),
        Ok(false) => {}
        Err(error) => tracing::warn!(node_id, %error, "failed to recover ready turn"),
    }
}

#[cfg(test)]
fn publish_ready_with_sink(
    sink: &dyn crate::agent::session_lifecycle::SessionLifecycleSink,
    node_id: i64,
    detail: &crate::agent::session_lifecycle::HookSignalDetail,
    on_ready: impl FnOnce(),
) {
    publish_hook_with_sink(
        sink,
        node_id,
        crate::agent::session_lifecycle::LifecycleKind::TurnCompleted,
        detail,
        on_ready,
    );
}

/// Publish one normalized hook fact. Ordering/ownership are checked by the caller;
/// lifecycle commits precede the independent rename consumer.
pub(crate) fn publish_hook(
    node_id: i64,
    app: &AppHandle,
    kind: crate::agent::session_lifecycle::LifecycleKind,
    detail: crate::agent::session_lifecycle::HookSignalDetail,
) {
    publish_hook_with_sink(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node_id,
        kind,
        &detail,
        || publish_passive(node_id, app),
    );
}

pub(crate) fn publish_hook_with_sink(
    sink: &dyn crate::agent::session_lifecycle::SessionLifecycleSink,
    node_id: i64,
    kind: crate::agent::session_lifecycle::LifecycleKind,
    detail: &crate::agent::session_lifecycle::HookSignalDetail,
    on_turn: impl FnOnce(),
) {
    use crate::agent::session_lifecycle::{self, LifecycleKind};
    let result = match kind {
        LifecycleKind::WorkResumed => {
            session_lifecycle::on_hook_running_with_detail(sink, node_id, detail)
        }
        LifecycleKind::TurnCompleted => session_lifecycle::on_turn_completed(sink, node_id, detail),
        LifecycleKind::BackgroundRunning => {
            session_lifecycle::on_background_running(sink, node_id, detail)
        }
        LifecycleKind::InputRequired
        | LifecycleKind::PermissionRequested
        | LifecycleKind::QuestionRequested
        | LifecycleKind::Error
        | LifecycleKind::SignalUnavailable => session_lifecycle::on_attention_with_signal(
            sink,
            node_id,
            detail.semantic_turn.clone(),
            detail,
        ),
        _ => return,
    };
    match result {
        Ok(true) if kind != LifecycleKind::WorkResumed => {
            if !matches!(
                kind,
                LifecycleKind::TurnCompleted | LifecycleKind::BackgroundRunning
            ) {
                crate::attention_autoclear::on_signal_marked(
                    node_id,
                    detail.kind,
                    detail.semantic_turn.as_ref().map(|turn| turn.kind),
                );
            }
            on_turn();
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(node_id, %error, "failed to publish normalized hook"),
    }
}

/// The attention-independent consumers, shared by both publish flavours.
fn publish_passive(node_id: i64, app: &AppHandle) {
    crate::session_naming::on_turn(node_id, app.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session_lifecycle::{testing::RecordingSink, HookSignalDetail};
    use crate::models::SessionStatus;
    use std::cell::Cell;

    #[test]
    fn ready_consumers_observe_persisted_ready_and_cleared_attention() {
        let sink = RecordingSink::with_status(SessionStatus::AwaitingInput);
        let called = Cell::new(false);
        publish_ready_with_sink(&sink, 42, &HookSignalDetail::default(), || {
            assert_eq!(sink.status(), Some(SessionStatus::Ready));
            assert_eq!(
                sink.effects(),
                vec![
                    "status-written",
                    "autoclear-disarmed",
                    "attention-cleared",
                    "lifecycle-emitted",
                ]
            );
            called.set(true);
        });
        assert!(called.get());
    }

    #[test]
    fn ready_consumers_do_not_run_after_failed_or_rejected_transition() {
        for sink in [
            RecordingSink::failing_writes(),
            RecordingSink::with_status(SessionStatus::Completed),
        ] {
            let called = Cell::new(false);
            publish_ready_with_sink(&sink, 42, &HookSignalDetail::default(), || called.set(true));
            assert!(!called.get());
            assert!(sink.effects().is_empty());
        }
    }
}
