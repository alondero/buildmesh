//! Muse-specific Tauri commands.
//!
//! [`get_muse_session_telemetry`] is **not** a Usage Meter. It returns
//! observed MSP session counters for one Agent Node, labelled so the
//! Usage probe cannot present them as remaining subscription quota
//! (issue #1680).

use crate::agent::provider::muse::telemetry::{self, ObservedMuseSessionTelemetry};
use tauri::command;

/// Observed Muse session telemetry for one node, or `None` when this node
/// has no captured MSP token/context events.
#[command]
pub fn get_muse_session_telemetry(node_id: i64) -> Option<ObservedMuseSessionTelemetry> {
    telemetry::snapshot(node_id)
}
