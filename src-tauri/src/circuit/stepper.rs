//! The pure circuit stepper (issue #1206, slice 1 of the Autopilot
//! Circuits spec #1205).
//!
//! [`advance`] is a **pure function**: it takes an in-memory
//! [`RunView`] snapshot plus one [`CircuitEvent`] and returns the list of
//! [`Effect`]s to perform. It never touches SQLite, PTYs, processes, or
//! the clock — every impure fact arrives inside the event (`Tick` carries
//! capacity, `AgentReady`/`AgentFinished` carry observed process state).
//! This is the established "pure core, thin impure seam" split the legacy
//! autopilot pipeline uses; the seam lives in
//! `services::circuit_worker`.
//!
//! ## Scheduling rules
//!
//! - A circuit node becomes **eligible** when every incoming edge is
//!   satisfied: its parent step is terminal AND the edge condition
//!   matches (`Always`, or `OnOutcome(o)` where the parent's outcome
//!   equals `o`). The exception is [`CircuitNodeKind::AnyCompleted`],
//!   whose fan-in rule is satisfied by ANY completed parent. Trigger
//!   roots have no incoming edges.
//! - Triggers auto-complete at run start — they fired to create the run.
//! - Every `SpawnAgentNode` needs a free slot in its run's durable agent
//!   lease; otherwise the step parks in `Queued` (`pending_slot` in the
//!   ledger) and promotes FIFO on a later `Tick`. The worker reserves the
//!   blueprint's declared spawn slots before admitting a run, so peer runs
//!   cannot steal capacity needed by a downstream reviewer; an optional
//!   app-wide pool remains the explicit host-safety backstop. Non-agent
//!   steps never wait on capacity: DAG eligibility is their only gate, as
//!   there is no per-circuit step budget (ADR 0042). Known milestone-1 scope
//!   note: FIFO ordering is per-run — cross-run ordering on one circuit is
//!   tick order until the multi-run scheduler milestone.
//! - `InjectPty` waits for `AgentReady` (the spawned agent's process is
//!   live) before firing; this keeps injection off the async stage-2
//!   spawn window. Human typing in the terminal never affects these
//!   events — they come from process/lifecycle observation, not
//!   keystrokes, so coexistence is structural rather than guarded.
//! - Joins execute instantly once their fan-in rule is satisfied.
//! - **Gates (milestone 2, #1207):**
//!   - `LlmTurnClassifier` parks Running until the seam observes the
//!     piloted agent's turn yield, classifies it, and feeds back a
//!     `TurnClassified` event; the step completes with outcome
//!     Completed/Blocked/Working (None degrades to Working). Edges pick
//!     successors with `OnOutcome(...)`; an unmatched outcome simply
//!     parks that branch.
//!   - `DeterministicVerification` parks Running until the seam runs its
//!     command and feeds `VerificationResult`; outcome Green/Red.
//!   - `CollaboratorCheck` with `require_approval` parks in the new
//!     `Blocked` status until a `CollaboratorApproved` event arrives;
//!     `AutoRun` passes through untouched (instant complete).
//!   - `RetryLimit` bounds re-execution: when reached via a FAILED
//!     parent, the parent is reset to Queued with `attempt + 1` while
//!     `attempt < max_retries` (total executions = max_retries); at the
//!     budget's end the gate fails and fail-fast resumes. A failed step
//!     wired to a downstream RetryLimit therefore does NOT immediately
//!     fail the run — the retry gate owns the failure.
//! - **Pause/resume (#1207):** `Paused` halts graph advancement — no
//!   scheduling and no completion while paused; lifecycle events still
//!   mark steps terminal so "the current step finishes", but nothing
//!   cascades until `Resumed`.
//! - `GithubAction` steps complete instantly and hand a `CallGithub`
//!   effect to the seam (milestone 3, issue #1208); a failed HTTP call
//!   fails the step from the seam's effect path.
//! - Fail-fast: any step ending `Failed` without a downstream RetryLimit,
//!   or a piloted agent being lost (closed/archived mid-run), fails the
//!   run.

use super::context::CircuitContext;
use super::model::{
    consumes_agent_slot, is_executable, CircuitGraph, CircuitNodeKind, EdgeCondition,
    GithubActionKind, SessionStatusKind, StepOutcome,
};
use crate::circuit::evaluator::Classification;

pub(crate) const MAX_CLASSIFIER_FAILURES: u32 = 5;

// ---------------------------------------------------------------------------
// State model — the pure mirror of the three ledger tables.
//
// The canonical enum bodies live in [`super::vocabulary`]. This module
// re-exports them so call sites (`services::circuit_worker`,
// `db::circuit::ledger`, the TS generated twins) keep saying
// `stepper::RunState` while only one owner defines the wire strings
// (issue #1660).
// ---------------------------------------------------------------------------

pub use super::vocabulary::{RunState, StepStatus};

impl StepStatus {
    pub fn outcome(self) -> Option<StepOutcome> {
        match self {
            Self::Completed => Some(StepOutcome::Completed),
            Self::Failed => Some(StepOutcome::Failed),
            Self::Cancelled => Some(StepOutcome::Cancelled),
            _ => None,
        }
    }
}

/// One circuit step within the run view.
#[derive(Debug, Clone, PartialEq)]
pub struct StepView {
    pub node_id: String,
    pub status: StepStatus,
    pub outcome: Option<StepOutcome>,
    pub error: Option<String>,
    /// The mesh agent node this step piloted (spawn steps only).
    pub agent_node_id: Option<i64>,
    /// Execution count (1 = first attempt). RetryLimit gates increment
    /// it when they reset a failed step for re-execution (#1207).
    pub attempt: i32,
}

impl StepView {
    pub(crate) fn cancellation_reason(&self, reason: &str) -> String {
        match self.error.as_deref().filter(|error| !error.is_empty()) {
            Some(checkpoint) => format!("{reason} Previous checkpoint: {checkpoint}"),
            None => reason.to_owned(),
        }
    }

    fn new(node_id: &str, status: StepStatus) -> Self {
        Self {
            node_id: node_id.to_string(),
            status,
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt: 1,
        }
    }
}

/// In-memory snapshot of one run: the run row, its steps, and the parsed
/// blueprint. The worker rebuilds this each tick from the DB.
#[derive(Debug, Clone, PartialEq)]
pub struct RunView {
    pub run_id: i64,
    pub graph: CircuitGraph,
    pub state: RunState,
    pub context: CircuitContext,
    pub steps: Vec<StepView>,
}

impl RunView {
    pub fn step(&self, node_id: &str) -> Option<&StepView> {
        self.steps.iter().find(|s| s.node_id == node_id)
    }

    fn has_human_wait(&self, node_id: &str) -> bool {
        self.step(node_id)
            .and_then(|step| {
                self.context
                    .get(&format!("node.{node_id}.evidence.{}", step.attempt))
            })
            .and_then(|json| serde_json::from_str::<super::observation::WorkEvidence>(json).ok())
            .map_or_else(
                || self.context.get(&format!("node.{node_id}.human_wait")) == Some("1"),
                |evidence| evidence.has_human_wait(),
            )
    }

    pub(crate) fn report_blocker(
        &self,
        node_id: &str,
    ) -> Option<super::observation::CircuitObservationBlocker> {
        use super::observation::CircuitObservationBlocker as B;
        let target = self
            .step(node_id)
            .and_then(|step| step.agent_node_id)
            .or_else(|| self.resolve_target_agent(node_id));
        for step in self.steps.iter().filter(|step| {
            step.node_id == node_id
                || step
                    .agent_node_id
                    .or_else(|| self.resolve_target_agent(&step.node_id))
                    .is_some_and(|id| Some(id) == target)
        }) {
            if self.has_human_wait(&step.node_id) {
                return Some(B::HumanResponseRequired);
            }
            if let Some(json) = self.context.lifecycle_blocker(&step.node_id) {
                match serde_json::from_str::<B>(json) {
                    Ok(blocker) => return Some(blocker),
                    Err(_) => return Some(B::LifecycleEvidenceUnavailable),
                }
            }
            if let Some(json) = self
                .context
                .get(&format!("node.{}.evidence.{}", step.node_id, step.attempt))
            {
                let Ok(evidence) = serde_json::from_str::<super::observation::WorkEvidence>(json)
                else {
                    return Some(B::EvidenceConflict);
                };
                if evidence.conflicted {
                    return Some(B::EvidenceConflict);
                }
                if evidence.children.values().any(|terminal| !terminal) {
                    return Some(B::KnownWorkOutstanding);
                }
            }
        }
        None
    }

    pub(crate) fn report_has_known_blockers(&self, node_id: &str) -> bool {
        self.report_blocker(node_id).is_some()
    }

    /// Human attestation resolves a handoff, never a review verdict or an
    /// unallocated spawn. Keep the UI offer and durable command on one policy.
    pub(crate) fn can_attest_completion(&self, node_id: &str) -> bool {
        self.state == RunState::Running
            && self.step(node_id).is_some_and(|step| {
                step.status == StepStatus::Unverified
                    && step
                        .agent_node_id
                        .or_else(|| self.resolve_target_agent(node_id))
                        .is_some()
            })
            && matches!(
                self.graph.node(node_id).map(|node| &node.kind),
                Some(
                    CircuitNodeKind::SpawnAgentNode { .. }
                        | CircuitNodeKind::AwaitAgentTurn { .. }
                        | CircuitNodeKind::LlmTurnClassifier { .. }
                )
            )
            && !self.report_has_known_blockers(node_id)
    }

    fn accepts_report_binding(
        &self,
        node_id: &str,
        binding: &ClassificationBinding,
        output: Option<&str>,
    ) -> bool {
        let Some(step) = self.step(node_id) else {
            return false;
        };
        let owner = &binding.owner;
        let guard = &binding.input_guard;
        guard.report_guard.as_ref().is_some_and(|report| {
            output == Some(report.text.as_str())
                && !report.text.trim().is_empty()
                && binding.report_revision == report.revision
                && owner.report_revision.as_deref() == Some(report.revision.as_str())
                && guard.observed_at_ms == report.published_at_ms
        }) && owner.run_id == self.run_id
            && owner.step_id == node_id
            && owner.attempt == step.attempt
            && step
                .agent_node_id
                .or_else(|| self.resolve_target_agent(node_id))
                == Some(owner.agent_node_id)
            && owner.agent_node_id == guard.agent_node_id
            && owner.session_id.as_deref() == Some(guard.session_id.as_str())
            && owner.session_incarnation.as_deref() == Some(guard.session_incarnation.as_str())
            && !self.report_has_known_blockers(node_id)
    }

    pub fn evidence_deadline_ms(&self, node_id: &str) -> Option<i64> {
        let step = self.step(node_id)?;
        if self.state != RunState::Running
            || step.status != StepStatus::Running
            || self.has_human_wait(node_id)
            || self.context.get(&format!("node.{node_id}.classification")) == Some("blocked")
        {
            return None;
        }
        let prefix = wait_prefix(node_id);
        if self
            .context
            .get(&format!("{prefix}.attempt"))?
            .parse::<i32>()
            .ok()?
            != step.attempt
        {
            return None;
        }
        let start = self
            .context
            .get(&format!("{prefix}.since_ms"))?
            .parse::<i64>()
            .ok()?;
        let timeout = self
            .context
            .get(&format!("{prefix}.timeout_ms"))?
            .parse::<i64>()
            .ok()?;
        let explicit = self.context.get(&format!("{prefix}.explicit_budget")) == Some("1");
        let observed = self.context.get(&format!("{prefix}.observed")) == Some("1");
        let budget = if !explicit && !observed {
            timeout.min(UNOBSERVED_WAIT_MS)
        } else {
            timeout
        };
        Some(start.saturating_add(budget))
    }

    /// Keep the evidence owner's identity when a downstream gate interprets its report.
    pub fn classifier_evidence(&self, node_id: &str) -> Option<super::observation::WorkEvidence> {
        let target = self.resolve_target_agent(node_id)?;
        let gate = self.step(node_id)?;
        if self.has_human_wait(node_id) {
            return None;
        }
        let own = match self
            .context
            .get(&format!("node.{node_id}.evidence.{}", gate.attempt))
        {
            Some(json) => {
                Some(serde_json::from_str::<super::observation::WorkEvidence>(json).ok()?)
            }
            None => None,
        };
        if own.as_ref().is_some_and(|evidence| {
            evidence.conflicted
                || evidence.lifecycle_invalidated
                || evidence.children.values().any(|terminal| !terminal)
                || evidence.latest.as_ref().is_some_and(|observation| {
                    matches!(
                        observation.fact,
                        super::observation::ObservedWorkFact::Unavailable
                            | super::observation::ObservedWorkFact::OwnershipUnavailable { .. }
                    )
                })
        }) {
            return None;
        }
        let candidates = std::iter::once(gate).chain(self.steps.iter().filter(|step| {
            step.node_id != node_id
                && step.agent_node_id == Some(target)
                && self.is_upstream_ancestor(node_id, &step.node_id)
        }));
        for owner in candidates {
            let Some(evidence) = self
                .context
                .get(&format!(
                    "node.{}.evidence.{}",
                    owner.node_id, owner.attempt
                ))
                .and_then(|json| {
                    serde_json::from_str::<super::observation::WorkEvidence>(json).ok()
                })
            else {
                continue;
            };
            let Some(identity) = evidence.identity.as_ref() else {
                continue;
            };
            if identity.run_id != self.run_id
                || identity.step_id != owner.node_id
                || identity.attempt != owner.attempt
                || identity.agent_node_id != target
                || !evidence.lifecycle_verified()
                || self.has_human_wait(&owner.node_id)
            {
                continue;
            }
            if own.as_ref().is_some_and(|current| {
                current.conflicted
                    || current.identity.as_ref().is_some_and(|current| {
                        [
                            (&current.session_id, &identity.session_id),
                            (&current.session_incarnation, &identity.session_incarnation),
                            (&current.turn_id, &identity.turn_id),
                        ]
                        .iter()
                        .any(|(a, b)| a.as_ref().zip(b.as_ref()).is_some_and(|(a, b)| a != b))
                    })
            }) {
                continue;
            }
            return Some(evidence);
        }
        None
    }

    pub(crate) fn step_mut(&mut self, node_id: &str) -> Option<&mut StepView> {
        self.steps.iter_mut().find(|s| s.node_id == node_id)
    }

    /// The most recent spawn step that has an attached agent node — the
    /// injection target for `InjectPty` steps (the linear
    /// walking-skeleton contract; parallel spawn fan-out is a later
    /// milestone's routing problem).
    pub fn latest_agent_node_id(&self) -> Option<i64> {
        self.steps.iter().rev().find_map(|s| s.agent_node_id)
    }

    /// Check if `ancestor` is an upstream ancestor of `descendant` via incoming edges.
    /// Delegates to [`CircuitGraph::is_ancestor`].
    pub fn is_upstream_ancestor(&self, descendant: &str, ancestor: &str) -> bool {
        self.graph.is_ancestor(descendant, ancestor)
    }

    /// Check if any upstream ancestor of `node_id` in the graph satisfies a predicate.
    /// Thin wrapper over [`CircuitGraph::has_ancestor_matching`] (issue #1660).
    pub fn has_upstream_node_of_kind<F>(&self, node_id: &str, predicate: F) -> bool
    where
        F: Fn(&CircuitNodeKind) -> bool,
    {
        self.graph
            .has_ancestor_matching(node_id, |node| predicate(&node.kind))
    }

    /// Resolve the target agent node for an `InjectPty`, `SetNodeStatus`,
    /// classifier, or close step:
    /// - If the node's `target_node_id` is explicitly set, validate that
    ///   it is an upstream `SpawnAgentNode` in this step's lineage (or is
    ///   the step itself) and return its `agent_node_id`. If invalid or
    ///   not upstream, fails closed (`None`).
    /// - If the node's `target_node_id` is `None`, walk backward from
    ///   `node_id` through incoming edges in the graph's dependency
    ///   lineage and return the `agent_node_id` of the nearest upstream
    ///   `SpawnAgentNode`. Fails closed (`None`) if none exists in branch.
    ///
    /// The target is read from the node itself so callers don't pattern-
    /// match on `CircuitNodeKind` to extract a target the resolver has
    /// direct access to.
    pub fn resolve_target_agent(&self, node_id: &str) -> Option<i64> {
        // A source binding is borrowed: never put it on a spawn step, whose
        // agent association grants cancellation permission to delete it.
        let target = match self.graph.node(node_id).map(|n| &n.kind) {
            Some(CircuitNodeKind::InjectPty { target_node_id, .. })
            | Some(CircuitNodeKind::LlmTurnClassifier { target_node_id })
            | Some(CircuitNodeKind::AwaitAgentTurn { target_node_id })
            | Some(CircuitNodeKind::ReviewVerdict { target_node_id }) => target_node_id.as_deref(),
            _ => None,
        };
        if target == Some("$source") {
            return self.context.source_agent_id();
        }
        resolve_target_agent(&self.graph, &self.steps, node_id)
    }

    /// The agent a bounded continuation may be sent to. A spawn step owns the
    /// agent it started, so it is its own continuation target; every other
    /// step uses the lineage target.
    pub fn continuation_target(&self, node_id: &str) -> Option<i64> {
        if matches!(
            self.graph.node(node_id).map(|n| &n.kind),
            Some(CircuitNodeKind::SpawnAgentNode { .. })
        ) {
            return self.step(node_id).and_then(|step| step.agent_node_id);
        }
        self.resolve_target_agent(node_id)
    }

    /// The agent a result reminder may be sent to: the continuation target,
    /// never a borrowed source agent.
    fn result_reminder_target(&self, node_id: &str) -> Option<i64> {
        self.continuation_target(node_id)
            .filter(|target| Some(*target) != self.context.source_agent_id())
    }

    /// True when this turn (identified by its report revision and lifecycle
    /// stamp) has already been reminded. The report can stay identical across
    /// turns, so the stamp is what tells a new turn from the same observation.
    fn result_reminder_observed(&self, node_id: &str, revision: &str, stamp: &str) -> bool {
        self.context
            .get(&format!("node.{node_id}.result_reminder_revision"))
            == Some(revision)
            && self
                .context
                .get(&format!("node.{node_id}.result_reminder_stamp"))
                == Some(stamp)
    }

    fn result_reminders_sent(&self, node_id: &str, attempt: i32) -> u32 {
        self.context
            .get(&format!("node.{node_id}.result_reminders.{attempt}"))
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0)
    }

    /// Durable per-attempt read failures, with the same ownership fence for
    /// the pure transition and the worker's attention decision.
    pub fn result_read_failures(
        &self,
        node_id: &str,
        attempt: i32,
        agent_node_id: i64,
    ) -> Option<u32> {
        if self.state != RunState::Running
            || !self.step(node_id).is_some_and(|step| {
                step.attempt == attempt
                    && matches!(step.status, StepStatus::Running | StepStatus::Unverified)
                    && step
                        .agent_node_id
                        .or_else(|| self.resolve_target_agent(node_id))
                        == Some(agent_node_id)
            })
        {
            return None;
        }
        Some(
            self.context
                .get(&format!("node.{node_id}.result_read_failures.{attempt}"))
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0),
        )
    }

    /// Decide what a finished turn that still owes its result file calls for.
    /// Shared by the stepper and the worker's blocking decision, so both act on
    /// the same rule.
    pub fn result_reminder_decision(
        &self,
        node_id: &str,
        attempt: i32,
        revision: &str,
        stamp: &str,
    ) -> ResultReminderDecision {
        if self.state != RunState::Running
            || !self.step(node_id).is_some_and(|s| {
                matches!(s.status, StepStatus::Running | StepStatus::Unverified)
                    && s.attempt == attempt
            })
            || self
                .context
                .get(&format!("node.{node_id}.result_attention.{attempt}"))
                == Some("1")
        {
            return ResultReminderDecision::Ignore;
        }
        let delivery = self
            .context
            .get(&format!("node.{node_id}.continuation.delivery"));
        if delivery == Some("claimed") {
            return ResultReminderDecision::Ignore;
        }
        if self.result_reminder_observed(node_id, revision, stamp) {
            // Nothing has changed since an undeliverable reminder, so sending it
            // again would loop; a delivered one is simply still in progress.
            return if delivery == Some("obsolete") {
                ResultReminderDecision::Exhausted
            } else {
                ResultReminderDecision::Ignore
            };
        }
        if self.result_reminder_target(node_id).is_none() {
            return ResultReminderDecision::NotOwned;
        }
        let sent = self.result_reminders_sent(node_id, attempt);
        if sent >= 2 {
            ResultReminderDecision::Exhausted
        } else {
            ResultReminderDecision::Remind(sent + 1)
        }
    }

    /// Resolve the implementation agent whose worktree an OpenPr action
    /// inspects. This is deliberately separate from target-agent resolution:
    /// an OpenPr step observes a repository, it does not pilot a process.
    pub fn resolve_open_pr_agent(&self, node_id: &str) -> Option<i64> {
        if !matches!(
            self.graph.node(node_id).map(|node| &node.kind),
            Some(CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                ..
            })
        ) {
            return None;
        }
        resolve_upstream_spawn_agent(&self.graph, &self.steps, node_id)
    }

    /// Attach the spawned mesh agent node to its step. Called by the seam
    /// right after the synchronous stage-1 row creation succeeds.
    pub fn attach_agent_node(&mut self, node_id: &str, agent_node_id: i64) {
        if let Some(step) = self.step_mut(node_id) {
            step.agent_node_id = Some(agent_node_id);
        }
    }
}

/// Resolve the target agent node id for any step kind. The target comes
/// from the node's own kind — explicit `target_node_id` if set, else the
/// nearest upstream `SpawnAgentNode` in the graph's dependency lineage.
///
/// Free-function form (not a method on `RunView`) so the per-tick
/// observation hot path doesn't pay a `RunView` clone per call. Both
/// `RunView::resolve_target_agent` (for ergonomic call sites that
/// already hold a view) and the circuit worker's orphan-detection helper
/// (`observed_agent_for_step`) call this.
pub fn resolve_target_agent(
    graph: &CircuitGraph,
    steps: &[StepView],
    node_id: &str,
) -> Option<i64> {
    let explicit = match graph.node(node_id).map(|n| &n.kind) {
        Some(CircuitNodeKind::InjectPty { target_node_id, .. }) => target_node_id.as_deref(),
        Some(CircuitNodeKind::LlmTurnClassifier { target_node_id }) => target_node_id.as_deref(),
        Some(CircuitNodeKind::AwaitAgentTurn { target_node_id })
        | Some(CircuitNodeKind::ReviewVerdict { target_node_id }) => target_node_id.as_deref(),
        Some(CircuitNodeKind::SetNodeStatus { target_node_id, .. }) => target_node_id.as_deref(),
        Some(CircuitNodeKind::CloseAgentNode { target_node_id, .. }) => target_node_id.as_deref(),
        // Notify / Join / RetryLimit / AnyCompleted /
        // Manual / Interval / SpawnAgentNode have no target lineage —
        // nothing to resolve. `SpawnAgentNode` owns its agent directly
        // via `step.agent_node_id`, not via this resolver.
        _ => return None,
    };
    if let Some(target) = explicit {
        let is_spawn = matches!(
            graph.node(target).map(|n| &n.kind),
            Some(CircuitNodeKind::SpawnAgentNode { .. })
        );
        if !is_spawn {
            return None;
        }
        if target != node_id && !graph.is_ancestor(node_id, target) {
            return None;
        }
        return steps
            .iter()
            .find(|s| s.node_id == target)
            .and_then(|s| s.agent_node_id);
    }
    resolve_upstream_spawn_agent(graph, steps, node_id)
}

/// Walk backward through incoming edges and return the nearest upstream
/// SpawnAgentNode's attached agent. This is shared by explicit lifecycle
/// targets and the repository-observation OpenPr seam, but only callers that
/// model a process target may use it for lifecycle events.
pub fn resolve_upstream_spawn_agent(
    graph: &CircuitGraph,
    steps: &[StepView],
    node_id: &str,
) -> Option<i64> {
    let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
    queue.push_back(node_id);
    visited.insert(node_id);
    while let Some(curr) = queue.pop_front() {
        for edge in graph.incoming(curr) {
            let from = edge.from.as_str();
            if visited.insert(from) {
                if let Some(node) = graph.node(from) {
                    if matches!(node.kind, CircuitNodeKind::SpawnAgentNode { .. }) {
                        if let Some(step) = steps.iter().find(|s| s.node_id == from) {
                            if let Some(agent_id) = step.agent_node_id {
                                return Some(agent_id);
                            }
                        }
                    }
                }
                queue.push_back(from);
            }
        }
    }
    None
}

/// Resolve the SpawnAgentNode that owns a classifier's target agent. A
/// classifier often runs after the spawn step is terminal, so its own step
/// cannot be used as the output namespace owner.
fn spawn_node_for_agent(run: &RunView, classifier_node_id: &str) -> Option<String> {
    let agent_node_id = run.resolve_target_agent(classifier_node_id)?;
    run.steps
        .iter()
        .find(|step| {
            step.agent_node_id == Some(agent_node_id)
                && matches!(
                    run.graph.node(&step.node_id).map(|node| &node.kind),
                    Some(CircuitNodeKind::SpawnAgentNode { .. })
                )
        })
        .map(|step| step.node_id.clone())
}

/// Capacity snapshot carried inside `CircuitEvent::Tick`. Computed by the
/// impure seam from live counts; the stepper only does arithmetic on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    /// Free slots in this run's durable agent lease, additionally bounded by
    /// the optional app-wide process pool.
    pub agent_free_slots: i64,
}

/// First-observation window for a busy agent (issue #1791). A wait whose
/// agent has never produced a session identity or a readable report fails
/// fast after this long instead of burning the full active-wait budget:
/// a busy-but-unobservable agent is a broken observer or a dead subprocess,
/// not a slow harness. Once any observation lands, the step keeps its
/// longer budget so legitimately slow harnesses are not penalised; an
/// authored per-step budget (`explicit_budget`) takes precedence entirely.
const UNOBSERVED_WAIT_MS: i64 = 15 * 60_000;

/// Terminal failure surfaced when a busy agent stays unobservable past
/// [`UNOBSERVED_WAIT_MS`].
const UNOBSERVED_AGENT_REASON: &str =
    "agent produced no session identity or report within 15 minutes — inspect the agent terminal";

/// An evidence window in operator wording. Per-harness yielded budgets are
/// under a minute, so whole-minute division would report them as "0 minutes".
fn describe_window(ms: i64) -> String {
    let seconds = ms / 1_000;
    if seconds > 0 && seconds % 60 == 0 {
        let minutes = seconds / 60;
        format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
    } else {
        format!("{seconds} second{}", if seconds == 1 { "" } else { "s" })
    }
}

/// Run-context key prefix for a step's wait bookkeeping.
fn wait_prefix(node_id: &str) -> String {
    format!("node.{node_id}.wait")
}

/// One observed fact from outside the pure core. Kept minimal: every
/// variant maps 1:1 to something the seam can observe cheaply each tick.
///
/// There is deliberately NO keystroke/user-input variant — human typing
/// in a piloted terminal cannot reach the stepper, which is exactly the
/// "manual PTY interaction never breaks a run" guarantee.
#[derive(Debug, Clone)]
pub enum CircuitEvent {
    ObservationBatch {
        receipt_id: i64,
        expected: super::observation::ObservationIdentity,
        observations: Vec<super::observation::CircuitObservation>,
        stale: bool,
        input_guard: Option<ObservationInputFence>,
    },
    Observed {
        expected: super::observation::ObservationIdentity,
        observation: Box<super::observation::CircuitObservation>,
    },
    EffectUncertain {
        node_id: String,
        attempt: i32,
        reason: String,
    },
    ObservationDeferred {
        node_id: String,
        attempt: i32,
        agent_node_id: i64,
        blocker: super::observation::CircuitObservationBlocker,
    },
    ContinuationObserved {
        node_id: String,
        attempt: i32,
        stamp: String,
        revision: String,
        input_stamp: String,
    },
    ContinuationRetry {
        node_id: String,
        attempt: i32,
    },
    ContinuationDelivered {
        node_id: String,
        attempt: i32,
    },
    ContinuationObsolete {
        node_id: String,
        attempt: i32,
    },
    ContinuationUncertain {
        node_id: String,
        attempt: i32,
        error: String,
    },
    /// The agent's turn finished but the result file its prompt asked for is
    /// missing. The stepper answers with a bounded reminder, or fails the step
    /// once the reminders are spent.
    ResultFileMissing {
        node_id: String,
        attempt: i32,
        result_path: String,
        stamp: String,
        revision: String,
        input_stamp: String,
    },
    /// The run was triggered. Renamed from `ManualTriggered` in #1208:
    /// runs are minted pending by ANY trigger dispatch (Trigger Now,
    /// a GitHub poll ingest, an interval fire) and this event is
    /// trigger-kind agnostic.
    Triggered,
    /// Periodic fast tick carrying current capacity.
    Tick(Capacity),
    /// A throttled observation of a waiting step. Progress is a transcript
    /// revision, never a polling timestamp or terminal redraw counter.
    WaitObserved {
        node_id: String,
        attempt: i32,
        now_ms: i64,
        progress: Option<String>,
        /// True when the seam has observed a session identity or a readable
        /// report for this wait's agent. The seam reports `true` for steps
        /// with nothing to observe (approval gates, prerequisite waits) so
        /// they keep their own budgets instead of tripping the fast fail.
        observed: bool,
        /// True when the author pinned this step's wall-clock budget
        /// (`SpawnAgentNode.timeout_seconds`, #1219). An authored budget takes
        /// precedence: it is the authority on when to give up, so the
        /// unobserved fast fail does not preempt it.
        explicit_budget: bool,
        reason: String,
        timeout_ms: i64,
    },
    /// The seam observed the injected prompt's target process is now live.
    AgentReady {
        node_id: String,
    },
    PromptDelivered {
        node_id: String,
        attempt: i32,
    },
    /// Legacy process callback. Success acknowledges only an empty-prompt
    /// spawn allocation; assigned work requires typed observation evidence.
    /// Failure reports a confirmed process error.
    AgentFinished {
        agent_node_id: i64,
        success: bool,
        output: Option<String>,
    },
    /// The step's piloted agent was closed/archived mid-run.
    AgentLost {
        agent_node_id: i64,
    },
    // -- Milestone 2 (#1207) --
    /// The user paused the run. Current steps finish; nothing advances.
    Paused,
    /// The user resumed a paused run.
    Resumed,
    /// The user approved a CollaboratorCheck gate parked in Blocked.
    CollaboratorApproved {
        node_id: String,
    },
    /// The seam classified the piloted agent's latest turn for this
    /// LlmTurnClassifier gate. `None` = classifier unavailable; it is recorded
    /// as a retryable error and never routed as a step outcome.
    TurnClassified {
        binding: Option<ClassificationBinding>,
        node_id: String,
        classification: Option<Classification>,
        /// Terminal output captured from the classifier's target agent.
        /// This is separate from the classification because the target
        /// SpawnAgentNode may have completed before this gate runs.
        output: Option<String>,
    },
    ClassifierErrorObserved {
        node_id: String,
        attempt: i32,
        error: String,
    },
    /// Readiness inference failed without establishing a report or turn verdict.
    ClassifierUnavailable {
        node_id: String,
        attempt: i32,
        error: String,
    },
    /// The seam observed the piloted agent's latest report for this gate and
    /// deliberately did not classify it: the agent is still working, so the
    /// report is not this gate's result. The stepper records the observation
    /// (so an unchanged report is not re-observed on the next probe) and
    /// nothing else — no classification, no *classifier-outage* error, and no
    /// share of the classifier-failure budget, which is reserved for a real
    /// outage (run 163's reviewer yielded mid-turn with a progress line). The
    /// gate's generic yielded-wait reason and its deadline still arrive from
    /// the separate `WaitObserved`, which is what bounds the wait.
    TurnParked {
        node_id: String,
        output: String,
        report_revision: Option<String>,
    },
    /// The seam ran the DeterministicVerification command.
    VerificationResult {
        node_id: String,
        green: bool,
    },
    /// The seam executed a GitHub action (e.g. OpenPr, AddLabel).
    GithubActionResult {
        node_id: String,
        success: bool,
        pr_number: Option<i64>,
        pr_url: Option<String>,
        pr_head_ref: Option<String>,
        pr_title: Option<String>,
        error: Option<String>,
    },
    /// The worker found an unacknowledged action after a prior pass or restart.
    GithubActionRetry {
        node_id: String,
    },
    /// Reconcile a saved PR target without replaying a mutation.
    GithubRecheckDue {
        node_id: String,
        attempt: i32,
        now_ms: i64,
    },
    /// Re-run a committed CloseAgentNode effect after a crash between the
    /// step commit and the node deletion/association cleanup.
    CloseAgentRetry {
        node_id: String,
    },
}

// ---------------------------------------------------------------------------
// Effects — everything the seam must do on the stepper's behalf.
// ---------------------------------------------------------------------------

/// One persisted mutation of a step row. For `error`, the outer `None` means
/// "leave as-is" while `Some(None)` explicitly clears a stored error.
#[derive(Debug, Clone, PartialEq)]
pub struct StepWrite {
    pub node_id: String,
    pub status: StepStatus,
    pub outcome: Option<Option<StepOutcome>>,
    pub error: Option<Option<String>>,
    pub agent_node_id: Option<i64>,
    /// The step's execution count after this write (retry bookkeeping).
    pub attempt: i32,
    /// True for a retry reset: clear outcome/error, restamp started_at.
    pub fresh_attempt: bool,
}

impl StepWrite {
    fn for_existing(node_id: &str, status: StepStatus, attempt: i32) -> Self {
        Self {
            node_id: node_id.to_string(),
            status,
            outcome: None,
            error: None,
            agent_node_id: None,
            attempt,
            fresh_attempt: false,
        }
    }
}

/// An explicit action the impure seam executes after committing the
/// transition. Kept small on purpose — milestone 1 covers the action
/// subset; gate/GitHub effects join in later milestones.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    SpawnAgentNode {
        node_id: String,
    },
    InjectPty {
        node_id: String,
        prompt: String,
        target_node_id: Option<String>,
    },
    /// Continue a classifier-owned agent after an explicit ordinary-work
    /// verdict. This has a separate lifecycle from author-authored PTY
    /// injections because delivery is claimed and settled independently.
    ContinueAgentTurn {
        node_id: String,
        target_agent_id: i64,
        prompt: String,
    },
    SetNodeStatus {
        node_id: String,
        status: String,
        target_node_id: Option<String>,
    },
    /// Kill and retire the targeted agent node, including deferred worktree
    /// cleanup. This is stronger than setting the status to `completed`.
    CloseAgentNode {
        node_id: String,
        target_node_id: Option<String>,
    },
    Notify {
        message: String,
    },
    /// Milestone 3 (issue #1208): perform a GitHub mutation against the
    /// run's trigger repo/issue. `label`/`comment` are the raw blueprint
    /// templates — the seam resolves them against the run context at
    /// execution time. A synchronous failure fails the step (the seam's
    /// effect-failure path), so the mutation is retried only by
    /// re-triggering, never silently.
    CallGithub {
        node_id: String,
        action: GithubActionKind,
        label: Option<String>,
        comment: Option<String>,
    },
}

/// Everything one `advance` call decided: the step-row writes and the
/// side-effecting actions. The caller applies writes first (atomically),
/// then executes effects.
#[derive(Debug, Default, PartialEq)]
pub struct Transition {
    pub input_guard: Option<ObservationInputFence>,
    pub observations: Vec<super::observation::RecordedObservation>,
    pub classifications: Vec<super::observation::RecordedClassification>,
    pub expected: Option<TransitionFence>,
    pub step_writes: Vec<StepWrite>,
    pub effects: Vec<Effect>,
    pub run_state_changed: bool,
    /// True when a context-only event changed the persisted blackboard.
    /// Agent output can arrive after its SpawnAgentNode step is terminal,
    /// so there may be no step write to signal the worker to commit it.
    pub context_changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassificationBinding {
    pub owner: super::observation::ObservationIdentity,
    pub report_revision: String,
    pub input_guard: ObservationInputFence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservationInputFence {
    pub(crate) transcript_guard: Option<crate::services::transcript_reader::NativeTurnSnapshot>,
    pub(crate) report_guard:
        Option<crate::services::transcript_reader::report_snapshot::ReportSnapshot>,
    pub agent_node_id: i64,
    pub input_stamp: String,
    pub observed_at_ms: i64,
    pub session_id: String,
    pub session_incarnation: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransitionFence {
    pub state: RunState,
    pub steps: Vec<StepView>,
    pub revision: Option<i64>,
}

impl Transition {
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.step_writes.is_empty()
            && self.observations.is_empty()
            && self.classifications.is_empty()
            && self.effects.is_empty()
            && !self.run_state_changed
            && !self.context_changed
    }
}

// ---------------------------------------------------------------------------
// The stepper.
// ---------------------------------------------------------------------------

/// What a finished turn that still owes its result file calls for. Produced by
/// [`RunView::result_reminder_decision`] and read by both the stepper and the
/// worker, so neither can block or send on a different rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultReminderDecision {
    /// Not waiting on this turn: the step moved on, a delivery is in flight, or
    /// this observation was already answered.
    Ignore,
    /// Send reminder `n` (1 or 2) for this attempt.
    Remind(u32),
    /// The reminders are spent, or the last one could not be delivered and the
    /// turn has not changed since, so the step cannot finish on its own.
    Exhausted,
    /// The agent is borrowed from the source, so no reminder may be sent.
    NotOwned,
}

fn result_reminder_prompt(path: &str) -> String {
    format!(
        "Save your final report, ending with the required result line, to {path} now using UTF-8 (overwrite it). The file is missing, blank, or not valid UTF-8. In Windows PowerShell, use Set-Content -Encoding UTF8. Do not redo the work."
    )
}

fn continuation_effect(node_id: &str, target_agent_id: i64) -> Effect {
    Effect::ContinueAgentTurn {
        node_id: node_id.into(),
        target_agent_id,
        prompt: "Continue the remaining work already assigned to you and run its relevant checks. Do not expand scope, approve permissions, or guess answers to questions requiring the user. If you are blocked on such a decision, report the blocker explicitly.".into(),
    }
}

/// Advance one run by one event and return what to persist before effects.
pub fn advance(run: &mut RunView, event: &CircuitEvent) -> Transition {
    let expected = TransitionFence {
        state: run.state,
        steps: run.steps.clone(),
        revision: run
            .context
            .get("evidence.revision")
            .and_then(|v| v.parse().ok()),
    };
    let mut transition = advance_inner(run, event);
    transition.expected = Some(expected);
    transition
}

fn apply_observation(
    run: &mut RunView,
    t: &mut Transition,
    expected: &super::observation::ObservationIdentity,
    observation: &super::observation::CircuitObservation,
) -> bool {
    use super::observation::{ObservationDisposition as D, RecordedObservation, WorkEvidence};
    let step = run.step(&expected.step_id);
    let current = run.run_id == expected.run_id
        && matches!(run.state, RunState::Running | RunState::Paused)
        && step.is_some_and(|s| s.attempt == expected.attempt && !s.status.is_terminal())
        && step
            .and_then(|s| s.agent_node_id)
            .or_else(|| run.resolve_target_agent(&expected.step_id))
            == Some(expected.agent_node_id);
    let key = format!("node.{}.evidence.{}", expected.step_id, expected.attempt);
    let mut evidence: WorkEvidence = run
        .context
        .get(&key)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let disposition = if current {
        evidence.observe(expected, observation)
    } else {
        D::Rejected
    };
    t.observations.push(RecordedObservation {
        observation: observation.clone(),
        disposition,
    });
    if disposition == D::Duplicate && !evidence.lifecycle_verified() && !evidence.has_human_wait() {
        if let super::observation::ObservedWorkFact::OwnershipUnavailable { reason } =
            &observation.fact
        {
            if run
                .step(&expected.step_id)
                .is_some_and(|step| step.status == StepStatus::Running)
            {
                unverify_step(run, t, &expected.step_id, reason.clone());
            }
        }
    }
    if matches!(disposition, D::Rejected | D::Duplicate) {
        return false;
    }
    run.context.set(
        &key,
        serde_json::to_string(&evidence).expect("observation contains serializable values"),
    );
    if evidence.has_human_wait() {
        run.context
            .set(&format!("node.{}.human_wait", expected.step_id), "1");
    } else if matches!(
        observation.fact,
        super::observation::ObservedWorkFact::HumanResponse { .. }
            | super::observation::ObservedWorkFact::ToolResponse { .. }
            | super::observation::ObservedWorkFact::ToolFailed { .. }
    ) && disposition == D::Accepted
    {
        run.context
            .set(&format!("node.{}.human_wait", expected.step_id), "0");
    }
    t.context_changed = true;
    if let super::observation::ObservedWorkFact::AssistantReport { text, .. } = &observation.fact {
        if disposition == D::Accepted {
            run.context
                .set(&format!("node.{}.output", expected.step_id), text.clone());
        }
    }
    if matches!(disposition, D::Unavailable | D::Conflicting) && !evidence.has_human_wait() {
        let reason = match &observation.fact {
            super::observation::ObservedWorkFact::OwnershipUnavailable { reason } => reason.clone(),
            _ => "Lifecycle or owned-work evidence is unavailable or conflicting. Inspect the agent and Recheck evidence.".into(),
        };
        unverify_step(run, t, &expected.step_id, reason);
    }
    disposition == D::Accepted
}

fn finish_observed_step(
    run: &mut RunView,
    t: &mut Transition,
    expected: &super::observation::ObservationIdentity,
) {
    if !run
        .step(&expected.step_id)
        .is_some_and(|s| s.attempt == expected.attempt && !s.status.is_terminal())
    {
        return;
    }
    if run.has_human_wait(&expected.step_id) {
        return;
    }
    let key = format!("node.{}.evidence.{}", expected.step_id, expected.attempt);
    let Some(evidence) = run
        .context
        .get(&key)
        .and_then(|s| serde_json::from_str::<super::observation::WorkEvidence>(s).ok())
    else {
        return;
    };
    let hands_off_to_classifier = matches!(
        run.graph.node(&expected.step_id).map(|n| &n.kind),
        Some(CircuitNodeKind::SpawnAgentNode { .. })
    ) && run
        .graph
        .edges
        .iter()
        .any(|edge| edge.from == expected.step_id)
        && run
            .graph
            .edges
            .iter()
            .filter(|edge| edge.from == expected.step_id)
            .all(|edge| {
                matches!(
                    run.graph.node(&edge.to).map(|n| &n.kind),
                    Some(
                        CircuitNodeKind::LlmTurnClassifier { .. }
                            | CircuitNodeKind::ReviewVerdict { .. }
                    )
                )
            });
    if evidence.completion_verified() || (hands_off_to_classifier && evidence.lifecycle_verified())
    {
        set_step(run, t, &expected.step_id, StepStatus::Completed);
        cascade_after_completion(run, t);
        finish_run_if_done(run, t);
    }
}

fn advance_inner(run: &mut RunView, event: &CircuitEvent) -> Transition {
    let mut t = Transition::default();
    match event {
        CircuitEvent::ObservationBatch {
            receipt_id,
            expected,
            observations,
            stale,
            input_guard,
        } => {
            if !matches!(run.state, RunState::Running | RunState::Paused) {
                return t;
            }
            let mut accepted = false;
            for observation in observations {
                if *stale {
                    t.observations
                        .push(super::observation::RecordedObservation {
                            observation: observation.clone(),
                            disposition: super::observation::ObservationDisposition::Rejected,
                        });
                } else {
                    accepted |= apply_observation(run, &mut t, expected, observation);
                }
            }
            if accepted {
                if let Some(guard) = input_guard {
                    let key = format!("node.{}.evidence.{}", expected.step_id, expected.attempt);
                    if let Some(mut evidence) = run.context.get(&key).and_then(|json| {
                        serde_json::from_str::<super::observation::WorkEvidence>(json).ok()
                    }) {
                        if let Some(report) = evidence.report.as_mut() {
                            if t.observations.iter().any(|record| record.disposition == super::observation::ObservationDisposition::Accepted
                                && record.observation.observed_at_ms == report.observed_at_ms
                                && matches!(&record.observation.fact, super::observation::ObservedWorkFact::AssistantReport { revision, .. } if revision == &report.revision)) {
                                report.input_stamp = Some(guard.input_stamp.clone());
                            }
                        }
                        run.context.set(
                            &key,
                            serde_json::to_string(&evidence).expect("serializable evidence"),
                        );
                        t.context_changed = true;
                    }
                }
            }
            if accepted
                && !t.observations.iter().any(|record| {
                    matches!(
                        record.disposition,
                        super::observation::ObservationDisposition::Unavailable
                            | super::observation::ObservationDisposition::Conflicting
                            | super::observation::ObservationDisposition::Rejected
                    )
                })
            {
                finish_observed_step(run, &mut t, expected);
            }
            if accepted || t.context_changed || !t.step_writes.is_empty() {
                t.input_guard = input_guard.clone();
            }
            if *receipt_id > 0 {
                run.context
                    .set("observer.receipt_cursor", receipt_id.to_string());
                t.context_changed = true;
            }
        }
        CircuitEvent::Observed {
            expected,
            observation,
        } => {
            if apply_observation(run, &mut t, expected, observation) {
                finish_observed_step(run, &mut t, expected);
            }
        }
        CircuitEvent::ObservationDeferred {
            node_id,
            attempt,
            agent_node_id,
            blocker,
        } => {
            if run.state != RunState::Running
                || !run.step(node_id).is_some_and(|step| {
                    step.attempt == *attempt
                        && matches!(step.status, StepStatus::Running | StepStatus::Unverified)
                        && step
                            .agent_node_id
                            .or_else(|| run.resolve_target_agent(node_id))
                            == Some(*agent_node_id)
                })
            {
                return t;
            }
            let key = format!("node.{node_id}.observation_blocker");
            let encoded = serde_json::to_string(blocker).expect("observation blocker");
            let mut reason = blocker.message();
            if matches!(
                blocker,
                super::observation::CircuitObservationBlocker::ResultFileUnavailable { .. }
            ) {
                let count = run
                    .result_read_failures(node_id, *attempt, *agent_node_id)
                    .expect("validated result read owner")
                    .saturating_add(1)
                    .min(3);
                let key = format!("node.{node_id}.result_read_failures.{attempt}");
                if run.context.get(&key) != Some(count.to_string().as_str()) {
                    run.context.set(&key, count.to_string());
                    t.context_changed = true;
                }
                reason = if count < 3 {
                    format!(
                        "{reason} Failed read {count} of 3; the next scheduled probe will retry."
                    )
                } else {
                    format!("{reason} Result remains unreadable after 3 failed probes; attention is required. Repair the file to allow observation to recover.")
                };
            }
            if run.context.get(&key) != Some(encoded.as_str()) {
                run.context.set(&key, encoded);
                t.context_changed = true;
            }
            let waiting_for_background = matches!(
                blocker,
                super::observation::CircuitObservationBlocker::KnownWorkOutstanding
            );
            if !waiting_for_background
                && run.step(node_id).is_some_and(|step| {
                    step.status != StepStatus::Unverified
                        || step.error.as_deref() != Some(reason.as_str())
                })
            {
                unverify_step(run, &mut t, node_id, reason);
            }
        }
        CircuitEvent::EffectUncertain {
            node_id,
            attempt,
            reason,
        } => {
            if run.state == RunState::Running
                && run
                    .step(node_id)
                    .is_some_and(|s| s.attempt == *attempt && s.status == StepStatus::Running)
            {
                unverify_step(run, &mut t, node_id, reason.clone());
            }
        }
        CircuitEvent::ContinuationObserved {
            node_id,
            attempt,
            stamp,
            revision,
            input_stamp,
        } => {
            if run.state == RunState::Running
                && run.step(node_id).is_some_and(|s| {
                    matches!(s.status, StepStatus::Running | StepStatus::Unverified)
                        && s.attempt == *attempt
                })
            {
                run.context
                    .set(&format!("node.{node_id}.continuation.stamp"), stamp.clone());
                run.context.set(
                    &format!("node.{node_id}.continuation.revision"),
                    revision.clone(),
                );
                run.context.set(
                    &format!("node.{node_id}.continuation.input"),
                    input_stamp.clone(),
                );
                t.context_changed = true;
            }
        }
        CircuitEvent::ContinuationRetry { node_id, attempt } => {
            if run.state == RunState::Running
                && run
                    .step(node_id)
                    .is_some_and(|s| s.status == StepStatus::Running && s.attempt == *attempt)
                && run.context.get(&format!("node.{node_id}.recheck_only")) != Some("1")
                && run
                    .context
                    .get(&format!("node.{node_id}.continuation.attempt"))
                    .and_then(|v| v.parse::<i32>().ok())
                    == Some(*attempt)
                && run
                    .context
                    .get(&format!("node.{node_id}.continuation.delivery"))
                    == Some("pending")
            {
                if let Some(target_agent_id) = run.continuation_target(node_id) {
                    run.context
                        .set(&format!("node.{node_id}.continuation.delivery"), "claimed");
                    t.context_changed = true;
                    t.effects
                        .push(continuation_effect(node_id, target_agent_id));
                }
            }
        }
        CircuitEvent::ContinuationDelivered { node_id, attempt }
        | CircuitEvent::ContinuationObsolete { node_id, attempt }
        | CircuitEvent::ContinuationUncertain {
            node_id, attempt, ..
        } => {
            let key = format!("node.{node_id}.continuation.delivery");
            let current = run.context.get(&key);
            if run.state == RunState::Running
                && run
                    .step(node_id)
                    .is_some_and(|s| s.status == StepStatus::Running && s.attempt == *attempt)
                && current == Some("claimed")
            {
                let delivery = match event {
                    CircuitEvent::ContinuationDelivered { .. } => "delivered",
                    CircuitEvent::ContinuationObsolete { .. } => "obsolete",
                    CircuitEvent::ContinuationUncertain { .. } => "uncertain",
                    _ => unreachable!(),
                };
                run.context.set(&key, delivery);
                if let CircuitEvent::ContinuationUncertain { error, .. } = event {
                    unverify_step(run, &mut t, node_id, format!("Continuation delivery is unverified: {error}. Inspect the agent; the prompt will not be replayed."));
                }
                t.context_changed = true;
            }
        }
        CircuitEvent::ResultFileMissing {
            node_id,
            attempt,
            result_path,
            stamp,
            revision,
            input_stamp,
        } => {
            // One reminder per observed turn (report revision and lifecycle
            // stamp). The decision is shared with the worker, which blocks the
            // agent on the same Exhausted/NotOwned outcome.
            match run.result_reminder_decision(node_id, *attempt, revision, stamp) {
                ResultReminderDecision::Ignore => return t,
                ResultReminderDecision::Remind(count) => {
                    let Some(target_agent_id) = run.result_reminder_target(node_id) else {
                        return t;
                    };
                    let count_key = format!("node.{node_id}.result_reminders.{attempt}");
                    run.context.set(&count_key, count.to_string());
                    run.context.set(
                        &format!("node.{node_id}.result_reminder_revision"),
                        revision.clone(),
                    );
                    run.context.set(
                        &format!("node.{node_id}.result_reminder_stamp"),
                        stamp.clone(),
                    );
                    run.context
                        .set(&format!("node.{node_id}.continuation.stamp"), stamp.clone());
                    run.context.set(
                        &format!("node.{node_id}.continuation.revision"),
                        revision.clone(),
                    );
                    run.context.set(
                        &format!("node.{node_id}.continuation.input"),
                        input_stamp.clone(),
                    );
                    run.context
                        .set(&format!("node.{node_id}.continuation.delivery"), "claimed");
                    run.context.set(
                        &format!("node.{node_id}.continuation.attempt"),
                        attempt.to_string(),
                    );
                    t.context_changed = true;
                    let error = format!(
                        "Agent finished without saving its result file; reminder {count} of 2 sent."
                    );
                    if let Some(step) = run.step_mut(node_id) {
                        step.status = StepStatus::Running;
                        step.error = Some(error.clone());
                    }
                    t.step_writes.push(StepWrite {
                        node_id: node_id.clone(),
                        status: StepStatus::Running,
                        outcome: None,
                        error: Some(Some(error)),
                        agent_node_id: None,
                        attempt: *attempt,
                        fresh_attempt: false,
                    });
                    t.effects.push(Effect::ContinueAgentTurn {
                        node_id: node_id.clone(),
                        target_agent_id,
                        prompt: result_reminder_prompt(result_path),
                    });
                }
                ResultReminderDecision::Exhausted => {
                    run.context
                        .set(&format!("node.{node_id}.result_attention.{attempt}"), "1");
                    t.context_changed = true;
                    let undeliverable = run.result_reminder_observed(node_id, revision, stamp)
                        && run
                            .context
                            .get(&format!("node.{node_id}.continuation.delivery"))
                            == Some("obsolete");
                    let reason = if undeliverable {
                        format!(
                            "Agent finished without saving its result file to {result_path}, and the reminder could not be delivered. Inspect the agent; no further reminder will be sent."
                        )
                    } else {
                        format!(
                            "Agent finished without saving its result file to {result_path} after 2 reminders. Ask it to save the result, or inspect its report."
                        )
                    };
                    unverify_step(run, &mut t, node_id, reason);
                }
                ResultReminderDecision::NotOwned => {
                    run.context
                        .set(&format!("node.{node_id}.result_attention.{attempt}"), "1");
                    t.context_changed = true;
                    unverify_step(
                    run,
                    &mut t,
                    node_id,
                    format!(
                        "Agent finished without saving its result file to {result_path}; the agent is not owned by this circuit, so no reminder was sent."
                    ),
                    );
                }
            }
        }
        CircuitEvent::Triggered => {
            if run.state == RunState::Pending {
                run.state = RunState::Running;
                t.run_state_changed = true;
                // Every trigger root auto-completes: it fired to create
                // this run. Cloned first — the loop mutates `run`.
                let roots: Vec<crate::circuit::model::CircuitNode> =
                    run.graph.roots().into_iter().cloned().collect();
                for node in roots {
                    if matches!(
                        node.kind,
                        CircuitNodeKind::Manual
                            | CircuitNodeKind::Interval { .. }
                            | CircuitNodeKind::GithubIssueLabel { .. }
                            | CircuitNodeKind::GithubPullRequestLabel { .. }
                    ) {
                        set_step(run, &mut t, &node.id, StepStatus::Completed);
                    } else {
                        fail_step(
                            run,
                            &mut t,
                            &node.id,
                            format!("trigger kind {:?} cannot start this run", node.kind),
                        );
                    }
                }
            }
        }
        CircuitEvent::Tick(capacity) => {
            if run.state == RunState::Running {
                schedule_ready(run, &mut t, *capacity);
                finish_run_if_done(run, &mut t);
            }
        }
        CircuitEvent::WaitObserved {
            node_id,
            attempt,
            now_ms,
            progress,
            observed,
            explicit_budget,
            reason,
            timeout_ms,
        } => {
            if run.state != RunState::Running
                || !run.step(node_id).is_some_and(|s| {
                    s.attempt == *attempt
                        && matches!(s.status, StepStatus::Running | StepStatus::Blocked)
                })
            {
                return t;
            }
            let prefix = wait_prefix(node_id);
            let same_attempt = run
                .context
                .get(&format!("{prefix}.attempt"))
                .and_then(|s| s.parse::<i32>().ok())
                == Some(*attempt);
            let same_mode = run
                .context
                .get(&format!("{prefix}.timeout_ms"))
                .and_then(|s| s.parse::<i64>().ok())
                == Some(*timeout_ms);
            let changed = progress
                .as_deref()
                .is_some_and(|p| run.context.get(&format!("{prefix}.progress")) != Some(p));
            let since = run
                .context
                .get(&format!("{prefix}.since_ms"))
                .and_then(|s| s.parse::<i64>().ok());
            let reset = !same_attempt || !same_mode || changed || since.is_none();
            if reset {
                run.context
                    .set(&format!("{prefix}.attempt"), attempt.to_string());
                run.context
                    .set(&format!("{prefix}.timeout_ms"), timeout_ms.to_string());
                run.context
                    .set(&format!("{prefix}.since_ms"), now_ms.to_string());
                // A reset starts a new observation window: the sticky flag is
                // re-derived from this event rather than carried across an
                // attempt or budget change.
                run.context.set(
                    &format!("{prefix}.observed"),
                    if *observed { "1" } else { "0" },
                );
                if let Some(progress) = progress {
                    run.context
                        .set(&format!("{prefix}.progress"), progress.clone());
                }
                t.context_changed = true;
            } else if *observed && run.context.get(&format!("{prefix}.observed")) != Some("1") {
                // A late session identity or report still lifts the fast fail
                // without restarting the budget its original wait began.
                run.context.set(&format!("{prefix}.observed"), "1");
                t.context_changed = true;
            }
            let explicit_value = if *explicit_budget { "1" } else { "0" };
            if run.context.get(&format!("{prefix}.explicit_budget")) != Some(explicit_value) {
                run.context
                    .set(&format!("{prefix}.explicit_budget"), explicit_value);
                t.context_changed = true;
            }
            let ever_observed = run.context.get(&format!("{prefix}.observed")) == Some("1");
            let elapsed = if reset {
                0
            } else {
                now_ms.saturating_sub(since.unwrap_or(*now_ms))
            };
            let expired = run
                .evidence_deadline_ms(node_id)
                .is_some_and(|deadline| *now_ms >= deadline);
            // A busy agent that has never been observable gets the short
            // first-observation window instead of the full active-wait budget
            // (issue #1791). An authored per-step budget (#1219) takes
            // precedence over that default window — the author chose when to
            // give up — so the fast fail only applies to default budgets. The
            // ordinary budget check below always still runs.
            if !*explicit_budget && expired && !ever_observed && elapsed >= UNOBSERVED_WAIT_MS {
                unverify_step(
                    run,
                    &mut t,
                    node_id,
                    format!(
                        "{UNOBSERVED_AGENT_REASON}. Recheck evidence after inspecting the agent."
                    ),
                );
                return t;
            }
            if expired {
                let detail = if reason.is_empty() {
                    "Agent produced no new report"
                } else {
                    reason
                };
                unverify_step(run, &mut t, node_id, format!("Evidence window ended after {}: {detail}. Recheck evidence after inspecting the agent.", describe_window(*timeout_ms)));
                return t;
            }
            if let Some(step) = run.step_mut(node_id) {
                let error = (!reason.is_empty()).then(|| reason.clone());
                if step.error != error {
                    step.error = error.clone();
                    let mut write = StepWrite::for_existing(node_id, step.status, *attempt);
                    write.error = Some(error);
                    t.step_writes.push(write);
                }
            }
        }
        CircuitEvent::PromptDelivered { node_id, attempt } => {
            if run.state == RunState::Running
                && run
                    .step(node_id)
                    .is_some_and(|s| s.status == StepStatus::Running && s.attempt == *attempt)
                && run
                    .context
                    .get(&format!("node.{node_id}.prompt_delivery.{attempt}"))
                    == Some("intent")
            {
                run.context.set(
                    &format!("node.{node_id}.prompt_delivery.{attempt}"),
                    "acknowledged",
                );
                t.context_changed = true;
                set_step(run, &mut t, node_id, StepStatus::Completed);
                cascade_after_completion(run, &mut t);
                finish_run_if_done(run, &mut t);
            }
        }
        CircuitEvent::AgentReady { node_id } => {
            // Fire the pending injection for a Running PTY step whose target
            // process just became live. `InjectPty` is the author-facing
            // generic action; the blueprint's finish prompt is resolved in
            // the persisted run context from the current finish.md and mesh
            // policy.
            let (prompt, target_node_id) = match run.graph.node(node_id) {
                Some(n) => match &n.kind {
                    CircuitNodeKind::InjectPty {
                        prompt,
                        target_node_id,
                    } => (Some(prompt.clone()), target_node_id.clone()),
                    _ => (None, None),
                },
                None => (None, None),
            };
            let attempt = run.step(node_id).map_or(1, |s| s.attempt);
            let delivery_key = format!("node.{node_id}.prompt_delivery.{attempt}");
            let is_running_inject = run.state == RunState::Running
                && matches!(run.step(node_id), Some(s) if s.status == StepStatus::Running)
                && run.context.get(&delivery_key).is_none();
            if let Some(prompt) = prompt {
                if is_running_inject {
                    t.effects.push(Effect::InjectPty {
                        node_id: node_id.clone(),
                        prompt: run.context.resolve(&prompt),
                        target_node_id,
                    });
                    run.context.set(&delivery_key, "intent");
                    t.context_changed = true;
                }
            }
        }
        CircuitEvent::AgentFinished {
            agent_node_id,
            success,
            output,
        } => {
            if run.state != RunState::Running {
                return t;
            }
            let bound: Option<String> = run
                .steps
                .iter()
                .find(|s| {
                    if *success {
                        s.status == StepStatus::Running && s.agent_node_id == Some(*agent_node_id)
                    } else {
                        matches!(s.status, StepStatus::Running | StepStatus::Unverified)
                            && s.agent_node_id
                                .or_else(|| run.resolve_target_agent(&s.node_id))
                                == Some(*agent_node_id)
                    }
                })
                .map(|s| s.node_id.clone());
            if let Some(step_node) = bound {
                if let Some(out) = output {
                    run.context.set(&format!("node.{step_node}.output"), out);
                }
                if *success {
                    let allocation_only = matches!(run.graph.node(&step_node).map(|node| &node.kind),
                        Some(CircuitNodeKind::SpawnAgentNode { prompt, .. }) if run.context.resolve(prompt).trim().is_empty());
                    if run.has_human_wait(&step_node) {
                        return t;
                    }
                    if !allocation_only {
                        unverify_step(run, &mut t, &step_node, "Process readiness or exit cannot establish assigned-work completion. Recheck lifecycle and owned-work evidence.".into());
                        return t;
                    }
                    set_step(run, &mut t, &step_node, StepStatus::Completed);
                } else {
                    fail_step(
                        run,
                        &mut t,
                        &step_node,
                        "piloted agent node reported error".to_string(),
                    );
                }
                cascade_after_completion(run, &mut t);
                finish_run_if_done(run, &mut t);
            }
        }
        CircuitEvent::AgentLost { agent_node_id } => {
            if !matches!(run.state, RunState::Running | RunState::Paused) {
                return t;
            }
            let source_lost = run.context.source_agent_id() == Some(*agent_node_id);
            // Match both direct (`step.agent_node_id == Some(*id)`) and
            // lineage-resolved targets — `InjectPty` / `LlmTurnClassifier` /
            // `SetNodeStatus` / `CloseAgentNode` steps carry their target
            // agent via the lineage walk, not on the step row. Without
            // the lineage fallback, an orphaned `InjectPty` step (target
            // agent row deleted mid-run) would never cancel.
            let bound: Option<String> = run
                .steps
                .iter()
                .filter(|s| matches!(s.status, StepStatus::Running | StepStatus::Unverified))
                .find(|s| {
                    if s.agent_node_id == Some(*agent_node_id) {
                        return true;
                    }
                    run.resolve_target_agent(&s.node_id) == Some(*agent_node_id)
                })
                .map(|s| s.node_id.clone());
            // A borrowed source is a run-level dependency. It can disappear
            // while a different piloted step is active, so retain that step's
            // checkpoint as the primary loss record instead of failing the run
            // with no step write.
            let step_to_cancel = bound.or_else(|| {
                if !source_lost {
                    return None;
                }
                run.steps
                    .iter()
                    .find(|step| {
                        matches!(step.status, StepStatus::Running | StepStatus::Unverified)
                    })
                    .or_else(|| run.steps.iter().find(|step| !step.status.is_terminal()))
                    .map(|step| step.node_id.clone())
            });
            if let Some(step_node) = step_to_cancel {
                cancel_step(run, &mut t, &step_node, &format!(
                    "Piloted agent node {agent_node_id} is no longer available (deleted, archived or marked lost)."));
                run.state = RunState::Failed;
                t.run_state_changed = true;
                finish_run_if_done(run, &mut t);
            } else if source_lost {
                // There may be no materialized step yet (or every step may
                // already be terminal), but the run still depends on this
                // borrowed source and must not remain admitted as Running.
                run.state = RunState::Failed;
                t.run_state_changed = true;
                finish_run_if_done(run, &mut t);
            }
        }
        // -- Milestone 2 (#1207): pause/resume + human-in-the-loop gates --
        CircuitEvent::Paused => {
            if run.state == RunState::Running {
                run.state = RunState::Paused;
                t.run_state_changed = true;
            }
        }
        CircuitEvent::Resumed => {
            if run.state == RunState::Paused {
                run.state = RunState::Running;
                t.run_state_changed = true;
                for step in &run.steps {
                    if !step.status.is_terminal() {
                        run.context
                            .set(&format!("{}.attempt", wait_prefix(&step.node_id)), "");
                    }
                }
                t.context_changed = true;
            }
        }
        CircuitEvent::CollaboratorApproved { node_id } => {
            let waiting = matches!(
                run.step(node_id),
                Some(s) if s.status == StepStatus::Blocked
            ) && matches!(
                run.graph.node(node_id).map(|n| &n.kind),
                Some(CircuitNodeKind::CollaboratorCheck {
                    require_approval: true
                })
            );
            if waiting && run.state == RunState::Running {
                set_step(run, &mut t, node_id, StepStatus::Completed);
                for child in run.graph.children(node_id) {
                    if let Some(child_step) = run.step(&child) {
                        if child_step.status.is_terminal() {
                            reset_step_for_retry(run, &mut t, &child, child_step.attempt + 1);
                        }
                    }
                }
                cascade_after_completion(run, &mut t);
                finish_run_if_done(run, &mut t);
            }
        }
        CircuitEvent::ClassifierUnavailable {
            node_id,
            attempt,
            error,
        } => {
            if run.state == RunState::Running
                && run.step(node_id).is_some_and(|step| {
                    step.attempt == *attempt
                        && matches!(step.status, StepStatus::Running | StepStatus::Unverified)
                })
            {
                record_classifier_failure(run, &mut t, node_id, *attempt, Some(error.as_str()));
            }
        }
        CircuitEvent::ClassifierErrorObserved {
            node_id,
            attempt,
            error,
        } => {
            if run.state == RunState::Running
                && run.step(node_id).is_some_and(|step| {
                    step.attempt == *attempt
                        && matches!(step.status, StepStatus::Running | StepStatus::Unverified)
                })
            {
                run.context
                    .set(&format!("node.{node_id}.classifier_error.{attempt}"), error);
                t.context_changed = true;
            }
        }
        CircuitEvent::TurnClassified {
            node_id,
            classification,
            output,
            binding,
        } => {
            let is_waiting_classifier = matches!(
                run.step(node_id),
                Some(s) if matches!(s.status, StepStatus::Running | StepStatus::Unverified)
            ) && matches!(
                run.graph.node(node_id).map(|n| &n.kind),
                Some(
                    CircuitNodeKind::LlmTurnClassifier { .. }
                        | CircuitNodeKind::AwaitAgentTurn { .. }
                        | CircuitNodeKind::ReviewVerdict { .. }
                        | CircuitNodeKind::SpawnAgentNode { .. }
                )
            );
            if is_waiting_classifier && run.state == RunState::Running {
                let Some(attempt) = run.step(node_id).map(|step| step.attempt) else {
                    return t;
                };
                if run
                    .context
                    .get(&format!("node.{node_id}.classifier_failures.{attempt}"))
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0)
                    >= MAX_CLASSIFIER_FAILURES
                {
                    return t;
                }
                if *classification == Some(Classification::Continue)
                    && run
                        .context
                        .get(&format!("node.{node_id}.evaluated_attempt"))
                        == Some(attempt.to_string().as_str())
                    && output.as_deref().is_some_and(|out| {
                        run.context.get(&format!("node.{node_id}.evaluated_output")) == Some(out)
                    })
                    && binding.as_ref().is_some_and(|binding| {
                        run.context
                            .get(&format!("node.{node_id}.classified_evidence_owner"))
                            == serde_json::to_string(&binding.owner).ok().as_deref()
                    })
                {
                    return t;
                }
                run.context.set(
                    &format!("node.{node_id}.evaluated_attempt"),
                    attempt.to_string(),
                );
                run.context.set(
                    &format!("node.{node_id}.classification"),
                    match classification {
                        Some(Classification::Completed) => "completed",
                        Some(Classification::Blocked) => "blocked",
                        Some(Classification::Working) => "working",
                        Some(Classification::Continue) => "continue",
                        None => "unavailable",
                    },
                );
                t.context_changed = true;
                let evidence = run.classifier_evidence(node_id);
                let lifecycle_verified =
                    binding
                        .as_ref()
                        .zip(evidence.as_ref())
                        .is_some_and(|(binding, evidence)| {
                            evidence.identity.as_ref() == Some(&binding.owner)
                                && evidence.report.as_ref().is_some_and(|report| {
                                    report.revision == binding.report_revision
                                        && output.as_deref() == Some(report.text.as_str())
                                        && report.input_stamp.as_deref()
                                            == Some(binding.input_guard.input_stamp.as_str())
                                        && report.observed_at_ms
                                            == binding.input_guard.observed_at_ms
                                })
                                && binding.owner.agent_node_id == binding.input_guard.agent_node_id
                                && binding.owner.session_id.as_deref()
                                    == Some(binding.input_guard.session_id.as_str())
                                && binding.owner.session_incarnation.as_deref()
                                    == Some(binding.input_guard.session_incarnation.as_str())
                        });
                let valid = !run.report_has_known_blockers(node_id)
                    && (lifecycle_verified
                        || binding.as_ref().is_some_and(|binding| {
                            run.accepts_report_binding(node_id, binding, output.as_deref())
                        }));
                if valid {
                    if let Some(binding) = binding {
                        run.context.set(
                            &format!("node.{node_id}.evaluated_report_revision"),
                            &binding.report_revision,
                        );
                    }
                    run.context
                        .set(&format!("node.{node_id}.observation_blocker"), "");
                } else if classification.is_some() && binding.is_some() {
                    // Issue #2138: a classification that could not be bound still
                    // observed a specific report, evidence owner and blocking
                    // state. Without recording that identity the gate parks
                    // Unverified with nothing stamped, which
                    // `should_classify_report` reads as "never judged" — so it
                    // re-classified the same report on every observation tick,
                    // appending an identical classification, `step_transition`
                    // and `checkpoint_reason` each time. Record what was observed
                    // (not accepted) so an unchanged report is not re-judged.
                    //
                    // The blocking state is part of the identity, not a detail.
                    // Acceptance reads live state — open owned work, lifecycle
                    // blockers, evidence conflicts — none of which move the report
                    // revision or the evidence owner. Keying suppression on those
                    // two alone would hide a cleared blocker indefinitely and leave
                    // the gate parked until a new report or a manual Recheck.
                    let binding = binding.as_ref().expect("classified binding");
                    run.context.set(
                        &format!("node.{node_id}.evaluated_report_revision"),
                        &binding.report_revision,
                    );
                    run.context.set(
                        &format!("node.{node_id}.evaluated_evidence_owner"),
                        serde_json::to_string(&binding.owner).expect("serializable identity"),
                    );
                    run.context.set(
                        &format!("node.{node_id}.evaluated_evidence_blocker"),
                        run.report_blocker(node_id)
                            .and_then(|blocker| serde_json::to_string(&blocker).ok())
                            .unwrap_or_default(),
                    );
                }
                if let Some(out) = output {
                    run.context
                        .set(&format!("node.{node_id}.evaluated_output"), out.clone());
                    t.context_changed = true;
                    if valid {
                        if matches!(
                            run.graph.node(node_id).map(|node| &node.kind),
                            Some(CircuitNodeKind::SpawnAgentNode { .. })
                        ) {
                            run.context
                                .set(&format!("node.{node_id}.output"), out.clone());
                        }
                        if run.context.source_agent_id() == run.resolve_target_agent(node_id) {
                            run.context.set("source.output", out.clone());
                        }
                        if let Some(spawn_node_id) = spawn_node_for_agent(run, node_id) {
                            run.context
                                .set(&format!("node.{spawn_node_id}.output"), out.clone());
                        }
                    }
                }
                t.classifications
                    .push(super::observation::RecordedClassification {
                        step_id: node_id.clone(),
                        attempt,
                        interpretation: (*classification).into(),
                        report_revision: binding
                            .as_ref()
                            .map(|binding| binding.report_revision.clone()),
                        evidence_owner: binding.as_ref().map(|binding| binding.owner.clone()),
                        lifecycle_verified,
                        report_text: output.clone(),
                        report_completeness: if lifecycle_verified {
                            super::observation::ReportCompleteness::Complete
                        } else if output.as_ref().is_some_and(|text| !text.is_empty()) {
                            super::observation::ReportCompleteness::Partial
                        } else {
                            super::observation::ReportCompleteness::Unavailable
                        },
                    });
                if let Some(classification) = classification {
                    if run.has_human_wait(node_id) {
                        return t;
                    }
                    if !valid {
                        unverify_step(run, &mut t, node_id, "No current report could be bound to this decision, or known work remains unresolved. Waiting for fresh evidence; inspection is available in the agent terminal.".into());
                        return t;
                    }
                    let binding = binding.as_ref().expect("validated classification binding");
                    t.input_guard = Some(binding.input_guard.clone());
                    run.context.set(
                        &format!("node.{node_id}.classified_report_revision"),
                        &binding.report_revision,
                    );
                    run.context.set(
                        &format!("node.{node_id}.classified_evidence_owner"),
                        serde_json::to_string(&binding.owner).expect("serializable identity"),
                    );
                    // A report handoff acknowledges a finished turn without
                    // inventing native lifecycle or complete ownership proof.
                    // Spawn steps hand their report to the downstream gate.
                    if matches!(
                        run.graph.node(node_id).map(|node| &node.kind),
                        Some(CircuitNodeKind::SpawnAgentNode { .. })
                    ) {
                        if *classification == Classification::Completed {
                            complete_with_outcome(run, &mut t, node_id, StepOutcome::Completed);
                            cascade_after_completion(run, &mut t);
                            finish_run_if_done(run, &mut t);
                        }
                        return t;
                    }
                    if run
                        .step(node_id)
                        .is_some_and(|step| step.status == StepStatus::Unverified)
                    {
                        set_step(run, &mut t, node_id, StepStatus::Running);
                    }
                    if matches!(
                        run.graph.node(node_id).map(|n| &n.kind),
                        Some(CircuitNodeKind::ReviewVerdict { .. })
                    ) {
                        run.context.set(
                            &format!("node.{node_id}.review_verdict_attempt"),
                            attempt.to_string(),
                        );
                        run.context.set(
                            &format!("node.{node_id}.review_verdict"),
                            match classification {
                                Classification::Completed => "approved",
                                Classification::Working | Classification::Continue => {
                                    "changes_requested"
                                }
                                Classification::Blocked => "blocked",
                            },
                        );
                    }
                    run.context.set(
                        &format!("node.{node_id}.classifier_failures.{attempt}"),
                        "0",
                    );
                    run.context
                        .set(&format!("node.{node_id}.classifier_error.{attempt}"), "");
                    let outcome = match classification {
                        Classification::Completed => StepOutcome::Completed,
                        Classification::Blocked => StepOutcome::Blocked,
                        Classification::Working | Classification::Continue => StepOutcome::Working,
                    };
                    // A classifier outcome only terminalizes the gate when the
                    // blueprint wires that outcome somewhere. An unwired
                    // BLOCKED/WORKING result is deliberately parked so a human
                    // reply or a later agent turn can be classified again; this
                    // is the issue-driven Autopilot contract and avoids silently
                    // completing a run on a transient yield.
                    if classifier_outcome_is_routed(run, node_id, outcome) {
                        complete_with_outcome(run, &mut t, node_id, outcome);
                        cascade_after_completion(run, &mut t);
                        finish_run_if_done(run, &mut t);
                    } else {
                        let count_key = format!("node.{node_id}.continuations.{attempt}");
                        let count = run
                            .context
                            .get(&count_key)
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(0);
                        let owns_target = run.resolve_target_agent(node_id).is_some_and(|id| {
                            Some(id) != run.context.source_agent_id()
                                && run.steps.iter().any(|s| s.agent_node_id == Some(id))
                        });
                        if *classification == Classification::Continue
                            && owns_target
                            && count < 2
                            && run.context.get(&format!("node.{node_id}.recheck_only")) != Some("1")
                            && run
                                .context
                                .get(&format!("node.{node_id}.continuation.stamp"))
                                .is_some()
                            && matches!(
                                run.graph.node(node_id).map(|n| &n.kind),
                                Some(CircuitNodeKind::LlmTurnClassifier { .. })
                            )
                        {
                            if let Some(target_agent_id) = run.resolve_target_agent(node_id) {
                                run.context.set(&count_key, (count + 1).to_string());
                                run.context.set(
                                    &format!("node.{node_id}.continuation.delivery"),
                                    "claimed",
                                );
                                run.context.set(
                                    &format!("node.{node_id}.continuation.attempt"),
                                    attempt.to_string(),
                                );
                                t.context_changed = true;
                                t.effects
                                    .push(continuation_effect(node_id, target_agent_id));
                            } else {
                                run.context.set(
                                    &format!("node.{node_id}.continuation.delivery"),
                                    "obsolete",
                                );
                                t.context_changed = true;
                            }
                        }
                        if *classification == Classification::Continue && owns_target && count >= 2
                        {
                            unverify_step(run, &mut t, node_id, "Automatic continuation budget exhausted after 2 attempts. Inspect the latest report and recheck evidence.".into());
                            return t;
                        }
                        let error = match classification {
                            Classification::Blocked => "Agent needs input. Continue the agent to produce a new report.",
                            Classification::Working => "Agent reported work remaining. Waiting for its next report; check the agent for a question or permission prompt.",
                            Classification::Completed => "Agent reported completion, but no completed route is wired; waiting for a new report.",
                            Classification::Continue if owns_target && count < 2 => "Sent a bounded continuation for the remaining assigned work; waiting for a new report.",
                            Classification::Continue => "Automatic continuation budget exhausted or source is borrowed; waiting for new progress before timeout.",
                        }.to_string();
                        if let Some(step) = run.step_mut(node_id) {
                            step.error = Some(error.clone());
                        }
                        t.step_writes.push(StepWrite {
                            node_id: node_id.clone(),
                            status: StepStatus::Running,
                            outcome: None,
                            error: Some(Some(error)),
                            agent_node_id: None,
                            attempt,
                            fresh_attempt: false,
                        });
                    }
                } else {
                    record_classifier_failure(run, &mut t, node_id, attempt, None);
                }
            }
        }
        CircuitEvent::TurnParked {
            node_id,
            output,
            report_revision,
        } => {
            // A deliberate wait is not a verdict, an outage, or a failure. The
            // report is recorded as observed so `should_classify_report` stops
            // re-asking about an unchanged one, and nothing else is written by
            // this event: no `classification`, no classifier-outage error, no
            // failure budget, and no SpawnAgentNode output stamp, so a progress
            // line never becomes this gate's recorded evidence. (The gate's
            // generic yielded-wait reason and deadline come from the separate
            // `WaitObserved`.) The gate is judged when the report changes.
            let is_waiting_classifier = matches!(
                run.step(node_id),
                Some(s) if matches!(s.status, StepStatus::Running | StepStatus::Unverified)
            ) && matches!(
                run.graph.node(node_id).map(|n| &n.kind),
                Some(
                    CircuitNodeKind::ReviewVerdict { .. }
                        | CircuitNodeKind::LlmTurnClassifier { .. }
                        | CircuitNodeKind::AwaitAgentTurn { .. }
                        | CircuitNodeKind::SpawnAgentNode { .. }
                )
            );
            if is_waiting_classifier && run.state == RunState::Running {
                if let Some(attempt) = run.step(node_id).map(|step| step.attempt) {
                    run.context.set(
                        &format!("node.{node_id}.evaluated_attempt"),
                        attempt.to_string(),
                    );
                    run.context
                        .set(&format!("node.{node_id}.evaluated_output"), output.clone());
                    if let Some(revision) = report_revision {
                        run.context.set(
                            &format!("node.{node_id}.evaluated_report_revision"),
                            revision,
                        );
                    }
                    t.context_changed = true;
                }
            }
        }
        CircuitEvent::VerificationResult { node_id, green } => {
            let is_waiting_verification = matches!(
                run.step(node_id),
                Some(s) if s.status == StepStatus::Running
            ) && matches!(
                run.graph.node(node_id).map(|n| &n.kind),
                Some(CircuitNodeKind::DeterministicVerification { .. })
            );
            if is_waiting_verification && run.state == RunState::Running {
                let outcome = if *green {
                    StepOutcome::Green
                } else {
                    StepOutcome::Red
                };
                if let Some(CircuitNodeKind::DeterministicVerification { command }) =
                    run.graph.node(node_id).map(|node| &node.kind)
                {
                    run.context
                        .set("verification.command", run.context.resolve(command));
                }
                run.context.set("verification.outcome", outcome.as_db_str());
                complete_with_outcome(run, &mut t, node_id, outcome);
                cascade_after_completion(run, &mut t);
                finish_run_if_done(run, &mut t);
            }
        }
        CircuitEvent::GithubActionResult {
            node_id,
            success,
            pr_number,
            pr_url,
            pr_head_ref,
            pr_title,
            error,
        } => {
            let is_waiting_action = matches!(
                run.step(node_id),
                Some(s) if s.status == StepStatus::Running
            ) && matches!(
                run.graph.node(node_id).map(|n| &n.kind),
                Some(CircuitNodeKind::GithubAction { .. })
            );
            if is_waiting_action && run.state == RunState::Running {
                if *success {
                    if let Some(num) = pr_number {
                        run.context.set("pr.number", num.to_string());
                    }
                    if let Some(url) = pr_url {
                        run.context.set("pr.url", url);
                    }
                    if let Some(head) = pr_head_ref {
                        run.context.set("pr.head_ref", head);
                    }
                    if let Some(title) = pr_title {
                        run.context.set("pr.title", title);
                    }
                    set_step(run, &mut t, node_id, StepStatus::Completed);
                    cascade_after_completion(run, &mut t);
                    finish_run_if_done(run, &mut t);
                } else if matches!(
                    run.graph.node(node_id).map(|node| &node.kind),
                    Some(CircuitNodeKind::GithubAction {
                        action: GithubActionKind::ConfirmPrMerged,
                        ..
                    })
                ) {
                    // "Not merged" is an answer, not a malfunction: route it so
                    // the blueprint can leave the work open and say why. The run
                    // is not failed, and nothing is attested on its behalf.
                    run.context.set(
                        "merge.unconfirmed_reason",
                        error.clone().unwrap_or_else(|| {
                            "GitHub did not report the pull request as merged".to_string()
                        }),
                    );
                    t.context_changed = true;
                    complete_with_outcome(run, &mut t, node_id, StepOutcome::Failed);
                    cascade_after_completion(run, &mut t);
                    finish_run_if_done(run, &mut t);
                } else {
                    let failure = error
                        .clone()
                        .unwrap_or_else(|| "GitHub action failed".to_string());
                    // An OpenPr action with an explicit existing-PR policy
                    // may feed a correction node. Other GitHub actions have
                    // no agent correction contract, so do not leak a stale
                    // wrap-up prompt into their run context.
                    let needs_agent_correction = matches!(
                        run.graph.node(node_id).map(|node| &node.kind),
                        Some(CircuitNodeKind::GithubAction {
                            action: GithubActionKind::OpenPr,
                            open_pr_policy: Some(policy),
                            ..
                        }) if policy.requires_existing()
                    );
                    if needs_agent_correction {
                        run.context.set(
                            "autopilot.wrapup_correction",
                            crate::circuit::verification::correction_prompt(
                                std::slice::from_ref(&failure),
                                "",
                            ),
                        );
                    }
                    fail_step(run, &mut t, node_id, failure);
                    finish_run_if_done(run, &mut t);
                }
            }
        }
        CircuitEvent::GithubActionRetry { node_id } => {
            if run.state == RunState::Running
                && matches!(
                    run.step(node_id),
                    Some(s) if s.status == StepStatus::Running
                )
                && matches!(
                    run.graph.node(node_id).map(|node| &node.kind),
                    Some(CircuitNodeKind::GithubAction { .. })
                )
            {
                unverify_step(run, &mut t, node_id,
                    "The external action may already have been applied. Inspect its result before recording an outcome or deliberately retrying.".into());
            }
        }
        CircuitEvent::GithubRecheckDue {
            node_id,
            attempt,
            now_ms,
        } => {
            if run.state == RunState::Running
                && run.step(node_id).is_some_and(|step| {
                    step.status == StepStatus::Unverified && step.attempt == *attempt
                })
                && matches!(
                    run.graph.node(node_id).map(|node| &node.kind),
                    Some(CircuitNodeKind::GithubAction {
                        action: GithubActionKind::OpenPr,
                        ..
                    })
                )
            {
                let prefix = format!("node.{node_id}.reconcile.{attempt}");
                let count = run
                    .context
                    .get(&format!("{prefix}.count"))
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(0);
                let next = run
                    .context
                    .get(&format!("{prefix}.next_ms"))
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0);
                if count < 5 && *now_ms >= next {
                    run.context
                        .set(&format!("{prefix}.count"), (count + 1).to_string());
                    run.context.set(
                        &format!("{prefix}.next_ms"),
                        now_ms.saturating_add(60_000).to_string(),
                    );
                    run.context
                        .set(&format!("node.{node_id}.recheck_only"), "1");
                    set_step(run, &mut t, node_id, StepStatus::Running);
                    if let Some(step) = run.step_mut(node_id) {
                        step.error = None;
                    }
                    if let Some(write) = t.step_writes.last_mut() {
                        write.error = Some(None);
                    }
                    t.context_changed = true;
                    t.effects.push(Effect::CallGithub {
                        node_id: node_id.clone(),
                        action: GithubActionKind::OpenPr,
                        label: None,
                        comment: None,
                    });
                }
            }
        }
        CircuitEvent::CloseAgentRetry { node_id } => {
            if matches!(
                run.step(node_id),
                Some(s) if s.status == StepStatus::Completed
            ) {
                if let Some(CircuitNodeKind::CloseAgentNode { target_node_id }) =
                    run.graph.node(node_id).map(|node| &node.kind)
                {
                    if run.resolve_target_agent(node_id).is_some() {
                        t.effects.push(Effect::CloseAgentNode {
                            node_id: node_id.clone(),
                            target_node_id: target_node_id.clone(),
                        });
                    }
                }
            }
        }
    }
    t
}

fn record_classifier_failure(
    run: &mut RunView,
    t: &mut Transition,
    node_id: &str,
    attempt: i32,
    error: Option<&str>,
) {
    let key = format!("node.{node_id}.classifier_failures.{attempt}");
    let previous = run
        .context
        .get(&key)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if previous >= MAX_CLASSIFIER_FAILURES {
        return;
    }
    if let Some(error) = error {
        run.context
            .set(&format!("node.{node_id}.classifier_error.{attempt}"), error);
    }
    let failures = previous + 1;
    run.context.set(&key, failures.to_string());
    t.context_changed = true;
    let diagnostic = run
        .context
        .get(&format!("node.{node_id}.classifier_error.{attempt}"))
        .filter(|error| !error.is_empty())
        .map(|error| format!(" Last failure: {error}"))
        .unwrap_or_default();
    if failures >= MAX_CLASSIFIER_FAILURES {
        unverify_step(run, t, node_id, format!("Classifier unavailable after {MAX_CLASSIFIER_FAILURES} attempts. Restore the configured classifier and recheck evidence.{diagnostic}"));
    } else {
        let error = format!("Classifier unavailable; retrying after 60 seconds. Check the Circuit classifier provider in app settings if this persists.{diagnostic}");
        if let Some(step) = run.step_mut(node_id) {
            step.error = Some(error.clone());
        }
        let mut write = StepWrite::for_existing(node_id, StepStatus::Running, attempt);
        write.error = Some(Some(error));
        t.step_writes.push(write);
    }
}

/// Preserve the attempt and owned work while evidence is unresolved.
///
/// Idempotent: re-parking a step that is already Unverified for the same reason
/// writes nothing. Every emitted `StepWrite` becomes a `step_transition` row and,
/// for `unverified`, a `checkpoint_reason` row (`db::circuit::ledger`), so an
/// unchanged observation must not append identical ledger rows on every tick.
fn unverify_step(run: &mut RunView, t: &mut Transition, node_id: &str, reason: String) {
    let Some(step) = run.step_mut(node_id) else {
        return;
    };
    if step.status.is_terminal() {
        return;
    }
    if step.status == StepStatus::Unverified
        && step.outcome.is_none()
        && step.error.as_deref() == Some(reason.as_str())
    {
        return;
    }
    step.status = StepStatus::Unverified;
    step.outcome = None;
    step.error = Some(reason.clone());
    let mut write = StepWrite::for_existing(node_id, StepStatus::Unverified, step.attempt);
    write.outcome = Some(None);
    write.error = Some(Some(reason));
    t.step_writes.push(write);
}

fn set_step(run: &mut RunView, t: &mut Transition, node_id: &str, status: StepStatus) {
    let incoming_attempt = run
        .graph
        .incoming(node_id)
        .iter()
        .filter_map(|e| run.step(&e.from).map(|ps| ps.attempt))
        .max()
        .unwrap_or(1);
    let mut fresh_attempt = false;
    let changed = match run.step_mut(node_id) {
        Some(step) => {
            if incoming_attempt > step.attempt {
                fresh_attempt = true;
                step.attempt = incoming_attempt;
                step.outcome = None;
                step.error = None;
            }
            if step.status != status || fresh_attempt {
                step.status = status;
                step.outcome = status.outcome();
                true
            } else {
                fresh_attempt
            }
        }
        None => {
            let mut step = StepView::new(node_id, status);
            step.attempt = incoming_attempt;
            step.outcome = status.outcome();
            run.steps.push(step);
            true
        }
    };
    if status.is_terminal() {
        let outcome_str = status
            .outcome()
            .map(|o| o.as_db_str())
            .unwrap_or(status.as_db_str());
        run.context
            .set(&format!("node.{node_id}.status"), outcome_str);
    }
    if changed {
        // Preserve the existing attempt count on re-transitions (a retry's
        // second run must not write attempt=1 over its reset value).
        let attempt = run.step(node_id).map(|s| s.attempt).unwrap_or(1);
        let mut write = StepWrite::for_existing(node_id, status, attempt);
        write.outcome = Some(status.outcome());
        write.fresh_attempt = fresh_attempt;
        t.step_writes.push(write);
    }
}

/// Complete a step with a specific gate outcome (Completed status but a
/// Blocked/Working/Green/Red routing outcome — #1207).
fn complete_with_outcome(
    run: &mut RunView,
    t: &mut Transition,
    node_id: &str,
    outcome: StepOutcome,
) {
    let attempt = match run.step_mut(node_id) {
        Some(step) => {
            step.status = StepStatus::Completed;
            step.outcome = Some(outcome);
            step.error = None;
            step.attempt
        }
        None => 1,
    };
    run.context
        .set(&format!("node.{node_id}.status"), outcome.as_db_str());
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Completed,
        outcome: Some(Some(outcome)),
        error: Some(None),
        agent_node_id: None,
        attempt,
        fresh_attempt: false,
    });
}

/// Whether a classifier result has an explicit downstream route. `Always`
/// remains a valid authoring shorthand for “any classification”; otherwise
/// only the matching `OnOutcome` edge consumes the result. Missing routes are
/// the parked state used by issue-style implementation/review classifiers.
fn classifier_outcome_is_routed(run: &RunView, node_id: &str, outcome: StepOutcome) -> bool {
    run.graph.edges.iter().any(|edge| {
        edge.from == node_id
            && match edge.condition {
                EdgeCondition::Always => true,
                EdgeCondition::OnOutcome(expected) => expected == outcome,
            }
    })
}

fn fail_step(run: &mut RunView, t: &mut Transition, node_id: &str, error: String) {
    if let Some(step) = run.step_mut(node_id) {
        if step.status.is_terminal() {
            return;
        }
        step.status = StepStatus::Failed;
        step.outcome = Some(StepOutcome::Failed);
        step.error = Some(error.clone());
    } else {
        let mut step = StepView::new(node_id, StepStatus::Failed);
        step.outcome = Some(StepOutcome::Failed);
        step.error = Some(error.clone());
        run.steps.push(step);
    }
    run.context.set(
        &format!("node.{node_id}.status"),
        StepOutcome::Failed.as_db_str(),
    );
    let attempt = run.step(node_id).map(|s| s.attempt).unwrap_or(1);
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Failed,
        outcome: Some(Some(StepOutcome::Failed)),
        error: Some(Some(error)),
        agent_node_id: None,
        attempt,
        fresh_attempt: false,
    });
    // Fail-fast — UNLESS the author wired this failure to a RetryLimit
    // gate (#1207). Then the gate owns the failure: the run stays
    // Running so the gate can decide retry vs exhaustion.
    if !has_retry_path(run, node_id) {
        run.state = RunState::Failed;
        t.run_state_changed = true;
        return;
    }
    // Re-arm any already-fired retry gate downstream: a gate that
    // completed an earlier round must run again for this new failure.
    // (A gate without a step yet is picked up by ordinary scheduling.)
    let gates: Vec<String> = run
        .graph
        .edges
        .iter()
        .filter(|e| {
            e.from == node_id
                && matches!(
                    e.condition,
                    EdgeCondition::Always | EdgeCondition::OnOutcome(StepOutcome::Failed)
                )
                && matches!(
                    run.graph.node(&e.to).map(|n| &n.kind),
                    Some(CircuitNodeKind::RetryLimit { .. })
                )
        })
        .map(|e| e.to.clone())
        .collect();
    for gate in gates {
        let rearm = match run.step_mut(&gate) {
            Some(step) if step.status.is_terminal() => {
                step.status = StepStatus::Queued;
                step.outcome = None;
                step.error = None;
                Some(step.attempt)
            }
            _ => None,
        };
        if let Some(attempt) = rearm {
            t.step_writes.push(StepWrite {
                node_id: gate,
                status: StepStatus::Queued,
                outcome: Some(None),
                error: Some(None),
                agent_node_id: None,
                attempt,
                fresh_attempt: true,
            });
        }
    }
}

/// Does `node_id` wire its failure into a downstream RetryLimit gate?
fn has_retry_path(run: &RunView, node_id: &str) -> bool {
    run.graph.edges.iter().any(|e| {
        e.from == node_id
            && matches!(
                e.condition,
                EdgeCondition::Always | EdgeCondition::OnOutcome(StepOutcome::Failed)
            )
            && matches!(
                run.graph.node(&e.to).map(|n| &n.kind),
                Some(CircuitNodeKind::RetryLimit { .. })
            )
    })
}

fn cancel_step(run: &mut RunView, t: &mut Transition, node_id: &str, reason: &str) {
    let reason = run.step(node_id).map_or_else(
        || reason.to_owned(),
        |step| step.cancellation_reason(reason),
    );
    match run.step_mut(node_id) {
        Some(step) => {
            if step.status.is_terminal() {
                return;
            }
            step.status = StepStatus::Cancelled;
            step.outcome = Some(StepOutcome::Cancelled);
            step.error = Some(reason.clone());
        }
        // Symmetrical with fail_step: an absent step still gets a
        // Cancelled StepView so run.steps and t.step_writes agree.
        None => {
            let mut step = StepView::new(node_id, StepStatus::Cancelled);
            step.outcome = Some(StepOutcome::Cancelled);
            step.error = Some(reason.clone());
            run.steps.push(step);
        }
    }
    run.context.set(
        &format!("node.{node_id}.status"),
        StepOutcome::Cancelled.as_db_str(),
    );
    let attempt = run.step(node_id).map(|s| s.attempt).unwrap_or(1);
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Cancelled,
        outcome: Some(Some(StepOutcome::Cancelled)),
        error: Some(Some(reason)),
        agent_node_id: None,
        attempt,
        fresh_attempt: false,
    });
}

/// Is `node_id` eligible to schedule? All incoming edges satisfied by
/// terminal parent steps with matching conditions — except
/// `AnyCompleted`, which satisfies on any completed parent, and except
/// edges FROM a RetryLimit gate (#1207) or unrun CollaboratorCheck in a cycle.
fn is_eligible(run: &RunView, node_id: &str) -> bool {
    let incoming: Vec<&super::model::CircuitEdge> = run
        .graph
        .incoming(node_id)
        .into_iter()
        .filter(|e| {
            if matches!(
                run.graph.node(&e.from).map(|n| &n.kind),
                Some(CircuitNodeKind::RetryLimit { .. })
            ) {
                // A RetryLimit edge is a gated loop-back. Before the gate
                // has executed, hide that edge so its target cannot bypass
                // the retry decision; after the gate completes, include it
                // so a target with no second incoming edge can be scheduled.
                return matches!(run.step(&e.from), Some(s) if s.status.is_terminal());
            }
            if matches!(
                run.graph.node(&e.from).map(|n| &n.kind),
                Some(CircuitNodeKind::CollaboratorCheck {
                    require_approval: true
                })
            ) && run.step(&e.from).is_none()
            {
                return false;
            }
            true
        })
        .collect();
    let incoming = incoming.as_slice();
    if incoming.is_empty() {
        return false; // roots are handled by the trigger path
    }
    let any_join = matches!(
        run.graph.node(node_id).map(|n| &n.kind),
        Some(CircuitNodeKind::AnyCompleted)
    );
    let edge_satisfied = |edge: &super::model::CircuitEdge| -> bool {
        run.step(&edge.from)
            .map(|parent| {
                parent.status.is_terminal()
                    && (parent.outcome != Some(StepOutcome::Completed)
                        || !matches!(
                            run.graph.node(&parent.node_id).map(|n| &n.kind),
                            Some(CircuitNodeKind::ReviewVerdict { .. })
                        )
                        || has_current_review_approval(run, parent))
                    && match edge.condition {
                        EdgeCondition::Always => true,
                        EdgeCondition::OnOutcome(o) => parent.outcome == Some(o),
                    }
            })
            .unwrap_or(false)
    };
    if any_join {
        incoming.iter().any(|e| e_satisfied_completed(e, run))
    } else {
        incoming.iter().all(|e| edge_satisfied(e))
    }
}

/// AnyCompleted's satisfaction predicate: the edge's parent produced
/// exactly the outcome the edge asks for (`Always` + Completed counts).
fn e_satisfied_completed(edge: &super::model::CircuitEdge, run: &RunView) -> bool {
    run.step(&edge.from)
        .map(|parent| {
            parent.outcome == Some(StepOutcome::Completed)
                && match edge.condition {
                    EdgeCondition::Always => true,
                    EdgeCondition::OnOutcome(o) => o == StepOutcome::Completed,
                }
        })
        .unwrap_or(false)
}

/// Schedule every eligible node that has no step yet, respecting
/// capacity. Runs to a FIXPOINT: instant-completing steps (Notify,
/// joins, SetNodeStatus) hand back their agent slot (if any) immediately, so
/// a chain of plain actions executes entirely within this one tick — no
/// 2s-per-node penalty. Queued (FIFO) steps promote first; agent spawns
/// occupy their slot until the piloted node finishes. Only agent spawns can
/// park: every other eligible step starts at once.
fn schedule_ready(run: &mut RunView, t: &mut Transition, capacity: Capacity) {
    let mut agent_free = capacity.agent_free_slots;

    loop {
        // Fail-fast: stop scheduling the moment anything failed.
        if run.state != RunState::Running {
            return;
        }
        let mut progressed = false;

        // FIFO promotion of queued steps first (the view preserves
        // ledger insertion order).
        let queued: Vec<String> = run
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Queued)
            .map(|s| s.node_id.clone())
            .collect();
        for node_id in queued {
            let kind = match run.graph.node(&node_id) {
                Some(n) => n.kind.clone(),
                None => continue,
            };
            let started = try_start(run, t, &node_id, &kind, &mut agent_free);
            progressed |= started;
            if started && step_completed_instantly(run, &node_id) {
                // Freed its slot again — but its successors are picked
                // up on the next fixpoint pass.
                if consumes_agent_slot(&kind) {
                    agent_free += 1;
                }
            }
        }

        for (node_id, kind) in collect_eligible(run) {
            if run.state != RunState::Running {
                break;
            }
            let needs_agent_slot = consumes_agent_slot(&kind);
            let agent_fits = !needs_agent_slot || agent_free > 0;
            if !agent_fits {
                set_step(run, t, &node_id, StepStatus::Queued);
                record_step_capacity_wait(run, t, &node_id, true);
                continue;
            }
            start_step(run, t, &node_id, &kind);
            progressed = true;
            if step_completed_instantly(run, &node_id) {
                // Instant completion: the slot is free again within this
                // same pass, so don't charge it.
            } else if needs_agent_slot {
                agent_free -= 1;
            }
        }

        if !progressed {
            return;
        }
    }
}

/// Did this step reach a terminal status during the start call? Instant
/// actions (Notify, SetNodeStatus, joins, trigger roots) complete inside
/// [`start_step`]; spawns/injects stay Running.
fn step_completed_instantly(run: &RunView, node_id: &str) -> bool {
    run.step(node_id)
        .map(|s| s.status.is_terminal())
        .unwrap_or(false)
}

/// Nodes with no step yet whose incoming edges are all satisfied, in
/// blueprint order. In a loop, terminal steps whose upstream parents
/// advanced to a higher attempt are also eligible.
fn collect_eligible(run: &RunView) -> Vec<(String, CircuitNodeKind)> {
    run.graph
        .nodes
        .iter()
        .filter(|n| {
            let eligible = is_eligible(run, &n.id);
            if !eligible {
                return false;
            }
            match run.step(&n.id) {
                None => true,
                Some(s) if s.status.is_terminal() => {
                    // Stale from an earlier loop iteration: eligible if an upstream parent advanced
                    run.graph.incoming(&n.id).iter().any(|e| {
                        run.step(&e.from)
                            .map(|ps| ps.attempt > s.attempt && ps.status.is_terminal())
                            .unwrap_or(false)
                    })
                }
                _ => false,
            }
        })
        .map(|n| (n.id.clone(), n.kind.clone()))
        .collect()
}

/// Attempt FIFO promotion of one queued step. Returns true when the step
/// left the queue.
fn try_start(
    run: &mut RunView,
    t: &mut Transition,
    node_id: &str,
    kind: &CircuitNodeKind,
    agent_free: &mut i64,
) -> bool {
    let needs_agent_slot = consumes_agent_slot(kind);
    let agent_fits = !needs_agent_slot || *agent_free > 0;
    if !agent_fits {
        record_step_capacity_wait(run, t, node_id, true);
        return false;
    }
    record_step_capacity_wait(run, t, node_id, false);
    start_step(run, t, node_id, kind);
    if needs_agent_slot {
        *agent_free -= 1;
    }
    true
}

fn record_step_capacity_wait(
    run: &mut RunView,
    t: &mut Transition,
    node_id: &str,
    agent_limit: bool,
) {
    let key = format!("node.{node_id}.capacity_wait");
    let value = serde_json::json!({ "agent_limit": agent_limit }).to_string();
    if run.context.get(&key) != Some(value.as_str()) {
        run.context.set(&key, value);
        t.context_changed = true;
    }
}

/// Begin executing one node: emit its effects; instant-completion kinds
/// finish in the same call. Assumes the step is already marked Running.
fn start_step(run: &mut RunView, t: &mut Transition, node_id: &str, kind: &CircuitNodeKind) {
    if !is_executable(kind) {
        fail_step(
            run,
            t,
            node_id,
            format!(
                "circuit node kind {:?} is not executed until a later milestone",
                kind
            ),
        );
        return;
    }
    set_step(run, t, node_id, StepStatus::Running);
    start_effects_and_completion(run, t, node_id, kind);
}

/// Shared tail of [`start_step`]/[`try_start`]: effect emission +
/// instant-completion handling for a step already in Running.
fn start_effects_and_completion(
    run: &mut RunView,
    t: &mut Transition,
    node_id: &str,
    kind: &CircuitNodeKind,
) {
    match kind {
        // Instant actions complete immediately; their effect carries the
        // resolved template text.
        CircuitNodeKind::Notify { message } => {
            t.effects.push(Effect::Notify {
                message: run.context.resolve(message),
            });
            set_step(run, t, node_id, StepStatus::Completed);
        }
        CircuitNodeKind::SetNodeStatus { status, target_node_id } => {
            let target_agent = run.resolve_target_agent(node_id);
            if target_agent.is_none() {
                fail_step(
                    run,
                    t,
                    node_id,
                    "no target agent node found in upstream lineage for SetNodeStatus".to_string(),
                );
            } else {
                let db_status = match status {
                    SessionStatusKind::Running => "running",
                    SessionStatusKind::Idle => "idle",
                    SessionStatusKind::Completed => "completed",
                };
                t.effects.push(Effect::SetNodeStatus {
                    node_id: node_id.to_string(),
                    status: db_status.to_string(),
                    target_node_id: target_node_id.clone(),
                });
                set_step(run, t, node_id, StepStatus::Completed);
            }
        }
        CircuitNodeKind::CloseAgentNode { target_node_id } => {
            let target_agent = run.resolve_target_agent(node_id);
            if target_agent.is_none() {
                fail_step(
                    run,
                    t,
                    node_id,
                    "no target agent node found in upstream lineage for CloseAgentNode".to_string(),
                );
            } else {
                t.effects.push(Effect::CloseAgentNode {
                    node_id: node_id.to_string(),
                    target_node_id: target_node_id.clone(),
                });
                set_step(run, t, node_id, StepStatus::Completed);
            }
        }
        CircuitNodeKind::InjectPty { target_node_id, .. }
            // Wait for AgentReady — the spawn's async stage-2 must land
            // and the agent process be live before we write bytes.
            // But without any earlier spawn in this run, inject has no
            // target at all — fail fast.
            if run.resolve_target_agent(node_id).is_none() =>
        {
            fail_step(
                run,
                t,
                node_id,
                "no agent node was spawned earlier in this run to inject into".to_string(),
            );
        }
        CircuitNodeKind::InjectPty { .. } => {
            // Stays Running until AgentReady.
        }
        CircuitNodeKind::GithubAction { action, label, comment, .. } => {
            t.effects.push(Effect::CallGithub {
                node_id: node_id.to_string(),
                action: *action,
                label: label.clone(),
                comment: comment.clone(),
            });
            // Stays Running until GithubActionResult event!
        }
        CircuitNodeKind::SpawnAgentNode { .. } => {
            t.effects.push(Effect::SpawnAgentNode {
                node_id: node_id.to_string(),
            });
            // Stays Running until AgentFinished/AgentLost.
        }
        // Joins are instant once scheduled (eligibility already proved
        // their fan-in rule).
        CircuitNodeKind::AllCompleted | CircuitNodeKind::AnyCompleted => {
            set_step(run, t, node_id, StepStatus::Completed);
        }
        // -- Gates (#1207) -------------------------------------------------
        CircuitNodeKind::LlmTurnClassifier { target_node_id }
        | CircuitNodeKind::AwaitAgentTurn { target_node_id }
        | CircuitNodeKind::ReviewVerdict { target_node_id }
            // Parks Running until the seam classifies the piloted
            // agent's next turn yield and feeds TurnClassified back.
            // Without any spawned agent there is nothing to classify —
            // fail fast rather than wedge.
            if run.resolve_target_agent(node_id).is_none() =>
        {
            fail_step(
                run,
                t,
                node_id,
                "no agent node was spawned earlier in this run to classify".to_string(),
            );
        }
        CircuitNodeKind::LlmTurnClassifier { .. }
        | CircuitNodeKind::AwaitAgentTurn { .. }
        | CircuitNodeKind::ReviewVerdict { .. } => {
            // Stays Running until TurnClassified.
        }
        CircuitNodeKind::DeterministicVerification { .. } => {
            // Parks Running until the seam executes the check and feeds
            // VerificationResult back (Green/Red routing outcome).
        }
        CircuitNodeKind::CollaboratorCheck { require_approval } => {
            if *require_approval {
                // Human-in-the-loop: park on a blocked badge until the
                // Approve click arrives as CollaboratorApproved.
                set_step(run, t, node_id, StepStatus::Blocked);
            } else {
                // AutoRun passes through untouched.
                set_step(run, t, node_id, StepStatus::Completed);
            }
        }
        CircuitNodeKind::RetryLimit { max_retries } => {
            // Node-started review runs carry their per-run round limit in
            // context so the shared preset can be reused safely by multiple
            // runs with different limits. Ordinary authored circuits must
            // always use each gate's graph value; otherwise one gate's
            // context write would silently override another gate.
            let max_retries = if run.context.get("source.review_preset") == Some("1") {
                run.context
                    .get("retry.max_retries")
                    .and_then(|value| value.parse::<i32>().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(*max_retries)
            } else {
                *max_retries
            };
            execute_retry_limit(run, t, node_id, max_retries);
        }
        // Triggers never normally reach here (auto-completed at trigger
        // time), but a re-tick racing the trigger write must not wedge.
        CircuitNodeKind::Manual
        | CircuitNodeKind::Interval { .. }
        | CircuitNodeKind::GithubIssueLabel { .. }
        | CircuitNodeKind::GithubPullRequestLabel { .. } => {
            set_step(run, t, node_id, StepStatus::Completed);
        }
    }
}

/// RetryLimit gate execution (#1207): find the target step to retry
/// (either via outgoing edge or most recent failed parent) and decide retry vs exhaustion.
///
/// Semantics: `max_retries` is the total allowed executions of the
/// failing step. `attempt < max_retries` → reset the target step to
/// Queued with `attempt + 1` (the FIFO promotion loop re-runs it) and
/// complete the gate. A bounded feedback cycle may also arrive through a
/// successful classifier; that is a deliberate review-loop re-entry and
/// uses the same attempt budget. Budget exhaustion fails ordinary retry
/// gates, while a review cycle completes with a Failed outcome so its
/// explicit exhaustion notification can run.
fn execute_retry_limit(run: &mut RunView, t: &mut Transition, node_id: &str, max_retries: i32) {
    run.context
        .set("retry.max_retries", max_retries.to_string());
    let failed_parent = run
        .steps
        .iter()
        .rev()
        .find(|s| {
            (s.status == StepStatus::Failed
                || s.outcome == Some(StepOutcome::Failed)
                || s.outcome == Some(StepOutcome::Red))
                && run
                    .graph
                    .incoming(node_id)
                    .iter()
                    .any(|e| e.from == s.node_id)
        })
        .map(|step| step.node_id.clone());
    let retry_target = run.graph.children(node_id).into_iter().next();
    let completed_parent = if failed_parent.is_none() {
        retry_target.as_deref().and_then(|target| {
            if !retry_gate_reaches_itself(run, node_id, target) {
                return None;
            }
            run.steps
                .iter()
                .rev()
                .find(|step| {
                    step.status == StepStatus::Completed
                        && step.outcome == Some(StepOutcome::Completed)
                        && run.graph.incoming(node_id).iter().any(|edge| {
                            edge.from == step.node_id
                                && matches!(
                                    edge.condition,
                                    EdgeCondition::Always
                                        | EdgeCondition::OnOutcome(StepOutcome::Completed)
                                )
                        })
                })
                .map(|step| step.node_id.clone())
        })
    } else {
        None
    };
    let is_feedback_cycle = failed_parent.is_none() && completed_parent.is_some();
    let Some(parent_node_id) = failed_parent.or(completed_parent) else {
        run.context.set("retry.attempt", "0");
        fail_step(
            run,
            t,
            node_id,
            "retry limit reached without a failed upstream step".to_string(),
        );
        return;
    };
    let target = retry_target.clone().unwrap_or(parent_node_id);
    let attempt = run.step(&target).map(|s| s.attempt).unwrap_or(1);
    run.context.set("retry.attempt", attempt.to_string());
    if attempt < max_retries {
        let next_attempt = attempt + 1;
        run.context.set("retry.attempt", next_attempt.to_string());
        reset_step_for_retry(run, t, &target, next_attempt);
        complete_with_outcome(run, t, node_id, StepOutcome::Completed);
    } else if is_feedback_cycle {
        // A review loop reports exhaustion through the graph's explicit
        // Failed route instead of silently leaving a completed gate with no
        // successor. Ordinary RetryLimit gates retain fail-fast semantics.
        complete_with_outcome(run, t, node_id, StepOutcome::Failed);
        cascade_after_completion(run, t);
        finish_run_if_done(run, t);
    } else {
        fail_step(
            run,
            t,
            node_id,
            format!("retry budget exhausted after {max_retries} attempts"),
        );
    }
}

/// Return true when a RetryLimit's first outgoing target can reach the gate
/// again. Completed-parent retries are only legal for this bounded feedback
/// shape; a one-off Always edge after success remains a configuration error.
fn retry_gate_reaches_itself(run: &RunView, gate_id: &str, target_id: &str) -> bool {
    let mut stack = vec![target_id.to_string()];
    let mut visited = std::collections::HashSet::new();
    while let Some(current) = stack.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if current == gate_id {
            return true;
        }
        stack.extend(run.graph.children(&current));
    }
    false
}

/// Reset a step for another execution: back to Queued with the
/// attempt count bumped and error/outcome cleared (preserving agent_node_id).
fn reset_step_for_retry(run: &mut RunView, t: &mut Transition, node_id: &str, next_attempt: i32) {
    if let Some(step) = run.step_mut(node_id) {
        step.status = StepStatus::Queued;
        step.outcome = None;
        step.error = None;
        step.attempt = next_attempt;
    } else {
        // A retry target may be a downstream step that has not run yet in
        // the current graph round (for example, the wrap-up correction
        // prompt). Materialise its queued view here so the next execution
        // retains the bumped attempt and downstream stale-step detection can
        // re-arm the classifier after that correction completes.
        let mut step = StepView::new(node_id, StepStatus::Queued);
        step.attempt = next_attempt;
        run.steps.push(step);
    }
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Queued,
        outcome: Some(None),
        error: Some(None),
        agent_node_id: None,
        attempt: next_attempt,
        fresh_attempt: true,
    });
}

/// After completions, promote every newly-eligible NON-agent node: with no
/// per-circuit step budget (ADR 0042) DAG eligibility is their only gate.
/// Agent spawns always wait for the next Tick (which also recounts slots
/// freed by the agent lifecycle), so they never cascade here.
fn cascade_after_completion(run: &mut RunView, t: &mut Transition) {
    if run.state != RunState::Running {
        return; // fail-fast: nothing new may start once the run failed
    }
    let eligible: Vec<(String, CircuitNodeKind)> = collect_eligible(run)
        .into_iter()
        .filter(|(_, kind)| !consumes_agent_slot(kind))
        .collect();
    for (node_id, kind) in eligible {
        start_step(run, t, &node_id, &kind);
    }
}

/// Terminal check: the run completes when all active branches are terminal
/// and no further steps are eligible (and at least one step completed).
/// Cancelled steps flip the run Failed instead of Completed.
fn has_current_review_approval(run: &RunView, step: &StepView) -> bool {
    run.context
        .get(&format!("node.{}.review_verdict", step.node_id))
        == Some("approved")
        && run
            .context
            .get(&format!("node.{}.review_verdict_attempt", step.node_id))
            == Some(step.attempt.to_string().as_str())
}

fn finish_run_if_done(run: &mut RunView, t: &mut Transition) {
    // A Failed/Cancelled run sweeps its leftovers: sibling Running/Queued steps are
    // cancelled so the ledger reflects reality and the concurrency
    // counters (`count_active_circuit_agent_nodes`, which only reads runs in
    // state 'running') stop leaking their slots.
    if run.state == RunState::Failed || run.state == RunState::Cancelled {
        let leftovers: Vec<String> = run
            .steps
            .iter()
            .filter(|s| !s.status.is_terminal())
            .map(|s| s.node_id.clone())
            .collect();
        for node_id in leftovers {
            let reason = if run.state == RunState::Cancelled {
                "Cancelled because the circuit run was cancelled."
            } else {
                "Cancelled because the circuit run failed."
            };
            cancel_step(run, t, &node_id, reason);
        }
        return;
    }
    if run.state != RunState::Running {
        return;
    }
    let any_cancelled = run.steps.iter().any(|s| s.status == StepStatus::Cancelled);
    if any_cancelled {
        run.state = RunState::Failed;
        t.run_state_changed = true;
        return;
    }
    let has_non_terminal = run.steps.iter().any(|s| !s.status.is_terminal());
    if has_non_terminal {
        return;
    }
    let any_completed = run.steps.iter().any(|s| s.status == StepStatus::Completed);
    let has_eligible = !collect_eligible(run).is_empty();
    // AgentReady transitions complete an InjectPty step and may cascade
    // instant successors, but their PTY write still has to execute while
    // the durable run is active. Defer the final run-state write until the
    // next tick so execute_effects cannot mistake the injection for a stale
    // post-completion command. Other terminal effects remain safe to run on
    // the same completing transition.
    let has_pending_injection = t.effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::InjectPty { .. } | Effect::ContinueAgentTurn { .. }
        )
    });
    if any_completed && !has_eligible && !has_pending_injection {
        // A handled failure may have run its notification, but exhausting a
        // retry budget still needs attention. Likewise a blocked review is
        // not approval merely because its notification finished.
        let has_review = run.steps.iter().any(|step| {
            matches!(
                run.graph.node(&step.node_id).map(|n| &n.kind),
                Some(CircuitNodeKind::ReviewVerdict { .. })
            )
        });
        let unresolved =
            run.steps.iter().any(
                |step| match run.graph.node(&step.node_id).map(|n| &n.kind) {
                    Some(CircuitNodeKind::RetryLimit { .. }) => {
                        has_review && step.outcome == Some(StepOutcome::Failed)
                    }
                    Some(CircuitNodeKind::ReviewVerdict { .. }) => {
                        !has_current_review_approval(run, step)
                    }
                    _ => false,
                },
            );
        // A failed run retires its owned agents through the terminal commit.
        // A completed run hands back every agent its graph did not close: an
        // approved review leaves the implementation agent open to merge.
        run.state = if unresolved {
            RunState::Failed
        } else {
            RunState::Completed
        };
        t.run_state_changed = true;
    }
}

#[cfg(test)]
mod tests;
