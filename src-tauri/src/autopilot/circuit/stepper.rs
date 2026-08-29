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
//! - `SpawnAgentNode` needs BOTH a free per-circuit step slot and a free
//!   mesh agent slot; otherwise the step parks in `Queued`
//!   (`pending_slot` in the ledger) and promotes FIFO by insertion order
//!   on a later `Tick`. Non-agent steps never wait on agent slots (they
//!   still respect the per-circuit limit). Known milestone-1 scope note:
//!   FIFO ordering is per-run — cross-run ordering on one circuit is
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
use crate::autopilot::evaluator::Classification;

// ---------------------------------------------------------------------------
// State model — the pure mirror of the three ledger tables.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Run row exists, trigger not yet processed.
    Pending,
    Running,
    /// Graceful pause (#1207): the current step may finish but the graph
    /// does not advance until resumed.
    Paused,
    Completed,
    Failed,
}

impl RunState {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// Parked because a concurrency/agent-slot limit blocked it; promotes
    /// FIFO when capacity frees (stored as `pending_slot`).
    Queued,
    Running,
    /// Parked on a CollaboratorCheck RequireApproval gate (#1207):
    /// waiting for the user's Approve click. Not terminal.
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl StepStatus {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Queued => "pending_slot",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "pending_slot" => Self::Queued,
            "running" => Self::Running,
            "blocked" => Self::Blocked,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Queued,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

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

    fn step_mut(&mut self, node_id: &str) -> Option<&mut StepView> {
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
    pub fn is_upstream_ancestor(&self, descendant: &str, ancestor: &str) -> bool {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(descendant);
        visited.insert(descendant);
        while let Some(curr) = queue.pop_front() {
            for edge in self.graph.incoming(curr) {
                let from = edge.from.as_str();
                if from == ancestor {
                    return true;
                }
                if visited.insert(from) {
                    queue.push_back(from);
                }
            }
        }
        false
    }

    /// Check if any upstream ancestor of `node_id` in the graph satisfies a predicate.
    pub fn has_upstream_node_of_kind<F>(&self, node_id: &str, predicate: F) -> bool
    where
        F: Fn(&CircuitNodeKind) -> bool,
    {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(node_id);
        visited.insert(node_id);
        while let Some(curr) = queue.pop_front() {
            for edge in self.graph.incoming(curr) {
                let from = edge.from.as_str();
                if visited.insert(from) {
                    if let Some(node) = self.graph.node(from) {
                        if predicate(&node.kind) {
                            return true;
                        }
                    }
                    queue.push_back(from);
                }
            }
        }
        false
    }

    /// Resolve the target agent node for an `InjectPty` or `SetNodeStatus` step:
    /// - If `target_node_id` is explicitly set, validate that it is an upstream
    ///   `SpawnAgentNode` in this step's lineage (or is the step itself) and return its
    ///   `agent_node_id`. If invalid or not upstream, fails closed (`None`).
    /// - If omitted (`None`), walk backward from `node_id` through incoming edges
    ///   in the graph's dependency lineage and return the `agent_node_id` of the
    ///   nearest upstream `SpawnAgentNode`. Fails closed (`None`) if none exists in branch.
    pub fn resolve_target_agent(&self, node_id: &str, target_node_id: Option<&str>) -> Option<i64> {
        if let Some(target) = target_node_id {
            let is_spawn = matches!(
                self.graph.node(target).map(|n| &n.kind),
                Some(CircuitNodeKind::SpawnAgentNode { .. })
            );
            if !is_spawn {
                return None;
            }
            if target != node_id && !self.is_upstream_ancestor(node_id, target) {
                return None;
            }
            return self.step(target).and_then(|s| s.agent_node_id);
        }
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(node_id);
        visited.insert(node_id);
        while let Some(curr) = queue.pop_front() {
            for edge in self.graph.incoming(curr) {
                let from = edge.from.as_str();
                if visited.insert(from) {
                    if let Some(node) = self.graph.node(from) {
                        if matches!(node.kind, CircuitNodeKind::SpawnAgentNode { .. }) {
                            if let Some(step) = self.step(from) {
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

    /// Attach the spawned mesh agent node to its step. Called by the seam
    /// right after the synchronous stage-1 row creation succeeds.
    pub fn attach_agent_node(&mut self, node_id: &str, agent_node_id: i64) {
        if let Some(step) = self.step_mut(node_id) {
            step.agent_node_id = Some(agent_node_id);
        }
    }
}

/// Capacity snapshot carried inside `CircuitEvent::Tick`. Computed by the
/// impure seam from live counts; the stepper only does arithmetic on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    /// Free per-circuit step slots (`concurrency_limit - running steps`).
    pub circuit_free_slots: i64,
    /// Free mesh-wide auto-spawned-agent slots
    /// (`meshes.autopilot_concurrency_limit - active circuit agents`).
    pub mesh_agent_free_slots: i64,
}

/// One observed fact from outside the pure core. Kept minimal: every
/// variant maps 1:1 to something the seam can observe cheaply each tick.
///
/// There is deliberately NO keystroke/user-input variant — human typing
/// in a piloted terminal cannot reach the stepper, which is exactly the
/// "manual PTY interaction never breaks a run" guarantee.
#[derive(Debug, Clone)]
pub enum CircuitEvent {
    /// The run was triggered. Renamed from `ManualTriggered` in #1208:
    /// runs are minted pending by ANY trigger dispatch (Trigger Now,
    /// a GitHub poll ingest, an interval fire) and this event is
    /// trigger-kind agnostic.
    Triggered,
    /// Periodic fast tick carrying current capacity.
    Tick(Capacity),
    /// The seam observed the injected prompt's target process is now live.
    AgentReady { node_id: String },
    /// The seam observed the step's piloted agent finished its turn/work
    /// (node status `awaiting_input` / `completed`). `success=false`
    /// covers the node landing in `error`. `output` holds the terminal
    /// turn text captured by the evaluator blackboard.
    AgentFinished {
        agent_node_id: i64,
        success: bool,
        output: Option<String>,
    },
    /// The step's piloted agent was closed/archived mid-run.
    AgentLost { agent_node_id: i64 },
    // -- Milestone 2 (#1207) --
    /// The user paused the run. Current steps finish; nothing advances.
    Paused,
    /// The user resumed a paused run.
    Resumed,
    /// The user approved a CollaboratorCheck gate parked in Blocked.
    CollaboratorApproved { node_id: String },
    /// The seam classified the piloted agent's latest turn for this
    /// LlmTurnClassifier gate. `None` = unclassifiable (degrades to
    /// Working).
    TurnClassified {
        node_id: String,
        classification: Option<Classification>,
    },
    /// The seam ran the DeterministicVerification command.
    VerificationResult { node_id: String, green: bool },
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
}

// ---------------------------------------------------------------------------
// Effects — everything the seam must do on the stepper's behalf.
// ---------------------------------------------------------------------------

/// One persisted mutation of a step row. `None` fields mean "leave as-is".
#[derive(Debug, Clone, PartialEq)]
pub struct StepWrite {
    pub node_id: String,
    pub status: StepStatus,
    pub outcome: Option<Option<StepOutcome>>,
    pub error: Option<String>,
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
    SetNodeStatus {
        node_id: String,
        status: String,
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
    pub step_writes: Vec<StepWrite>,
    pub effects: Vec<Effect>,
    pub run_state_changed: bool,
}

impl Transition {
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.step_writes.is_empty() && self.effects.is_empty() && !self.run_state_changed
    }
}

// ---------------------------------------------------------------------------
// The stepper.
// ---------------------------------------------------------------------------

/// Advance one run by one event. Mutates `run` in place (so callers can
/// chain events without re-reading) and returns what to persist + execute.
pub fn advance(run: &mut RunView, event: &CircuitEvent) -> Transition {
    let mut t = Transition::default();
    match event {
        CircuitEvent::Triggered => {
            if run.state == RunState::Pending {
                run.state = RunState::Running;
                t.run_state_changed = true;
                // Every trigger root auto-completes: it fired to create
                // this run. Cloned first — the loop mutates `run`.
                let roots: Vec<crate::autopilot::circuit::model::CircuitNode> =
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
        CircuitEvent::AgentReady { node_id } => {
            // Fire the pending injection for a Running inject step whose
            // target process just became live.
            let (prompt, target_node_id) = match run.graph.node(node_id) {
                Some(n) => match &n.kind {
                    CircuitNodeKind::InjectPty { prompt, target_node_id } => {
                        (Some(prompt.clone()), target_node_id.clone())
                    }
                    _ => (None, None),
                },
                None => (None, None),
            };
            let is_running_inject =
                matches!(run.step(node_id), Some(s) if s.status == StepStatus::Running);
            if let Some(prompt) = prompt {
                if is_running_inject {
                    t.effects.push(Effect::InjectPty {
                        node_id: node_id.clone(),
                        prompt: run.context.resolve(&prompt),
                        target_node_id,
                    });
                    set_step(run, &mut t, node_id, StepStatus::Completed);
                    cascade_after_completion(run, &mut t, 1);
                    finish_run_if_done(run, &mut t);
                }
            }
        }
        CircuitEvent::AgentFinished {
            agent_node_id,
            success,
            output,
        } => {
            let bound: Option<String> = run
                .steps
                .iter()
                .find(|s| s.status == StepStatus::Running && s.agent_node_id == Some(*agent_node_id))
                .map(|s| s.node_id.clone());
            if let Some(step_node) = bound {
                if let Some(out) = output {
                    run.context.set(&format!("node.{}.output", step_node), out);
                }
                if *success {
                    set_step(run, &mut t, &step_node, StepStatus::Completed);
                } else {
                    fail_step(
                        run,
                        &mut t,
                        &step_node,
                        "piloted agent node reported error".to_string(),
                    );
                }
                cascade_after_completion(run, &mut t, 1);
                finish_run_if_done(run, &mut t);
            }
        }
        CircuitEvent::AgentLost { agent_node_id } => {
            let bound: Option<String> = run
                .steps
                .iter()
                .find(|s| s.status == StepStatus::Running && s.agent_node_id == Some(*agent_node_id))
                .map(|s| s.node_id.clone());
            if let Some(step_node) = bound {
                cancel_step(run, &mut t, &step_node);
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
            }
        }
        CircuitEvent::CollaboratorApproved { node_id } => {
            let waiting = matches!(
                run.step(node_id),
                Some(s) if s.status == StepStatus::Blocked
            ) && matches!(
                run.graph.node(node_id).map(|n| &n.kind),
                Some(CircuitNodeKind::CollaboratorCheck { require_approval: true })
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
                cascade_after_completion(run, &mut t, 1);
                finish_run_if_done(run, &mut t);
            }
        }
        CircuitEvent::TurnClassified {
            node_id,
            classification,
        } => {
            let is_waiting_classifier = matches!(
                run.step(node_id),
                Some(s) if s.status == StepStatus::Running
            ) && matches!(
                run.graph.node(node_id).map(|n| &n.kind),
                Some(CircuitNodeKind::LlmTurnClassifier)
            );
            if is_waiting_classifier && run.state == RunState::Running {
                let outcome = match classification {
                    Some(Classification::Completed) => StepOutcome::Completed,
                    Some(Classification::Blocked) => StepOutcome::Blocked,
                    Some(Classification::Working) | None => StepOutcome::Working,
                };
                complete_with_outcome(run, &mut t, node_id, outcome);
                cascade_after_completion(run, &mut t, 1);
                finish_run_if_done(run, &mut t);
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
                let outcome = if *green { StepOutcome::Green } else { StepOutcome::Red };
                complete_with_outcome(run, &mut t, node_id, outcome);
                cascade_after_completion(run, &mut t, 1);
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
                    cascade_after_completion(run, &mut t, 1);
                    finish_run_if_done(run, &mut t);
                } else {
                    fail_step(
                        run,
                        &mut t,
                        node_id,
                        error.clone().unwrap_or_else(|| "GitHub action failed".to_string()),
                    );
                    finish_run_if_done(run, &mut t);
                }
            }
        }
    }
    t
}

/// Mark a step (creating it if absent) with a new status. Terminal
/// Mark a step (creating it if absent) with a new status. Terminal
/// statuses stamp the matching outcome.
fn set_step(run: &mut RunView, t: &mut Transition, node_id: &str, status: StepStatus) {
    let incoming_attempt = run
        .graph
        .incoming(node_id)
        .iter()
        .filter_map(|e| run.step(&e.from).map(|ps| ps.attempt))
        .max()
        .unwrap_or(1);
    let changed = match run.step_mut(node_id) {
        Some(step) => {
            if incoming_attempt > step.attempt {
                step.attempt = incoming_attempt;
                step.outcome = None;
                step.error = None;
            }
            if step.status != status {
                step.status = status;
                step.outcome = status.outcome();
                true
            } else {
                false
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
        let outcome_str = status.outcome().map(|o| o.as_db_str()).unwrap_or(status.as_db_str());
        run.context.set(&format!("node.{}.status", node_id), outcome_str);
    }
    if changed {
        // Preserve the existing attempt count on re-transitions (a retry's
        // second run must not write attempt=1 over its reset value).
        let attempt = run.step(node_id).map(|s| s.attempt).unwrap_or(1);
        t.step_writes.push(StepWrite::for_existing(node_id, status, attempt));
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
            step.attempt
        }
        None => 1,
    };
    run.context.set(&format!("node.{}.status", node_id), outcome.as_db_str());
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Completed,
        outcome: Some(Some(outcome)),
        error: None,
        agent_node_id: None,
        attempt,
        fresh_attempt: false,
    });
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
    run.context.set(&format!("node.{}.status", node_id), StepOutcome::Failed.as_db_str());
    let attempt = run.step(node_id).map(|s| s.attempt).unwrap_or(1);
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Failed,
        outcome: Some(Some(StepOutcome::Failed)),
        error: Some(error),
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
                error: None,
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

fn cancel_step(run: &mut RunView, t: &mut Transition, node_id: &str) {
    const CANCEL_REASON: &str = "piloted agent node was closed";
    match run.step_mut(node_id) {
        Some(step) => {
            if step.status.is_terminal() {
                return;
            }
            step.status = StepStatus::Cancelled;
            step.outcome = Some(StepOutcome::Cancelled);
            step.error = Some(CANCEL_REASON.to_string());
        }
        // Symmetrical with fail_step: an absent step still gets a
        // Cancelled StepView so run.steps and t.step_writes agree.
        None => {
            let mut step = StepView::new(node_id, StepStatus::Cancelled);
            step.outcome = Some(StepOutcome::Cancelled);
            step.error = Some(CANCEL_REASON.to_string());
            run.steps.push(step);
        }
    }
    run.context.set(&format!("node.{}.status", node_id), StepOutcome::Cancelled.as_db_str());
    let attempt = run.step(node_id).map(|s| s.attempt).unwrap_or(1);
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Cancelled,
        outcome: Some(Some(StepOutcome::Cancelled)),
        error: Some(CANCEL_REASON.to_string()),
        agent_node_id: None,
        attempt,
        fresh_attempt: false,
    });
    run.state = RunState::Failed;
    t.run_state_changed = true;
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
                return false;
            }
            if matches!(
                run.graph.node(&e.from).map(|n| &n.kind),
                Some(CircuitNodeKind::CollaboratorCheck { require_approval: true })
            ) && run.step(&e.from).is_none() {
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
/// joins, SetNodeStatus) free their circuit slot again immediately, so a
/// chain of plain actions executes entirely within this one tick — no
/// 2s-per-node penalty. Queued (FIFO) steps promote first; agent spawns
/// occupy their slot until the piloted node finishes.
fn schedule_ready(run: &mut RunView, t: &mut Transition, capacity: Capacity) {
    let mut circuit_free = capacity.circuit_free_slots;
    let mut mesh_agent_free = capacity.mesh_agent_free_slots;

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
            let started = try_start(run, t, &node_id, &kind, &mut circuit_free, &mut mesh_agent_free);
            progressed |= started;
            if started && step_completed_instantly(run, &node_id) {
                // Freed its slot again — but its successors are picked
                // up on the next fixpoint pass.
                circuit_free += 1;
                if consumes_agent_slot(&kind) {
                    mesh_agent_free += 1;
                }
            }
        }

        for (node_id, kind) in collect_eligible(run) {
            if run.state != RunState::Running {
                break;
            }
            let already_has_agent = run.step(&node_id).and_then(|s| s.agent_node_id).is_some();
            let needs_agent_slot = consumes_agent_slot(&kind) && !already_has_agent;
            let agent_fits = !needs_agent_slot || mesh_agent_free > 0;
            if circuit_free <= 0 || !agent_fits {
                set_step(run, t, &node_id, StepStatus::Queued);
                continue;
            }
            start_step(run, t, &node_id, &kind);
            progressed = true;
            if step_completed_instantly(run, &node_id) {
                // Instant completion: the slot is free again within this
                // same pass, so don't charge it.
            } else {
                circuit_free -= 1;
                if needs_agent_slot {
                    mesh_agent_free -= 1;
                }
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
    run.step(node_id).map(|s| s.status.is_terminal()).unwrap_or(false)
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
                        run.step(&e.from).map(|ps| ps.attempt > s.attempt && ps.status.is_terminal()).unwrap_or(false)
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
    circuit_free: &mut i64,
    mesh_agent_free: &mut i64,
) -> bool {
    let already_has_agent = run.step(node_id).and_then(|s| s.agent_node_id).is_some();
    let needs_agent_slot = consumes_agent_slot(kind) && !already_has_agent;
    let agent_fits = !needs_agent_slot || *mesh_agent_free > 0;
    if *circuit_free <= 0 || !agent_fits {
        return false;
    }
    start_step(run, t, node_id, kind);
    *circuit_free -= 1;
    if needs_agent_slot {
        *mesh_agent_free -= 1;
    }
    true
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
    let incoming_attempt = run
        .graph
        .incoming(node_id)
        .iter()
        .filter_map(|e| run.step(&e.from).map(|ps| ps.attempt))
        .max()
        .unwrap_or(1);
    if let Some(step) = run.step_mut(node_id) {
        if incoming_attempt > step.attempt {
            step.attempt = incoming_attempt;
            step.outcome = None;
            step.error = None;
        }
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
            let target_agent = run.resolve_target_agent(node_id, target_node_id.as_deref());
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
        CircuitNodeKind::InjectPty { target_node_id, .. }
            // Wait for AgentReady — the spawn's async stage-2 must land
            // and the agent process be live before we write bytes.
            // But without any earlier spawn in this run, inject has no
            // target at all — fail fast.
            if run.resolve_target_agent(node_id, target_node_id.as_deref()).is_none() =>
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
        CircuitNodeKind::GithubAction { action, label, comment } => {
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
        CircuitNodeKind::LlmTurnClassifier
            // Parks Running until the seam classifies the piloted
            // agent's next turn yield and feeds TurnClassified back.
            // Without any spawned agent there is nothing to classify —
            // fail fast rather than wedge.
            if run.resolve_target_agent(node_id, None).is_none() =>
        {
            fail_step(
                run,
                t,
                node_id,
                "no agent node was spawned earlier in this run to classify".to_string(),
            );
        }
        CircuitNodeKind::LlmTurnClassifier => {
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
            execute_retry_limit(run, t, node_id, *max_retries);
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
/// complete the gate. Budget exhausted → the gate fails, resuming the
/// normal fail-fast path.
fn execute_retry_limit(run: &mut RunView, t: &mut Transition, node_id: &str, max_retries: i32) {
    let failed_parent = run.steps.iter().rev().find(|s| {
        (s.status == StepStatus::Failed
            || s.outcome == Some(StepOutcome::Failed)
            || s.outcome == Some(StepOutcome::Red))
            && run.graph.incoming(node_id).iter().any(|e| e.from == s.node_id)
    });
    let Some(failed_parent) = failed_parent else {
        fail_step(
            run,
            t,
            node_id,
            "retry limit reached without a failed upstream step".to_string(),
        );
        return;
    };
    let target = run
        .graph
        .children(node_id)
        .into_iter()
        .next()
        .unwrap_or_else(|| failed_parent.node_id.clone());
    let attempt = run.step(&target).map(|s| s.attempt).unwrap_or(1);
    if attempt < max_retries {
        reset_step_for_retry(run, t, &target, attempt + 1);
        set_step(run, t, node_id, StepStatus::Completed);
    } else {
        fail_step(
            run,
            t,
            node_id,
            format!("retry budget exhausted after {} attempts", max_retries),
        );
    }
}

/// Reset a step for another execution: back to Queued with the
/// attempt count bumped and error/outcome cleared (preserving agent_node_id).
fn reset_step_for_retry(
    run: &mut RunView,
    t: &mut Transition,
    node_id: &str,
    next_attempt: i32,
) {
    if let Some(step) = run.step_mut(node_id) {
        step.status = StepStatus::Queued;
        step.outcome = None;
        step.error = None;
        step.attempt = next_attempt;
    }
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Queued,
        outcome: Some(None),
        error: None,
        agent_node_id: None,
        attempt: next_attempt,
        fresh_attempt: true,
    });
}

/// After completions, promote newly-eligible NON-agent nodes — but at
/// most `budget` of them, one per step that just went terminal. Each
/// terminal step frees exactly its own circuit slot, so capping the
/// cascade at that count preserves the per-circuit concurrency limit
/// between Ticks (the Tick's authoritative capacity snapshot re-checks).
/// Agent spawns always wait for the next Tick (which also recounts mesh
/// slots freed by the autopilot lifecycle), so they never cascade here.
fn cascade_after_completion(run: &mut RunView, t: &mut Transition, budget: usize) {
    if run.state != RunState::Running || budget == 0 {
        return; // fail-fast: nothing new may start once the run failed
    }
    let eligible: Vec<(String, CircuitNodeKind)> = collect_eligible(run)
        .into_iter()
        .filter(|(_, kind)| !consumes_agent_slot(kind))
        .take(budget)
        .collect();
    for (node_id, kind) in eligible {
        start_step(run, t, &node_id, &kind);
    }
}

/// Terminal check: the run completes when all active branches are terminal
/// and no further steps are eligible (and at least one step completed).
/// Cancelled steps flip the run Failed instead of Completed.
fn finish_run_if_done(run: &mut RunView, t: &mut Transition) {
    // A Failed run sweeps its leftovers: sibling Running/Queued steps are
    // cancelled so the ledger reflects reality and the concurrency
    // counters (`count_running_circuit_steps` /
    // `count_active_circuit_agent_nodes`, which only read runs in
    // state 'running') stop leaking their slots.
    if run.state == RunState::Failed {
        let leftovers: Vec<String> = run
            .steps
            .iter()
            .filter(|s| !s.status.is_terminal())
            .map(|s| s.node_id.clone())
            .collect();
        for node_id in leftovers {
            cancel_step(run, t, &node_id);
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
    if any_completed && !has_eligible {
        run.state = RunState::Completed;
        t.run_state_changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot::circuit::model::{
        CircuitEdge, CircuitNode, GithubActionKind,
    };

    fn spawn_kind(prompt: &str) -> CircuitNodeKind {
        CircuitNodeKind::SpawnAgentNode {
            prompt: prompt.into(),
            name: None,
            provider: None,
            model: None,
            effort: None,
            extra_args: None,
        }
    }

    fn inject_kind(prompt: &str) -> CircuitNodeKind {
        CircuitNodeKind::InjectPty {
            prompt: prompt.into(),
            target_node_id: None,
        }
    }

    fn agent_finished(agent_node_id: i64, success: bool) -> CircuitEvent {
        CircuitEvent::AgentFinished {
            agent_node_id,
            success,
            output: None,
        }
    }

    // -- fixtures -------------------------------------------------------------

    /// A minimal trigger → spawn → notify chain, built inline so the
    /// fixture is independent of [`CircuitGraph::walking_skeleton`]'s
    /// canonical shape (which has its own end-to-end test below).
    fn linear_run() -> RunView {
        let mut ctx = CircuitContext::new();
        ctx.with_circuit(7, "nightly-sweep", 3);
        ctx.with_run(42);
        RunView {
            run_id: 42,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "trigger".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode {
                        id: "spawn".into(),
                        kind: spawn_kind("fix it"),
                    },
                    CircuitNode {
                        id: "notify".into(),
                        kind: CircuitNodeKind::Notify { message: "done {{circuit.name}}".into() },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "trigger".into(), to: "spawn".into(), condition: Default::default() },
                    CircuitEdge { from: "spawn".into(), to: "notify".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: ctx,
            steps: vec![],
        }
    }

    fn capacity(circuit_free: i64, mesh_agent_free: i64) -> Capacity {
        Capacity { circuit_free_slots: circuit_free, mesh_agent_free_slots: mesh_agent_free }
    }

    fn tick(c: i64, m: i64) -> CircuitEvent {
        CircuitEvent::Tick(capacity(c, m))
    }

    fn status_of(run: &RunView, node: &str) -> StepStatus {
        run.step(node).map(|s| s.status).expect("step should exist")
    }

    // -- trigger --------------------------------------------------------------

    #[test]
    fn manual_trigger_starts_run_and_completes_trigger_roots() {
        let mut run = linear_run();
        let t = advance(&mut run, &CircuitEvent::Triggered);
        assert!(t.run_state_changed);
        assert_eq!(run.state, RunState::Running);
        assert_eq!(t.step_writes.len(), 1);
        assert_eq!(t.step_writes[0].node_id, "trigger");
        assert_eq!(t.step_writes[0].status, StepStatus::Completed);
        assert_eq!(status_of(&run, "trigger"), StepStatus::Completed);
        // No scheduling happens without a Tick — every start is
        // capacity-checked.
        assert!(t.effects.is_empty());
        assert!(run.step("spawn").is_none());
    }

    #[test]
    fn trigger_is_idempotent_on_an_already_running_run() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        let second = advance(&mut run, &CircuitEvent::Triggered);
        assert!(!second.run_state_changed);
        assert!(second.is_empty());
    }

    #[test]
    fn every_trigger_kind_root_auto_completes_on_manual_fire() {
        // Trigger Now fires the circuit regardless of which trigger kind
        // its blueprint declares — the user is the trigger.
        for kind in [
            CircuitNodeKind::Manual,
            CircuitNodeKind::Interval { interval_seconds: 60 },
            CircuitNodeKind::GithubIssueLabel { label: "buildmesh:run".into() },
            CircuitNodeKind::GithubPullRequestLabel { label: "review".into() },
        ] {
            let mut run = linear_run();
            run.graph.nodes[0].kind = kind;
            advance(&mut run, &CircuitEvent::Triggered);
            assert_eq!(status_of(&run, "trigger"), StepStatus::Completed);
        }
    }

    // -- scheduling & capacity --------------------------------------------------

    #[test]
    fn first_tick_schedules_spawn_when_both_capacities_allow() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(1, 1));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Running);
        assert_eq!(
            t.effects,
            vec![Effect::SpawnAgentNode { node_id: "spawn".to_string() }]
        );
    }

    #[test]
    fn spawn_queues_when_mesh_agent_slots_are_exhausted() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(2, 0));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Queued);
        assert!(t.effects.is_empty(), "queued steps emit no spawn effect");
    }

    #[test]
    fn spawn_queues_when_circuit_concurrency_is_exhausted() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(0, 5));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Queued);
        assert!(t.effects.is_empty());
    }

    #[test]
    fn queued_step_promotes_fifo_when_a_mesh_slot_frees() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(2, 0));
        let t = advance(&mut run, &tick(2, 1));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Running);
        assert_eq!(
            t.effects,
            vec![Effect::SpawnAgentNode { node_id: "spawn".to_string() }]
        );
    }

    #[test]
    fn queued_step_stays_parked_until_both_limits_clear() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(0, 3)); // circuit full → queue
        let t = advance(&mut run, &tick(1, 0)); // agent slots still gone
        assert_eq!(status_of(&run, "spawn"), StepStatus::Queued);
        assert!(t.effects.is_empty());
    }

    #[test]
    fn re_tick_with_a_running_step_is_a_no_op() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        let again = advance(&mut run, &tick(1, 1));
        assert!(again.is_empty(), "re-tick must be a no-op: {:?}", again);
    }

    // -- agent lifecycle --------------------------------------------------------

    #[test]
    fn agent_finished_completes_the_bound_spawn_step_and_unblocks_successors() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);
        let t = advance(&mut run, &agent_finished(900, true));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Completed);
        // Notify is a non-agent successor — cascades immediately.
        assert_eq!(status_of(&run, "notify"), StepStatus::Completed);
        assert_eq!(
            t.effects,
            vec![Effect::Notify {
                message: "done nightly-sweep".to_string()
            }]
        );
    }

    #[test]
    fn simple_linear_run_lands_completed_end_to_end() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);
        advance(&mut run, &agent_finished(900, true));
        assert_eq!(run.state, RunState::Completed);
        assert!(run.steps.iter().all(|s| s.status.is_terminal()));
    }

    /// The canonical blueprint the Probe tab creates: the prompt rides
    /// InjectPty (not spawn prefill), so a full run walks
    /// trigger → spawn → AgentFinished → AgentReady(inject) → notify.
    #[test]
    fn canonical_walking_skeleton_lands_completed_through_pty_injection() {
        let mut ctx = CircuitContext::new();
        ctx.with_circuit(7, "nightly-sweep", 3);
        ctx.with_run(42);
        let mut run = RunView {
            run_id: 42,
            graph: CircuitGraph::walking_skeleton("fix the flaky test"),
            state: RunState::Pending,
            context: ctx,
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Running);
        run.attach_agent_node("spawn", 900);

        // The agent finishes its fresh boot turn; inject schedules but
        // must not fire until the process is observed live.
        let t = advance(&mut run, &agent_finished(900, true));
        assert!(t.effects.is_empty(), "injection waits for AgentReady");
        assert_eq!(status_of(&run, "inject"), StepStatus::Running);

        // The injection fires AND the trailing Notify cascades in the
        // same advance call.
        let t = advance(&mut run, &CircuitEvent::AgentReady { node_id: "inject".to_string() });
        assert_eq!(
            t.effects,
            vec![
                Effect::InjectPty {
                    node_id: "inject".to_string(),
                    prompt: "fix the flaky test".to_string(),
                    target_node_id: None,
                },
                Effect::Notify {
                    message: "Circuit run started nightly-sweep".to_string()
                },
            ]
        );
        assert_eq!(status_of(&run, "inject"), StepStatus::Completed);
        assert_eq!(status_of(&run, "notify"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
    }

    #[test]
    fn agent_error_fails_the_spawn_step_and_the_run_fail_fast() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);
        let t = advance(&mut run, &agent_finished(900, false));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Failed);
        assert_eq!(run.state, RunState::Failed);
        assert!(t.run_state_changed);
        assert!(run.step("notify").is_none(), "successors must not start after failure");
    }

    #[test]
    fn closing_the_piloted_node_mid_run_cancels_its_step_and_fails_the_run() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);
        let t = advance(&mut run, &CircuitEvent::AgentLost { agent_node_id: 900 });
        assert_eq!(status_of(&run, "spawn"), StepStatus::Cancelled);
        assert_eq!(run.state, RunState::Failed);
        let w = t.step_writes.last().unwrap();
        assert_eq!(w.status, StepStatus::Cancelled);
        assert_eq!(w.outcome, Some(Some(StepOutcome::Cancelled)));
    }

    #[test]
    fn a_failed_run_sweeps_its_still_running_siblings_so_slots_are_not_leaked() {
        // Fan-out: a errors while b is mid-flight. The failure must also
        // cancel b — otherwise its piloted agent keeps consuming a mesh
        // slot the counters no longer attribute to this (failed) run.
        let mut run = fan_out_run(CircuitNodeKind::AllCompleted);
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "b"), StepStatus::Running);
        run.attach_agent_node("a", 11);
        let t = advance(&mut run, &agent_finished(11, false));
        assert_eq!(run.state, RunState::Failed);
        assert_eq!(status_of(&run, "b"), StepStatus::Cancelled);
        assert!(
            t.step_writes.iter().any(|w| w.node_id == "b" && w.status == StepStatus::Cancelled),
            "the sibling cancellation must be persisted"
        );
    }

    #[test]
    fn cascade_starts_at_most_one_successor_per_freed_slot() {
        // spawn → (notify, notify-b): completing the spawn frees exactly
        // one circuit slot, so the cascade may start only ONE successor
        // even though both are eligible. The next Tick's authoritative
        // capacity snapshot admits the other. This is what keeps the
        // per-circuit concurrency limit honest between ticks.
        let mut run = linear_run();
        run.graph.nodes.push(CircuitNode {
            id: "notify-b".to_string(),
            kind: CircuitNodeKind::Notify { message: "b".to_string() },
        });
        run.graph.edges.push(CircuitEdge {
            from: "spawn".to_string(),
            to: "notify-b".to_string(),
            condition: Default::default(),
        });
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("spawn", 900);
        let t = advance(&mut run, &agent_finished(900, true));
        let started = ["notify", "notify-b"]
            .iter()
            .filter(|n| matches!(run.step(n), Some(s) if s.status == StepStatus::Completed))
            .count();
        assert_eq!(started, 1, "one freed slot admits exactly one successor");
        assert!(t.effects.len() <= 1);
    }

    #[test]
    fn lifecycle_events_for_unknown_agents_are_no_ops() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        let before = run.clone();
        advance(&mut run, &agent_finished(12345, true));
        advance(&mut run, &CircuitEvent::AgentLost { agent_node_id: 67890 });
        assert_eq!(run, before, "unrelated events must not mutate the run");
    }

    // -- inject -------------------------------------------------------------------

    fn two_step_inject_run() -> RunView {
        // trigger → spawn → inject → final, built explicitly so the
        // spawn step has exactly one successor.
        let mut run = linear_run();
        run.graph.nodes.clear();
        run.graph.edges.clear();
        run.graph.nodes.push(CircuitNode { id: "trigger".into(), kind: CircuitNodeKind::Manual });
        run.graph.nodes.push(CircuitNode {
            id: "spawn".into(),
            kind: spawn_kind("fix it"),
        });
        run.graph.nodes.push(CircuitNode {
            id: "inject".to_string(),
            kind: inject_kind("now wrap up {{circuit.name}}"),
        });
        run.graph.nodes.push(CircuitNode {
            id: "final".to_string(),
            kind: CircuitNodeKind::Notify { message: "done".to_string() },
        });
        run.graph.edges.push(CircuitEdge { from: "trigger".into(), to: "spawn".into(), condition: Default::default() });
        run.graph.edges.push(CircuitEdge { from: "spawn".to_string(), to: "inject".to_string(), condition: Default::default() });
        run.graph.edges.push(CircuitEdge { from: "inject".to_string(), to: "final".to_string(), condition: Default::default() });
        run
    }

    #[test]
    fn inject_waits_for_agent_ready_then_fires_resolved_prompt() {
        let mut run = two_step_inject_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(3, 3));
        run.attach_agent_node("spawn", 900);
        // Spawn finished → inject schedules but must NOT fire yet (no
        // live process observed).
        advance(&mut run, &agent_finished(900, true));
        assert_eq!(status_of(&run, "inject"), StepStatus::Running);

        let t = advance(&mut run, &CircuitEvent::AgentReady { node_id: "inject".to_string() });
        // The injection fires AND the cascade completes the trailing
        // Notify in the same advance call.
        assert_eq!(
            t.effects,
            vec![
                Effect::InjectPty {
                    node_id: "inject".to_string(),
                    prompt: "now wrap up nightly-sweep".to_string(),
                    target_node_id: None,
                },
                Effect::Notify { message: "done".to_string() },
            ]
        );
        assert_eq!(status_of(&run, "inject"), StepStatus::Completed);
        assert_eq!(status_of(&run, "final"), StepStatus::Completed);
    }

    #[test]
    fn agent_ready_for_an_unscheduled_step_is_a_no_op() {
        let mut run = two_step_inject_run();
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &CircuitEvent::AgentReady { node_id: "inject".to_string() });
        assert!(t.is_empty());
        assert!(run.step("inject").is_none());
    }

    #[test]
    fn inject_without_any_prior_spawn_fails_fast() {
        let mut run = linear_run();
        run.graph.nodes.insert(
            1,
            CircuitNode {
                id: "early-inject".to_string(),
                kind: inject_kind("hi"),
            },
        );
        run.graph.edges.insert(
            0,
            CircuitEdge {
                from: "trigger".to_string(),
                to: "early-inject".to_string(),
                condition: Default::default(),
            },
        );
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "early-inject"), StepStatus::Failed);
        assert_eq!(run.state, RunState::Failed);
        assert!(
            t.effects.iter().all(|e| !matches!(e, Effect::InjectPty { .. })),
            "no injection may fire without a spawned agent"
        );
    }

    // -- joins ---------------------------------------------------------------------

    fn fan_out_run(join_kind: CircuitNodeKind) -> RunView {
        RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode {
                        id: "a".into(),
                        kind: spawn_kind("pa"),
                    },
                    CircuitNode {
                        id: "b".into(),
                        kind: spawn_kind("pb"),
                    },
                    CircuitNode { id: "j".into(), kind: join_kind },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "a".into(), condition: Default::default() },
                    CircuitEdge { from: "t".into(), to: "b".into(), condition: Default::default() },
                    CircuitEdge { from: "a".into(), to: "j".into(), condition: Default::default() },
                    CircuitEdge { from: "b".into(), to: "j".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        }
    }

    #[test]
    fn all_completed_join_executes_only_when_every_branch_finished() {
        let mut run = fan_out_run(CircuitNodeKind::AllCompleted);
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "a"), StepStatus::Running);
        assert_eq!(status_of(&run, "b"), StepStatus::Running);
        assert!(run.step("j").is_none(), "join must wait while branches run");

        run.attach_agent_node("a", 11);
        advance(&mut run, &agent_finished(11, true));
        assert!(run.step("j").is_none(), "all_completed still waits for b");

        run.attach_agent_node("b", 12);
        let t = advance(&mut run, &agent_finished(12, true));
        assert_eq!(status_of(&run, "j"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
        assert!(t.run_state_changed);
    }

    #[test]
    fn any_completed_join_executes_when_one_branch_finishes() {
        let mut run = fan_out_run(CircuitNodeKind::AnyCompleted);
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("a", 11);
        run.attach_agent_node("b", 12);
        advance(&mut run, &agent_finished(11, true));
        assert_eq!(status_of(&run, "j"), StepStatus::Completed);

        // b still runs — the join fired but the run can't be terminal
        // while a step is live.
        assert_eq!(run.state, RunState::Running);
        advance(&mut run, &agent_finished(12, true));
        assert_eq!(run.state, RunState::Completed);
    }

    // -- conditional edges ----------------------------------------------------------

    #[test]
    fn failed_parent_does_not_traverse_an_on_completed_edge() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode {
                        id: "work".into(),
                        kind: spawn_kind("p"),
                    },
                    CircuitNode {
                        id: "on-green".into(),
                        kind: CircuitNodeKind::Notify { message: "green".into() },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "work".into(), condition: Default::default() },
                    CircuitEdge {
                        from: "work".into(),
                        to: "on-green".into(),
                        condition: EdgeCondition::OnOutcome(StepOutcome::Completed),
                    },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("work", 5);
        advance(&mut run, &agent_finished(5, false));
        assert_eq!(run.state, RunState::Failed);
        assert!(
            run.step("on-green").is_none(),
            "an OnOutcome(Completed) edge must not traverse on failure"
        );
    }

    // -- gate execution (milestone 2, #1207) -----------------------------------------

    #[test]
    fn llm_classifier_parks_running_until_classified() {
        // The milestone-1 behavior (fail with "not executed until a later
        // milestone") is replaced in #1207: the gate waits for the seam.
        let mut run = gate_run("classify", CircuitNodeKind::LlmTurnClassifier, &[]);
        fire_to_gate(&mut run, "classify");
        assert_eq!(status_of(&run, "classify"), StepStatus::Running);
        assert_eq!(run.state, RunState::Running);
    }

    #[test]
    fn classifier_without_any_prior_spawn_fails_fast() {
        let mut run = gate_run("classify", CircuitNodeKind::LlmTurnClassifier, &[]);
        run.graph.nodes.retain(|n| n.id != "work");
        run.graph.edges.retain(|e| e.from != "work");
        run.graph.edges.push(CircuitEdge {
            from: "trigger".into(),
            to: "classify".into(),
            condition: Default::default(),
        });
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "classify"), StepStatus::Failed);
        assert_eq!(run.state, RunState::Failed);
        let _ = t;
    }

    
    // -- GithubAction execution (milestone 3, issue #1208) -------------------------

    #[test]
    fn github_action_completes_instantly_and_emits_the_call_effect_with_templates() {
        // Milestone 3: a GithubAction step is instant-completing like
        // Notify — the HTTP call happens in the seam after the commit.
        // The raw templates ride the effect; resolution is the seam's
        // job (execution time, not decision time).
        let mut run = linear_run();
        run.graph.nodes.push(CircuitNode {
            id: "label".into(),
            kind: CircuitNodeKind::GithubAction {
                action: GithubActionKind::AddLabel,
                label: Some("in-progress".into()),
                comment: None,
            },
        });
        run.graph.edges.push(CircuitEdge {
            from: "spawn".into(),
            to: "label".into(),
            condition: Default::default(),
        });
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        // The spawn holds the only slot; the label step waits.
        assert!(run.step("label").is_none());

        run.attach_agent_node("spawn", 900);
        let _ =
            advance(&mut run, &agent_finished(900, true));
        // The cascade frees exactly one slot; the
        // label step schedules on the next Tick's authoritative snapshot.
        let t = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "label"), StepStatus::Running);
        assert_eq!(
            t.effects,
            vec![
                Effect::CallGithub {
                    node_id: "label".to_string(),
                    action: GithubActionKind::AddLabel,
                    label: Some("in-progress".to_string()),
                    comment: None,
                },
            ]
        );
        // Worker delivers the successful GitHub action result
        advance(&mut run, &CircuitEvent::GithubActionResult {
            node_id: "label".into(),
            success: true,
            pr_number: None,
            pr_url: None,
            pr_head_ref: None,
            pr_title: None,
            error: None,
        });
        assert_eq!(status_of(&run, "label"), StepStatus::Completed);
    }

    #[test]
    fn github_action_chain_advances_and_completes_with_action_result() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode {
                        id: "comment".into(),
                        kind: CircuitNodeKind::GithubAction {
                            action: GithubActionKind::PostComment,
                            label: None,
                            comment: Some("started {{issue.number}}".into()),
                        },
                    },
                ],
                edges: vec![CircuitEdge {
                    from: "t".into(),
                    to: "comment".into(),
                    condition: Default::default(),
                }],
            },
            state: RunState::Pending,
            context: {
                let mut ctx = CircuitContext::new();
                ctx.set("issue.number", "42");
                ctx
            },
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(2, 2));
        assert_eq!(status_of(&run, "comment"), StepStatus::Running);
        assert_eq!(
            t.effects,
            vec![Effect::CallGithub {
                node_id: "comment".to_string(),
                action: GithubActionKind::PostComment,
                label: None,
                comment: Some("started {{issue.number}}".to_string()),
            }]
        );
        advance(&mut run, &CircuitEvent::GithubActionResult {
            node_id: "comment".into(),
            success: true,
            pr_number: None,
            pr_url: None,
            pr_head_ref: None,
            pr_title: None,
            error: None,
        });
        assert_eq!(status_of(&run, "comment"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
    }

    // -- pending-run guard ------------------------------------------------------------

    #[test]
    fn pending_run_does_not_schedule_from_ticks_alone() {
        let mut run = linear_run();
        let t = advance(&mut run, &tick(9, 9));
        assert!(t.is_empty());
        assert_eq!(run.state, RunState::Pending);
    }

    #[test]
    fn instant_action_chain_executes_entirely_within_one_tick() {
        // Trigger → notify-a → notify-b → notify-c: every link completes
        // instantly, so one Tick must run the whole chain — not one node
        // per 2-second tick.
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode { id: "a".into(), kind: CircuitNodeKind::Notify { message: "a".into() } },
                    CircuitNode { id: "b".into(), kind: CircuitNodeKind::Notify { message: "b".into() } },
                    CircuitNode { id: "c".into(), kind: CircuitNodeKind::Notify { message: "c".into() } },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "a".into(), condition: Default::default() },
                    CircuitEdge { from: "a".into(), to: "b".into(), condition: Default::default() },
                    CircuitEdge { from: "b".into(), to: "c".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(1, 1));
        for n in ["a", "b", "c"] {
            assert_eq!(status_of(&run, n), StepStatus::Completed, "node {n} must finish in the same tick");
        }
        assert_eq!(run.state, RunState::Completed);
        assert_eq!(
            t.effects,
            vec![
                Effect::Notify { message: "a".to_string() },
                Effect::Notify { message: "b".to_string() },
                Effect::Notify { message: "c".to_string() },
            ]
        );
    }

    #[test]
    fn instant_chain_respects_the_concurrency_limit_between_instant_completions() {
        // limit=1 with a two-node instant chain: a starts+completes (slot
        // freed), b then fits in the same tick — but a SPAWN between them
        // must still gate: trigger → spawn → notify runs spawn first and
        // holds the slot; notify waits for the agent event.
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(1, 1));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Running);
        assert!(run.step("notify").is_none(), "notify must wait while the spawn holds the only slot");
        assert!(matches!(
            t.effects.first(),
            Some(Effect::SpawnAgentNode { .. })
        ));
    }

    #[test]
    fn empty_graph_never_marks_completed() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph { version: 1, nodes: vec![], edges: vec![] },
            state: RunState::Running,
            context: CircuitContext::new(),
            steps: vec![],
        };
        advance(&mut run, &tick(9, 9));
        assert_eq!(run.state, RunState::Running, "an empty blueprint must not auto-complete");
    }

    // -- DB string round-trips ---------------------------------------------------------

#[test]
    fn run_state_db_strings_round_trip() {
        for s in [
            RunState::Pending,
            RunState::Running,
            RunState::Paused,
            RunState::Completed,
            RunState::Failed,
        ] {
            assert_eq!(RunState::from_db_str(s.as_db_str()), s);
        }
        assert_eq!(RunState::from_db_str("garbage"), RunState::Pending);
    }

    #[test]
    fn step_status_db_strings_match_the_pending_slot_vocabulary() {
        assert_eq!(StepStatus::Queued.as_db_str(), "pending_slot");
        for s in [
            StepStatus::Queued,
            StepStatus::Running,
            StepStatus::Blocked,
            StepStatus::Completed,
            StepStatus::Failed,
            StepStatus::Cancelled,
        ] {
            assert_eq!(StepStatus::from_db_str(s.as_db_str()), s);
        }
        assert_eq!(StepStatus::from_db_str("garbage"), StepStatus::Queued);
    }

    // -- milestone-2 gates (#1207) ----------------------------------------------

    /// A trigger → spawn → GATE blueprint with one Notify branch per
    /// routing outcome, wired `OnOutcome(...)`.
    fn gate_run(gate_id: &str, kind: CircuitNodeKind, branches: &[(StepOutcome, &str)]) -> RunView {
        let mut ctx = CircuitContext::new();
        ctx.with_circuit(7, "gates", 5);
        ctx.with_run(42);
        let mut nodes = vec![
            CircuitNode { id: "trigger".into(), kind: CircuitNodeKind::Manual },
            CircuitNode {
                id: "work".into(),
                kind: spawn_kind("p"),
            },
            CircuitNode { id: gate_id.to_string(), kind },
        ];
        let mut edges = vec![
            CircuitEdge { from: "trigger".into(), to: "work".into(), condition: Default::default() },
            CircuitEdge { from: "work".to_string(), to: gate_id.to_string(), condition: Default::default() },
        ];
        for (outcome, branch_id) in branches {
            nodes.push(CircuitNode {
                id: branch_id.to_string(),
                kind: CircuitNodeKind::Notify { message: branch_id.to_string() },
            });
            edges.push(CircuitEdge {
                from: gate_id.to_string(),
                to: branch_id.to_string(),
                condition: EdgeCondition::OnOutcome(*outcome),
            });
        }
        RunView {
            run_id: 42,
            graph: CircuitGraph { version: 1, nodes, edges },
            state: RunState::Pending,
            context: ctx,
            steps: vec![],
        }
    }

    /// Drive a gate_run from Pending up to the gate step existing. The
    /// gate's exact status depends on its kind (classifier/verification
    /// park Running; AutoRun completes instantly) — callers assert that.
    fn fire_to_gate(run: &mut RunView, gate_id: &str) {
        advance(run, &CircuitEvent::Triggered);
        advance(run, &tick(5, 5));
        run.attach_agent_node("work", 900);
        advance(run, &agent_finished(900, true));
        assert!(run.step(gate_id).is_some(), "gate {} must have started", gate_id);
    }

    fn classified(node_id: &str, c: Option<Classification>) -> CircuitEvent {
        CircuitEvent::TurnClassified { node_id: node_id.to_string(), classification: c }
    }

    #[test]
    fn classifier_completed_routes_only_the_on_completed_branch() {
        let mut run = gate_run(
            "classify",
            CircuitNodeKind::LlmTurnClassifier,
            &[(StepOutcome::Completed, "green-path"), (StepOutcome::Blocked, "help-path")],
        );
        fire_to_gate(&mut run, "classify");
        let t = advance(&mut run, &classified("classify", Some(Classification::Completed)));
        assert_eq!(status_of(&run, "classify"), StepStatus::Completed);
        assert_eq!(run.step("classify").unwrap().outcome, Some(StepOutcome::Completed));
        assert_eq!(status_of(&run, "green-path"), StepStatus::Completed);
        assert!(run.step("help-path").is_none(), "blocked branch must not traverse");
        assert!(t.effects.iter().any(|e| matches!(e, Effect::Notify { message } if message == "green-path")));
    }

    #[test]
    fn classifier_blocked_routes_the_help_branch() {
        let mut run = gate_run(
            "classify",
            CircuitNodeKind::LlmTurnClassifier,
            &[(StepOutcome::Completed, "green-path"), (StepOutcome::Blocked, "help-path")],
        );
        fire_to_gate(&mut run, "classify");
        advance(&mut run, &classified("classify", Some(Classification::Blocked)));
        assert_eq!(run.step("classify").unwrap().outcome, Some(StepOutcome::Blocked));
        assert_eq!(status_of(&run, "help-path"), StepStatus::Completed);
        assert!(run.step("green-path").is_none());
    }

    #[test]
    fn classifier_working_and_unparseable_route_the_working_branch() {
        for classification in [Some(Classification::Working), None] {
            let mut run = gate_run(
                "classify",
                CircuitNodeKind::LlmTurnClassifier,
                &[(StepOutcome::Working, "keep-going"), (StepOutcome::Completed, "done-path")],
            );
            fire_to_gate(&mut run, "classify");
            advance(&mut run, &classified("classify", classification));
            assert_eq!(run.step("classify").unwrap().outcome, Some(StepOutcome::Working));
            assert_eq!(status_of(&run, "keep-going"), StepStatus::Completed);
            assert!(run.step("done-path").is_none());
        }
    }

    #[test]
    fn turn_classified_for_unknown_or_non_running_steps_is_a_no_op() {
        let mut run = gate_run(
            "classify",
            CircuitNodeKind::LlmTurnClassifier,
            &[(StepOutcome::Completed, "done-path")],
        );
        let before = run.clone();
        advance(
            &mut run,
            &CircuitEvent::TurnClassified { node_id: "nowhere".to_string(), classification: Some(Classification::Completed) },
        );
        assert_eq!(run, before);
    }

    #[test]
    fn verification_gate_routes_green_and_red() {
        for (green, expected, hit, miss) in [
            (true, StepOutcome::Green, "pass", "fail"),
            (false, StepOutcome::Red, "fail", "pass"),
        ] {
            let mut run = gate_run(
                "verify",
                CircuitNodeKind::DeterministicVerification { command: "cargo test".into() },
                &[(StepOutcome::Green, "pass"), (StepOutcome::Red, "fail")],
            );
            fire_to_gate(&mut run, "verify");
            advance(
                &mut run,
                &CircuitEvent::VerificationResult { node_id: "verify".to_string(), green },
            );
            assert_eq!(run.step("verify").unwrap().outcome, Some(expected));
            assert_eq!(status_of(&run, hit), StepStatus::Completed);
            assert!(run.step(miss).is_none());
        }
    }

    #[test]
    fn collaborator_check_autorun_passes_through_untouched() {
        let mut run = gate_run(
            "gate",
            CircuitNodeKind::CollaboratorCheck { require_approval: false },
            &[(StepOutcome::Completed, "after")],
        );
        fire_to_gate(&mut run, "gate");
        // AutoRun never parks: it cascaded straight through.
        assert_eq!(status_of(&run, "gate"), StepStatus::Completed);
        assert_eq!(
            run.state,
            RunState::Running,
            "the successor waits for the next tick's capacity pass"
        );
        advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "after"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
    }

    #[test]
    fn require_approval_parks_blocked_then_approve_cascades() {
        let mut run = gate_run(
            "gate",
            CircuitNodeKind::CollaboratorCheck { require_approval: true },
            &[(StepOutcome::Completed, "after")],
        );
        fire_to_gate(&mut run, "gate");
        assert_eq!(status_of(&run, "gate"), StepStatus::Blocked);
        assert_eq!(run.state, RunState::Running, "the run parks on the badge, not failed");
        assert!(
            run.steps.iter().all(|s| s.status != StepStatus::Completed || s.node_id != "after"),
            "nothing may advance past an unapproved gate"
        );

        let t = advance(
            &mut run,
            &CircuitEvent::CollaboratorApproved { node_id: "gate".to_string() },
        );
        assert_eq!(status_of(&run, "gate"), StepStatus::Completed);
        assert_eq!(status_of(&run, "after"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
        assert!(t.run_state_changed);
    }

    #[test]
    fn approving_a_non_blocked_step_is_a_no_op() {
        let mut run = gate_run(
            "gate",
            CircuitNodeKind::CollaboratorCheck { require_approval: false },
            &[],
        );
        fire_to_gate(&mut run, "gate");
        let before = run.clone();
        advance(&mut run, &CircuitEvent::CollaboratorApproved { node_id: "gate".to_string() });
        advance(&mut run, &CircuitEvent::CollaboratorApproved { node_id: "elsewhere".to_string() });
        assert_eq!(run, before, "approvals only act on Blocked collaborator gates");
    }

    // -- pause / resume (#1207) ---------------------------------------------------

    #[test]
    fn pause_halts_advancement_while_the_current_step_finishes() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);

        let t = advance(&mut run, &CircuitEvent::Paused);
        assert!(t.run_state_changed);
        assert_eq!(run.state, RunState::Paused);

        // Ticks do nothing while paused.
        let again = advance(&mut run, &tick(9, 9));
        assert!(again.is_empty());

        // The current step may finish — but nothing cascades.
        run.attach_agent_node("spawn", 900);
        advance(&mut run, &agent_finished(900, true));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Completed);
        assert!(run.step("notify").is_none(), "paused runs must not advance");
        assert_eq!(run.state, RunState::Paused);
    }

    #[test]
    fn resume_continues_exactly_where_the_pause_stopped() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);
        advance(&mut run, &CircuitEvent::Paused);
        advance(&mut run, &agent_finished(900, true));

        let t = advance(&mut run, &CircuitEvent::Resumed);
        assert!(t.run_state_changed);
        assert_eq!(run.state, RunState::Running);

        // The finished spawn's successor picks up on the next tick.
        advance(&mut run, &tick(1, 1));
        assert_eq!(status_of(&run, "notify"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
    }

    #[test]
    fn pause_and_resume_are_idempotent() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &CircuitEvent::Paused);
        let again = advance(&mut run, &CircuitEvent::Paused);
        assert!(!again.run_state_changed);
        advance(&mut run, &CircuitEvent::Resumed);
        let again = advance(&mut run, &CircuitEvent::Resumed);
        assert!(!again.run_state_changed);
    }

    #[test]
    fn approvals_do_not_fire_while_paused() {
        let mut run = gate_run(
            "gate",
            CircuitNodeKind::CollaboratorCheck { require_approval: true },
            &[(StepOutcome::Completed, "after")],
        );
        fire_to_gate(&mut run, "gate");
        advance(&mut run, &CircuitEvent::Paused);
        let before = run.clone();
        advance(&mut run, &CircuitEvent::CollaboratorApproved { node_id: "gate".to_string() });
        assert_eq!(run, before, "a paused run must not consume approvals");
        advance(&mut run, &CircuitEvent::Resumed);
        advance(&mut run, &CircuitEvent::CollaboratorApproved { node_id: "gate".to_string() });
        assert_eq!(status_of(&run, "after"), StepStatus::Completed);
    }

    // -- retry limits (#1207) -------------------------------------------------------

    /// trigger → work →(Failed)→ retry →(Always)→ work (loop-back).
    fn retry_run(max_retries: i32) -> RunView {
        RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode {
                        id: "work".into(),
                        kind: spawn_kind("p"),
                    },
                    CircuitNode { id: "retry".into(), kind: CircuitNodeKind::RetryLimit { max_retries } },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "work".into(), condition: Default::default() },
                    CircuitEdge {
                        from: "work".into(),
                        to: "retry".into(),
                        condition: EdgeCondition::OnOutcome(StepOutcome::Failed),
                    },
                    CircuitEdge { from: "retry".into(), to: "work".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        }
    }

    #[test]
    fn retry_limit_resets_the_failed_step_within_its_budget() {
        let mut run = retry_run(2);
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("work", 11);

        // First failure does NOT fail-fast: the retry gate owns it. The
        // same advance call cascades into the gate, which resets the
        // failed step for its second execution.
        let t =
            advance(&mut run, &agent_finished(11, false));
        assert_eq!(status_of(&run, "retry"), StepStatus::Completed);
        assert_eq!(status_of(&run, "work"), StepStatus::Queued);
        assert_eq!(run.step("work").unwrap().attempt, 2);
        assert_eq!(run.step("work").unwrap().outcome, None, "reset clears the stale outcome");
        assert_eq!(run.state, RunState::Running, "a wired RetryLimit suppresses fail-fast");

        // Both decisions are persisted: the failure AND the fresh attempt.
        assert!(
            t.step_writes.iter().any(|w| w.node_id == "work" && w.status == StepStatus::Failed),
            "the failure itself must be persisted"
        );
        let reset = t
            .step_writes
            .iter()
            .find(|w| w.node_id == "work" && w.fresh_attempt)
            .expect("reset must be persisted as a fresh attempt");
        assert_eq!(reset.attempt, 2);

        // Next tick re-executes the retried step.
        advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "work"), StepStatus::Running);
    }

    #[test]
    fn flaky_step_succeeding_on_retry_completes_the_run() {
        let mut run = retry_run(2);
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("work", 11);
        advance(&mut run, &agent_finished(11, false));
        advance(&mut run, &tick(5, 5)); // promote the attempt-2 execution
        assert_eq!(run.step("work").unwrap().attempt, 2);
        let t =
            advance(&mut run, &agent_finished(11, true));
        assert_eq!(status_of(&run, "work"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
        assert!(t.effects.is_empty());
    }

    #[test]
    fn exhausted_retry_budget_fails_the_run() {
        let mut run = retry_run(2);
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("work", 11);
        // Attempt 1 fails → reset to 2.
        advance(&mut run, &agent_finished(11, false));
        advance(&mut run, &tick(5, 5));
        // Attempt 2 fails → budget spent. The gate is re-armed by the
        // failure, then executes on the next pass and fails the run.
        advance(&mut run, &agent_finished(11, false));
        assert_eq!(run.state, RunState::Running, "the gate still owns the failure");
        let t = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "retry"), StepStatus::Failed);
        assert!(run.step("retry").unwrap().error.as_deref().unwrap().contains("exhausted"));
        assert_eq!(run.state, RunState::Failed);
        assert!(t.run_state_changed);
    }

    #[test]
    fn retry_limit_without_a_failed_upstream_fails_explicitly() {
        // Reached via an Always edge after SUCCESS — a wiring mistake the
        // stepper surfaces instead of silently resetting anything.
        let mut run = retry_run(3);
        run.graph.edges.retain(|e| !(e.from == "work" && e.to == "retry"));
        run.graph.edges.push(CircuitEdge {
            from: "t".into(),
            to: "retry".into(),
            condition: Default::default(),
        });
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "retry"), StepStatus::Failed);
        assert!(run.step("retry").unwrap().error.as_deref().unwrap().contains("without a failed upstream"));
        assert_eq!(run.state, RunState::Failed);
        let _ = t;
    }

    // -- Issue #1357: Multi-agent targeted execution, Blackboard & Loop Retention --

    #[test]
    fn targeted_multi_agent_injection_targets_specified_node() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode { id: "agent_a".into(), kind: spawn_kind("pa") },
                    CircuitNode { id: "agent_b".into(), kind: spawn_kind("pb") },
                    CircuitNode {
                        id: "inject_a".into(),
                        kind: CircuitNodeKind::InjectPty {
                            prompt: "msg to A".into(),
                            target_node_id: Some("agent_a".into()),
                        },
                    },
                    CircuitNode {
                        id: "inject_b".into(),
                        kind: CircuitNodeKind::InjectPty {
                            prompt: "msg to B".into(),
                            target_node_id: Some("agent_b".into()),
                        },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "agent_a".into(), condition: Default::default() },
                    CircuitEdge { from: "t".into(), to: "agent_b".into(), condition: Default::default() },
                    CircuitEdge { from: "agent_a".into(), to: "inject_a".into(), condition: Default::default() },
                    CircuitEdge { from: "agent_b".into(), to: "inject_b".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };

        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("agent_a", 101);
        run.attach_agent_node("agent_b", 202);

        // Finish agent_a
        advance(&mut run, &agent_finished(101, true));
        assert_eq!(status_of(&run, "inject_a"), StepStatus::Running);

        // Fire ready for inject_a
        let t_a = advance(&mut run, &CircuitEvent::AgentReady { node_id: "inject_a".into() });
        assert_eq!(
            t_a.effects,
            vec![Effect::InjectPty {
                node_id: "inject_a".into(),
                prompt: "msg to A".into(),
                target_node_id: Some("agent_a".into()),
            }]
        );
        assert_eq!(run.resolve_target_agent("inject_a", Some("agent_a")), Some(101));

        // Finish agent_b
        advance(&mut run, &agent_finished(202, true));
        assert_eq!(status_of(&run, "inject_b"), StepStatus::Running);

        let t_b = advance(&mut run, &CircuitEvent::AgentReady { node_id: "inject_b".into() });
        assert_eq!(
            t_b.effects,
            vec![Effect::InjectPty {
                node_id: "inject_b".into(),
                prompt: "msg to B".into(),
                target_node_id: Some("agent_b".into()),
            }]
        );
        assert_eq!(run.resolve_target_agent("inject_b", Some("agent_b")), Some(202));
    }

    #[test]
    fn lineage_target_agent_resolution_finds_upstream_spawn_branch() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode { id: "spawn_branch_1".into(), kind: spawn_kind("p1") },
                    CircuitNode { id: "spawn_branch_2".into(), kind: spawn_kind("p2") },
                    CircuitNode {
                        id: "inject_branch_1".into(),
                        kind: CircuitNodeKind::InjectPty {
                            prompt: "lineage 1".into(),
                            target_node_id: None,
                        },
                    },
                    CircuitNode {
                        id: "inject_branch_2".into(),
                        kind: CircuitNodeKind::InjectPty {
                            prompt: "lineage 2".into(),
                            target_node_id: None,
                        },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "spawn_branch_1".into(), condition: Default::default() },
                    CircuitEdge { from: "t".into(), to: "spawn_branch_2".into(), condition: Default::default() },
                    CircuitEdge { from: "spawn_branch_1".into(), to: "inject_branch_1".into(), condition: Default::default() },
                    CircuitEdge { from: "spawn_branch_2".into(), to: "inject_branch_2".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };

        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("spawn_branch_1", 1001);
        run.attach_agent_node("spawn_branch_2", 2002);

        // Target agent for inject_branch_1 traverses upstream and finds spawn_branch_1 (1001)
        assert_eq!(run.resolve_target_agent("inject_branch_1", None), Some(1001));
        // Target agent for inject_branch_2 traverses upstream and finds spawn_branch_2 (2002)
        assert_eq!(run.resolve_target_agent("inject_branch_2", None), Some(2002));
    }

    #[test]
    fn node_output_blackboard_captures_agent_output_and_status_in_context() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode { id: "worker".into(), kind: spawn_kind("do task") },
                    CircuitNode {
                        id: "notify_output".into(),
                        kind: CircuitNodeKind::Notify {
                            message: "Worker finished with status '{{ node.worker.status }}' and output: '{{ node.worker.output }}'".into(),
                        },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "worker".into(), condition: Default::default() },
                    CircuitEdge { from: "worker".into(), to: "notify_output".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };

        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("worker", 777);

        // Agent finishes with captured tail text
        let t = advance(
            &mut run,
            &CircuitEvent::AgentFinished {
                agent_node_id: 777,
                success: true,
                output: Some("All 15 tests passed with 0 errors".into()),
            },
        );

        assert_eq!(run.context.get("node.worker.output"), Some("All 15 tests passed with 0 errors"));
        assert_eq!(run.context.get("node.worker.status"), Some("completed"));

        assert_eq!(
            t.effects,
            vec![Effect::Notify {
                message: "Worker finished with status 'completed' and output: 'All 15 tests passed with 0 errors'".into(),
            }]
        );
    }

    #[test]
    fn multi_iteration_feedback_loop_with_retry_limit_retains_agent_and_steps() {
        // Blueprint: trigger -> implementer -> reviewer -> verify -> (Red) -> retry_gate -> implementer
        //                                                         -> (Green) -> success
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode { id: "implementer".into(), kind: spawn_kind("impl") },
                    CircuitNode { id: "reviewer".into(), kind: spawn_kind("review") },
                    CircuitNode {
                        id: "verify".into(),
                        kind: CircuitNodeKind::DeterministicVerification { command: "cargo check".into() },
                    },
                    CircuitNode {
                        id: "retry_gate".into(),
                        kind: CircuitNodeKind::RetryLimit { max_retries: 3 },
                    },
                    CircuitNode {
                        id: "success".into(),
                        kind: CircuitNodeKind::Notify { message: "all good".into() },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "implementer".into(), condition: Default::default() },
                    CircuitEdge { from: "implementer".into(), to: "reviewer".into(), condition: Default::default() },
                    CircuitEdge { from: "reviewer".into(), to: "verify".into(), condition: Default::default() },
                    CircuitEdge {
                        from: "verify".into(),
                        to: "retry_gate".into(),
                        condition: EdgeCondition::OnOutcome(StepOutcome::Red),
                    },
                    CircuitEdge {
                        from: "verify".into(),
                        to: "success".into(),
                        condition: EdgeCondition::OnOutcome(StepOutcome::Green),
                    },
                    CircuitEdge { from: "retry_gate".into(), to: "implementer".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };

        // --- Iteration 1 ---
        advance(&mut run, &CircuitEvent::Triggered);
        let t1 = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "implementer"), StepStatus::Running);
        assert_eq!(t1.effects, vec![Effect::SpawnAgentNode { node_id: "implementer".into() }]);
        run.attach_agent_node("implementer", 101);

        // Implementer finishes iteration 1
        advance(&mut run, &agent_finished(101, true));
        assert_eq!(status_of(&run, "implementer"), StepStatus::Completed);

        // Reviewer starts iteration 1
        let t_rev1 = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "reviewer"), StepStatus::Running);
        assert_eq!(t_rev1.effects, vec![Effect::SpawnAgentNode { node_id: "reviewer".into() }]);
        run.attach_agent_node("reviewer", 202);

        // Reviewer finishes iteration 1
        advance(&mut run, &agent_finished(202, true));
        assert_eq!(status_of(&run, "reviewer"), StepStatus::Completed);

        // Verify starts and returns Red (fails)
        advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "verify"), StepStatus::Running);
        let _t_v1 = advance(&mut run, &CircuitEvent::VerificationResult { node_id: "verify".into(), green: false });
        assert_eq!(run.step("verify").unwrap().outcome, Some(StepOutcome::Red));

        // Retry limit should trigger and reset implementer for attempt 2
        assert_eq!(status_of(&run, "retry_gate"), StepStatus::Completed);
        assert_eq!(status_of(&run, "implementer"), StepStatus::Queued);
        assert_eq!(run.step("implementer").unwrap().attempt, 2);
        // Ensure implementer kept its existing agent_node_id (no leaking or creating new agents!)
        assert_eq!(run.step("implementer").unwrap().agent_node_id, Some(101));

        // --- Iteration 2 ---
        // Promoting implementer attempt 2
        let t2 = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "implementer"), StepStatus::Running);
        // Emits SpawnAgentNode so worker submits prompt to existing agent or respawns in worktree!
        assert_eq!(t2.effects, vec![Effect::SpawnAgentNode { node_id: "implementer".into() }]);
        assert_eq!(run.step("implementer").unwrap().agent_node_id, Some(101));

        // Implementer finishes iteration 2
        advance(&mut run, &agent_finished(101, true));
        assert_eq!(status_of(&run, "implementer"), StepStatus::Completed);
        assert_eq!(run.step("implementer").unwrap().attempt, 2);

        // Reviewer re-promotes for iteration 2
        let t_rev2 = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "reviewer"), StepStatus::Running);
        assert_eq!(run.step("reviewer").unwrap().attempt, 2);
        assert_eq!(run.step("reviewer").unwrap().agent_node_id, Some(202));
        assert_eq!(t_rev2.effects, vec![Effect::SpawnAgentNode { node_id: "reviewer".into() }]);

        // Reviewer finishes iteration 2
        advance(&mut run, &agent_finished(202, true));
        assert_eq!(status_of(&run, "reviewer"), StepStatus::Completed);

        // Verify runs again (attempt 2) and returns Green (passes!)
        advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "verify"), StepStatus::Running);
        let _t_v2 = advance(&mut run, &CircuitEvent::VerificationResult { node_id: "verify".into(), green: true });
        assert_eq!(run.step("verify").unwrap().outcome, Some(StepOutcome::Green));
        assert_eq!(status_of(&run, "success"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
    }

    #[test]
    fn cross_branch_target_resolution_fails_closed() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode { id: "branch_a".into(), kind: spawn_kind("pa") },
                    CircuitNode { id: "branch_b".into(), kind: spawn_kind("pb") },
                    CircuitNode {
                        id: "step_in_a".into(),
                        kind: CircuitNodeKind::SetNodeStatus {
                            status: SessionStatusKind::Completed,
                            target_node_id: Some("branch_b".into()), // cross-branch target!
                        },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "branch_a".into(), condition: Default::default() },
                    CircuitEdge { from: "t".into(), to: "branch_b".into(), condition: Default::default() },
                    CircuitEdge { from: "branch_a".into(), to: "step_in_a".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("branch_a", 101);
        run.attach_agent_node("branch_b", 202);

        // Explicit target "branch_b" is not in step_in_a's lineage -> must fail closed (None)!
        assert_eq!(run.resolve_target_agent("step_in_a", Some("branch_b")), None);

        // Advancing into step_in_a fails fast because target cannot be resolved!
        advance(&mut run, &agent_finished(101, true));
        let t = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "step_in_a"), StepStatus::Failed);
        assert!(run.step("step_in_a").unwrap().error.as_deref().unwrap().contains("no target agent node found"));
        let _ = t;
    }

    #[test]
    fn set_node_status_fails_fast_when_target_agent_is_missing() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode {
                        id: "status_step".into(),
                        kind: CircuitNodeKind::SetNodeStatus {
                            status: SessionStatusKind::Completed,
                            target_node_id: None,
                        },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "status_step".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: CircuitContext::new(),
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        let t = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "status_step"), StepStatus::Failed);
        assert!(run.step("status_step").unwrap().error.as_deref().unwrap().contains("no target agent node found"));
        let _ = t;
    }

    #[test]
    fn open_pr_result_updates_context_and_cascades_to_downstream_notify() {
        let mut run = RunView {
            run_id: 1,
            graph: CircuitGraph {
                version: 1,
                nodes: vec![
                    CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                    CircuitNode {
                        id: "open_pr".into(),
                        kind: CircuitNodeKind::GithubAction {
                            action: GithubActionKind::OpenPr,
                            label: None,
                            comment: Some("ready for review".into()),
                        },
                    },
                    CircuitNode {
                        id: "notify".into(),
                        kind: CircuitNodeKind::Notify {
                            message: "PR #{{pr.number}} created at {{pr.url}} for branch {{pr.head_ref}}".into(),
                        },
                    },
                ],
                edges: vec![
                    CircuitEdge { from: "t".into(), to: "open_pr".into(), condition: Default::default() },
                    CircuitEdge { from: "open_pr".into(), to: "notify".into(), condition: Default::default() },
                ],
            },
            state: RunState::Pending,
            context: {
                let mut ctx = CircuitContext::new();
                ctx.set("issue.number", "42");
                ctx.set("issue.title", "feat: exciting new feature");
                ctx
            },
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        let t1 = advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "open_pr"), StepStatus::Running);
        assert_eq!(
            t1.effects,
            vec![Effect::CallGithub {
                node_id: "open_pr".into(),
                action: GithubActionKind::OpenPr,
                label: None,
                comment: Some("ready for review".into()),
            }]
        );

        // Worker emits GithubActionResult with PR metadata
        let t2 = advance(&mut run, &CircuitEvent::GithubActionResult {
            node_id: "open_pr".into(),
            success: true,
            pr_number: Some(1361),
            pr_url: Some("https://github.com/owner/repo/pull/1361".into()),
            pr_head_ref: Some("buildmesh-auto/issue-42".into()),
            pr_title: Some("feat: exciting new feature".into()),
            error: None,
        });

        assert_eq!(status_of(&run, "open_pr"), StepStatus::Completed);
        assert_eq!(run.context.get("pr.number"), Some("1361"));
        assert_eq!(run.context.get("pr.url"), Some("https://github.com/owner/repo/pull/1361"));
        assert_eq!(run.context.get("pr.head_ref"), Some("buildmesh-auto/issue-42"));

        // Cascaded Notify resolves immediately against the populated PR context!
        assert_eq!(status_of(&run, "notify"), StepStatus::Completed);
        assert_eq!(
            t2.effects,
            vec![Effect::Notify {
                message: "PR #1361 created at https://github.com/owner/repo/pull/1361 for branch buildmesh-auto/issue-42".into(),
            }]
        );
        assert_eq!(run.state, RunState::Completed);
    }
}
