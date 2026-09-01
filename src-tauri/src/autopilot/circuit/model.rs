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

/// Current blueprint AST version (issue #1356). v1 `graph_json` still
/// parses: new optional fields default to `None`. Writers emit this
/// version so a save upgrades the stored blueprint.
pub const CIRCUIT_GRAPH_VERSION: i32 = 2;

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
    /// JSON without them deserialises as `None`.
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
            | CircuitNodeKind::CollaboratorCheck { require_approval: true }
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
            | CircuitNodeKind::DeterministicVerification { .. }
            | CircuitNodeKind::CollaboratorCheck { .. }
            | CircuitNodeKind::RetryLimit { .. }
            | CircuitNodeKind::AllCompleted
            | CircuitNodeKind::AnyCompleted
    )
}

impl CircuitGraph {
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

    /// Incoming edges targeting `node_id`, in blueprint order.
    pub fn incoming(&self, node_id: &str) -> Vec<&CircuitEdge> {
        self.edges.iter().filter(|e| e.to == node_id).collect()
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

    /// Normalize the original issue-review blueprint so already-saved circuits
    /// use their real task as the spawned agent's first turn. Match the exact
    /// server-authored prompts and two-edge intermediary shape: prompts and
    /// topology are editable, so a customized graph must not be rewritten.
    pub(crate) fn upgrade_legacy_issue_review_first_turns(&mut self) -> bool {
        let marker_changed = self.blueprint.is_none() && self.has_legacy_issue_review_shape();
        if marker_changed {
            self.blueprint = Some(CircuitBlueprintKind::IssueDrivenAutopilotReview);
        }
        if !self.is_issue_driven_autopilot_review() {
            return false;
        }

        let implementer_changed = self.replace_legacy_injected_first_turn(
            "implementer",
            "implementation_prompt",
            "implementation_classifier",
            "",
            "{{issue.prefill}}",
            "{{issue.prefill}}",
        );
        let reviewer_changed = self.replace_legacy_injected_first_turn(
            "reviewer",
            "review_prompt",
            "review_classifier",
            "The pull request URL is {{pr.url}}. Use it as additional review context.",
            Self::PR_REVIEW_PROMPT,
            &format!(
                "{}. The pull request URL is {{{{pr.url}}}}.",
                Self::PR_REVIEW_PROMPT
            ),
        );
        marker_changed || implementer_changed || reviewer_changed
    }

    fn replace_legacy_injected_first_turn(
        &mut self,
        spawn_id: &str,
        prompt_id: &str,
        next_id: &str,
        expected_spawn_prompt: &str,
        expected_injected_prompt: &str,
        replacement_prompt: &str,
    ) -> bool {
        let spawn_matches = matches!(
            self.node(spawn_id).map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt == expected_spawn_prompt
        );
        let prompt_matches = matches!(
            self.node(prompt_id).map(|node| &node.kind),
            Some(CircuitNodeKind::InjectPty { prompt, target_node_id })
                if prompt == expected_injected_prompt
                    && target_node_id.as_deref() == Some(spawn_id)
        );
        let incident_edges: Vec<&CircuitEdge> = self
            .edges
            .iter()
            .filter(|edge| edge.from == prompt_id || edge.to == prompt_id)
            .collect();
        let topology_matches = incident_edges.len() == 2
            && incident_edges.iter().any(|edge| {
                edge.from == spawn_id
                    && edge.to == prompt_id
                    && edge.condition == EdgeCondition::Always
            })
            && incident_edges.iter().any(|edge| {
                edge.from == prompt_id
                    && edge.to == next_id
                    && edge.condition == EdgeCondition::Always
            });

        if !(spawn_matches && prompt_matches && topology_matches) {
            return false;
        }

        if let Some(CircuitNode {
            kind: CircuitNodeKind::SpawnAgentNode { prompt, .. },
            ..
        }) = self.nodes.iter_mut().find(|node| node.id == spawn_id)
        {
            *prompt = replacement_prompt.to_string();
        }
        self.nodes.retain(|node| node.id != prompt_id);
        for edge in &mut self.edges {
            if edge.from == spawn_id && edge.to == prompt_id {
                edge.to = next_id.to_string();
            }
        }
        self.edges
            .retain(|edge| !(edge.from == prompt_id && edge.to == next_id));
        true
    }

    /// The canonical walking-skeleton blueprint (issue #1206): Manual
    /// trigger → SpawnAgentNode (fresh, no prefill) → InjectPty (the
    /// configured prompt, over PTY once the process is live) → Notify.
    /// This is what the Probe tab's create form builds server-side, so
    /// the AST stays canonical in Rust instead of being hand-rolled in
    /// TypeScript. Routing the prompt through InjectPty (rather than
    /// staging it as spawn prefill) exercises the full
    /// spawn → observe-live-process → inject → await-turn chain the
    /// acceptance criteria describe.
    pub fn walking_skeleton(prompt: &str) -> Self {
        Self::triggered_skeleton(prompt, CircuitNodeKind::Manual)
    }

    /// The milestone-3 authoring shape (issue #1208): the same
    /// spawn → inject → notify chain as [`Self::walking_skeleton`] but
    /// with a caller-chosen trigger root — Interval, GithubIssueLabel,
    /// or GithubPullRequestLabel. The trigger kind is validated by the
    /// IPC boundary; this builder accepts any trigger verbatim so the
    /// AST stays canonical in one place.
    pub fn triggered_skeleton(prompt: &str, trigger: CircuitNodeKind) -> Self {
        debug_assert!(matches!(
            trigger,
            CircuitNodeKind::Manual
                | CircuitNodeKind::Interval { .. }
                | CircuitNodeKind::GithubIssueLabel { .. }
                | CircuitNodeKind::GithubPullRequestLabel { .. }
        ));
        Self {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: Some(CircuitBlueprintKind::WalkingSkeleton),
            nodes: vec![
                CircuitNode { id: "trigger".to_string(), kind: trigger },
                CircuitNode {
                    id: "spawn".to_string(),
                    kind: CircuitNodeKind::SpawnAgentNode {
                        prompt: String::new(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                    },
                },
                CircuitNode {
                    id: "inject".to_string(),
                    kind: CircuitNodeKind::InjectPty {
                        prompt: prompt.to_string(),
                        target_node_id: None,
                    },
                },
                CircuitNode {
                    id: "notify".to_string(),
                    kind: CircuitNodeKind::Notify {
                        message: "Circuit run started {{circuit.name}}".to_string(),
                    },
                },
            ],
            edges: vec![
                CircuitEdge { from: "trigger".to_string(), to: "spawn".to_string(), condition: EdgeCondition::Always },
                CircuitEdge { from: "spawn".to_string(), to: "inject".to_string(), condition: EdgeCondition::Always },
                CircuitEdge { from: "inject".to_string(), to: "notify".to_string(), condition: EdgeCondition::Always },
            ],
        }
    }

    /// Exact reviewer instruction used by the issue-driven review blueprint.
    /// Keep this text stable: it is both the user-requested contract and the
    /// prompt that a reviewer sees after its PTY becomes ready.
    pub const PR_REVIEW_PROMPT: &'static str = "review PR {{pr.number}} as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture. Add the review comments to the PR as a comment";

    /// Build the issue-driven Autopilot blueprint with a post-PR review loop.
    ///
    /// The implementation agent is finished through the same customizable
    /// `finish.md` prompt used by legacy Autopilot. The review blueprint
    /// requires that the agent already raised its PR; the worker discovers
    /// that PR by branch and populates `pr.*`, then spawns the reviewer.
    ///
    /// The reviewer and implementation agent are separate agent nodes. The
    /// implementation node is idle after its wrap-up turn while the reviewer
    /// works, so the two processes can coexist without sharing a worktree.
    pub fn issue_driven_autopilot_review(trigger_label: &str) -> Self {
        use EdgeCondition::{Always, OnOutcome};
        use StepOutcome::Completed;

        fn node(id: &str, kind: CircuitNodeKind) -> CircuitNode {
            CircuitNode { id: id.to_string(), kind }
        }
        fn edge(from: &str, to: &str) -> CircuitEdge {
            CircuitEdge { from: from.to_string(), to: to.to_string(), condition: Always }
        }
        fn outcome(from: &str, to: &str, value: StepOutcome) -> CircuitEdge {
            CircuitEdge {
                from: from.to_string(),
                to: to.to_string(),
                condition: OnOutcome(value),
            }
        }

        Self {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: Some(CircuitBlueprintKind::IssueDrivenAutopilotReview),
            nodes: vec![
                node(
                    "trigger",
                    CircuitNodeKind::GithubIssueLabel { label: trigger_label.trim().to_string() },
                ),
                node(
                    "collaborator_gate",
                    CircuitNodeKind::CollaboratorCheck { require_approval: true },
                ),
                node(
                    "implementer",
                    CircuitNodeKind::SpawnAgentNode {
                        prompt: "{{issue.prefill}}".to_string(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                    },
                ),
                node(
                    "implementation_classifier",
                    CircuitNodeKind::LlmTurnClassifier {
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                node(
                    "finish",
                    CircuitNodeKind::InjectPty {
                        prompt: "{{autopilot.finish_prompt}}".to_string(),
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                node("finish_round", CircuitNodeKind::AnyCompleted),
                node(
                    "finish_classifier",
                    CircuitNodeKind::LlmTurnClassifier {
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                node(
                    "open_pr",
                    CircuitNodeKind::GithubAction {
                        action: GithubActionKind::OpenPr,
                        label: None,
                        comment: Some("Closes #{{issue.number}}".to_string()),
                    },
                ),
                node(
                    "wrapup_retry",
                    CircuitNodeKind::RetryLimit { max_retries: 3 },
                ),
                node(
                    "wrapup_correction",
                    CircuitNodeKind::InjectPty {
                        prompt: "{{autopilot.wrapup_correction}}".to_string(),
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                node(
                    "reviewer",
                    CircuitNodeKind::SpawnAgentNode {
                        prompt: format!(
                            "{}. The pull request URL is {{{{pr.url}}}}.",
                            Self::PR_REVIEW_PROMPT
                        ),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                    },
                ),
                node(
                    "review_classifier",
                    CircuitNodeKind::LlmTurnClassifier {
                        target_node_id: Some("reviewer".to_string()),
                    },
                ),
                node(
                    "follow_feedback",
                    CircuitNodeKind::InjectPty {
                        prompt: "Follow the feedback comments on PR #{{pr.number}} ({{pr.url}}). Reviewer report: {{node.reviewer.output}}. Address every valid comment, run the relevant tests, and update the PR. Do not ignore architectural or clean-code concerns; report what you changed.".to_string(),
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                node(
                    "close_reviewer",
                    CircuitNodeKind::CloseAgentNode {
                        target_node_id: Some("reviewer".to_string()),
                    },
                ),
                node(
                    "feedback_classifier",
                    CircuitNodeKind::LlmTurnClassifier {
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                node("review_retry", CircuitNodeKind::RetryLimit { max_retries: 3 }),
                node(
                    "complete",
                    CircuitNodeKind::Notify {
                        message: "PR review feedback was sent to the implementation agent for PR #{{pr.number}} ({{issue.title}})".to_string(),
                    },
                ),
            ],
            edges: vec![
                edge("trigger", "collaborator_gate"),
                edge("collaborator_gate", "implementer"),
                edge("implementer", "implementation_classifier"),
                outcome("implementation_classifier", "finish", Completed),
                edge("finish", "finish_round"),
                outcome("finish_classifier", "open_pr", Completed),
                outcome("open_pr", "wrapup_retry", StepOutcome::Failed),
                edge("wrapup_retry", "wrapup_correction"),
                edge("wrapup_correction", "finish_round"),
                edge("finish_round", "finish_classifier"),
                outcome("open_pr", "reviewer", Completed),
                edge("reviewer", "review_classifier"),
                outcome("review_classifier", "follow_feedback", Completed),
                edge("follow_feedback", "close_reviewer"),
                edge("close_reviewer", "feedback_classifier"),
                outcome("feedback_classifier", "review_retry", Completed),
                outcome("review_retry", "finish", Completed),
                outcome("review_retry", "complete", StepOutcome::Failed),
            ],
        }
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
            CircuitNodeKind::Interval { interval_seconds: 300 },
            CircuitNodeKind::GithubIssueLabel { label: "buildmesh:run".into() },
            CircuitNodeKind::GithubPullRequestLabel { label: "review-me".into() },
            spawn_kind("p", Some("fix-it")),
            inject_kind("wrap up"),
            CircuitNodeKind::GithubAction { action: GithubActionKind::AddLabel, label: Some("done".into()), comment: None },
            set_status_kind(SessionStatusKind::Completed),
            CircuitNodeKind::Notify { message: "hi".into() },
            CircuitNodeKind::LlmTurnClassifier { target_node_id: None },
            CircuitNodeKind::DeterministicVerification { command: "cargo test".into() },
            CircuitNodeKind::CollaboratorCheck { require_approval: true },
            CircuitNodeKind::RetryLimit { max_retries: 3 },
            CircuitNodeKind::AllCompleted,
            CircuitNodeKind::AnyCompleted,
        ];
        for kind in kinds {
            let node = CircuitNode { id: "n1".into(), kind };
            let json = serde_json::to_string(&node).unwrap();
            assert!(json.contains("\"type\""), "every variant must tag its type: {}", json);
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
        assert_eq!(CircuitGraph::from_json(&format!("{{\"version\":2,\"nodes\":[],\"edges\":[{}]}}", json)).unwrap().edges[0].condition, EdgeCondition::OnOutcome(StepOutcome::Failed));
    }

    #[test]
    fn missing_condition_field_defaults_to_always() {
        let parsed: CircuitEdge =
            serde_json::from_str(r#"{"from":"a","to":"b"}"#).unwrap();
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
                CircuitNode { id: "t".into(), kind: CircuitNodeKind::Manual },
                CircuitNode { id: "a".into(), kind: CircuitNodeKind::Notify { message: "x".into() } },
                CircuitNode { id: "b".into(), kind: CircuitNodeKind::AllCompleted },
                CircuitNode { id: "c".into(), kind: CircuitNodeKind::AnyCompleted },
            ],
            edges: vec![
                CircuitEdge { from: "t".into(), to: "a".into(), condition: EdgeCondition::default() },
                CircuitEdge { from: "a".into(), to: "b".into(), condition: EdgeCondition::default() },
                CircuitEdge { from: "a".into(), to: "c".into(), condition: EdgeCondition::default() },
                CircuitEdge { from: "t".into(), to: "c".into(), condition: EdgeCondition::default() },
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
                CircuitNode { id: "a".into(), kind: CircuitNodeKind::Manual },
                CircuitNode { id: "b".into(), kind: CircuitNodeKind::Notify { message: "m".into() } },
            ],
            edges: vec![
                CircuitEdge { from: "a".into(), to: "b".into(), condition: EdgeCondition::OnOutcome(StepOutcome::Completed) },
                CircuitEdge { from: "a".into(), to: "b".into(), condition: EdgeCondition::OnOutcome(StepOutcome::Failed) },
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
        assert!(!consumes_agent_slot(&CircuitNodeKind::Notify { message: "n".into() }));
        assert!(!consumes_agent_slot(&CircuitNodeKind::AllCompleted));
    }

    #[test]
    fn executability_vocabulary() {
        assert!(is_executable(&CircuitNodeKind::Manual));
        assert!(is_executable(&spawn_kind("p", None)));
        assert!(is_executable(&inject_kind("p")));
        assert!(is_executable(&CircuitNodeKind::Notify { message: "n".into() }));
        // Milestone 2 (#1207): gates execute.
        assert!(is_executable(&CircuitNodeKind::LlmTurnClassifier { target_node_id: None }));
        assert!(is_executable(&CircuitNodeKind::DeterministicVerification { command: "cargo test".into() }));
        assert!(is_executable(&CircuitNodeKind::CollaboratorCheck { require_approval: true }));
        assert!(is_executable(&CircuitNodeKind::RetryLimit { max_retries: 3 }));
        // Milestone 3 (issue #1208): all five GitHub actions execute.
        for action in [
            GithubActionKind::AddLabel,
            GithubActionKind::RemoveLabel,
            GithubActionKind::PostComment,
            GithubActionKind::OpenPr,
            GithubActionKind::CloseIssue,
        ] {
            assert!(
                is_executable(&CircuitNodeKind::GithubAction { action, label: None, comment: None }),
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
        CircuitNode { id: id.into(), kind }
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
            nodes: vec![node("t", CircuitNodeKind::Manual), node("t", CircuitNodeKind::Manual)],
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

    #[test]
    fn validate_accepts_a_linear_graph() {
        let g = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                node("a", CircuitNodeKind::Manual),
                node("b", CircuitNodeKind::Notify { message: String::new() }),
                node("c", CircuitNodeKind::Notify { message: String::new() }),
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
                node("b", CircuitNodeKind::Notify { message: String::new() }),
                node("c", CircuitNodeKind::Notify { message: String::new() }),
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
                node("gate", CircuitNodeKind::CollaboratorCheck { require_approval: true }),
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
                node("b", CircuitNodeKind::Notify { message: String::new() }),
                node("c", CircuitNodeKind::Notify { message: String::new() }),
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
                node("gate", CircuitNodeKind::CollaboratorCheck { require_approval: false }),
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
                node("c", CircuitNodeKind::Notify { message: "x".into() }),
                node("d", CircuitNodeKind::Notify { message: "y".into() }),
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
            nodes: vec![node("retry", CircuitNodeKind::RetryLimit { max_retries: 3 })],
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
            } => {
                assert_eq!(prompt, "p");
                assert_eq!(name.as_deref(), Some("fix-it"));
                assert_eq!(provider, &None);
                assert_eq!(model, &None);
                assert_eq!(effort, &None);
                assert_eq!(extra_args, &None);
            }
            other => panic!("expected spawn, got {other:?}"),
        }
        match &parsed.node("i").unwrap().kind {
            CircuitNodeKind::InjectPty { prompt, target_node_id } => {
                assert_eq!(prompt, "hi");
                assert_eq!(target_node_id, &None);
            }
            other => panic!("expected inject, got {other:?}"),
        }
        match &parsed.node("st").unwrap().kind {
            CircuitNodeKind::SetNodeStatus { status, target_node_id } => {
                assert_eq!(*status, SessionStatusKind::Completed);
                assert_eq!(target_node_id, &None);
            }
            other => panic!("expected set_node_status, got {other:?}"),
        }
    }

    #[test]
    fn v2_targeted_and_harness_fields_round_trip() {
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
        assert_eq!(parsed.version, 2);
    }

    // -- walking skeleton shape ----------------------------------------------

    #[test]
    fn walking_skeleton_is_the_manual_spawn_inject_notify_chain() {
        let g = CircuitGraph::walking_skeleton("do the thing");
        assert_eq!(g.version, CIRCUIT_GRAPH_VERSION);
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["trigger", "spawn", "inject", "notify"]);
        assert_eq!(
            g.edges.iter().map(|e| (e.from.as_str(), e.to.as_str())).collect::<Vec<_>>(),
            vec![("trigger", "spawn"), ("spawn", "inject"), ("inject", "notify")]
        );
        match &g.node("spawn").unwrap().kind {
            CircuitNodeKind::SpawnAgentNode {
                prompt,
                name,
                provider,
                model,
                effort,
                extra_args,
            } => {
                assert_eq!(prompt, "", "spawn starts fresh — the prompt rides InjectPty");
                assert_eq!(*name, None);
                assert_eq!(provider, &None);
                assert_eq!(model, &None);
                assert_eq!(effort, &None);
                assert_eq!(extra_args, &None);
            }
            other => panic!("expected spawn node, got {:?}", other),
        }
        match &g.node("inject").unwrap().kind {
            CircuitNodeKind::InjectPty { prompt, target_node_id } => {
                assert_eq!(prompt, "do the thing");
                assert_eq!(target_node_id, &None);
            }
            other => panic!("expected inject node, got {:?}", other),
        }
    }

    #[test]
    fn triggered_skeleton_swaps_only_the_trigger_root() {
        // Milestone 3 authoring: a GitHub-labelled trigger keeps the rest
        // of the chain identical to the manual skeleton — the trigger is
        // the only thing the create form varies.
        for trigger in [
            CircuitNodeKind::GithubIssueLabel { label: "buildmesh:run".into() },
            CircuitNodeKind::GithubPullRequestLabel { label: "review-me".into() },
            CircuitNodeKind::Interval { interval_seconds: 300 },
        ] {
            let g = CircuitGraph::triggered_skeleton("fix it", trigger);
            assert_eq!(g.nodes.len(), 4);
            let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
            assert_eq!(ids, vec!["trigger", "spawn", "inject", "notify"]);
            match &g.node("trigger").unwrap().kind {
                CircuitNodeKind::GithubIssueLabel { label } => assert_eq!(label, "buildmesh:run"),
                CircuitNodeKind::GithubPullRequestLabel { label } => assert_eq!(label, "review-me"),
                CircuitNodeKind::Interval { interval_seconds } => {
                    assert_eq!(*interval_seconds, 300)
                }
                other => panic!("trigger kind not preserved: {:?}", other),
            }
            // Round-trips through graph_json like any blueprint.
            let parsed = CircuitGraph::from_json(&g.to_json().unwrap()).unwrap();
            assert_eq!(parsed, g);
        }
    }

    #[test]
    fn issue_driven_autopilot_review_is_a_valid_two_agent_blueprint() {
        let g = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        g.validate().expect("review blueprint must pass graph validation");
        assert!(g.is_issue_driven_autopilot_review());
        assert_eq!(
            g.blueprint,
            Some(CircuitBlueprintKind::IssueDrivenAutopilotReview)
        );

        assert!(matches!(
            g.node("trigger").map(|n| &n.kind),
            Some(CircuitNodeKind::GithubIssueLabel { label }) if label == "buildmesh:run"
        ));
        assert!(matches!(
            g.node("implementer").map(|n| &n.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt == "{{issue.prefill}}"
        ));
        assert!(
            g.node("implementation_prompt").is_none(),
            "the issue task must be the implementer's spawn-time first turn"
        );
        assert!(matches!(
            g.node("reviewer").map(|n| &n.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt.contains("{{pr.url}}")
                    && prompt.contains(CircuitGraph::PR_REVIEW_PROMPT)
        ));
        assert!(matches!(
            g.node("collaborator_gate").map(|n| &n.kind),
            Some(CircuitNodeKind::CollaboratorCheck { require_approval: true })
        ));
        assert!(
            g.node("review_prompt").is_none(),
            "the review instruction must be the reviewer's spawn-time first turn"
        );
        assert!(matches!(
            g.node("close_reviewer").map(|n| &n.kind),
            Some(CircuitNodeKind::CloseAgentNode { target_node_id })
                if target_node_id.as_deref() == Some("reviewer")
        ));
        assert!(matches!(
            g.node("review_retry").map(|n| &n.kind),
            Some(CircuitNodeKind::RetryLimit { max_retries }) if *max_retries == 3
        ));
        assert!(matches!(
            g.node("complete").map(|n| &n.kind),
            Some(CircuitNodeKind::Notify { message })
                if message.contains("{{pr.number}}") && message.contains("{{issue.title}}")
        ));
        assert!(
            g.edges.iter().any(|edge| {
                edge.from == "review_retry"
                    && edge.to == "finish"
                    && edge.condition == EdgeCondition::OnOutcome(StepOutcome::Completed)
            }),
            "review retry must re-enter the implementation wrap-up"
        );

        let parsed = CircuitGraph::from_json(&g.to_json().unwrap()).unwrap();
        assert_eq!(parsed, g);
    }

    #[test]
    fn stored_issue_review_blueprint_upgrades_injected_first_turns_to_spawn_prompts() {
        let mut graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");

        let implementer = graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "implementer")
            .unwrap();
        if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut implementer.kind {
            prompt.clear();
        }
        graph.nodes.push(CircuitNode {
            id: "implementation_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: "{{issue.prefill}}".into(),
                target_node_id: Some("implementer".into()),
            },
        });
        let implementer_edge = graph
            .edges
            .iter_mut()
            .find(|edge| edge.from == "implementer" && edge.to == "implementation_classifier")
            .unwrap();
        implementer_edge.to = "implementation_prompt".into();
        graph.edges.push(CircuitEdge {
            from: "implementation_prompt".into(),
            to: "implementation_classifier".into(),
            condition: EdgeCondition::Always,
        });

        let reviewer = graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "reviewer")
            .unwrap();
        if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut reviewer.kind {
            *prompt = "The pull request URL is {{pr.url}}. Use it as additional review context."
                .into();
        }
        graph.nodes.push(CircuitNode {
            id: "review_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: CircuitGraph::PR_REVIEW_PROMPT.into(),
                target_node_id: Some("reviewer".into()),
            },
        });
        let reviewer_edge = graph
            .edges
            .iter_mut()
            .find(|edge| edge.from == "reviewer" && edge.to == "review_classifier")
            .unwrap();
        reviewer_edge.to = "review_prompt".into();
        graph.edges.push(CircuitEdge {
            from: "review_prompt".into(),
            to: "review_classifier".into(),
            condition: EdgeCondition::Always,
        });

        let mut parsed = CircuitGraph::from_json(&graph.to_json().unwrap()).unwrap();
        assert!(parsed.upgrade_legacy_issue_review_first_turns());

        assert!(parsed.node("implementation_prompt").is_none());
        assert!(parsed.node("review_prompt").is_none());
        assert!(matches!(
            parsed.node("implementer").map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt == "{{issue.prefill}}"
        ));
        assert!(matches!(
            parsed.node("reviewer").map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt.contains(CircuitGraph::PR_REVIEW_PROMPT)
                    && prompt.contains("{{pr.url}}")
        ));
        parsed.validate().unwrap();
    }

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
        };
        assert!(g.is_issue_driven_autopilot_review());
    }

    #[test]
    fn legacy_review_graphs_get_a_marker_without_inspecting_prompt_text() {
        let mut graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        graph.nodes.push(CircuitNode {
            id: "review_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: "author-customized review instruction".into(),
                target_node_id: Some("reviewer".into()),
            },
        });
        let mut raw: serde_json::Value = serde_json::from_str(&graph.to_json().unwrap()).unwrap();
        raw.as_object_mut().unwrap().remove("blueprint");

        let mut parsed = CircuitGraph::from_json(&serde_json::to_string(&raw).unwrap()).unwrap();
        assert!(parsed.upgrade_legacy_issue_review_first_turns());
        assert_eq!(
            parsed.blueprint,
            Some(CircuitBlueprintKind::IssueDrivenAutopilotReview)
        );
        assert!(parsed.is_issue_driven_autopilot_review());
    }
}
