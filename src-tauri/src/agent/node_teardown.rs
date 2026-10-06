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
//! teardown. Only [`release_after_delete`], and the explicit
//! [`crate::http::ws::retire_pty_channel`] call inside `agent_node::delete`,
//! may retire that channel — and only after the row delete has committed.

/// Release all callback and attention state owned by `node_id`.
///
/// Process-scoped state only. The node's PTY broadcast channel survives,
/// because a killed or exited agent is restarted under the same node id.
pub(crate) fn release(node_id: i64) {
    crate::agent::hook_state::forget(node_id);
    crate::attention_autoclear::disarm(node_id);
    crate::agent::provider::muse::telemetry::forget(node_id);
}

/// Release every process-scoped store for a node whose row is **already
/// gone**, and retire its PTY broadcast channel.
///
/// Call this only once the deletion has committed. Retiring a channel records a
/// tombstone that refuses future channel creation, so a node whose row survived
/// a failed delete would be stranded with no way to stream again (#2019). When
/// the delete is still in flight, call [`release`] for the process-scoped stores
/// and retire the channel afterwards — that is the shape `agent_node::delete`
/// uses, because its per-node cleanup deliberately runs before the row delete.
pub(crate) fn release_after_delete(node_id: i64) {
    release(node_id);
    crate::http::ws::retire_pty_channel(node_id);
}
