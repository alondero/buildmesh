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

/// Publish a Node Turn that is only a background-wait yield (issue #878): the
/// harness ended its turn with background tasks still running and will
/// re-invoke itself, so the user is NOT needed. Naming and the autopilot
/// pipeline still see the turn (both are safe on a non-final turn — rename is
/// idempotent, the pipeline classifies the tail and no-ops on "working"), but
/// the node is not marked for attention.
pub fn publish_without_attention(node_id: i64, app: &AppHandle) {
    publish_passive(node_id, app);
}

/// The attention-independent consumers, shared by both publish flavours.
fn publish_passive(node_id: i64, app: &AppHandle) {
    crate::session_naming::on_turn(node_id, app.clone());
    crate::autopilot::pipeline::on_turn(node_id, app);
}
