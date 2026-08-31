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
    crate::commands::attention::mark_attention_with_signal(node_id, app, semantic_turn, &detail);
    publish_passive(node_id, app);
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
    publish_passive(node_id, app);
    let _ = crate::agent::session_lifecycle::on_turn_completed(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node_id,
        &detail,
    );
}

/// Publish a Node Turn that is only a background-wait yield (issue #878,
/// #1364): the harness ended its turn with background tasks still running
/// and will re-invoke itself, so the user is NOT needed. No status write —
/// the node stays `Running` — but the `agent-lifecycle` `BackgroundRunning`
/// event is emitted on both transports so clients can distinguish "busy on
/// background work" from "waiting for input".
pub fn publish_background(
    node_id: i64,
    app: &AppHandle,
    detail: crate::agent::session_lifecycle::HookSignalDetail,
) {
    publish_passive(node_id, app);
    let _ = crate::agent::session_lifecycle::on_background_running(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node_id,
        &detail,
    );
}

/// The attention-independent consumers, shared by both publish flavours.
fn publish_passive(node_id: i64, app: &AppHandle) {
    crate::session_naming::on_turn(node_id, app.clone());
    crate::autopilot::pipeline::on_turn(node_id, app);
}
