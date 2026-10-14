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
//! teardown. [`release_after_delete`] retires that channel and removes saved
//! handover files only after the row delete has committed.

/// Release all callback and attention state owned by `node_id`.
///
/// Process-scoped state only. The node's PTY broadcast channel survives,
/// because a killed or exited agent is restarted under the same node id.
pub(crate) fn release(node_id: i64) {
    crate::agent::hook_state::forget(node_id);
    crate::attention_autoclear::disarm(node_id);
    crate::agent::provider::muse::telemetry::forget(node_id);
}

/// Release callback state, retire the PTY broadcast channel, and remove saved
/// handover files after the node's row is **already gone**.
///
/// Call this only once the deletion has committed. Retiring a channel records a
/// tombstone that refuses future channel creation, so a node whose row survived
/// a failed delete would be stranded with no way to stream again (#2019). When
/// the delete is still in flight, `agent_node::delete` releases the
/// process-scoped stores before committing the row delete. This second release
/// closes the interval in which callbacks could register state again.
pub(crate) fn release_after_delete(node_id: i64, app_data_dir: Option<&std::path::Path>) {
    release(node_id);
    crate::http::ws::retire_pty_channel(node_id);
    crate::agent::handover::remove_node_files(node_id, app_data_dir);
}
