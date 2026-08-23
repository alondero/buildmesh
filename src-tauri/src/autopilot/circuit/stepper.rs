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
//! - Kinds the milestone-1 engine deliberately does not execute (gates,
//!   GitHub actions) fail their step with an explicit error rather than
//!   stalling the run forever.
//! - Fail-fast: any step ending `Failed`, or a piloted agent being lost
//!   (closed/archived mid-run), fails the run.

use super::context::CircuitContext;
use super::model::{
    consumes_agent_slot, is_executable, CircuitGraph, CircuitNodeKind, EdgeCondition,
    SessionStatusKind, StepOutcome,
};

// ---------------------------------------------------------------------------
// State model — the pure mirror of the three ledger tables.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Run row exists, trigger not yet processed.
    Pending,
    Running,
    Completed,
    Failed,
}

impl RunState {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "running" => Self::Running,
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
    Completed,
    Failed,
    Cancelled,
}

impl StepStatus {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Queued => "pending_slot",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "pending_slot" => Self::Queued,
            "running" => Self::Running,
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
}

impl StepView {
    fn new(node_id: &str, status: StepStatus) -> Self {
        Self {
            node_id: node_id.to_string(),
            status,
            outcome: None,
            error: None,
            agent_node_id: None,
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
    /// The run was triggered (Trigger Now). Moves Pending → Running;
    /// triggers auto-complete; actual scheduling happens on the next Tick
    /// so every start decision is capacity-checked.
    ManualTriggered,
    /// Periodic fast tick carrying current capacity.
    Tick(Capacity),
    /// The seam observed the injected prompt's target process is now live.
    AgentReady { node_id: String },
    /// The seam observed the step's piloted agent finished its turn/work
    /// (node status `awaiting_input` / `completed`). `success=false`
    /// covers the node landing in `error`.
    AgentFinished { agent_node_id: i64, success: bool },
    /// The step's piloted agent was closed/archived mid-run.
    AgentLost { agent_node_id: i64 },
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
}

impl StepWrite {
    fn insert(node_id: &str, status: StepStatus) -> Self {
        Self {
            node_id: node_id.to_string(),
            status,
            outcome: None,
            error: None,
            agent_node_id: None,
        }
    }
}

/// An explicit action the impure seam executes after committing the
/// transition. Kept small on purpose — milestone 1 covers the action
/// subset; gate/GitHub effects join in later milestones.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    SpawnAgentNode { node_id: String },
    InjectPty { node_id: String, prompt: String },
    SetNodeStatus { node_id: String, status: String },
    Notify { message: String },
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
        CircuitEvent::ManualTriggered => {
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
            let prompt: Option<String> = match run.graph.node(node_id) {
                Some(n) => match &n.kind {
                    CircuitNodeKind::InjectPty { prompt } => Some(prompt.clone()),
                    _ => None,
                },
                None => None,
            };
            let is_running_inject =
                matches!(run.step(node_id), Some(s) if s.status == StepStatus::Running);
            if let Some(prompt) = prompt {
                if is_running_inject {
                    t.effects.push(Effect::InjectPty {
                        node_id: node_id.clone(),
                        prompt: run.context.resolve(&prompt),
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
        } => {
            let bound: Option<String> = run
                .steps
                .iter()
                .find(|s| s.status == StepStatus::Running && s.agent_node_id == Some(*agent_node_id))
                .map(|s| s.node_id.clone());
            if let Some(step_node) = bound {
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
    }
    t
}

/// Mark a step (creating it if absent) with a new status. Terminal
/// statuses stamp the matching outcome.
fn set_step(run: &mut RunView, t: &mut Transition, node_id: &str, status: StepStatus) {
    let changed = match run.step_mut(node_id) {
        Some(step) => {
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
            step.outcome = status.outcome();
            run.steps.push(step);
            true
        }
    };
    if changed {
        t.step_writes.push(StepWrite::insert(node_id, status));
    }
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
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Failed,
        outcome: Some(Some(StepOutcome::Failed)),
        error: Some(error),
        agent_node_id: None,
    });
    run.state = RunState::Failed;
    t.run_state_changed = true;
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
    t.step_writes.push(StepWrite {
        node_id: node_id.to_string(),
        status: StepStatus::Cancelled,
        outcome: Some(Some(StepOutcome::Cancelled)),
        error: Some(CANCEL_REASON.to_string()),
        agent_node_id: None,
    });
    run.state = RunState::Failed;
    t.run_state_changed = true;
}

/// Is `node_id` eligible to schedule? All incoming edges satisfied by
/// terminal parent steps with matching conditions — except
/// `AnyCompleted`, which satisfies on any completed parent.
fn is_eligible(run: &RunView, node_id: &str) -> bool {
    let incoming = run.graph.incoming(node_id);
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
            let needs_agent_slot = consumes_agent_slot(&kind);
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
/// blueprint order.
fn collect_eligible(run: &RunView) -> Vec<(String, CircuitNodeKind)> {
    run.graph
        .nodes
        .iter()
        .filter(|n| run.step(&n.id).is_none() && is_eligible(run, &n.id))
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
    let needs_agent_slot = consumes_agent_slot(kind);
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
        CircuitNodeKind::SetNodeStatus { status } => {
            let db_status = match status {
                SessionStatusKind::Running => "running",
                SessionStatusKind::Idle => "idle",
                SessionStatusKind::Completed => "completed",
            };
            t.effects.push(Effect::SetNodeStatus {
                node_id: node_id.to_string(),
                status: db_status.to_string(),
            });
            set_step(run, t, node_id, StepStatus::Completed);
        }
        CircuitNodeKind::InjectPty { .. }
            // Wait for AgentReady — the spawn's async stage-2 must land
            // first. If nothing was ever spawned, fail fast instead of
            // waiting forever.
            if run.latest_agent_node_id().is_none() =>
        {
            fail_step(
                run,
                t,
                node_id,
                "no agent node was spawned earlier in this run to inject into".to_string(),
            );
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
        // Triggers never normally reach here (auto-completed at trigger
        // time), but a re-tick racing the trigger write must not wedge.
        CircuitNodeKind::Manual
        | CircuitNodeKind::Interval { .. }
        | CircuitNodeKind::GithubIssueLabel { .. }
        | CircuitNodeKind::GithubPullRequestLabel { .. } => {
            set_step(run, t, node_id, StepStatus::Completed);
        }
        // Gates / GitHub actions are filtered by `is_executable` before
        // this match; unreachable today, kept exhaustive for when the
        // later milestones add their effects.
        _ => {}
    }
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

/// Terminal check: the run completes when every blueprint node has a
/// terminal step (and the blueprint is non-empty). Cancelled steps flip
/// the run Failed instead of Completed.
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
    let all_terminal = !run.graph.nodes.is_empty()
        && run
            .graph
            .nodes
            .iter()
            .all(|n| run.step(&n.id).map(|s| s.status.is_terminal()).unwrap_or(false));
    if all_terminal {
        run.state = RunState::Completed;
        t.run_state_changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot::circuit::model::{CircuitEdge, CircuitNode};

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
                        kind: CircuitNodeKind::SpawnAgentNode { prompt: "fix it".into(), name: None },
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
        let t = advance(&mut run, &CircuitEvent::ManualTriggered);
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        let second = advance(&mut run, &CircuitEvent::ManualTriggered);
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
            advance(&mut run, &CircuitEvent::ManualTriggered);
            assert_eq!(status_of(&run, "trigger"), StepStatus::Completed);
        }
    }

    // -- scheduling & capacity --------------------------------------------------

    #[test]
    fn first_tick_schedules_spawn_when_both_capacities_allow() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::ManualTriggered);
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        let t = advance(&mut run, &tick(2, 0));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Queued);
        assert!(t.effects.is_empty(), "queued steps emit no spawn effect");
    }

    #[test]
    fn spawn_queues_when_circuit_concurrency_is_exhausted() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::ManualTriggered);
        let t = advance(&mut run, &tick(0, 5));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Queued);
        assert!(t.effects.is_empty());
    }

    #[test]
    fn queued_step_promotes_fifo_when_a_mesh_slot_frees() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::ManualTriggered);
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(0, 3)); // circuit full → queue
        let t = advance(&mut run, &tick(1, 0)); // agent slots still gone
        assert_eq!(status_of(&run, "spawn"), StepStatus::Queued);
        assert!(t.effects.is_empty());
    }

    #[test]
    fn re_tick_with_a_running_step_is_a_no_op() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(1, 1));
        let again = advance(&mut run, &tick(1, 1));
        assert!(again.is_empty(), "re-tick must be a no-op: {:?}", again);
    }

    // -- agent lifecycle --------------------------------------------------------

    #[test]
    fn agent_finished_completes_the_bound_spawn_step_and_unblocks_successors() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);
        let t =
            advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 900, success: true });
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);
        advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 900, success: true });
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(1, 1));
        assert_eq!(status_of(&run, "spawn"), StepStatus::Running);
        run.attach_agent_node("spawn", 900);

        // The agent finishes its fresh boot turn; inject schedules but
        // must not fire until the process is observed live.
        let t =
            advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 900, success: true });
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("spawn", 900);
        let t =
            advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 900, success: false });
        assert_eq!(status_of(&run, "spawn"), StepStatus::Failed);
        assert_eq!(run.state, RunState::Failed);
        assert!(t.run_state_changed);
        assert!(run.step("notify").is_none(), "successors must not start after failure");
    }

    #[test]
    fn closing_the_piloted_node_mid_run_cancels_its_step_and_fails_the_run() {
        let mut run = linear_run();
        advance(&mut run, &CircuitEvent::ManualTriggered);
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "b"), StepStatus::Running);
        run.attach_agent_node("a", 11);
        let t =
            advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 11, success: false });
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("spawn", 900);
        let t =
            advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 900, success: true });
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(1, 1));
        let before = run.clone();
        advance(
            &mut run,
            &CircuitEvent::AgentFinished { agent_node_id: 12345, success: true },
        );
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
            kind: CircuitNodeKind::SpawnAgentNode { prompt: "fix it".into(), name: None },
        });
        run.graph.nodes.push(CircuitNode {
            id: "inject".to_string(),
            kind: CircuitNodeKind::InjectPty { prompt: "now wrap up {{circuit.name}}".to_string() },
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(3, 3));
        run.attach_agent_node("spawn", 900);
        // Spawn finished → inject schedules but must NOT fire yet (no
        // live process observed).
        advance(
            &mut run,
            &CircuitEvent::AgentFinished { agent_node_id: 900, success: true },
        );
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
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
                kind: CircuitNodeKind::InjectPty { prompt: "hi".to_string() },
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
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
                        kind: CircuitNodeKind::SpawnAgentNode { prompt: "pa".into(), name: None },
                    },
                    CircuitNode {
                        id: "b".into(),
                        kind: CircuitNodeKind::SpawnAgentNode { prompt: "pb".into(), name: None },
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(5, 5));
        assert_eq!(status_of(&run, "a"), StepStatus::Running);
        assert_eq!(status_of(&run, "b"), StepStatus::Running);
        assert!(run.step("j").is_none(), "join must wait while branches run");

        run.attach_agent_node("a", 11);
        advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 11, success: true });
        assert!(run.step("j").is_none(), "all_completed still waits for b");

        run.attach_agent_node("b", 12);
        let t =
            advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 12, success: true });
        assert_eq!(status_of(&run, "j"), StepStatus::Completed);
        assert_eq!(run.state, RunState::Completed);
        assert!(t.run_state_changed);
    }

    #[test]
    fn any_completed_join_executes_when_one_branch_finishes() {
        let mut run = fan_out_run(CircuitNodeKind::AnyCompleted);
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("a", 11);
        run.attach_agent_node("b", 12);
        advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 11, success: true });
        assert_eq!(status_of(&run, "j"), StepStatus::Completed);

        // b still runs — the join fired but the run can't be terminal
        // while a step is live.
        assert_eq!(run.state, RunState::Running);
        advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 12, success: true });
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
                        kind: CircuitNodeKind::SpawnAgentNode { prompt: "p".into(), name: None },
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(1, 1));
        run.attach_agent_node("work", 5);
        advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 5, success: false });
        assert_eq!(run.state, RunState::Failed);
        assert!(
            run.step("on-green").is_none(),
            "an OnOutcome(Completed) edge must not traverse on failure"
        );
    }

    // -- unsupported kinds -----------------------------------------------------------

    #[test]
    fn gate_nodes_fail_explicitly_instead_of_stalling() {
        let mut run = fan_out_run(CircuitNodeKind::LlmTurnClassifier);
        run.graph.edges.retain(|e| e.to != "j" && e.from != "b");
        run.graph.nodes.retain(|n| n.id != "b");
        run.graph.edges.push(CircuitEdge {
            from: "a".into(),
            to: "j".into(),
            condition: Default::default(),
        });
        advance(&mut run, &CircuitEvent::ManualTriggered);
        advance(&mut run, &tick(5, 5));
        run.attach_agent_node("a", 11);
        let t =
            advance(&mut run, &CircuitEvent::AgentFinished { agent_node_id: 11, success: true });
        assert_eq!(status_of(&run, "j"), StepStatus::Failed);
        let w = t.step_writes.iter().find(|w| w.node_id == "j").unwrap();
        assert!(
            w.error
                .as_deref()
                .unwrap_or("")
                .contains("not executed until a later milestone"),
            "error must say why: {:?}",
            w.error
        );
        assert_eq!(run.state, RunState::Failed);
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
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
        advance(&mut run, &CircuitEvent::ManualTriggered);
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
        for s in [RunState::Pending, RunState::Running, RunState::Completed, RunState::Failed] {
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
            StepStatus::Completed,
            StepStatus::Failed,
            StepStatus::Cancelled,
        ] {
            assert_eq!(StepStatus::from_db_str(s.as_db_str()), s);
        }
        assert_eq!(StepStatus::from_db_str("garbage"), StepStatus::Queued);
    }
}
