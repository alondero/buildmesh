use serde::Serialize;
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Wire types — Tauri event payloads (issue #161)
// ---------------------------------------------------------------------------

/// Payload of the `agent-output` Tauri event. Production PTY bytes go
/// over the binary Channel (`OutputSink`), not this event — mixing the
/// two IPC paths has no ordering and would split ANSI. `line` remains
/// the wire shape for test injection (`inject_test_output`). `data`
/// (base64) is kept so older listeners still typecheck.
///
/// Exactly one of `data` (base64-encoded bytes) or `line` (raw UTF-8
/// string) is populated — the listener branches on which is `Some`. The
/// empty-both case is meaningless and ignored.
///
/// Generated to `src/types/generated/AgentOutputPayload.ts`; the TS half is
/// imported by `src/components/Terminal/TerminalRegistry.ts`. The wire key is
/// `session_id` (NOT `node_id`) — historically this mismatched the test
/// server's emit (which used `node_id`) and silently dropped test
/// injections; issue #161 realigns both sides on `session_id`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AgentOutputPayload.ts")]
pub struct AgentOutputPayload {
    #[ts(as = "i32")]
    pub session_id: i64,
    pub data: Option<String>,
    pub line: Option<String>,
}

/// Payload of the `agent-spawned` Tauri event. Emitted once stage-2 of the
/// two-stage spawn (the slow path that registers the process in
/// `PROCESS_REGISTRY`) completes. The frontend listener re-pushes the
/// terminal's fitted dimensions via `resize_agent`, closing the auto-spawn
/// attach-fit race (issue #332).
///
/// Generated to `src/types/generated/AgentSpawnedPayload.ts`; the TS half is
/// imported by `src/components/Terminal/TerminalRegistry.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "AgentSpawnedPayload.ts")]
pub struct AgentSpawnedPayload {
    #[ts(as = "i32")]
    pub session_id: i64,
    #[ts(as = "i32")]
    pub rows: i32,
    #[ts(as = "i32")]
    pub cols: i32,
}

/// Payload of the `provider-error` Tauri event. Emitted by the sandbox-spawn
/// and direct-pty branches when the agent CLI could not be launched
/// (network denied, AppContainer mis-config, cwrap spawn failed, etc). The
/// frontend surfaces it as an error toast; the `provider` field drives the
/// toast label so the user knows which harness choked.
///
/// Generated to `src/types/generated/ProviderErrorPayload.ts`; the TS half is
/// imported by `src/App.tsx`. `session_id` is included for completeness
/// (lets future listners jump to the failing node) — pre-#161 the inline TS
/// type only declared `provider` + `message` and silently dropped it.
/// `provider` is the typed [`Provider`] enum (not a plain string) so the
/// serde-derived lowercase wire value (`"anthropic"`, `"codex"`, …) is
/// preserved across the typed rewrite.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "ProviderErrorPayload.ts")]
pub struct ProviderErrorPayload {
    #[ts(as = "i32")]
    pub session_id: i64,
    pub provider: crate::models::Provider,
    pub message: String,
}

/// Payload of the `mesh-sync-warning` Tauri event. Emitted for any non-fatal
/// auto-sync failure (network down, diverged history, repo unusable, PR-head
/// unfetchable, PR SHA drift). The `outcome` discriminator drives the toast
/// label / extra actions in the frontend; per-variant fields populate
/// context for that copy.
///
/// Generated to `src/types/generated/MeshSyncWarningPayload.ts`; the TS half
/// is imported by `src/App.tsx`. Kept as a flat struct with `Option<...>`
/// fields rather than a tagged-enum so a single TS type covers all six
/// outcomes — the frontend only reads `message` regardless.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "MeshSyncWarningPayload.ts")]
pub struct MeshSyncWarningPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub mesh_path: String,
    pub outcome: MeshSyncOutcome,
    #[ts(as = "Option<i32>")]
    pub new_commits: Option<u32>,
    #[ts(as = "Option<i32>")]
    pub pr_number: Option<i64>,
    pub head_ref: Option<String>,
    pub expected_sha: Option<String>,
    pub actual_sha: Option<String>,
    pub fallback_base_ref: Option<String>,
    pub head_repo_owner: Option<String>,
    pub head_repo_clone_url: Option<String>,
    pub message: String,
}

/// `outcome` discriminant for [`MeshSyncWarningPayload`]. The Rust enum's
/// `#[serde(rename_all = "snake_case")]` keeps the wire variants in the
/// shape the pre-#161 inline TS union expected.
#[derive(Debug, Clone, Copy, Serialize, TS)]
#[ts(export, export_to = "MeshSyncOutcome.ts")]
#[serde(rename_all = "snake_case")]
pub enum MeshSyncOutcome {
    Diverged,
    FetchFailed,
    RepoUnusable,
    PrHeadUnfetchable,
    PrForkUnfetchable,
    PrShaDrift,
}
