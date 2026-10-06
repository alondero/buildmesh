//! Shared cleanup for a node that is no longer able to receive callbacks.
//!
//! Node retirement has several callers (PTY exit, user close, mesh cascade,
//! and autopilot archive), but the callback-facing state is the same. Keep
//! that invariant in one seam so a new retirement path cannot forget one of
//! the process-scoped stores.
//!
//! Process exit and permanent deletion are deliberately *different* calls
//! (issue #2019): a restart reuses the node id, so its PTY fanout channel and
//! the terminal context a reconnecting client replays must survive process
//! teardown. Only [`release_for_deleted_node`] may retire that channel.

/// Release all callback and attention state owned by `node_id`.
///
/// Process-scoped state only. The node's PTY broadcast channel survives,
/// because a killed or exited agent is restarted under the same node id.
pub(crate) fn release(node_id: i64) {
    crate::agent::hook_state::forget(node_id);
    crate::attention_autoclear::disarm(node_id);
    crate::agent::provider::muse::telemetry::forget(node_id);
}

/// Release every process-scoped store for a **permanently deleted** node and
/// retire its PTY broadcast channel.
///
/// This is the deletion-only half of [`release`]. Calling [`release`] from a
/// process-exit path leaves the channel in place on purpose: the retained
/// terminal context is intentional, and the node id comes back on restart.
/// Calling *this* from a path that is not permanent deletion would strand a
/// live node with no fanout, so the two entry points stay separate rather than
/// being merged behind a flag.
pub(crate) fn release_for_deleted_node(node_id: i64) {
    release(node_id);
    crate::http::ws::retire_pty_channel(node_id);
}
