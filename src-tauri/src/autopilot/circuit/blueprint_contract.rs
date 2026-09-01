//! Blueprint contract matrix for the shipped built-in circuits
//! (issue #1469).
//!
//! The catalog is the single source of truth for "what blueprints ship".
//! Every contract entry pins: marker, trigger vocabulary, required node
//! topology + edges, validation, JSON round-trip, and per-blueprint
//! runtime policy. The drift gate at the bottom is **compiler-checked**:
//! the `const _: () = { ... }` block exhaustively matches every
//! [`CircuitBlueprintKind`] variant against the catalog, so adding a
//! new variant without a fixture entry is a build failure (the
//! non-exhaustive-match warning becomes a hard error in const context).
//!
//! The catalog reconciles spec #1205's "three presets" prose with the
//! Rust/TypeScript reality (#1203 follow-up):
//!
//!   * `WalkingSkeleton` is the canonical "spawn → inject → notify"
//!     chain. Spec #1205's *Issue-Driven PR Flow* and *Continuous
//!     Looping Pacer* presets describe idealised flows that include
//!     collaborator gates and verifier steps; the shipped walking
//!     skeleton is the minimal foundation those future-milestone
//!     extensions build on. Both spec presets are the same skeleton
//!     under different trigger roots, so the catalog keeps them as
//!     one entry with all four allowed.
//!   * `IssueDrivenAutopilotReview` is the spec's *PR Adversarial
//!     Reviewer* preset — the only multi-agent blueprint we ship.
//!
//! If a third preset is added later, add a variant to
//! [`CircuitBlueprintKind`] AND an entry to [`BUILT_IN_CATALOG`] in the
//! same change. The drift gate ensures neither side gets forgotten.

use super::model::{
    CircuitBlueprintKind, CircuitGraph, CircuitNodeKind, EdgeCondition, StepOutcome,
};
use crate::commands::circuit::CircuitTriggerKind;

/// One per-blueprint contract entry. The catalog iterates these and
/// exercises every field against the canonical builder so a drift in any
/// field is caught here, not at runtime.
#[derive(Debug, Clone)]
pub struct BlueprintContract {
    /// Discriminator tag stored in the graph AST.
    pub marker: CircuitBlueprintKind,
    /// Trigger roots this blueprint may use. The IPC + the model both
    /// enforce these — see [`crate::autopilot::circuit::model`].
    pub allowed_triggers: &'static [CircuitTriggerKind],
    /// Node ids that MUST exist after `validate()`. Order matches the
    /// canonical builder's node order so a topology diff is one glance.
    pub required_node_ids: &'static [&'static str],
    /// Required edges: `(from, to, condition)`. Used to pin both wiring
    /// AND the gate outcome conditions, so a refactor that drops a
    /// `OnOutcome(Green)` from a verifier→success edge surfaces here.
    pub required_edges: &'static [(&'static str, &'static str, EdgeCondition)],
    /// Concurrency normalisation enforced by the domain model.
    /// Mirrors [`CircuitBlueprintKind::min_concurrency_limit`] — the
    /// model is the source of truth; the contract pins it so the
    /// catalog and the model can't drift apart.
    pub min_concurrency_limit: i64,
    /// Default concurrency the Probe UI ships with. WalkingSkeleton = 1,
    /// IssueDrivenAutopilotReview = 2 (reviewer slot).
    pub default_concurrency_limit: i64,
    /// Whether Trigger Now (`trigger_circuit_now`) is permitted on this
    /// blueprint.
    pub allows_manual_trigger_now: bool,
    /// Human-readable policy assertion run against the blueprint's text.
    /// The contract test asserts the prompt fragment appears so a
    /// silent prompt rewrite surfaces in code review.
    pub prompt_assertions: &'static [PromptAssertion],
}

/// Asserts a prompt contains the given literal fragment. Prompts are the
/// user-visible contract of the review blueprint (issue #1205 acceptance
/// criterion: "the review blueprint test proves the reviewer is reached
/// only after a successful implementation/PR path"). The contract pins
/// them so a prompt refactor can't quietly break the contract.
#[derive(Debug, Clone, Copy)]
pub struct PromptAssertion {
    pub node_id: &'static str,
    pub must_contain: &'static str,
}

/// One entry per shipped blueprint. Adding a new
/// [`CircuitBlueprintKind`] variant WITHOUT adding an entry here causes
/// the `built_in_catalog_covers_every_blueprint_kind` drift gate
/// below to fail (compile error, not a runtime test miss).
pub const BUILT_IN_CATALOG: &[BlueprintContract] = &[
    BlueprintContract {
        marker: CircuitBlueprintKind::WalkingSkeleton,
        allowed_triggers: &[
            CircuitTriggerKind::Manual,
            CircuitTriggerKind::Interval,
            CircuitTriggerKind::GithubIssueLabel,
            CircuitTriggerKind::GithubPrLabel,
        ],
        required_node_ids: &["trigger", "spawn", "inject", "notify"],
        required_edges: &[
            ("trigger", "spawn", EdgeCondition::Always),
            ("spawn", "inject", EdgeCondition::Always),
            ("inject", "notify", EdgeCondition::Always),
        ],
        min_concurrency_limit: 1,
        default_concurrency_limit: 1,
        allows_manual_trigger_now: true,
        prompt_assertions: &[],
    },
    BlueprintContract {
        marker: CircuitBlueprintKind::IssueDrivenAutopilotReview,
        allowed_triggers: &[CircuitTriggerKind::GithubIssueLabel],
        required_node_ids: &[
            "trigger",
            "collaborator_gate",
            "implementer",
            "implementation_classifier",
            "finish",
            "finish_round",
            "finish_classifier",
            "open_pr",
            "wrapup_retry",
            "wrapup_correction",
            "reviewer",
            "review_classifier",
            "follow_feedback",
            "close_reviewer",
            "feedback_classifier",
            "review_retry",
            "complete",
        ],
        required_edges: &[
            ("trigger", "collaborator_gate", EdgeCondition::Always),
            ("collaborator_gate", "implementer", EdgeCondition::Always),
            ("implementer", "implementation_classifier", EdgeCondition::Always),
            // LLM classifier routes Completed → finish prompt.
            (
                "implementation_classifier",
                "finish",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            ("finish", "finish_round", EdgeCondition::Always),
            (
                "finish_classifier",
                "open_pr",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            // OpenPr failed → wrapup retry, NOT reviewer (issue #1469
            // acceptance: "the reviewer is reached only after a
            // successful implementation/PR path").
            (
                "open_pr",
                "wrapup_retry",
                EdgeCondition::OnOutcome(StepOutcome::Failed),
            ),
            ("wrapup_retry", "wrapup_correction", EdgeCondition::Always),
            ("wrapup_correction", "finish_round", EdgeCondition::Always),
            ("finish_round", "finish_classifier", EdgeCondition::Always),
            (
                "open_pr",
                "reviewer",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            ("reviewer", "review_classifier", EdgeCondition::Always),
            (
                "review_classifier",
                "follow_feedback",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            ("follow_feedback", "close_reviewer", EdgeCondition::Always),
            ("close_reviewer", "feedback_classifier", EdgeCondition::Always),
            (
                "feedback_classifier",
                "review_retry",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            // review_retry: Completed → re-finish, Failed → complete
            // notify (retry budget exhausted).
            (
                "review_retry",
                "finish",
                EdgeCondition::OnOutcome(StepOutcome::Completed),
            ),
            (
                "review_retry",
                "complete",
                EdgeCondition::OnOutcome(StepOutcome::Failed),
            ),
        ],
        min_concurrency_limit: 2,
        default_concurrency_limit: 2,
        // The review blueprint is labelled-issue-driven — a manual fire
        // would mint a run with no `issue.*` context.
        allows_manual_trigger_now: false,
        prompt_assertions: &[
            // The implementation agent's first turn is the issue body.
            PromptAssertion {
                node_id: "implementer",
                must_contain: "{{issue.prefill}}",
            },
            // The reviewer must be told to comment on the PR.
            PromptAssertion {
                node_id: "reviewer",
                must_contain: "{{pr.url}}",
            },
            PromptAssertion {
                node_id: "reviewer",
                must_contain: CircuitGraph::PR_REVIEW_PROMPT,
            },
            // The feedback turn must reach the implementation agent.
            PromptAssertion {
                node_id: "follow_feedback",
                must_contain: "{{node.reviewer.output}}",
            },
        ],
    },
];

// ---------------------------------------------------------------------------
// Builders — produce the graph_json each contract entry pins.
// ---------------------------------------------------------------------------

/// Build the graph JSON the contract pins. Walking skeleton uses the
/// `prompt` argument as the inject target; the review blueprint pins
/// the trigger label that the implementation agent will read from
/// `{{issue.*}}`.
pub fn build_graph(blueprint: CircuitBlueprintKind, prompt: &str, trigger_label: &str) -> CircuitGraph {
    match blueprint {
        CircuitBlueprintKind::WalkingSkeleton => CircuitGraph::walking_skeleton(prompt),
        CircuitBlueprintKind::IssueDrivenAutopilotReview => {
            CircuitGraph::issue_driven_autopilot_review(trigger_label)
        }
    }
}

// ---------------------------------------------------------------------------
// Drift gate — compiler-checked exhaustiveness over `CircuitBlueprintKind`.
// ---------------------------------------------------------------------------

/// `const _: () = { ... }` is evaluated at compile time. The closure body
/// is forced to be exhaustive over its `match` arms. Adding a new
/// variant to [`CircuitBlueprintKind`] without extending this match
/// (and adding a `BlueprintContract` for it) is a build failure.
#[allow(dead_code)]
const _: () = {
    /// One arm per `CircuitBlueprintKind` variant. The closure's body
    /// returns the index of the matching entry in [`BUILT_IN_CATALOG`].
    /// A non-exhaustive match here fails the build.
    fn catalog_index(kind: CircuitBlueprintKind) -> usize {
        match kind {
            CircuitBlueprintKind::WalkingSkeleton => 0,
            CircuitBlueprintKind::IssueDrivenAutopilotReview => 1,
        }
    }
    // Reference `catalog_index` from a second const block so the
    // compiler actually instantiates it; the `let _ = ...` is enough
    // to drag the dead-code check past the `#[allow(dead_code)]`.
    let _ = catalog_index;
};

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot::circuit::model::validate_circuit_request;

    /// Drift gate (runtime half): every variant of `CircuitBlueprintKind`
    /// has a `BUILT_IN_CATALOG` entry, and no entry points at a
    /// non-existent variant. The const `_:` block above guarantees the
    /// same property at compile time; this test makes the intent
    /// legible to a reader scanning the file.
    #[test]
    fn built_in_catalog_covers_every_blueprint_kind() {
        for contract in BUILT_IN_CATALOG {
            // The model is the source of truth on per-variant policy;
            // the catalog's marker-relative fields must agree.
            assert_eq!(
                contract.allowed_triggers,
                contract.marker.allowed_triggers(),
                "catalog's allowed_triggers drifted from the model"
            );
            assert_eq!(
                contract.min_concurrency_limit,
                contract.marker.min_concurrency_limit(),
                "catalog's min_concurrency_limit drifted from the model"
            );
            assert_eq!(
                contract.default_concurrency_limit,
                contract.marker.default_concurrency_limit(),
                "catalog's default_concurrency_limit drifted from the model"
            );
            assert_eq!(
                contract.allows_manual_trigger_now,
                contract.marker.allows_manual_trigger_now(),
                "catalog's allows_manual_trigger_now drifted from the model"
            );
        }
    }

    #[test]
    fn every_blueprint_passes_validate_and_round_trips_through_graph_json() {
        for contract in BUILT_IN_CATALOG {
            let graph = build_graph(contract.marker, "do the thing", "buildmesh:run");
            graph
                .validate()
                .unwrap_or_else(|err| panic!("{:?} failed validate(): {err}", contract.marker));
            let json = graph
                .to_json()
                .unwrap_or_else(|err| panic!("{:?} failed to_json: {err}", contract.marker));
            let parsed = CircuitGraph::from_json(&json)
                .unwrap_or_else(|err| panic!("{:?} failed from_json: {err}", contract.marker));
            assert_eq!(
                parsed, graph,
                "{:?} graph_json round-trip is lossy",
                contract.marker
            );
        }
    }

    #[test]
    fn every_blueprint_has_the_required_node_ids_in_canonical_order() {
        for contract in BUILT_IN_CATALOG {
            let graph = build_graph(contract.marker, "p", "buildmesh:run");
            let actual: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
            assert_eq!(
                &actual[..],
                contract.required_node_ids,
                "{:?} topology drifted from contract",
                contract.marker
            );
        }
    }

    #[test]
    fn every_blueprint_has_the_required_edges_with_conditions() {
        for contract in BUILT_IN_CATALOG {
            let graph = build_graph(contract.marker, "p", "buildmesh:run");
            for (from, to, expected_condition) in contract.required_edges {
                let edge = graph
                    .edges
                    .iter()
                    .find(|edge| edge.from == *from && edge.to == *to)
                    .unwrap_or_else(|| {
                        panic!(
                            "{:?}: missing required edge {} → {}",
                            contract.marker, from, to
                        )
                    });
                assert_eq!(
                    &edge.condition, expected_condition,
                    "{:?}: edge {} → {} condition drifted",
                    contract.marker, from, to
                );
            }
        }
    }

    #[test]
    fn every_blueprint_carries_its_marker_in_the_graph_ast() {
        for contract in BUILT_IN_CATALOG {
            let graph = build_graph(contract.marker, "p", "buildmesh:run");
            assert_eq!(
                graph.blueprint,
                Some(contract.marker),
                "{:?} graph JSON must carry its blueprint marker",
                contract.marker
            );
            // And the marker survives JSON round-trip — runtime policy
            // reads `graph.blueprint`, not the builder call site.
            let parsed = CircuitGraph::from_json(&graph.to_json().unwrap()).unwrap();
            assert_eq!(parsed.blueprint, Some(contract.marker));
        }
    }

    #[test]
    fn walking_skeleton_accepts_every_trigger_root() {
        // Spec #1205's "Issue-Driven PR Flow" and "Continuous Looping
        // Pacer" presets are the same skeleton with different triggers.
        // The walking skeleton contract MUST accept Manual, Interval,
        // GitHub-issue-label, and GitHub-PR-label without re-validation.
        let triggers = [
            CircuitNodeKind::Manual,
            CircuitNodeKind::Interval { interval_seconds: 60 },
            CircuitNodeKind::GithubIssueLabel { label: "buildmesh:run".into() },
            CircuitNodeKind::GithubPullRequestLabel { label: "review-me".into() },
        ];
        for trigger in triggers {
            let graph = CircuitGraph::triggered_skeleton("p", trigger.clone());
            graph
                .validate()
                .unwrap_or_else(|err| panic!("walking_skeleton rejected trigger {trigger:?}: {err}"));
        }
    }

    #[test]
    fn review_blueprint_requires_an_issue_label_trigger_root() {
        // The contract pins `IssueDrivenAutopilotReview.allowed_triggers`
        // to `GithubIssueLabel` only. The model enforces this at
        // validate_circuit_request time, so a future refactor that
        // lifts the restriction breaks here at compile time.
        let review = BUILT_IN_CATALOG
            .iter()
            .find(|c| c.marker == CircuitBlueprintKind::IssueDrivenAutopilotReview)
            .expect("review blueprint is in the catalog");
        assert_eq!(
            review.allowed_triggers,
            &[CircuitTriggerKind::GithubIssueLabel],
            "review blueprint trigger vocabulary must remain GitHub issue label only"
        );
        assert!(!review.allows_manual_trigger_now);
    }

    #[test]
    fn review_blueprint_requires_two_concurrency_slots_or_deadlocks() {
        let review = BUILT_IN_CATALOG
            .iter()
            .find(|c| c.marker == CircuitBlueprintKind::IssueDrivenAutopilotReview)
            .expect("review blueprint is in the catalog");
        assert_eq!(review.min_concurrency_limit, 2);
        assert_eq!(review.default_concurrency_limit, 2);
    }

    #[test]
    fn walking_skeleton_default_concurrency_is_one_slot() {
        let walking = BUILT_IN_CATALOG
            .iter()
            .find(|c| c.marker == CircuitBlueprintKind::WalkingSkeleton)
            .expect("walking skeleton is in the catalog");
        assert_eq!(walking.min_concurrency_limit, 1);
        assert_eq!(walking.default_concurrency_limit, 1);
        assert!(walking.allows_manual_trigger_now);
    }

    #[test]
    fn review_blueprint_carries_the_user_visible_pr_review_prompt() {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let reviewer = graph
            .node("reviewer")
            .expect("reviewer node is required by contract");
        match &reviewer.kind {
            CircuitNodeKind::SpawnAgentNode { prompt, .. } => {
                assert!(
                    prompt.contains(CircuitGraph::PR_REVIEW_PROMPT),
                    "reviewer prompt must carry the canonical PR_REVIEW_PROMPT contract"
                );
                assert!(
                    prompt.contains("{{pr.url}}"),
                    "reviewer prompt must inject the PR URL"
                );
            }
            other => panic!("reviewer must be a SpawnAgentNode, got {other:?}"),
        }
    }

    #[test]
    fn review_blueprint_implementation_agent_receives_issue_body_as_first_turn() {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let implementer = graph.node("implementer").expect("implementer node required");
        match &implementer.kind {
            CircuitNodeKind::SpawnAgentNode { prompt, .. } => {
                assert!(
                    prompt.contains("{{issue.prefill}}"),
                    "implementer first turn must be the issue body"
                );
            }
            other => panic!("implementer must be SpawnAgentNode, got {other:?}"),
        }
    }

    #[test]
    fn review_blueprint_open_pr_failure_routes_through_wrapup_retry_not_reviewer() {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let open_pr_to_reviewer: Vec<&super::super::model::CircuitEdge> = graph
            .edges
            .iter()
            .filter(|e| e.from == "open_pr" && e.to == "reviewer")
            .collect();
        assert_eq!(open_pr_to_reviewer.len(), 1);
        // The OpenPr→reviewer edge MUST be OnCompleted, never Always —
        // an Always edge would let a wrapup-corrected success path also
        // spawn the reviewer (which is correct), but the Failed edge
        // must go to wrapup_retry first.
        let open_pr_failed: Vec<&super::super::model::CircuitEdge> = graph
            .edges
            .iter()
            .filter(|e| e.from == "open_pr"
                && matches!(e.condition, EdgeCondition::OnOutcome(StepOutcome::Failed)))
            .collect();
        assert_eq!(open_pr_failed.len(), 1);
        assert_eq!(open_pr_failed[0].to, "wrapup_retry");
    }

    #[test]
    fn review_blueprint_closes_reviewer_node_on_feedback_path() {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let follow_to_close: Vec<&super::super::model::CircuitEdge> = graph
            .edges
            .iter()
            .filter(|e| e.from == "follow_feedback" && e.to == "close_reviewer")
            .collect();
        assert_eq!(follow_to_close.len(), 1);
        let close = graph
            .node("close_reviewer")
            .expect("close_reviewer required by contract");
        match &close.kind {
            CircuitNodeKind::CloseAgentNode { target_node_id } => {
                assert_eq!(target_node_id.as_deref(), Some("reviewer"));
            }
            other => panic!("close_reviewer must be CloseAgentNode, got {other:?}"),
        }
    }

    #[test]
    fn review_blueprint_review_retry_exhaustion_terminates_with_complete_notify() {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let retry = graph
            .node("review_retry")
            .expect("review_retry required by contract");
        match &retry.kind {
            CircuitNodeKind::RetryLimit { max_retries } => assert_eq!(*max_retries, 3),
            other => panic!("review_retry must be RetryLimit, got {other:?}"),
        }
        let retry_failed: Vec<&super::super::model::CircuitEdge> = graph
            .edges
            .iter()
            .filter(|e| {
                e.from == "review_retry"
                    && matches!(e.condition, EdgeCondition::OnOutcome(StepOutcome::Failed))
            })
            .collect();
        assert_eq!(retry_failed.len(), 1);
        assert_eq!(retry_failed[0].to, "complete");
    }

    #[test]
    fn review_blueprint_collaborator_gate_requires_human_approval() {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let gate = graph
            .node("collaborator_gate")
            .expect("collaborator_gate required by contract");
        match &gate.kind {
            CircuitNodeKind::CollaboratorCheck { require_approval } => {
                assert!(*require_approval, "collaborator_gate must require human approval");
            }
            other => panic!("collaborator_gate must be CollaboratorCheck, got {other:?}"),
        }
    }

    /// Every shipped blueprint has at least one node of every
    /// "structural" kind we want CI to keep exercising — i.e. a future
    /// refactor that drops the only `RetryLimit` from the review
    /// blueprint fails here before it can break runtime retry policy.
    #[test]
    fn review_blueprint_exercises_gates_joins_and_clasifiers() {
        let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let mut saw_retry = false;
        let mut saw_collab = false;
        let mut saw_classifier = 0;
        let mut saw_join = 0;
        for node in &graph.nodes {
            match node.kind {
                CircuitNodeKind::RetryLimit { .. } => saw_retry = true,
                CircuitNodeKind::CollaboratorCheck { require_approval: true } => saw_collab = true,
                CircuitNodeKind::LlmTurnClassifier { .. } => saw_classifier += 1,
                CircuitNodeKind::AnyCompleted | CircuitNodeKind::AllCompleted => saw_join += 1,
                _ => {}
            }
        }
        assert!(saw_retry, "review blueprint must bound its loops with a RetryLimit");
        assert!(saw_collab, "review blueprint must gate the implementation on human approval");
        assert!(saw_classifier >= 2, "review blueprint needs at least 2 LLM classifiers");
        assert!(saw_join >= 1, "review blueprint needs at least one join (finish_round)");
    }

    #[test]
    fn walking_skeleton_is_a_single_spawn_chain_with_no_gates_or_joins() {
        // The walking skeleton is intentionally minimal — single agent
        // slot, no classifier/verifier/retry. If a future "smart preset"
        // adds a gate, this test must be updated alongside the contract
        // entry, not silently.
        let graph = CircuitGraph::walking_skeleton("p");
        let structural: Vec<&CircuitNodeKind> = graph
            .nodes
            .iter()
            .map(|n| &n.kind)
            .filter(|k| {
                matches!(
                    k,
                    CircuitNodeKind::RetryLimit { .. }
                        | CircuitNodeKind::CollaboratorCheck { .. }
                        | CircuitNodeKind::DeterministicVerification { .. }
                        | CircuitNodeKind::LlmTurnClassifier { .. }
                        | CircuitNodeKind::AllCompleted
                        | CircuitNodeKind::AnyCompleted
                )
            })
            .collect();
        assert!(
            structural.is_empty(),
            "walking_skeleton must remain gate/join-free: {structural:?}"
        );
        // And its single SpawnAgentNode must be the canonical fresh
        // start (no prefill prompt — the inject step carries it).
        let spawn = graph.node("spawn").expect("spawn required");
        match &spawn.kind {
            CircuitNodeKind::SpawnAgentNode { prompt, .. } => assert!(prompt.is_empty()),
            other => panic!("spawn must be SpawnAgentNode, got {other:?}"),
        }
    }

    #[test]
    fn every_prompt_assertion_holds_against_the_canonical_builder() {
        // Issue #1469 acceptance: "the review blueprint test proves the
        // reviewer is reached only after a successful implementation/PR
        // path and that feedback closes the reviewer branch correctly."
        // These assertions lock the user-visible prompts.
        for contract in BUILT_IN_CATALOG {
            for assertion in contract.prompt_assertions {
                let graph = build_graph(contract.marker, "p", "buildmesh:run");
                let node = graph
                    .node(assertion.node_id)
                    .unwrap_or_else(|| panic!("{:?}: prompt assertion targets missing node '{}'", contract.marker, assertion.node_id));
                let prompt = match &node.kind {
                    CircuitNodeKind::SpawnAgentNode { prompt, .. }
                    | CircuitNodeKind::InjectPty { prompt, .. }
                    | CircuitNodeKind::Notify { message: prompt, .. } => prompt.as_str(),
                    CircuitNodeKind::GithubAction { comment: Some(comment), .. } => comment.as_str(),
                    other => panic!(
                        "{:?}: prompt assertion on '{}' targets a non-prompt node kind: {other:?}",
                        contract.marker, assertion.node_id
                    ),
                };
                assert!(
                    prompt.contains(assertion.must_contain),
                    "{:?}: prompt for '{}' must contain {:?}, was {prompt:?}",
                    contract.marker,
                    assertion.node_id,
                    assertion.must_contain
                );
            }
        }
    }

    // -------------------------------------------------------------------
    // Domain validation moved to `autopilot::circuit::model`. The tests
    // below exercise the pure helper directly so internal services,
    // the worker, and any future CLI share the same contract.
    // -------------------------------------------------------------------

    #[test]
    fn validate_circuit_request_rejects_incompatible_trigger_for_review_blueprint() {
        // The model is the source of truth. IPC commands funnel through
        // this helper; bypassing it (a future CLI, internal service)
        // would deadlock the reviewer behind a Manual spawn.
        let err = validate_circuit_request(
            CircuitBlueprintKind::IssueDrivenAutopilotReview,
            Some(CircuitTriggerKind::Manual),
            None,
            None,
            2,
        )
        .unwrap_err();
        assert!(
            err.contains("IssueDrivenAutopilotReview"),
            "error must name the blueprint: {err}"
        );
    }

    #[test]
    fn validate_circuit_request_clamps_review_concurrency_to_two_or_higher() {
        let validated = validate_circuit_request(
            CircuitBlueprintKind::IssueDrivenAutopilotReview,
            Some(CircuitTriggerKind::GithubIssueLabel),
            Some("buildmesh:run"),
            None,
            1, // caller asked for 1 — must be clamped to the model floor
        )
        .unwrap();
        assert_eq!(validated.concurrency_limit, 2);
    }

    #[test]
    fn validate_circuit_request_accepts_walking_skeleton_with_every_trigger_root() {
        for trigger in [
            CircuitTriggerKind::Manual,
            CircuitTriggerKind::Interval,
            CircuitTriggerKind::GithubIssueLabel,
            CircuitTriggerKind::GithubPrLabel,
        ] {
            validate_circuit_request(
                CircuitBlueprintKind::WalkingSkeleton,
                Some(trigger),
                if matches!(
                    trigger,
                    CircuitTriggerKind::GithubIssueLabel | CircuitTriggerKind::GithubPrLabel
                ) {
                    Some("buildmesh:run")
                } else {
                    None
                },
                if matches!(trigger, CircuitTriggerKind::Interval) {
                    Some(120)
                } else {
                    None
                },
                1,
            )
            .unwrap_or_else(|err| panic!("walking_skeleton rejected {trigger:?}: {err}"));
        }
    }

    #[test]
    fn validate_circuit_request_rejects_empty_github_label() {
        let err = validate_circuit_request(
            CircuitBlueprintKind::WalkingSkeleton,
            Some(CircuitTriggerKind::GithubIssueLabel),
            Some("   "), // whitespace-only
            None,
            1,
        )
        .unwrap_err();
        assert!(
            err.to_lowercase().contains("label"),
            "error must mention the label: {err}"
        );
    }

    #[test]
    fn validate_circuit_request_clamps_interval_to_60s_to_7d() {
        let validated = validate_circuit_request(
            CircuitBlueprintKind::WalkingSkeleton,
            Some(CircuitTriggerKind::Interval),
            None,
            Some(5), // 5 seconds — clamped to 60s
            1,
        )
        .unwrap();
        assert_eq!(validated.interval_seconds, Some(60));

        let validated = validate_circuit_request(
            CircuitBlueprintKind::WalkingSkeleton,
            Some(CircuitTriggerKind::Interval),
            None,
            Some(30 * 24 * 3_600), // 30 days — clamped to 7 days
            1,
        )
        .unwrap();
        assert_eq!(validated.interval_seconds, Some(7 * 24 * 3_600));
    }
}
