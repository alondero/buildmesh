//! Payload for the `node-activated` event.
//!
//! The frontend listens for this event. Only the development test bridge emits
//! it. That bridge is compiled when `debug_assertions` or the `dev-tools`
//! feature is on; the release entry points do not pass that feature. This
//! file does not prove a release binary omits the listener. The type stays
//! here so the generated binding does not pull the bridge module into production.

use serde::Serialize;
use ts_rs::TS;

/// Payload of the `node-activated` Tauri event. Emitted only by the
/// HTTP-based E2E test server (`handle_set_active_node`) — production never
/// broadcasts it; the active-node state is read via the IPC refetch.
///
/// Generated to `src/types/generated/NodeActivatedPayload.ts`; the TS half
/// is imported by `src/stores/agentNodeListeners.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "NodeActivatedPayload.ts")]
#[cfg_attr(not(any(debug_assertions, feature = "dev-tools")), allow(dead_code))]
pub struct NodeActivatedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
}
