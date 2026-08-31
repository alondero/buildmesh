use serde::Serialize;
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Wire types — Tauri event payloads (single source of truth, issue #359)
// ---------------------------------------------------------------------------

/// Payload of the `naming-backend-failed` Tauri event. Emitted from
/// [`on_turn_with`] after `MAX_RENAME_ATTEMPTS` failed LLM calls for a single
/// node (the sticky-lockout branch); the frontend listener surfaces it as a
/// toast pointing the user at Settings → Auto-naming.
///
/// Generated to `src/types/generated/NamingBackendFailedPayload.ts`; the TS
/// half is imported by [`crate::hooks::use_naming_backend_failure_toast`]
/// (frontend). Intentionally minimal so a follow-up that adds a deep-link
/// to the Auto-naming Settings picker can extend it without a wire-shape
/// break — the existing frontend listener re-renders the toast with the new
/// field without a coordination cutover (issue #846).
///
/// `node_id` is `i64` on the wire (matches every other id in the project)
/// but tagged `#[ts(as = "i32")]` so ts-rs emits `number` rather than the
/// `bigint` it would default to — `serde_json` sends `i64` as a JS number,
/// not a BigInt, so the TS type must agree.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "NamingBackendFailedPayload.ts")]
pub struct NamingBackendFailedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub reason: String,
}

/// Payload of the `node-renamed` Tauri event. Emitted by the LLM rename
/// pipeline ([`on_turn_with`]) AND by the post-spawn preassigned-name
/// adoption path in `crate::agent::spawn` (which re-labels the throwaway
/// slug the row was created under). The frontend listener patches the node
/// list in place.
///
/// Generated to `src/types/generated/NodeRenamedPayload.ts`; the TS half is
/// imported by `src/stores/agentNodeStore.ts`. Renamed from `session-renamed`
/// alongside the IPC rename in issue #490 — the wire key is `node_id`, NOT
/// `session_id`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "NodeRenamedPayload.ts")]
pub struct NodeRenamedPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub name: String,
}
