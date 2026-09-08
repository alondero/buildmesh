//! Autopilot Circuit ledger wire types.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Autopilot Circuits (spec #1205 / walking skeleton #1206).
//
// Wire shapes for the three circuit ledger tables. The Graph Blueprint
// AST itself is NOT re-declared here — it serialises to the
// `graph_json` TEXT column and travels over IPC as that string; the
// canonical Rust type lives in `autopilot::circuit::model::CircuitGraph`.
// ---------------------------------------------------------------------------

/// One Autopilot Circuit — the persisted blueprint row.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "AutopilotCircuit.ts")]
pub struct AutopilotCircuit {
    #[ts(as = "i32")]
    pub id: i64,
    #[ts(as = "i32")]
    pub mesh_id: i64,
    pub name: String,
    pub description: String,
    /// Enabled circuits are eligible for the worker's trigger pass.
    pub enabled: bool,
    #[ts(as = "i32")]
    pub concurrency_limit: i64,
    /// The `CircuitGraph` blueprint, JSON-encoded (see module note above).
    pub graph_json: String,
    pub created_at: String,
    pub updated_at: String,
    /// Built-in execution preset, hidden from user-authored Circuit lists.
    pub is_preset: bool,
}

/// One Circuit Run — a single execution instance of a circuit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "AutopilotCircuitRun.ts")]
pub struct AutopilotCircuitRun {
    #[ts(as = "i32")]
    pub id: i64,
    #[ts(as = "i32")]
    pub circuit_id: i64,
    #[ts(as = "i32")]
    pub mesh_id: i64,
    /// Borrowed Agent Node that triggered this run, when launched from a
    /// title bar. Kept relational so ownership queries stay indexable.
    #[ts(as = "Option<i32>")]
    pub source_agent_node_id: Option<i64>,
    /// Dedupe identity of what fired this run (e.g. `manual:<unix-ms>`;
    /// GitHub triggers use `<issue|pr>:<number>:<label>` in later
    /// milestones). Scoped per-circuit so two circuits may process the
    /// same source independently.
    pub trigger_identity: String,
    /// `pending` | `running` | `completed` | `failed`.
    pub state: String,
    /// The run's resolved template context (`circuit.*`, `node.*`), JSON.
    pub context_json: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One Circuit Step — a single circuit node's execution within a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "AutopilotCircuitRunStep.ts")]
pub struct AutopilotCircuitRunStep {
    #[ts(as = "i32")]
    pub id: i64,
    #[ts(as = "i32")]
    pub run_id: i64,
    /// The circuit node id inside the blueprint graph (NOT an agent node).
    pub node_id: String,
    /// The mesh agent node this step spawned/piloted, when any.
    #[ts(as = "Option<i32>")]
    pub agent_node_id: Option<i64>,
    /// `pending_slot` | `running` | `completed` | `failed` | `cancelled`.
    pub status: String,
    #[ts(as = "i32")]
    pub attempt: i32,
    /// Terminal outcome (`completed` | `failed` | `cancelled`); NULL while in flight.
    pub outcome: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}
