//! Circuit graph blueprint AST (issue #1206, slice 1 of the Autopilot
//! Circuits spec #1205).
//!
//! A [`CircuitGraph`] is the serialisable blueprint of one Autopilot
//! Circuit: a small set of typed nodes ([`CircuitNodeKind`]) wired by
//! conditional edges ([`CircuitEdge`]). The blueprint persists as the
//! `graph_json` TEXT column on `autopilot_circuits`; there is no
//! per-node-kind migration — the AST evolves inside the JSON.
//!
//! Terminology (spec §Domain model): **Circuit** = the blueprint,
//! **Circuit Run** = one execution instance, **Circuit Step** = per-node
//! execution state within a run. "Node" is overloaded in Buildmesh prose
//! — here a *circuit node* is a graph vertex; an *agent node* is the
//! mesh's spawned session. Doc comments qualify which is meant.
//!
//! Milestone 1 scope: every node kind parses and round-trips, but the
//! engine only *executes* the Manual trigger plus the action/join
//! subset. The Interval/GitHub trigger kinds auto-complete when a run
//! they sit in is fired by hand (the user is the trigger), and gain
//! their own firing machinery in later milestones; gate kinds fail
//! their step with an explicit "not supported until later milestone"
//! error when reached (pinned in `stepper::tests`) rather than silently
//! stalling.

use serde::{Deserialize, Serialize};

/// Current blueprint AST version (issue #1356 for the v1→v2 bump; issue
/// #1219 added `timeout_seconds` for the v2→v3 bump). Older `graph_json`
/// still parses: new optional fields default to `None`. Writers emit
/// this version so a save upgrades the stored blueprint.
pub const CIRCUIT_GRAPH_VERSION: i32 = 3;

/// Prefix the stepper writes on every step it cancels when a run fails or
/// is cancelled. The sole source of truth for "is this a sibling sweep
/// note?" callers (Rust recovery module, frontend `runFailureStep`).
/// Keep the literal in lockstep with `runStepPresentation.ts`.
pub const CIRCUIT_SWEEP_NOTE_PREFIX: &str = "Cancelled because the circuit run";

/// The exact string the stepper writes for the failure sweep.
pub const CIRCUIT_SWEEP_NOTE_FAILED: &str = "Cancelled because the circuit run failed.";

/// Maximum `SpawnAgentNode.timeout_seconds` the validator accepts
/// (#1219 review). One week gives every realistic circuit room to wait
/// (a flaky CI run, a long-running PR review) while bounding the
/// user-typable range so a typo like `999999999` doesn't ship a value
/// the orchestrator's eventual watchdog (#1219 follow-up) can't reason
/// about. The inspector mirrors the same upper bound on the TS side
/// (see `InspectorPanel.tsx::Step timeout` clamp) so the contract is
/// enforced in one place per language and the error is field-named.
pub const MAX_STEP_TIMEOUT_SECONDS: u32 = 604_800; // 7 days

/// Server-owned circuit blueprints. Keeping the discriminator in the graph
/// AST means runtime policy does not have to infer a blueprint from an
/// author-editable prompt or node topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitBlueprintKind.ts")]
#[serde(rename_all = "snake_case")]
pub enum CircuitBlueprintKind {
    /// The small authorable trigger -> spawn -> inject -> notify graph.
    WalkingSkeleton,
    /// Issue-driven Autopilot with an independent PR reviewer and feedback
    /// turn on the implementation agent.
    IssueDrivenAutopilotReview,
}

/// The trigger vocabulary of circuit creation (issue #1208). Lives in
/// the domain model so the AST does not import the IPC adapter (issue #1660).
/// Generated to `CircuitTriggerKind.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitTriggerKind.ts")]
#[serde(rename_all = "snake_case")]
pub enum CircuitTriggerKind {
    /// Fire-by-hand only (Trigger Now).
    Manual,
    /// Fire on a fixed cadence (`interval_seconds`, cooldown-paced).
    Interval,
    /// Fire when an open issue gains `trigger_label`.
    GithubIssueLabel,
    /// Fire when an open PR gains `trigger_label`.
    GithubPrLabel,
}

impl CircuitBlueprintKind {
    /// The trigger vocabulary this blueprint accepts. The IPC boundary
    /// (`commands::circuit::create_circuit`) consults this; the domain
    /// model owns the truth so internal services, the background worker,
    /// and a future CLI all share the same restriction.
    pub const fn allowed_triggers(self) -> &'static [CircuitTriggerKind] {
        use CircuitTriggerKind as T;
        match self {
            // Spec #1205's "Issue-Driven PR Flow" and "Continuous Looping
            // Pacer" presets share this skeleton under different trigger
            // roots. The shipped walking skeleton is the minimal
            // foundation both build on.
            Self::WalkingSkeleton => &[
                T::Manual,
                T::Interval,
                T::GithubIssueLabel,
                T::GithubPrLabel,
            ],
            // Review blueprint is labelled-issue-driven — its `trigger.*`
            // context is the implementation agent's `{{issue.prefill}}`
            // first turn.
            Self::IssueDrivenAutopilotReview => &[T::GithubIssueLabel],
        }
    }

    /// Whether Trigger Now (`trigger_circuit_now`) is permitted on this
    /// blueprint. The review blueprint is labelled-issue-driven — a
    /// manual fire would mint a run with no `issue.*` context.
    pub const fn allows_manual_trigger_now(self) -> bool {
        match self {
            Self::WalkingSkeleton => true,
            Self::IssueDrivenAutopilotReview => false,
        }
    }
}

/// The full blueprint AST for one circuit.
///
// Milestone 4 (#1209): the canvas editor is the first TypeScript
// consumer of the AST, so every type here derives ts-rs and exports its
// generated `.ts` twin (`src/types/generated/CircuitGraph.ts` & co) —
// the TS side imports them rather than hand-declaring wire shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitGraph.ts")]
pub struct CircuitGraph {
    pub version: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint: Option<CircuitBlueprintKind>,
    pub nodes: Vec<CircuitNode>,
    pub edges: Vec<CircuitEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitNode.ts")]
pub struct CircuitNode {
    /// Stable within one blueprint. Referenced by [`CircuitEdge`] ends and
    /// persisted as `autopilot_circuit_run_steps.node_id`.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: CircuitNodeKind,
}

/// Every node kind the AST can express. Serde tags each variant with its
/// snake_case `type` discriminator so `graph_json` reads like the spec's
/// node vocabulary (`{"type": "manual"}`, `{"type": "spawn_agent_node",
/// "prompt": "..."}`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitNodeKind.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CircuitNodeKind {
    // ---- Triggers ----
    /// Fire-by-hand entry point (the walking skeleton's Trigger Now).
    Manual,
    /// Fixed-interval pacer. Parsed but not yet executed (later milestone).
    Interval {
        // serde_json sends `i64` as a JS number — the CLAUDE.md rule for
        // wire-level 64-bit ints (clamped to 60s–7d, fits i32).
        #[ts(as = "i32")]
        interval_seconds: i64,
    },
    /// Fire when a GitHub issue gains `label`. Not yet executed.
    GithubIssueLabel { label: String },
    /// Fire when a GitHub PR gains `label`. Not yet executed.
    GithubPullRequestLabel { label: String },

    // ---- Actions ----
    /// Spawn a new agent node into the mesh with `prompt` as the initial
    /// prompt (routed through `SpawnIntent::Loop`, so it stages as prefill).
    /// Optional harness fields (issue #1356) cascade through the same
    /// Node > Mesh > App > Native resolver later slices wire up; v1
    /// JSON without them deserialises as `None`. The v3 add
    /// (`timeout_seconds`, issue #1219) carries a per-step wall-clock
    /// budget for the orchestrator; v2 graphs default it to `None`.
    SpawnAgentNode {
        prompt: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
        #[serde(default)]
        extra_args: Option<String>,
        /// Optional per-step timeout in seconds (#1219). `None` = inherit
        /// the fixed watchdog default; `Some(0)` collapses to `None` at
        /// the resolver seam (`resolve_circuit_spawn_inputs`) so a
        /// zero-int overflow at save time can't request an instant
        /// expiry. The valid range is `1..=MAX_STEP_TIMEOUT_SECONDS`
        /// (7 days) — `validate()` rejects larger values with a
        /// field-named error. The worker's `observe_waits` emits it as
        /// the `WaitObserved.timeout_ms` the stepper enforces; the
        /// orchestrator also carries it via
        /// `ExplicitSpawnOverrides::timeout_seconds` for launch logging.
        /// Process-level enforcement (cancelling stuck spawns) is still
        /// a follow-up.
        #[serde(default)]
        #[ts(as = "Option<i32>")]
        timeout_seconds: Option<u32>,
    },
    /// Inject `prompt` into an agent node over PTY, once that process is
    /// live. `target_node_id` names an upstream `SpawnAgentNode` id for
    /// later slices; `None` keeps the v1 "latest agent in this run"
    /// behaviour. The worker still falls back to latest until that
    /// resolution lands.
    InjectPty {
        prompt: String,
        #[serde(default)]
        target_node_id: Option<String>,
    },
    /// GitHub mutation (add/remove label, comment, open PR, close issue).
    /// Parsed but not yet executed.
    GithubAction {
        action: GithubActionKind,
        // OpenPr's reconciliation policy. `None` preserves v1 graphs and
        // means create the PR when no existing PR is found.
        #[serde(default)]
        open_pr_policy: Option<OpenPrPolicy>,
        label: Option<String>,
        comment: Option<String>,
    },
    /// Set an agent node's status. `target_node_id` names an upstream
    /// `SpawnAgentNode` id for later slices; `None` keeps the v1 "latest
    /// agent in this run" behaviour. The worker still falls back to
    /// latest until that resolution lands.
    SetNodeStatus {
        status: SessionStatusKind,
        #[serde(default)]
        target_node_id: Option<String>,
    },
    /// Close an agent node and queue its worktree for cleanup. This is
    /// deliberately separate from `SetNodeStatus(Completed)`: completing a
    /// circuit step must not leave a reviewer process and its worktree alive.
    CloseAgentNode {
        #[serde(default)]
        target_node_id: Option<String>,
    },
    /// Surface a message to the user (toast / notification event).
    Notify { message: String },

    // ---- Gates ----
    /// LLM turn classification (Completed | Blocked | Working). The target
    /// is explicit for fan-out graphs; `None` preserves the nearest upstream
    /// spawn behaviour of older blueprints.
    LlmTurnClassifier {
        #[serde(default)]
        target_node_id: Option<String>,
    },
    /// Classify task completion, including a transcript turn already finished
    /// before this run was attached. `$source` binds to the triggering agent.
    /// The built-in local review preset accepts clean source turns here;
    /// its independent reviewer assesses completeness and correctness.
    AwaitAgentTurn { target_node_id: Option<String> },
    /// Distinguish reviewer approval from a completed review with findings.
    ReviewVerdict { target_node_id: Option<String> },
    /// Deterministic verification command (Green | Red). Not yet executed.
    DeterministicVerification { command: String },
    /// Collaborator approval gate. Not yet executed.
    CollaboratorCheck { require_approval: bool },
    /// Retry budget cap. Not yet executed.
    RetryLimit { max_retries: i32 },

    // ---- Joins ----
    /// Fan-in: continue when all incoming branches completed.
    AllCompleted,
    /// Fan-in: continue when any incoming branch completed.
    AnyCompleted,
}

/// The GitHub mutation vocabulary of [`CircuitNodeKind::GithubAction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "GithubActionKind.ts")]
#[serde(rename_all = "snake_case")]
pub enum GithubActionKind {
    AddLabel,
    RemoveLabel,
    PostComment,
    OpenPr,
    CloseIssue,
    /// Read-only check that the run's pull request was squash-merged on
    /// GitHub. It mutates nothing, so a result is safe to re-read. A PR that
    /// is not merged is a routed `Failed` outcome rather than a failed run, so
    /// a blueprint can leave work open and say why.
    ConfirmPrMerged,
}

/// How an OpenPr action reconciles the remote pull request.
///
/// This belongs to the graph node rather than the worker or a blueprint
/// marker: a custom graph can explicitly require its agent to create the PR,
/// while the walking skeleton can retain the create-if-missing behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "OpenPrPolicy.ts")]
#[serde(rename_all = "snake_case")]
pub enum OpenPrPolicy {
    CreateIfMissing,
    RequireExisting,
}

impl OpenPrPolicy {
    pub fn requires_existing(self) -> bool {
        matches!(self, Self::RequireExisting)
    }
}

/// The agent-node status a [`CircuitNodeKind::SetNodeStatus`] step writes.
/// Mirrors `models::SessionStatus`'s DB vocabulary; kept as a separate
/// small enum so the graph JSON stays stable if `SessionStatus` grows
/// spawn-machinery variants the graph author should not set by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "SessionStatusKind.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusKind {
    Running,
    Idle,
    Completed,
}

/// One directed wire between two circuit nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitEdge.ts")]
pub struct CircuitEdge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub condition: EdgeCondition,
}

/// When does the edge carry its parent's outcome forward?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[ts(export, export_to = "EdgeCondition.ts")]
#[serde(rename_all = "snake_case")]
pub enum EdgeCondition {
    /// Traverse regardless of how the parent step ended.
    #[default]
    Always,
    /// Traverse only when the parent step produced exactly this outcome.
    OnOutcome(StepOutcome),
}

/// Terminal step outcomes edges and the ledger route on. The DB column
/// stores the snake_case string (same discipline as `SessionStatus`).
///
/// Milestone 2 (#1207) adds the gate outcomes: an LLM turn classifier
/// routes Completed/Blocked/Working; a deterministic verification gate
/// routes Green/Red. Each is a terminal step outcome — edges pick their
/// successors with `OnOutcome(...)`, so a gate whose branches don't
/// cover an outcome simply parks that branch (the run waits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "StepOutcome.ts")]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Completed,
    Failed,
    Cancelled,
    // -- Gates (milestone 2) --
    /// LlmTurnClassifier: the agent needs a human.
    Blocked,
    /// LlmTurnClassifier: mid-task yield.
    Working,
    /// DeterministicVerification: the check exited 0.
    Green,
    /// DeterministicVerification: the check failed.
    Red,
}

impl StepOutcome {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
            Self::Working => "working",
            Self::Green => "green",
            Self::Red => "red",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "blocked" => Some(Self::Blocked),
            "working" => Some(Self::Working),
            "green" => Some(Self::Green),
            "red" => Some(Self::Red),
            _ => None,
        }
    }

    /// Is this a terminal-outcome DB string? Mirrors the ledger's
    /// `completed_at` stamping rule in `db::commit_circuit_advance`.
    pub fn is_terminal_db_str(s: &str) -> bool {
        matches!(
            s,
            "completed" | "failed" | "cancelled" | "blocked" | "working" | "green" | "red"
        )
    }
}

/// Does this kind consume one of the mesh's auto-spawned agent slots?
/// Pure helper the stepper's capacity gating calls; only
/// [`CircuitNodeKind::SpawnAgentNode`] does today.
pub fn consumes_agent_slot(kind: &CircuitNodeKind) -> bool {
    matches!(kind, CircuitNodeKind::SpawnAgentNode { .. })
}

/// A gate that structurally bounds a directed cycle so the stepper
/// cannot walk it forever. `CollaboratorCheck` only counts when it
/// actually parks for a human (`require_approval`); auto-pass is not
/// a bound. Used by [`CircuitGraph::validate`].
fn is_bounded_gate(kind: &CircuitNodeKind) -> bool {
    matches!(
        kind,
        CircuitNodeKind::RetryLimit { .. }
            | CircuitNodeKind::CollaboratorCheck {
                require_approval: true
            }
    )
}

/// Is this kind executable by the engine? Since #1207 every gate kind
/// and since #1208 (#1206's follow-ups) the GitHub actions too — the
/// whole AST vocabulary executes.
pub fn is_executable(kind: &CircuitNodeKind) -> bool {
    matches!(
        kind,
        CircuitNodeKind::Manual
            | CircuitNodeKind::Interval { .. }
            | CircuitNodeKind::GithubIssueLabel { .. }
            | CircuitNodeKind::GithubPullRequestLabel { .. }
            | CircuitNodeKind::SpawnAgentNode { .. }
            | CircuitNodeKind::InjectPty { .. }
            | CircuitNodeKind::GithubAction { .. }
            | CircuitNodeKind::SetNodeStatus { .. }
            | CircuitNodeKind::CloseAgentNode { .. }
            | CircuitNodeKind::Notify { .. }
            | CircuitNodeKind::LlmTurnClassifier { .. }
            | CircuitNodeKind::AwaitAgentTurn { .. }
            | CircuitNodeKind::ReviewVerdict { .. }
            | CircuitNodeKind::DeterministicVerification { .. }
            | CircuitNodeKind::CollaboratorCheck { .. }
            | CircuitNodeKind::RetryLimit { .. }
            | CircuitNodeKind::AllCompleted
            | CircuitNodeKind::AnyCompleted
    )
}

impl CircuitGraph {
    pub fn requires_source_agent(&self) -> bool {
        self.nodes.iter().any(|node| match &node.kind {
            CircuitNodeKind::InjectPty { target_node_id, .. }
            | CircuitNodeKind::AwaitAgentTurn { target_node_id }
            | CircuitNodeKind::LlmTurnClassifier { target_node_id }
            | CircuitNodeKind::ReviewVerdict { target_node_id } => {
                target_node_id.as_deref() == Some("$source")
            }
            _ => false,
        })
    }

    /// Parse a blueprint from its stored `graph_json`. Unknown node kinds
    /// are a hard error at the read boundary (the writer always writes
    /// what this build understands) rather than a silent skip.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let graph: Self =
            serde_json::from_str(json).map_err(|e| format!("invalid circuit graph_json: {}", e))?;

        Ok(graph)
    }

    /// Serialise to the `graph_json` column form (compact, sorted field
    /// order is serde's default struct order).
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("could not encode circuit graph: {}", e))
    }

    /// Semantic checks beyond what serde can express: no duplicate node
    /// ids, every edge endpoint resolves, no self-loops, and every
    /// directed cycle is bounded by a [`CircuitNodeKind::RetryLimit`] or
    /// a [`CircuitNodeKind::CollaboratorCheck`] with `require_approval`.
    /// Unbounded cycles are rejected — the stepper would otherwise walk
    /// them forever. Writers at trust boundaries (the canvas editor's
    /// save command) must call this after `from_json`.
    pub fn validate(&self) -> Result<(), String> {
        let mut ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id.as_str()) {
                return Err(format!("duplicate node id '{}'", node.id));
            }
            if let CircuitNodeKind::SpawnAgentNode {
                timeout_seconds: Some(t),
                ..
            } = &node.kind
            {
                if *t > MAX_STEP_TIMEOUT_SECONDS {
                    return Err(format!(
                        "SpawnAgentNode '{}' timeout_seconds={} exceeds the maximum {} \
                         (set to 0 or leave blank to inherit the orchestrator default)",
                        node.id, t, MAX_STEP_TIMEOUT_SECONDS
                    ));
                }
            }
        }
        if self.requires_source_agent()
            && self
                .roots()
                .iter()
                .any(|root| !matches!(root.kind, CircuitNodeKind::Manual))
        {
            return Err("Circuits using $source must have only Manual root triggers".into());
        }
        for edge in &self.edges {
            if !ids.contains(edge.from.as_str()) {
                return Err(format!(
                    "edge {} -> {} references unknown source node",
                    edge.from, edge.to
                ));
            }
            if !ids.contains(edge.to.as_str()) {
                return Err(format!(
                    "edge {} -> {} references unknown target node",
                    edge.from, edge.to
                ));
            }
            if edge.from == edge.to {
                return Err(format!("node '{}' connects to itself", edge.from));
            }
        }
        self.reject_unbounded_cycles()
    }

    /// Enumerate every simple directed cycle (Johnson-style: start at
    /// each node, only walk nodes at or after that start index so each
    /// cycle is reported once). Permit the cycle iff it contains a
    /// bounded gate; otherwise return an actionable error naming the
    /// nodes.
    fn reject_unbounded_cycles(&self) -> Result<(), String> {
        let n = self.nodes.len();
        if n == 0 {
            return Ok(());
        }
        let index_of: std::collections::HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (node.id.as_str(), i))
            .collect();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for edge in &self.edges {
            let from = index_of[edge.from.as_str()];
            let to = index_of[edge.to.as_str()];
            adj[from].push(to);
        }
        let kinds: Vec<&CircuitNodeKind> = self.nodes.iter().map(|node| &node.kind).collect();
        let ids: Vec<&str> = self.nodes.iter().map(|node| node.id.as_str()).collect();

        for start in 0..n {
            let mut path = Vec::new();
            let mut in_path = vec![false; n];
            Self::walk_simple_cycles(start, start, &adj, &mut path, &mut in_path, &kinds, &ids)?;
        }
        Ok(())
    }

    fn walk_simple_cycles(
        start: usize,
        current: usize,
        adj: &[Vec<usize>],
        path: &mut Vec<usize>,
        in_path: &mut [bool],
        kinds: &[&CircuitNodeKind],
        ids: &[&str],
    ) -> Result<(), String> {
        path.push(current);
        in_path[current] = true;
        for &nxt in &adj[current] {
            if nxt == start && path.len() >= 2 {
                if !path.iter().any(|&i| is_bounded_gate(kinds[i])) {
                    let mut names: Vec<&str> = path.iter().map(|&i| ids[i]).collect();
                    names.push(ids[start]);
                    return Err(format!(
                        "unbounded cycle {} — add a RetryLimit or a CollaboratorCheck with require_approval so the loop cannot run forever",
                        names.join(" → ")
                    ));
                }
            } else if nxt >= start && !in_path[nxt] {
                Self::walk_simple_cycles(start, nxt, adj, path, in_path, kinds, ids)?;
            }
        }
        in_path[current] = false;
        path.pop();
        Ok(())
    }

    /// Children of `node_id` in edge order, deduplicated (parallel edges
    /// with different conditions keep ONE child entry; the stepper checks
    /// every incoming edge's condition individually).
    pub fn children(&self, node_id: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for edge in &self.edges {
            if edge.from == node_id && !out.contains(&edge.to) {
                out.push(edge.to.clone());
            }
        }
        out
    }

    /// Find the nearest upstream agent step for an activity node.
    ///
    /// This is a graph/domain relationship, not a persistence concern. It
    /// lets the worker persist a stable presentation parent when a reviewer
    /// agent is attached, while remaining valid for user-authored graphs and
    /// multiple review branches.
    pub fn nearest_upstream_agent_step(&self, node_id: &str) -> Option<String> {
        use std::collections::{HashMap, HashSet, VecDeque};

        let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in &self.edges {
            parents
                .entry(edge.to.as_str())
                .or_default()
                .push(edge.from.as_str());
        }
        let mut queue = VecDeque::from([node_id]);
        let mut seen = HashSet::from([node_id]);
        while let Some(current) = queue.pop_front() {
            for parent in parents.get(current).into_iter().flatten() {
                if !seen.insert(parent) {
                    continue;
                }
                if self
                    .node(parent)
                    .is_some_and(|node| matches!(node.kind, CircuitNodeKind::SpawnAgentNode { .. }))
                {
                    return Some((*parent).to_string());
                }
                queue.push_back(parent);
            }
        }
        None
    }

    /// Incoming edges targeting `node_id`, in blueprint order.
    pub fn incoming(&self, node_id: &str) -> Vec<&CircuitEdge> {
        self.edges.iter().filter(|e| e.to == node_id).collect()
    }

    /// True if any ancestor of `node_id` satisfies `predicate`. Walks
    /// incoming edges; the start node itself is not tested.
    pub fn has_ancestor_matching<F>(&self, node_id: &str, predicate: F) -> bool
    where
        F: Fn(&CircuitNode) -> bool,
    {
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
        queue.push_back(node_id);
        visited.insert(node_id);
        while let Some(curr) = queue.pop_front() {
            for edge in self.incoming(curr) {
                let from = edge.from.as_str();
                if let Some(node) = self.node(from) {
                    if predicate(node) {
                        return true;
                    }
                }
                if visited.insert(from) {
                    queue.push_back(from);
                }
            }
        }
        false
    }

    /// True if `ancestor` lies upstream of `descendant` via incoming
    /// edges. The stepper's target-resolution seam relies on this for
    /// both the explicit-target validation and the implicit lineage
    /// walk when a step's `target_node_id` is `None`.
    pub fn is_ancestor(&self, descendant: &str, ancestor: &str) -> bool {
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
        queue.push_back(descendant);
        visited.insert(descendant);
        while let Some(curr) = queue.pop_front() {
            for edge in self.incoming(curr) {
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

    /// Nodes with no incoming edges — the triggers.
    pub fn roots(&self) -> Vec<&CircuitNode> {
        self.nodes
            .iter()
            .filter(|n| self.incoming(&n.id).is_empty())
            .collect()
    }

    pub fn node(&self, node_id: &str) -> Option<&CircuitNode> {
        self.nodes.iter().find(|n| n.id == node_id)
    }

    /// Whether this graph is the issue-driven review blueprint. The marker is
    /// deliberately independent of prompts and topology because both are
    /// editable in the canvas.
    pub fn is_issue_driven_autopilot_review(&self) -> bool {
        self.blueprint == Some(CircuitBlueprintKind::IssueDrivenAutopilotReview)
    }

    /// The blueprint governing this graph's runtime policy. When the explicit
    /// marker is absent — graphs persisted before the discriminator existed —
    /// fall back to the legacy-shape heuristic, mirroring the TS `parseGraph`
    /// upgrade path, so a legacy review circuit is never mistaken for a
    /// walking skeleton.
    pub fn effective_blueprint(&self) -> CircuitBlueprintKind {
        self.blueprint.unwrap_or_else(|| {
            if self.has_legacy_issue_review_shape() {
                CircuitBlueprintKind::IssueDrivenAutopilotReview
            } else {
                CircuitBlueprintKind::WalkingSkeleton
            }
        })
    }

    /// Recognize graph_json written before `blueprint` was added. This is a
    /// one-time compatibility migration, not the runtime blueprint policy;
    /// notably it does not compare any prompt text.
    pub(crate) fn has_legacy_issue_review_shape(&self) -> bool {
        matches!(
            self.node("trigger").map(|n| &n.kind),
            Some(CircuitNodeKind::GithubIssueLabel { .. })
        ) && matches!(
            self.node("review_prompt").map(|n| &n.kind),
            Some(CircuitNodeKind::InjectPty { target_node_id, .. })
                if target_node_id.as_deref() == Some("reviewer")
        ) && matches!(
            self.node("close_reviewer").map(|n| &n.kind),
            Some(CircuitNodeKind::CloseAgentNode { target_node_id })
                if target_node_id.as_deref() == Some("reviewer")
        ) && matches!(
            self.node("reviewer").map(|n| &n.kind),
            Some(CircuitNodeKind::SpawnAgentNode { .. })
        )
    }

    /// Same review control flow as `template`, allowing edits a reviewer may
    /// legitimately make: prompts, messages, launch settings and round limits.
    /// Each template edge must appear exactly once and no other edge may exist.
    pub(crate) fn has_review_topology_of(&self, template: &CircuitGraph) -> bool {
        if self.nodes.len() != template.nodes.len() || self.edges.len() != template.edges.len() {
            return false;
        }
        let nodes_match = template.nodes.iter().all(|expected| {
            let Some(actual) = self.node(&expected.id) else {
                return false;
            };
            match (&expected.kind, &actual.kind) {
                (
                    CircuitNodeKind::SpawnAgentNode { .. },
                    CircuitNodeKind::SpawnAgentNode { .. },
                )
                | (CircuitNodeKind::Notify { .. }, CircuitNodeKind::Notify { .. }) => true,
                (
                    CircuitNodeKind::RetryLimit { .. },
                    CircuitNodeKind::RetryLimit { max_retries },
                ) => *max_retries > 0,
                (
                    CircuitNodeKind::InjectPty {
                        target_node_id: expected,
                        ..
                    },
                    CircuitNodeKind::InjectPty {
                        target_node_id: actual,
                        ..
                    },
                ) => expected == actual,
                // Older saved presets used a task classifier after feedback.
                (
                    CircuitNodeKind::AwaitAgentTurn {
                        target_node_id: expected,
                    },
                    CircuitNodeKind::LlmTurnClassifier {
                        target_node_id: actual,
                    },
                ) => expected == actual,
                (
                    CircuitNodeKind::GithubIssueLabel { .. },
                    CircuitNodeKind::GithubIssueLabel { .. },
                ) => true,
                (expected, actual) => expected == actual,
            }
        });
        nodes_match
            && template
                .edges
                .iter()
                .all(|edge| self.edges.iter().filter(|actual| *actual == edge).count() == 1)
    }
}

// ---------------------------------------------------------------------------
// Domain validation — pure, side-effect-free, owned by the model so the
// IPC boundary (Tauri command) stays a dumb router. Internal services,
// the background worker, and a future CLI all reach for the same
// `validate_circuit_request` helper; the Tauri command's job is just to
// parse arguments and persist.
// ---------------------------------------------------------------------------

/// Pure validation for a circuit creation request. Returns the
/// normalised [`CircuitTriggerKind`], the trimmed trigger label (if any),
/// and the validated interval seconds (if any). The caller builds the
/// [`CircuitGraph`] from the returned trigger kind and writes the row to
/// the DB.
///
/// Keeping this in the domain model — rather than inside the Tauri
/// command — means internal callers, the background worker, and any
/// future CLI all share the same restrictions. A review blueprint
/// cannot be constructed with a Manual trigger; the model refuses here so
/// the runtime never mints a run with no `issue.*` context.
pub fn validate_circuit_request(
    blueprint: CircuitBlueprintKind,
    trigger_kind: Option<CircuitTriggerKind>,
    trigger_label: Option<&str>,
    interval_seconds: Option<i64>,
) -> Result<ValidatedCircuitRequest, String> {
    use CircuitTriggerKind as T;

    let selected_trigger = trigger_kind.unwrap_or(T::Manual);
    if !blueprint.allowed_triggers().contains(&selected_trigger) {
        return Err(format!(
            "blueprint {:?} requires one of {:?}, got {:?}",
            blueprint,
            blueprint.allowed_triggers(),
            selected_trigger
        ));
    }

    // GitHub triggers require a non-empty label (after trim). The IPC
    // layer never has to remember this — the model is the single
    // source of truth.
    let needs_label = matches!(selected_trigger, T::GithubIssueLabel | T::GithubPrLabel);
    let trigger_label = trigger_label
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned);
    if needs_label && trigger_label.is_none() {
        return Err("GitHub triggers require a non-empty trigger label".to_string());
    }

    let interval_seconds = match selected_trigger {
        T::Interval => Some(interval_seconds.unwrap_or(300).clamp(60, 7 * 24 * 3_600)),
        _ => None,
    };

    Ok(ValidatedCircuitRequest {
        trigger_kind: selected_trigger,
        trigger_label,
        interval_seconds,
    })
}

/// Pure result of [`validate_circuit_request`]. Carries every normalised
/// field the IPC + DB write need, decoupled from the original inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCircuitRequest {
    pub trigger_kind: CircuitTriggerKind,
    pub trigger_label: Option<String>,
    pub interval_seconds: Option<i64>,
}

/// Convert a validated [`CircuitTriggerKind`]
/// into the [`CircuitNodeKind`] the graph builder expects. The model
/// has already enforced label-required-for-github inside
/// [`validate_circuit_request`], so this is infallible for the GitHub
/// variants — `unreachable!` documents the invariant.
pub fn trigger_kind_to_node_kind(req: &ValidatedCircuitRequest) -> CircuitNodeKind {
    use CircuitTriggerKind as T;
    match req.trigger_kind {
        T::Manual => CircuitNodeKind::Manual,
        T::Interval => CircuitNodeKind::Interval {
            interval_seconds: req.interval_seconds.unwrap_or(300),
        },
        T::GithubIssueLabel => CircuitNodeKind::GithubIssueLabel {
            label: req.trigger_label.clone().expect(
                "validate_circuit_request guarantees a non-empty label for GitHub triggers",
            ),
        },
        T::GithubPrLabel => CircuitNodeKind::GithubPullRequestLabel {
            label: req.trigger_label.clone().expect(
                "validate_circuit_request guarantees a non-empty label for GitHub triggers",
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_kind(prompt: &str, name: Option<&str>) -> CircuitNodeKind {
        CircuitNodeKind::SpawnAgentNode {
            prompt: prompt.into(),
            name: name.map(str::to_string),
            provider: None,
            model: None,
            effort: None,
            extra_args: None,
            timeout_seconds: None,
        }
    }

    fn inject_kind(prompt: &str) -> CircuitNodeKind {
        CircuitNodeKind::InjectPty {
            prompt: prompt.into(),
            target_node_id: None,
        }
    }

    fn set_status_kind(status: SessionStatusKind) -> CircuitNodeKind {
        CircuitNodeKind::SetNodeStatus {
            status,
            target_node_id: None,
        }
    }

    fn always(from: &str, to: &str) -> CircuitEdge {
        CircuitEdge {
            from: from.into(),
            to: to.into(),
            condition: EdgeCondition::default(),
        }
    }

    // -- serde round-trip ---------------------------------------------------

    #[test]
    fn walking_skeleton_round_trips_through_graph_json() {
        let graph = CircuitGraph::walking_skeleton("fix the flaky test");
        let json = graph.to_json().unwrap();
        let parsed = CircuitGraph::from_json(&json).unwrap();
        assert_eq!(parsed, graph);
    }

    #[test]
    fn every_node_kind_serialises_with_its_snake_case_discriminator() {
        let kinds = vec![
            CircuitNodeKind::Manual,
            CircuitNodeKind::Interval {
                interval_seconds: 300,
            },
            CircuitNodeKind::GithubIssueLabel {
                label: "buildmesh:run".into(),
            },
            CircuitNodeKind::GithubPullRequestLabel {
                label: "review-me".into(),
            },
            spawn_kind("p", Some("fix-it")),
            inject_kind("wrap up"),
            CircuitNodeKind::GithubAction {
                action: GithubActionKind::AddLabel,
                open_pr_policy: None,
                label: Some("done".into()),
                comment: None,
            },
            set_status_kind(SessionStatusKind::Completed),
            CircuitNodeKind::Notify {
                message: "hi".into(),
            },
            CircuitNodeKind::LlmTurnClassifier {
                target_node_id: None,
            },
            CircuitNodeKind::DeterministicVerification {
                command: "cargo test".into(),
            },
            CircuitNodeKind::CollaboratorCheck {
                require_approval: true,
            },
            CircuitNodeKind::RetryLimit { max_retries: 3 },
            CircuitNodeKind::AllCompleted,
            CircuitNodeKind::AnyCompleted,
        ];
        for kind in kinds {
            let node = CircuitNode {
                id: "n1".into(),
                kind,
            };
            let json = serde_json::to_string(&node).unwrap();
            assert!(
                json.contains("\"type\""),
                "every variant must tag its type: {}",
                json
            );
            let back: CircuitNode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, node);
        }
    }

    #[test]
    fn edge_condition_on_outcome_round_trips() {
        let edge = CircuitEdge {
            from: "a".into(),
            to: "b".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Failed),
        };
        let json = serde_json::to_string(&edge).unwrap();
        assert_eq!(
            CircuitGraph::from_json(&format!(
                "{{\"version\":2,\"nodes\":[],\"edges\":[{}]}}",
                json
            ))
            .unwrap()
            .edges[0]
                .condition,
            EdgeCondition::OnOutcome(StepOutcome::Failed)
        );
    }

    #[test]
    fn missing_condition_field_defaults_to_always() {
        let parsed: CircuitEdge = serde_json::from_str(r#"{"from":"a","to":"b"}"#).unwrap();
        assert_eq!(parsed.condition, EdgeCondition::Always);
    }

    #[test]
    fn unknown_node_kind_is_a_read_error_not_a_silent_skip() {
        let err = CircuitGraph::from_json(
            r#"{"version":1,"nodes":[{"id":"n","type":"time_travel"}],"edges":[]}"#,
        )
        .unwrap_err();
        assert!(err.contains("invalid circuit graph_json"), "{}", err);
    }

    // -- graph helpers ------------------------------------------------------

    fn sample_graph() -> CircuitGraph {
        CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![
                CircuitNode {
                    id: "t".into(),
                    kind: CircuitNodeKind::Manual,
                },
                CircuitNode {
                    id: "a".into(),
                    kind: CircuitNodeKind::Notify {
                        message: "x".into(),
                    },
                },
                CircuitNode {
                    id: "b".into(),
                    kind: CircuitNodeKind::AllCompleted,
                },
                CircuitNode {
                    id: "c".into(),
                    kind: CircuitNodeKind::AnyCompleted,
                },
            ],
            edges: vec![
                CircuitEdge {
                    from: "t".into(),
                    to: "a".into(),
                    condition: EdgeCondition::default(),
                },
                CircuitEdge {
                    from: "a".into(),
                    to: "b".into(),
                    condition: EdgeCondition::default(),
                },
                CircuitEdge {
                    from: "a".into(),
                    to: "c".into(),
                    condition: EdgeCondition::default(),
                },
                CircuitEdge {
                    from: "t".into(),
                    to: "c".into(),
                    condition: EdgeCondition::default(),
                },
            ],
        }
    }

    #[test]
    fn roots_are_nodes_without_incoming_edges() {
        let g = sample_graph();
        let root_ids: Vec<&str> = g.roots().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(root_ids, vec!["t"]);
    }

    #[test]
    fn children_and_incoming_walk_the_edges() {
        let g = sample_graph();
        assert_eq!(g.children("a"), vec!["b".to_string(), "c".to_string()]);
        assert_eq!(g.children("t"), vec!["a".to_string(), "c".to_string()]);
        assert_eq!(g.incoming("c").len(), 2);
        assert_eq!(g.incoming("t").len(), 0);
    }

    #[test]
    fn parallel_edges_to_one_child_dedupe_in_children() {
        let g = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![
                CircuitNode {
                    id: "a".into(),
                    kind: CircuitNodeKind::Manual,
                },
                CircuitNode {
                    id: "b".into(),
                    kind: CircuitNodeKind::Notify {
                        message: "m".into(),
                    },
                },
            ],
            edges: vec![
                CircuitEdge {
                    from: "a".into(),
                    to: "b".into(),
                    condition: EdgeCondition::OnOutcome(StepOutcome::Completed),
                },
                CircuitEdge {
                    from: "a".into(),
                    to: "b".into(),
                    condition: EdgeCondition::OnOutcome(StepOutcome::Failed),
                },
            ],
        };
        assert_eq!(g.children("a"), vec!["b".to_string()]);
        assert_eq!(g.incoming("b").len(), 2);
    }

    // -- classification helpers ---------------------------------------------

    #[test]
    fn only_spawn_consumes_an_agent_slot() {
        assert!(consumes_agent_slot(&spawn_kind("p", None)));
        assert!(!consumes_agent_slot(&inject_kind("p")));
        assert!(!consumes_agent_slot(&CircuitNodeKind::Notify {
            message: "n".into()
        }));
        assert!(!consumes_agent_slot(&CircuitNodeKind::AllCompleted));
    }

    #[test]
    fn executability_vocabulary() {
        assert!(is_executable(&CircuitNodeKind::Manual));
        assert!(is_executable(&spawn_kind("p", None)));
        assert!(is_executable(&inject_kind("p")));
        assert!(is_executable(&CircuitNodeKind::Notify {
            message: "n".into()
        }));
        // Milestone 2 (#1207): gates execute.
        assert!(is_executable(&CircuitNodeKind::LlmTurnClassifier {
            target_node_id: None
        }));
        assert!(is_executable(&CircuitNodeKind::DeterministicVerification {
            command: "cargo test".into()
        }));
        assert!(is_executable(&CircuitNodeKind::CollaboratorCheck {
            require_approval: true
        }));
        assert!(is_executable(&CircuitNodeKind::RetryLimit {
            max_retries: 3
        }));
        // Milestone 3 (issue #1208): every GitHub action executes.
        for action in [
            GithubActionKind::AddLabel,
            GithubActionKind::RemoveLabel,
            GithubActionKind::PostComment,
            GithubActionKind::OpenPr,
            GithubActionKind::CloseIssue,
            GithubActionKind::ConfirmPrMerged,
        ] {
            assert!(
                is_executable(&CircuitNodeKind::GithubAction {
                    action,
                    open_pr_policy: None,
                    label: None,
                    comment: None
                }),
                "{action:?} must be executable"
            );
        }
    }

    // -- outcome DB strings ---------------------------------------------------

    #[test]
    fn step_outcome_db_strings_round_trip() {
        for o in [
            StepOutcome::Completed,
            StepOutcome::Failed,
            StepOutcome::Cancelled,
            StepOutcome::Blocked,
            StepOutcome::Working,
            StepOutcome::Green,
            StepOutcome::Red,
        ] {
            assert_eq!(StepOutcome::from_db_str(o.as_db_str()), Some(o));
        }
        assert_eq!(StepOutcome::from_db_str("nonsense"), None);
    }

    #[test]
    fn gate_outcomes_are_terminal_in_the_ledger_vocabulary() {
        assert!(StepOutcome::is_terminal_db_str("green"));
        assert!(StepOutcome::is_terminal_db_str("working"));
        assert!(!StepOutcome::is_terminal_db_str("running"));
    }

    // -- semantic validation (canvas editor save boundary) ------------------

    fn node(id: &str, kind: CircuitNodeKind) -> CircuitNode {
        CircuitNode {
            id: id.into(),
            kind,
        }
    }

    #[test]
    fn validate_accepts_the_walking_skeleton() {
        CircuitGraph::walking_skeleton("x").validate().unwrap();
    }

    #[test]
    fn validate_rejects_duplicate_node_ids() {
        let g = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![
                node("t", CircuitNodeKind::Manual),
                node("t", CircuitNodeKind::Manual),
            ],
            edges: vec![],
        };
        assert!(g.validate().unwrap_err().contains("duplicate node id"));
    }

    #[test]
    fn validate_rejects_edges_pointing_at_unknown_nodes() {
        let g = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![node("t", CircuitNodeKind::Manual)],
            edges: vec![CircuitEdge {
                from: "t".into(),
                to: "ghost".into(),
                condition: EdgeCondition::default(),
            }],
        };
        assert!(g.validate().unwrap_err().contains("unknown target node"));
    }

    #[test]
    fn validate_rejects_self_loops() {
        let g = CircuitGraph {
            version: 1,
            blueprint: None,
            nodes: vec![node("t", CircuitNodeKind::Manual)],
            edges: vec![CircuitEdge {
                from: "t".into(),
                to: "t".into(),
                condition: EdgeCondition::default(),
            }],
        };
        assert!(g.validate().unwrap_err().contains("connects to itself"));
    }

    /// #1219 review: `timeout_seconds` is bounded to `MAX_STEP_TIMEOUT_SECONDS`
    /// (7 days). A user can type any `u32` in the inspector and the JSON
    /// round-trips freely, so the validator is the only enforcement point.
    /// Pin the rejection message so a future typo from a serializer
    /// (e.g. handing us an `i32` overflow) yields a field-named error.
    #[test]
    fn validate_rejects_timeout_seconds_above_max() {
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![node(
                "s",
                CircuitNodeKind::SpawnAgentNode {
                    prompt: "p".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: Some(MAX_STEP_TIMEOUT_SECONDS + 1),
                },
            )],
            edges: vec![],
        };
        let err = g.validate().unwrap_err();
        assert!(
            err.contains("timeout_seconds="),
            "rejection must name the field: {err}"
        );
        assert!(
            err.contains("'s'"),
            "rejection must name the offending node id: {err}"
        );
    }

    /// The inspector's contract is "0 or blank = inherit default";
    /// `Some(0)` therefore must NOT be rejected — the resolver collapses
    /// it to `None` at the seam so the cascade falls through. Pin that
    /// the validator accepts it as a valid `u32` value AND that it does
    /// not trip the upper-bound check.
    #[test]
    fn validate_accepts_zero_timeout_seconds() {
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![node(
                "s",
                CircuitNodeKind::SpawnAgentNode {
                    prompt: "p".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: Some(0),
                },
            )],
            edges: vec![],
        };
        g.validate().unwrap();
    }

    /// The upper bound is inclusive — `Some(MAX_STEP_TIMEOUT_SECONDS)`
    /// is a legitimate "one week" override that the validator must
    /// accept. (The clamp on the inspector side mirrors this.)
    #[test]
    fn validate_accepts_max_timeout_seconds() {
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![node(
                "s",
                CircuitNodeKind::SpawnAgentNode {
                    prompt: "p".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: Some(MAX_STEP_TIMEOUT_SECONDS),
                },
            )],
            edges: vec![],
        };
        g.validate().unwrap();
    }

    #[test]
    fn validate_rejects_source_bindings_on_automatic_roots() {
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node(
                    "trigger",
                    CircuitNodeKind::Interval {
                        interval_seconds: 60,
                    },
                ),
                node(
                    "gate",
                    CircuitNodeKind::AwaitAgentTurn {
                        target_node_id: Some("$source".into()),
                    },
                ),
            ],
            edges: vec![always("trigger", "gate")],
        };
        assert!(g.validate().unwrap_err().contains("Manual root"));
    }

    #[test]
    fn validate_accepts_a_linear_graph() {
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node("a", CircuitNodeKind::Manual),
                node(
                    "b",
                    CircuitNodeKind::Notify {
                        message: String::new(),
                    },
                ),
                node(
                    "c",
                    CircuitNodeKind::Notify {
                        message: String::new(),
                    },
                ),
            ],
            edges: vec![always("a", "b"), always("b", "c")],
        };
        g.validate().unwrap();
    }

    #[test]
    fn validate_accepts_diamond_joins() {
        // Diamond (valid): a -> b -> d, a -> c -> d.
        let diamond = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node("a", CircuitNodeKind::Manual),
                node(
                    "b",
                    CircuitNodeKind::Notify {
                        message: String::new(),
                    },
                ),
                node(
                    "c",
                    CircuitNodeKind::Notify {
                        message: String::new(),
                    },
                ),
                node("d", CircuitNodeKind::AllCompleted),
            ],
            edges: vec![
                always("a", "b"),
                always("a", "c"),
                always("b", "d"),
                always("c", "d"),
            ],
        };
        diamond.validate().unwrap();
    }

    #[test]
    fn validate_accepts_a_cycle_bounded_by_retry_limit() {
        // implement -> review -> retry -> implement
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node("implement", spawn_kind("write it", Some("implementer"))),
                node("review", spawn_kind("review it", Some("reviewer"))),
                node("retry", CircuitNodeKind::RetryLimit { max_retries: 3 }),
            ],
            edges: vec![
                always("implement", "review"),
                always("review", "retry"),
                always("retry", "implement"),
            ],
        };
        g.validate().unwrap();
    }

    #[test]
    fn validate_accepts_a_cycle_bounded_by_collaborator_check() {
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node("work", spawn_kind("p", None)),
                node(
                    "gate",
                    CircuitNodeKind::CollaboratorCheck {
                        require_approval: true,
                    },
                ),
            ],
            edges: vec![always("work", "gate"), always("gate", "work")],
        };
        g.validate().unwrap();
    }

    #[test]
    fn validate_rejects_unbounded_cycles_with_actionable_message() {
        let cyclic = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node("a", CircuitNodeKind::Manual),
                node(
                    "b",
                    CircuitNodeKind::Notify {
                        message: String::new(),
                    },
                ),
                node(
                    "c",
                    CircuitNodeKind::Notify {
                        message: String::new(),
                    },
                ),
            ],
            edges: vec![always("a", "b"), always("b", "c"), always("c", "a")],
        };
        let err = cyclic.validate().unwrap_err();
        assert!(err.contains("unbounded cycle"), "{err}");
        assert!(err.contains("RetryLimit"), "{err}");
        assert!(err.contains("CollaboratorCheck"), "{err}");
        for id in ["a", "b", "c"] {
            assert!(err.contains(id), "cycle message must name node {id}: {err}");
        }
    }

    #[test]
    fn validate_rejects_auto_pass_collaborator_check_as_a_bound() {
        // require_approval: false is not a bound — the gate auto-completes.
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node("work", spawn_kind("p", None)),
                node(
                    "gate",
                    CircuitNodeKind::CollaboratorCheck {
                        require_approval: false,
                    },
                ),
            ],
            edges: vec![always("work", "gate"), always("gate", "work")],
        };
        let err = g.validate().unwrap_err();
        assert!(err.contains("unbounded cycle"), "{err}");
    }

    #[test]
    fn validate_rejects_a_graph_with_one_bounded_and_one_unbounded_cycle() {
        // A bounded loop must not launder a sibling unbounded loop.
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node("a", spawn_kind("p", None)),
                node("retry", CircuitNodeKind::RetryLimit { max_retries: 2 }),
                node(
                    "c",
                    CircuitNodeKind::Notify {
                        message: "x".into(),
                    },
                ),
                node(
                    "d",
                    CircuitNodeKind::Notify {
                        message: "y".into(),
                    },
                ),
            ],
            edges: vec![
                always("a", "retry"),
                always("retry", "a"),
                always("c", "d"),
                always("d", "c"),
            ],
        };
        let err = g.validate().unwrap_err();
        assert!(err.contains("unbounded cycle"), "{err}");
        assert!(
            err.contains("c → d") || err.contains("d → c"),
            "unbounded cycle must name the c/d loop: {err}"
        );
    }

    #[test]
    fn validate_rejects_self_loop_even_on_a_retry_limit() {
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![node(
                "retry",
                CircuitNodeKind::RetryLimit { max_retries: 3 },
            )],
            edges: vec![always("retry", "retry")],
        };
        assert!(g.validate().unwrap_err().contains("connects to itself"));
    }

    // -- v1 → v2 field defaults ----------------------------------------------

    #[test]
    fn v1_spawn_inject_and_status_json_default_the_new_optional_fields() {
        // Internally-tagged `CircuitNodeKind` sits in CircuitNode's `type`
        // field, so a v1 payload looks like `{id, type: {type, ...}}`.
        let parsed = CircuitGraph::from_json(
            r#"{"version":1,"nodes":[
                {"id":"s","type":{"type":"spawn_agent_node","prompt":"p","name":"fix-it"}},
                {"id":"i","type":{"type":"inject_pty","prompt":"hi"}},
                {"id":"st","type":{"type":"set_node_status","status":"completed"}}
            ],"edges":[]}"#,
        )
        .unwrap();
        match &parsed.node("s").unwrap().kind {
            CircuitNodeKind::SpawnAgentNode {
                prompt,
                name,
                provider,
                model,
                effort,
                extra_args,
                timeout_seconds,
            } => {
                assert_eq!(prompt, "p");
                assert_eq!(name.as_deref(), Some("fix-it"));
                assert_eq!(provider, &None);
                assert_eq!(model, &None);
                assert_eq!(timeout_seconds, &None);
                assert_eq!(effort, &None);
                assert_eq!(extra_args, &None);
            }
            other => panic!("expected spawn, got {other:?}"),
        }
        match &parsed.node("i").unwrap().kind {
            CircuitNodeKind::InjectPty {
                prompt,
                target_node_id,
            } => {
                assert_eq!(prompt, "hi");
                assert_eq!(target_node_id, &None);
            }
            other => panic!("expected inject, got {other:?}"),
        }
        match &parsed.node("st").unwrap().kind {
            CircuitNodeKind::SetNodeStatus {
                status,
                target_node_id,
            } => {
                assert_eq!(*status, SessionStatusKind::Completed);
                assert_eq!(target_node_id, &None);
            }
            other => panic!("expected set_node_status, got {other:?}"),
        }
    }

    #[test]
    fn v3_targeted_harness_and_timeout_round_trip() {
        // #1219: v3 added `timeout_seconds` to SpawnAgentNode. Older
        // graphs (v1/v2) still parse — `#[serde(default)]` defaults the
        // new field to `None`; this test round-trips a populated v3.
        let graph = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node(
                    "spawn",
                    CircuitNodeKind::SpawnAgentNode {
                        prompt: "implement".into(),
                        name: Some("implementer".into()),
                        provider: Some("anthropic".into()),
                        model: Some("opus-4-1".into()),
                        effort: Some("high".into()),
                        extra_args: Some("--dangerously-skip-permissions".into()),
                        timeout_seconds: Some(1800),
                    },
                ),
                node(
                    "inject",
                    CircuitNodeKind::InjectPty {
                        prompt: "address review".into(),
                        target_node_id: Some("spawn".into()),
                    },
                ),
                node(
                    "done",
                    CircuitNodeKind::SetNodeStatus {
                        status: SessionStatusKind::Completed,
                        target_node_id: Some("spawn".into()),
                    },
                ),
            ],
            edges: vec![always("spawn", "inject"), always("inject", "done")],
        };
        let parsed = CircuitGraph::from_json(&graph.to_json().unwrap()).unwrap();
        assert_eq!(parsed, graph);
        assert_eq!(parsed.version, 3);
    }

    #[test]
    fn v2_spawn_json_defaults_timeout_seconds_to_none() {
        // #1219: a v2 graph_json (no `timeout_seconds` field at all) must
        // still parse, default the new field to `None`, and pass
        // `validate()`. This is the cross-version compatibility contract.
        let parsed = CircuitGraph::from_json(
            r#"{"version":2,"nodes":[
                {"id":"s","type":{"type":"spawn_agent_node","prompt":"p","name":"fix-it"}}
            ],"edges":[]}"#,
        )
        .unwrap();
        match &parsed.node("s").unwrap().kind {
            CircuitNodeKind::SpawnAgentNode {
                timeout_seconds, ..
            } => {
                assert_eq!(timeout_seconds, &None);
            }
            other => panic!("expected spawn, got {other:?}"),
        }
        parsed.validate().unwrap();
    }

    // -- walking skeleton shape ----------------------------------------------

    #[test]
    fn editing_the_review_prompt_does_not_change_blueprint_identity() {
        let mut g = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let node = g
            .nodes
            .iter_mut()
            .find(|node| node.id == "reviewer")
            .expect("reviewer exists");
        node.kind = CircuitNodeKind::SpawnAgentNode {
            prompt: "custom review".into(),
            name: None,
            provider: None,
            model: None,
            effort: None,
            extra_args: None,
            timeout_seconds: None,
        };
        assert!(g.is_issue_driven_autopilot_review());
    }

    #[test]
    fn effective_blueprint_classifies_a_markerless_legacy_review_graph() {
        // Mirrors the canvas's `parseGraph`: a graph persisted before the
        // `blueprint` discriminator existed is classified by shape, so the
        // editor command cannot mistake it for a walking skeleton (floor 1)
        // and let its reviewer deadlock.
        let mut graph = crate::circuit::test_support::pre_publication_issue_review();
        graph.nodes.push(CircuitNode {
            id: "review_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: "legacy".into(),
                target_node_id: Some("reviewer".into()),
            },
        });
        let mut raw: serde_json::Value = serde_json::from_str(&graph.to_json().unwrap()).unwrap();
        raw.as_object_mut().unwrap().remove("blueprint");
        let legacy = CircuitGraph::from_json(&serde_json::to_string(&raw).unwrap()).unwrap();

        assert_eq!(legacy.blueprint, None);
        assert_eq!(
            legacy.effective_blueprint(),
            CircuitBlueprintKind::IssueDrivenAutopilotReview
        );
        // An explicit marker always wins; a plain skeleton stays a skeleton.
        assert_eq!(
            CircuitGraph::walking_skeleton("p").effective_blueprint(),
            CircuitBlueprintKind::WalkingSkeleton
        );
    }
}

#[cfg(test)]
mod has_ancestor_matching_tests {
    use super::*;

    fn linear_graph() -> CircuitGraph {
        CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                CircuitNode {
                    id: "a".into(),
                    kind: CircuitNodeKind::Manual,
                },
                CircuitNode {
                    id: "b".into(),
                    kind: CircuitNodeKind::Manual,
                },
                CircuitNode {
                    id: "c".into(),
                    kind: CircuitNodeKind::Manual,
                },
            ],
            edges: vec![
                CircuitEdge {
                    from: "a".into(),
                    to: "b".into(),
                    condition: EdgeCondition::Always,
                },
                CircuitEdge {
                    from: "b".into(),
                    to: "c".into(),
                    condition: EdgeCondition::Always,
                },
            ],
        }
    }

    fn diamond_graph() -> CircuitGraph {
        CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                CircuitNode {
                    id: "t".into(),
                    kind: CircuitNodeKind::Manual,
                },
                CircuitNode {
                    id: "spawn_a".into(),
                    kind: CircuitNodeKind::SpawnAgentNode {
                        prompt: "a".into(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                        timeout_seconds: None,
                    },
                },
                CircuitNode {
                    id: "spawn_b".into(),
                    kind: CircuitNodeKind::SpawnAgentNode {
                        prompt: "b".into(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                        timeout_seconds: None,
                    },
                },
                CircuitNode {
                    id: "join".into(),
                    kind: CircuitNodeKind::AllCompleted,
                },
                CircuitNode {
                    id: "c".into(),
                    kind: CircuitNodeKind::Manual,
                },
            ],
            edges: vec![
                CircuitEdge {
                    from: "t".into(),
                    to: "spawn_a".into(),
                    condition: EdgeCondition::Always,
                },
                CircuitEdge {
                    from: "t".into(),
                    to: "spawn_b".into(),
                    condition: EdgeCondition::Always,
                },
                CircuitEdge {
                    from: "spawn_a".into(),
                    to: "join".into(),
                    condition: EdgeCondition::Always,
                },
                CircuitEdge {
                    from: "spawn_b".into(),
                    to: "join".into(),
                    condition: EdgeCondition::Always,
                },
                CircuitEdge {
                    from: "join".into(),
                    to: "c".into(),
                    condition: EdgeCondition::Always,
                },
            ],
        }
    }

    #[test]
    fn finds_kind_in_immediate_parent() {
        let g = diamond_graph();
        assert!(g.has_ancestor_matching("join", |node| matches!(
            node.kind,
            CircuitNodeKind::SpawnAgentNode { .. }
        )));
    }

    #[test]
    fn finds_kind_through_multiple_hops() {
        let g = linear_graph();
        assert!(g.has_ancestor_matching("c", |node| node.id == "a"));
    }

    #[test]
    fn returns_false_when_no_ancestor_matches() {
        let g = linear_graph();
        assert!(!g.has_ancestor_matching("c", |node| matches!(
            node.kind,
            CircuitNodeKind::SpawnAgentNode { .. }
        )));
    }

    #[test]
    fn does_not_test_the_start_node_itself() {
        let g = diamond_graph();
        assert!(!g.has_ancestor_matching("join", |node| node.id == "join"));
    }

    #[test]
    fn is_cycle_safe() {
        // a -> b -> a. The walk visits each node at most once; it must
        // terminate rather than spin between a and b.
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                CircuitNode {
                    id: "a".into(),
                    kind: CircuitNodeKind::Manual,
                },
                CircuitNode {
                    id: "b".into(),
                    kind: CircuitNodeKind::Manual,
                },
            ],
            edges: vec![
                CircuitEdge {
                    from: "a".into(),
                    to: "b".into(),
                    condition: EdgeCondition::Always,
                },
                CircuitEdge {
                    from: "b".into(),
                    to: "a".into(),
                    condition: EdgeCondition::Always,
                },
            ],
        };
        // Predicate matches nothing; if the walk terminates the result is false.
        let r = g.has_ancestor_matching("b", |node| node.id == "ghost");
        assert!(!r);
    }

    /// Cross-boundary pin for the AST version constant (#1219).
    ///
    /// `CIRCUIT_GRAPH_VERSION` is hand-maintained on BOTH sides (Rust
    /// here, TS at `src/components/Circuits/circuitGraphModel.ts`) and
    /// the ts-rs drift gate does NOT cover hand-maintained constants.
    /// When this assertion fails, mirror the bump to the TS mirror or
    /// the next canvas save will silently downgrade a v3 graph back
    /// to v2.
    #[test]
    fn circuit_graph_version_is_v3() {
        assert_eq!(CIRCUIT_GRAPH_VERSION, 3);
    }
}
