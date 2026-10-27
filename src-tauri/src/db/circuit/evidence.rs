//! Durable external-effect claims and append-only Circuit Run History.

mod read_view;
pub use read_view::*;

mod native_hooks;
pub(crate) use native_hooks::*;
mod outcome;
pub use outcome::*;

mod waits;
pub(crate) use waits::*;

mod transition;
pub use transition::*;

mod effects;
pub use effects::*;

use crate::db::{DbError, SqlResult};
use rusqlite::{params, Connection, OptionalExtension};

pub(crate) mod prompt_delivery;

const OBSERVATION_FRESHNESS_REJECTION_PREFIX: &str = "Observation freshness fence rejected:";

pub(crate) fn restore_projection_conflicts(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    evidence: &mut crate::circuit::observation::WorkEvidence,
) -> SqlResult<bool> {
    let conn = crate::db::read_conn()?;
    restore_projection_conflicts_inner(&conn, run_id, node_id, attempt, evidence)
}

fn restore_projection_conflicts_inner(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    evidence: &mut crate::circuit::observation::WorkEvidence,
) -> SqlResult<bool> {
    let mut query = db.prepare(
        "SELECT detail FROM circuit_run_history
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='observation'
          AND source='agent_status_projection' AND disposition='conflicting'",
    )?;
    let rows = query.query_map(params![run_id, node_id, attempt], |row| {
        row.get::<_, String>(0)
    })?;
    let mut changed = false;
    for row in rows {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&row?) else {
            continue;
        };
        if let Some(observation) = value
            .get("observation")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
        {
            changed |= evidence.restore_projection_conflict(&observation);
        }
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Circuit Run History provenance vocabulary (issue #1909 / #1847).
//
// Every append carries a `source` (who/what produced the event) and a
// `disposition` (what Buildmesh recorded doing with it), alongside the
// existing identity (`node_id`/`attempt`) and time (`observed_at`). This is
// the single vocabulary: the Probe renders these values and tests pin them,
// so add here rather than inlining literals at call sites.
// ---------------------------------------------------------------------------

/// The circuit worker's stepper committed a run/step transition or effect.
pub(super) const SOURCE_CIRCUIT_WORKER: &str = "circuit_worker";
/// The worker's admission gate parked a pending run on a capacity reason.
pub(super) const SOURCE_ADMISSION: &str = "circuit_worker.admission";
/// The worker's step-capacity gate parked a step.
pub(super) const SOURCE_CAPACITY: &str = "circuit_worker.capacity";
/// The worker reconciled an evidence window against its projection.
pub(super) const SOURCE_RECONCILIATION: &str = "circuit_worker.reconciliation";
/// The run pinned its configuration snapshot.
pub(super) const SOURCE_RUN_CONFIGURATION: &str = "run.configuration";
/// A native harness hook receipt.
pub(super) const SOURCE_NATIVE_HOOK: &str = "native_hook";
/// A Buildmesh PTY prompt submission awaiting a harness acknowledgement
/// (issue #1898).
pub(super) const SOURCE_PROMPT_SUBMISSION: &str = "prompt_submission";
/// The report classifier interpreted a report.
pub(super) const SOURCE_CLASSIFIER: &str = "classifier";
/// A GitHub read-only lookup or recorded action target.
pub(super) const SOURCE_GITHUB: &str = "github";
/// An operator action: attestation, evidence recheck, or continuation.
pub(super) const SOURCE_OPERATOR: &str = "operator";

/// A committed projection or operator action took effect.
pub(super) const DISPOSITION_APPLIED: &str = "applied";
/// An unresolved wait (admission, step capacity, or evidence window).
pub(super) const DISPOSITION_WAITING: &str = "waiting";
/// A wait whose binding window cleared — no longer active (issue #1909).
pub(super) const DISPOSITION_RESOLVED: &str = "resolved";
pub(super) const DISPOSITION_INTENT: &str = "intent";
pub(super) const DISPOSITION_POSSIBLE_DISPATCH: &str = "possible_dispatch";
pub(super) const DISPOSITION_ACKNOWLEDGED: &str = "acknowledged";
pub(super) const DISPOSITION_RECONCILED: &str = "reconciled";
pub(super) const DISPOSITION_RECORDED: &str = "recorded";
pub(super) const DISPOSITION_RECEIVED: &str = "received";
pub(super) const DISPOSITION_INTERPRETED: &str = "interpreted";
pub(super) const DISPOSITION_REQUESTED: &str = "requested";

/// Buildmesh's recorded disposition for one observation, matching the
/// snake_case serde form of [`ObservationDisposition`].
fn observation_disposition_str(
    value: crate::circuit::observation::ObservationDisposition,
) -> &'static str {
    use crate::circuit::observation::ObservationDisposition as D;
    match value {
        D::Accepted => "accepted",
        D::ReducedConfidence => "reduced_confidence",
        D::Duplicate => "duplicate",
        D::Rejected => "rejected",
        D::Unavailable => "unavailable",
        D::Conflicting => "conflicting",
    }
}

/// A wait event's disposition: `waiting` while its window still binds,
/// `resolved` once it clears. Without this, a freed capacity/evidence window
/// would be recorded as if the wait were still active (issue #1909).
fn wait_disposition(binding: bool) -> &'static str {
    if binding {
        DISPOSITION_WAITING
    } else {
        DISPOSITION_RESOLVED
    }
}

pub(crate) fn is_observation_freshness_rejection(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::InvalidParameterName(message)
        if message.starts_with(OBSERVATION_FRESHNESS_REJECTION_PREFIX))
}

fn observation_freshness_rejection(reason: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(format!(
        "{OBSERVATION_FRESHNESS_REJECTION_PREFIX} {reason}"
    ))
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitHistoryEntry.ts")]
pub struct CircuitHistoryEntry {
    #[ts(as = "i32")]
    pub id: i64,
    pub node_id: Option<String>,
    pub attempt: Option<i32>,
    pub kind: String,
    pub detail: String,
    /// Who/what produced the event (issue #1909 / #1847). Null on pre-v45 rows.
    pub source: Option<String>,
    /// What Buildmesh recorded doing with the event. Null on pre-v45 rows.
    pub disposition: Option<String>,
    pub observed_at: String,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitCheckpoint.ts")]
pub struct CircuitCheckpoint {
    pub node_id: String,
    pub attempt: i32,
    pub actions: Vec<CheckpointAction>,
    /// Completing this step starts another review pass.
    pub starts_next_review: bool,
    /// Completing this step uses the last review round.
    pub ends_review: bool,
    /// Review again can add another round after the run stops. False when
    /// recovery would reject this graph as a custom circuit.
    pub review_again: bool,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitEvidenceView.ts")]
pub struct CircuitEvidenceView {
    pub entries: Vec<CircuitHistoryEntry>,
    pub checkpoints: Vec<CircuitCheckpoint>,
    pub coverage: Vec<CircuitStepObservationCoverage>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitStepObservationCoverage.ts")]
pub struct CircuitStepObservationCoverage {
    pub node_id: String,
    pub attempt: i32,
    pub platform: String,
    pub capabilities: crate::services::circuit_worker::observer_policy::CircuitObserverCapabilities,
    #[ts(as = "Option<i32>")]
    pub deadline_ms: Option<i64>,
    pub waits_active: bool,
    pub human_waits: Vec<crate::circuit::observation::HumanWait>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub observation_blocker: Option<crate::circuit::observation::CircuitObservationBlocker>,
}

/// Everything the run card needs to tell a person what to do next, without the
/// (potentially very long) history: the unverified steps with their allowed
/// actions, how a failed run can be recovered, and the revision to act against.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitRunAttention.ts")]
pub struct CircuitRunAttention {
    #[ts(as = "i32")]
    pub revision: i64,
    pub checkpoints: Vec<CircuitCheckpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recovery: Option<super::step_recovery::RunRecovery>,
    /// Running or unverified steps a person can move on by hand, overriding
    /// stale evidence. See [`super::operator_push`].
    pub pushes: Vec<super::operator_push::CircuitStepPush>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "CheckpointAction.ts")]
pub enum CheckpointAction {
    ManualDelivered,
    Completed,
    NotPerformed,
    Retry,
    Recheck,
    ReviewPass,
    ReviewFail,
}

#[derive(Debug, Clone, serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CheckpointRequest.ts")]
pub struct CheckpointRequest {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub node_id: String,
    pub attempt: i32,
    #[ts(as = "i32")]
    pub expected_revision: i64,
    pub action: CheckpointAction,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Spawn,
    Prompt,
    Github,
}

impl EffectKind {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Prompt => "prompt",
            Self::Github => "github",
        }
    }
}

impl From<crate::circuit::stepper::EffectJournal> for EffectKind {
    fn from(kind: crate::circuit::stepper::EffectJournal) -> Self {
        match kind {
            crate::circuit::stepper::EffectJournal::Spawn => Self::Spawn,
            crate::circuit::stepper::EffectJournal::Prompt => Self::Prompt,
            crate::circuit::stepper::EffectJournal::Github => Self::Github,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectIntent {
    pub node_id: String,
    pub attempt: i32,
    pub kind: EffectKind,
}

/// A local status mutation that is committed with the transition which
/// completes its Circuit step. It has no external dispatch window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatusEffect {
    pub node_id: String,
    pub attempt: i32,
    pub agent_node_id: i64,
    pub status: crate::models::SessionStatus,
}

#[derive(Debug, Clone)]
pub struct ReconciledEffect {
    pub intent: EffectIntent,
    pub detail: String,
}

pub(super) fn pin_graph(db: &Connection, run_id: i64) -> SqlResult<()> {
    use sha2::{Digest, Sha256};
    // `behavior_revision` is a recorded placeholder constant (issue #1909); it
    // is not a live revision counter. Graph identity is pinned by the
    // `graph_sha256` in the history entry, which is what the operator surface
    // relies on. Redefining the revision scheme is a separate decision.
    let inserted = db.execute("INSERT OR IGNORE INTO circuit_run_snapshots (run_id,graph_json,behavior_revision)
        SELECT r.id,c.graph_json,1 FROM autopilot_circuit_runs r JOIN autopilot_circuits c ON c.id=r.circuit_id WHERE r.id=?1", [run_id])?;
    if inserted == 0 {
        return Ok(());
    }
    let (graph, context): (String, String) = db.query_row("SELECT s.graph_json,r.context_json FROM circuit_run_snapshots s JOIN autopilot_circuit_runs r ON r.id=s.run_id WHERE r.id=?1", [run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    // The launch plans live in the bulky half of the run's context, so the pin
    // reads both halves: a plan that only exists in the side table would pin
    // no reviewer at all.
    let stored = super::artifacts::load_values(db, run_id)?;
    let context = super::artifacts::merge_values(&context, &stored);
    let reviewers: Vec<serde_json::Value> = context.iter().filter_map(|(key, value)| {
        let node_id = key.strip_prefix("review.launch.")?;
        let plan: crate::preferences::spawn_configurations::SpawnConfiguration = serde_json::from_str(value).ok()?;
        // Explicit allowlist: launch arguments, environment, endpoints and prompts
        // can contain private values and must not leak into operator history.
        Some(serde_json::json!({"node_id":node_id,"configuration_id":plan.id,
            "spawn_option_id":plan.spawn_option_id,"harness_id":plan.harness_id,
            "provider_route_id":plan.provider_route_id,"model":plan.model,"effort":plan.effort,
            "extra_arguments_configured":plan.extra_args.as_ref().is_some_and(|args|!args.is_empty())}))
    }).collect();
    append_history(
        db,
        HistoryEvent {
            run_id,
            node_id: None,
            attempt: None,
            kind: "configuration_pinned",
            detail: &serde_json::json!({
                "behavior_revision":1,"graph_sha256":hex::encode(Sha256::digest(graph.as_bytes())),
                "reviewers":reviewers
            })
            .to_string(),
            source: Some(SOURCE_RUN_CONFIGURATION),
            disposition: Some(DISPOSITION_APPLIED),
        },
    )?;
    Ok(())
}

pub(super) fn run_graph(
    db: &Connection,
    run_id: i64,
) -> Result<crate::circuit::model::CircuitGraph, DbError> {
    let json = run_graph_json(db, run_id)?;
    Ok(crate::circuit::model::CircuitGraph::from_json(&json)?)
}

pub(super) fn run_graph_json(db: &Connection, run_id: i64) -> SqlResult<String> {
    db.query_row(
        "SELECT COALESCE(s.graph_json,c.graph_json)
        FROM autopilot_circuit_runs r JOIN autopilot_circuits c ON c.id=r.circuit_id
        LEFT JOIN circuit_run_snapshots s ON s.run_id=r.id WHERE r.id=?1",
        [run_id],
        |r| r.get(0),
    )
}

/// One Circuit Run History event: the single row [`append_history`] writes.
///
/// `source` and `disposition` are the provenance vocabulary above; both are
/// nullable for pre-v45 rows (issue #1909 / #1847). `observed_at` defaults to
/// the append time, which for these synchronous events is the observed time.
///
/// A history event is a named thing rather than a tuple: every call site
/// spells out its provenance fields instead of trusting a positional order,
/// so a field added later cannot be transposed into the wrong column.
pub(super) struct HistoryEvent<'a> {
    pub(super) run_id: i64,
    pub(super) node_id: Option<&'a str>,
    pub(super) attempt: Option<i32>,
    pub(super) kind: &'a str,
    pub(super) detail: &'a str,
    pub(super) source: Option<&'a str>,
    pub(super) disposition: Option<&'a str>,
}

/// Append one Circuit Run History event.
pub(super) fn append_history(db: &Connection, event: HistoryEvent<'_>) -> SqlResult<()> {
    let HistoryEvent {
        run_id,
        node_id,
        attempt,
        kind,
        detail,
        source,
        disposition,
    } = event;
    db.execute("INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail,source,disposition) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![run_id, node_id, attempt, kind, crate::secret_scrubber::SecretScrubber::scrub(detail), source, disposition])?;
    Ok(())
}

pub(super) fn revision_inner(db: &Connection, run_id: i64) -> SqlResult<i64> {
    db.query_row(
        "SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=?1",
        [run_id],
        |r| r.get(0),
    )
}

/// Whether history since `expected` contains a fact the person has not seen.
///
/// Duplicate observations are the same fact recorded again. They must not
/// refuse an operator action the person already refreshed for. Any other row
/// still refuses, including a new observation and a row whose disposition was
/// never set. A revision ahead of the ledger is treated as moved.
pub(super) fn revision_moved(db: &Connection, run_id: i64, expected: i64) -> SqlResult<bool> {
    let current = revision_inner(db, run_id)?;
    if current == expected {
        return Ok(false);
    }
    if expected > current {
        return Ok(true);
    }
    // `IS`, not `=`. A missing disposition is unknown under `=`, so `NOT`
    // would drop that row and a stale click would succeed.
    db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM circuit_run_history
            WHERE run_id=?1 AND id>?2
              AND NOT (kind='observation' AND disposition IS ?3)
        )",
        params![
            run_id,
            expected,
            observation_disposition_str(
                crate::circuit::observation::ObservationDisposition::Duplicate
            )
        ],
        |row| row.get(0),
    )
}

pub fn observation_revision(run: &crate::models::AutopilotCircuitRun) -> SqlResult<i64> {
    let db = crate::db::read_conn()?;
    db.query_row(
        "SELECT (SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=r.id)
        FROM autopilot_circuit_runs r WHERE r.id=?1 AND r.context_json=?2 AND r.state=?3",
        params![run.id, run.context_json, run.state],
        |r| r.get(0),
    )
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
