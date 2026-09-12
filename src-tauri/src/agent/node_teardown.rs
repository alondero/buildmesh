//! Shared cleanup for a node that is no longer able to receive callbacks.
//!
//! Node retirement has several callers (PTY exit, user close, mesh cascade,
//! and autopilot archive), but the callback-facing state is the same. Keep
//! that invariant in one seam so a new retirement path cannot forget one of
//! the process-scoped stores.

/// Release all callback and attention state owned by `node_id`.
pub(crate) fn release(node_id: i64) {
    crate::agent::hook_state::forget(node_id);
    crate::attention_autoclear::disarm(node_id);
    crate::agent::provider::muse::telemetry::forget(node_id);
}
