//! What the phone shows for an Agent Node that is inside a Circuit Run.
//!
//! One read model per implementation node: the run's blueprint, humanised
//! position, review round, latest reviewer verdict, and everything a person
//! would have to act on. The eligibility decisions are not made here. The
//! actions come from [`super::evidence::attention_inner`], the same function
//! the desktop run card reads, so the phone offers what the command accepts for
//! the state it read (a review approval, PR lookup or spawn is not offered Run
//! Recovery). The command still re-checks when the person acts.

use rusqlite::Connection;
use serde::Serialize;

use super::evidence::CheckpointAction;
use super::step_recovery::RunRecovery;
use crate::circuit::context::CircuitContext;
use crate::circuit::model::{CircuitGraph, CircuitNodeKind};
use crate::circuit::observation::{HumanWait, HumanWaitKind, WorkEvidence};
use crate::db::DbError;
use crate::models::{AutopilotCircuitRun, AutopilotCircuitRunStep};

/// Reviewer reports are whole transcripts; a status line wants the gist.
pub const MAX_SUMMARY_CHARS: usize = 600;

/// Why a run is asking for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "NodeCircuitNeedKind.ts")]
pub enum NodeCircuitNeedKind {
    /// A `CollaboratorCheck` gate is parked (`blocked`, shown as "Needs approval").
    Approval,
    /// Buildmesh cannot verify a step; the person rechecks or records an outcome.
    Unverified,
    /// The agent asked a question or for a permission; answered in its terminal.
    Input,
    /// The run failed at this step; see the run's `recovery` options.
    Failed,
}

/// A circuit step in the words a person would use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "NodeCircuitStep.ts")]
pub struct NodeCircuitStep {
    /// The circuit node id (not an Agent Node).
    pub node_id: String,
    /// `Review`, `Approval gate`, ... never the raw scheduler token.
    pub role: String,
    #[ts(as = "i32")]
    pub attempt: i32,
    /// Raw step status token; the client humanises it.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "NodeCircuitReview.ts")]
pub struct NodeCircuitReview {
    /// The reviewer's current pass, 1-based.
    #[ts(as = "i32")]
    pub round: i32,
    #[ts(as = "Option<i32>")]
    pub max_rounds: Option<i32>,
    /// `approved`, `changes_requested` or `blocked`, for the current pass only.
    pub verdict: Option<String>,
    /// The reviewer's report for that pass, shortened.
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "NodeCircuitNeed.ts")]
pub struct NodeCircuitNeed {
    pub kind: NodeCircuitNeedKind,
    pub step: NodeCircuitStep,
    /// The recorded error, or what the agent is waiting for.
    pub detail: Option<String>,
    /// Checkpoint actions the desktop offers for this step. Empty unless
    /// `kind` is `unverified`.
    pub actions: Vec<CheckpointAction>,
    /// Completing this unverified step starts another review pass.
    pub starts_next_review: bool,
    /// Completing this unverified step uses the last review round.
    pub ends_review: bool,
    /// Review again can add another round after the run stops. False when
    /// recovery would reject this graph as a custom circuit.
    pub review_again: bool,
    /// The agent to open to answer an `input` need.
    #[ts(as = "Option<i32>")]
    pub agent_node_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "NodeCircuitRun.ts")]
pub struct NodeCircuitRun {
    #[ts(as = "i32")]
    pub node_id: i64,
    #[ts(as = "i32")]
    pub run_id: i64,
    pub blueprint_name: String,
    /// Raw run state token; the client humanises it.
    pub state: String,
    pub step: Option<NodeCircuitStep>,
    pub review: Option<NodeCircuitReview>,
    pub needs_you: Vec<NodeCircuitNeed>,
    /// The history revision every outcome and recovery request must echo back.
    #[ts(as = "i32")]
    pub revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recovery: Option<RunRecovery>,
}

/// Live and attention-needing runs, one per implementation node.
pub fn list_node_circuit_runs() -> Result<Vec<NodeCircuitRun>, DbError> {
    let db = crate::db::read_conn()?;
    let tx = db.unchecked_transaction()?;
    let runs = list_node_circuit_runs_inner(&tx)?;
    tx.commit()?;
    Ok(runs)
}

pub(crate) fn list_node_circuit_runs_inner(
    db: &Connection,
) -> Result<Vec<NodeCircuitRun>, DbError> {
    // The desktop header already decides which run "owns" each visible node
    // (steps and source, newest wins). Reuse that, and keep only the
    // implementation nodes: a reviewer helper is part of its source's run.
    let ownerships = super::leases::list_circuit_agent_ownerships_inner(db)?;
    // One query for every run's bulky values, rather than one per run: the
    // phone lists a handful of runs and each of them carries its report and
    // its evidence.
    let run_ids: Vec<i64> = ownerships.iter().map(|(_, run_id, ..)| *run_id).collect();
    let stored = super::artifacts::load_values_for_runs(db, &run_ids)?;
    let mut listed = Vec::new();
    for (node_id, run_id, _circuit_id, blueprint_name, _state, parent_node_id) in ownerships {
        if parent_node_id.is_some() {
            continue;
        }
        let Some(run) = super::ledger::get_circuit_run_inner(db, run_id)? else {
            continue;
        };
        if is_listed(&run) {
            listed.push(build(db, node_id, blueprint_name, run, &stored)?);
        }
    }
    Ok(listed)
}

/// A live run, or a failure nobody has acknowledged on the desktop. A finished
/// run has nothing left to ask of the phone, and "Dismiss all" on the desktop
/// is the same acknowledgement.
fn is_listed(run: &AutopilotCircuitRun) -> bool {
    match run.state.as_str() {
        "pending" | "running" | "paused" => true,
        "failed" => run.attention_dismissed_at.is_none(),
        _ => false,
    }
}

fn build(
    db: &Connection,
    node_id: i64,
    blueprint_name: String,
    run: AutopilotCircuitRun,
    stored: &std::collections::BTreeMap<i64, Vec<(String, String)>>,
) -> Result<NodeCircuitRun, DbError> {
    let graph = super::evidence::run_graph(db, run.id)?;
    let steps = super::ledger::list_circuit_run_steps_inner(db, run.id)?;
    // The run's context is two halves: the column and the stored bulky
    // values, which carry the review report and the evidence.
    let values = stored
        .get(&run.id)
        .map(|values| values.as_slice())
        .unwrap_or(&[]);
    let context = super::artifacts::joined_context(&run.context_json, values)?;
    // Same function the desktop run card calls: the actions below are its
    // answer, not a recomputation.
    let attention = super::evidence::attention_inner(db, run.id)?;

    let step_view = |step: &AutopilotCircuitRunStep| NodeCircuitStep {
        node_id: step.node_id.clone(),
        role: role_label(&graph, &step.node_id),
        attempt: step.attempt,
        status: step.status.clone(),
    };

    let mut needs_you = Vec::new();
    for step in &steps {
        let waiting_on_person = matches!(run.state.as_str(), "running" | "paused");
        match step.status.as_str() {
            "blocked" if waiting_on_person => needs_you.push(NodeCircuitNeed {
                kind: NodeCircuitNeedKind::Approval,
                step: step_view(step),
                detail: None,
                actions: Vec::new(),
                starts_next_review: false,
                ends_review: false,
                review_again: false,
                agent_node_id: None,
            }),
            "unverified" => {
                if let Some(checkpoint) = attention
                    .checkpoints
                    .iter()
                    .find(|c| c.node_id == step.node_id && c.attempt == step.attempt)
                {
                    needs_you.push(NodeCircuitNeed {
                        kind: NodeCircuitNeedKind::Unverified,
                        step: step_view(step),
                        detail: step.error_message.clone(),
                        actions: checkpoint.actions.clone(),
                        starts_next_review: checkpoint.starts_next_review,
                        ends_review: checkpoint.ends_review,
                        review_again: checkpoint.review_again,
                        agent_node_id: None,
                    });
                }
            }
            "completed" | "failed" | "cancelled" => {}
            _ if run.state == "running" => {
                if let Some(wait) = open_human_wait(&context, step) {
                    needs_you.push(NodeCircuitNeed {
                        kind: NodeCircuitNeedKind::Input,
                        step: step_view(step),
                        detail: Some(describe_wait(wait.wait_kind).to_string()),
                        actions: Vec::new(),
                        starts_next_review: false,
                        ends_review: false,
                        review_again: false,
                        agent_node_id: Some(wait.identity.agent_node_id),
                    });
                }
            }
            _ => {}
        }
    }
    if let Some(recovery) = &attention.recovery {
        needs_you.push(NodeCircuitNeed {
            kind: NodeCircuitNeedKind::Failed,
            step: NodeCircuitStep {
                node_id: recovery.node_id.clone(),
                role: role_label(&graph, &recovery.node_id),
                attempt: recovery.attempt,
                status: recovery.status.clone(),
            },
            detail: recovery.error.clone(),
            actions: Vec::new(),
            starts_next_review: false,
            ends_review: false,
            review_again: false,
            agent_node_id: None,
        });
    }

    let current = current_step(&run, &steps);
    Ok(NodeCircuitRun {
        node_id,
        run_id: run.id,
        blueprint_name,
        state: run.state.clone(),
        step: current.map(step_view),
        review: review_progress(&graph, &steps, &context),
        needs_you,
        revision: attention.revision,
        recovery: attention.recovery,
    })
}

/// The step a person would point at: the one that is waiting on them, else the
/// one in flight, else the most recent.
fn current_step<'a>(
    run: &AutopilotCircuitRun,
    steps: &'a [AutopilotCircuitRunStep],
) -> Option<&'a AutopilotCircuitRunStep> {
    if run.state == "failed" {
        if let Some(step) = super::step_recovery::problem_step(steps) {
            return Some(step);
        }
    }
    let first = |statuses: &[&str]| steps.iter().find(|s| statuses.contains(&s.status.as_str()));
    first(&["blocked", "unverified"])
        .or_else(|| {
            steps
                .iter()
                .rev()
                .find(|s| matches!(s.status.as_str(), "running" | "pending_slot"))
        })
        .or_else(|| steps.last())
}

/// Which pass of the review loop the run is on, and what the reviewer said
/// about that pass. Mirrors the desktop's "pass N" (the review gate's attempt)
/// and only reports a verdict recorded for the *current* attempt: a retry
/// reuses the node id, so an unmatched key is a previous pass's finding.
fn review_progress(
    graph: &CircuitGraph,
    steps: &[AutopilotCircuitRunStep],
    context: &CircuitContext,
) -> Option<NodeCircuitReview> {
    let gate = graph
        .nodes
        .iter()
        .find(|node| matches!(node.kind, CircuitNodeKind::ReviewVerdict { .. }))?;
    let round = steps
        .iter()
        .rev()
        .find(|step| step.node_id == gate.id)
        .map_or(1, |step| step.attempt.max(1));
    // A node-started review carries its limit in context so the shared preset
    // can serve runs with different limits; an authored graph uses its own.
    let graph_limit = graph.nodes.iter().find_map(|node| match node.kind {
        CircuitNodeKind::RetryLimit { max_retries } => Some(max_retries),
        _ => None,
    });
    let context_limit = (context.get("source.review_preset") == Some("1"))
        .then(|| context.get("retry.max_retries")?.parse::<i32>().ok())
        .flatten();
    let override_limit = context
        .get("review.round_limit")
        .and_then(|value| value.parse::<i32>().ok());
    let max_rounds = override_limit
        .or(context_limit)
        .or(graph_limit)
        .filter(|limit| *limit > 0);

    let recorded_this_pass = context.get(&format!("node.{}.review_verdict_attempt", gate.id))
        == Some(round.to_string().as_str());
    let verdict = recorded_this_pass
        .then(|| context.get(&format!("node.{}.review_verdict", gate.id)))
        .flatten()
        .map(str::to_string);
    let summary = recorded_this_pass
        .then(|| context.get(&format!("node.{}.evaluated_output", gate.id)))
        .flatten()
        .map(shorten_summary)
        .filter(|text| !text.is_empty());
    Some(NodeCircuitReview {
        round,
        max_rounds,
        verdict,
        summary,
    })
}

/// An unresolved permission or question the agent raised on this attempt.
fn open_human_wait(context: &CircuitContext, step: &AutopilotCircuitRunStep) -> Option<HumanWait> {
    let evidence: WorkEvidence = serde_json::from_str(
        context.get(&format!("node.{}.evidence.{}", step.node_id, step.attempt))?,
    )
    .ok()?;
    evidence
        .human_waits
        .into_iter()
        // `agent_status_projection` is Buildmesh restating the agent's status,
        // not a request the agent made; the evidence panel hides it too.
        .find(|wait| wait.resolved_at_ms.is_none() && wait.source != "agent_status_projection")
}

fn describe_wait(kind: HumanWaitKind) -> &'static str {
    match kind {
        HumanWaitKind::Permission => "Waiting for a permission decision",
        HumanWaitKind::Question => "Waiting for an answer to a question",
        HumanWaitKind::Input => "Waiting for input",
        HumanWaitKind::ReviewApproval => "Waiting for a review approval",
    }
}

/// A circuit node in a person's words. Kinds not listed fall back to the node
/// id, which is honest if unlovely (an authored circuit names its own nodes).
fn role_label(graph: &CircuitGraph, node_id: &str) -> String {
    let Some(node) = graph.node(node_id) else {
        return node_id.to_string();
    };
    match &node.kind {
        CircuitNodeKind::SpawnAgentNode { name, .. } => name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(node_id)
            .to_string(),
        CircuitNodeKind::InjectPty { .. } => "Send feedback".into(),
        CircuitNodeKind::GithubAction { .. } => "GitHub action".into(),
        CircuitNodeKind::CloseAgentNode { .. } => "Close agent".into(),
        CircuitNodeKind::LlmTurnClassifier { .. } => "Classifier".into(),
        CircuitNodeKind::AwaitAgentTurn { .. } => "Await task".into(),
        CircuitNodeKind::ReviewVerdict { .. } => "Review".into(),
        CircuitNodeKind::DeterministicVerification { .. } => "Verification".into(),
        CircuitNodeKind::CollaboratorCheck { .. } => "Approval gate".into(),
        CircuitNodeKind::RetryLimit { .. } => "Retry budget".into(),
        CircuitNodeKind::Notify { .. } => "Notify".into(),
        _ => node_id.to_string(),
    }
}

/// A reviewer report shortened to [`MAX_SUMMARY_CHARS`] on a character boundary.
pub(crate) fn shorten_summary(report: &str) -> String {
    let report = report.trim();
    let mut chars = report.chars();
    let head: String = chars.by_ref().take(MAX_SUMMARY_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}
