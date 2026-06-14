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
/// user. Fans out to the two independent consumers.
pub fn publish(node_id: i64, app: &AppHandle) {
    crate::commands::attention::mark_attention(node_id, app);
    crate::session_naming::on_turn(node_id, app.clone());
}
