#[cfg(test)]
use super::model::CircuitNode;
use super::model::{CircuitEdge, CircuitGraph, CircuitNodeKind as K, EdgeCondition, StepOutcome};

pub(crate) const LOCAL_APPROVED_MESSAGE: &str = "Review approved for {{source.name}} (agent {{source.agent_id}}). It was asked to squash-merge its pull request and has been handed back to you.";
pub(crate) const LOCAL_REVIEW_INCONCLUSIVE_MESSAGE: &str = "Review needs attention for {{source.name}}: the reviewer could not give a verdict. See the review report in the run context.";
pub(crate) const PR_REVIEW_INCONCLUSIVE_MESSAGE: &str = "Review needs attention for PR #{{pr.number}}: the reviewer could not give a verdict. See the review report in the run context.";

impl CircuitGraph {
    /// Continuation accepts the review shape with editable prompts and launch settings.
    /// Changed control flow or additional actions need deliberate manual recovery.
    /// Runs pinned before the publication flow keep their older shape, which
    /// satisfies the same contract.
    pub fn has_local_review_contract(&self) -> bool {
        let Some(K::RetryLimit { max_retries }) = self.node("retry").map(|node| &node.kind) else {
            return false;
        };
        if !(1..=10000).contains(max_retries) {
            return false;
        }
        let expected = Self::agent_review(None, None, *max_retries);
        if self.has_review_topology_of(&expected) {
            return true;
        }
        let mut upgraded = self.clone();
        // Pinned stock graphs retain the old inconclusive-review branch.
        // ReviewVerdict now parks that outcome as Unverified. Normalize only
        // its exact former route in this comparison copy, keeping the ledger intact.
        if matches!(self.node("close_blocked").map(|node| &node.kind),
            Some(K::CloseAgentNode { target_node_id }) if target_node_id.as_deref() == Some("reviewer"))
            && matches!(
                self.node("blocked").map(|node| &node.kind),
                Some(K::Notify { .. })
            )
        {
            for edge in [
                CircuitEdge {
                    from: "verdict".into(),
                    to: "close_blocked".into(),
                    condition: EdgeCondition::OnOutcome(StepOutcome::Blocked),
                },
                CircuitEdge {
                    from: "close_blocked".into(),
                    to: "blocked".into(),
                    condition: EdgeCondition::Always,
                },
            ] {
                let Some(index) = upgraded.edges.iter().position(|actual| actual == &edge) else {
                    return false;
                };
                upgraded.edges.remove(index);
            }
            for id in ["close_blocked", "blocked"] {
                let index = upgraded
                    .nodes
                    .iter()
                    .position(|node| node.id == id)
                    .unwrap();
                upgraded.nodes.remove(index);
            }
            if upgraded.has_review_topology_of(&expected) {
                return true;
            }
        }
        super::stored_graph::upgrade(&mut upgraded, super::stored_graph::UpgradeTarget::Preset)
            && upgraded.has_review_topology_of(&expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit::evaluator::Classification;
    use crate::circuit::model::{CircuitEdge, StepOutcome as O};
    use crate::circuit::test_support::advance_with_report_evidence;
    use crate::circuit::{context::CircuitContext, stepper::*};

    fn tick(run: &mut RunView) -> Transition {
        advance(
            run,
            &CircuitEvent::Tick(Capacity {
                agent_free_slots: 1,
            }),
        )
    }

    fn classified(run: &mut RunView, node: &str, classification: Classification) -> Transition {
        advance_with_report_evidence(
            run,
            &CircuitEvent::TurnClassified {
                binding: None,
                node_id: node.into(),
                classification: Some(classification),
                output: Some("review report".into()),
            },
        )
    }

    /// Dispatch an InjectPty step and acknowledge its delivery, returning the
    /// dispatch effects and the delivery transition's effects.
    fn inject(run: &mut RunView, node: &str) -> (Vec<Effect>, Vec<Effect>) {
        let ready = advance(
            run,
            &CircuitEvent::AgentReady {
                node_id: node.into(),
            },
        );
        let attempt = run.step(node).unwrap().attempt;
        let delivered = advance(
            run,
            &CircuitEvent::PromptDelivered {
                node_id: node.into(),
                attempt,
            },
        );
        (ready.effects, delivered.effects)
    }

    fn injected_prompt(effects: &[Effect], node: &str, target: &str) -> String {
        effects
            .iter()
            .find_map(|effect| match effect {
                Effect::InjectPty {
                    node_id,
                    target_node_id: Some(to),
                    prompt,
                } if node_id == node && to == target => Some(prompt.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{node} must inject into {target}: {effects:?}"))
    }

    fn reviewing(rounds: i32) -> RunView {
        let graph = CircuitGraph::agent_review(None, None, rounds);
        graph.validate().unwrap();
        let mut context = CircuitContext::new();
        context.set("source.agent_id", "42");
        let mut run = RunView {
            run_id: 1,
            graph,
            state: RunState::Pending,
            context,
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        let waiting = tick(&mut run);
        assert!(waiting.effects.is_empty());
        assert_eq!(
            run.step("await_source").unwrap().status,
            StepStatus::Running
        );
        classified(&mut run, "await_source", Classification::Completed);
        tick(&mut run);
        let (publish, _) = inject(&mut run, "publish");
        assert!(injected_prompt(&publish, "publish", "$source").contains("gh pr create"));
        assert!(
            run.step("reviewer").is_none(),
            "the reviewer waits for the published pull request"
        );
        assert_eq!(
            run.step("await_publish").unwrap().status,
            StepStatus::Running
        );
        classified(&mut run, "await_publish", Classification::Completed);
        tick(&mut run);
        assert_eq!(run.step("reviewer").unwrap().status, StepStatus::Running);
        assert!(run.steps.iter().all(|s| s.agent_node_id != Some(42)));
        finish_review_turn(&mut run, 100);
        run
    }

    #[test]
    fn built_in_review_graph_does_not_bind_a_source_provider() {
        let graph = CircuitGraph::agent_review(None, None, 3);
        match &graph.node("reviewer").expect("reviewer node").kind {
            K::SpawnAgentNode { provider, .. } => {
                assert_eq!(provider, &None);
            }
            other => panic!("reviewer must be a SpawnAgentNode, got {other:?}"),
        }
    }

    fn finish_review_turn(run: &mut RunView, id: i64) {
        run.attach_agent_node("reviewer", id);
        crate::circuit::test_support::advance_with_completion_evidence(
            run,
            &CircuitEvent::AgentFinished {
                agent_node_id: id,
                success: true,
                output: Some("findings".into()),
            },
        );
        tick(run);
        assert_eq!(run.step("verdict").unwrap().status, StepStatus::Running);
    }

    /// Findings go back to the borrowed source; once its fix turn is done the
    /// retry gate queues a re-review of the still-open reviewer.
    fn request_fixes(run: &mut RunView) -> Vec<Effect> {
        classified(run, "verdict", Classification::Working);
        assert_eq!(run.resolve_target_agent("feedback"), Some(42));
        let (feedback, delivered) = inject(run, "feedback");
        let prompt = injected_prompt(&feedback, "feedback", "$source");
        assert!(prompt.contains("review report") && prompt.contains("push"));
        assert!(
            feedback
                .iter()
                .chain(&delivered)
                .all(|e| !matches!(e, Effect::CloseAgentNode { .. })),
            "a fix round keeps the reviewer open"
        );
        assert_eq!(run.step("await_fixes").unwrap().status, StepStatus::Running);
        let result = classified(run, "await_fixes", Classification::Completed);
        let mut effects = result.effects;
        effects.extend(tick(run).effects);
        effects
    }

    fn approve_and_merge(run: &mut RunView) -> Vec<Effect> {
        let mut effects = classified(run, "verdict", Classification::Completed).effects;
        effects.extend(tick(run).effects);
        assert!(effects.iter().any(
            |e| matches!(e, Effect::CloseAgentNode { node_id, .. } if node_id == "close_approved")
        ));
        assert_eq!(
            run.state,
            RunState::Running,
            "the run waits to deliver the merge request"
        );
        let (merge, delivered) = inject(run, "merge");
        let prompt = injected_prompt(&merge, "merge", "$source");
        assert!(prompt.contains("gh pr merge --squash") && prompt.contains("gh pr ready"));
        effects.extend(merge);
        effects.extend(delivered);
        effects
    }

    #[test]
    fn agent_review_approval_asks_the_source_to_merge_without_fixing_or_retrying() {
        let mut run = reviewing(3);
        let effects = approve_and_merge(&mut run);
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::Notify { message } if message.contains("approved") && message.contains("handed back"))));
        assert!(run.step("feedback").is_none());
        assert!(run.step("re_review").is_none());
        assert_eq!(run.state, RunState::Completed);
    }

    #[test]
    fn agent_review_findings_return_to_source_then_reprompt_the_same_reviewer() {
        let mut run = reviewing(3);
        let effects = request_fixes(&mut run);
        assert!(effects
            .iter()
            .all(|e| !matches!(e, Effect::SpawnAgentNode { .. })));
        assert_eq!(run.step("re_review").unwrap().status, StepStatus::Running);
        let (reprompt, _) = inject(&mut run, "re_review");
        assert!(injected_prompt(&reprompt, "re_review", "reviewer").contains("round 2 of 3"));
        assert_eq!(run.resolve_target_agent("re_review"), Some(100));
        tick(&mut run);
        assert_eq!(run.step("verdict").unwrap().status, StepStatus::Running);
        assert_eq!(run.step("verdict").unwrap().attempt, 2);
        assert_eq!(
            run.step("reviewer").unwrap().attempt,
            1,
            "the reviewer is spawned once"
        );
        approve_and_merge(&mut run);
        assert_eq!(run.state, RunState::Completed);
        assert!(run.steps.iter().all(|s| s.agent_node_id != Some(42)));
    }

    /// #1910: across a whole review round — findings, feedback to the
    /// borrowed source, a re-review, then approval and the merge request —
    /// a review never spawns anything after its first reviewer and never
    /// calls GitHub itself. A Review Successor reuses this exact graph, so
    /// this bounds what a continuation can dispatch.
    #[test]
    fn a_full_review_loop_dispatches_only_the_reviewer() {
        let mut emitted: Vec<Effect> = Vec::new();
        let mut run = reviewing(3);
        emitted.extend(request_fixes(&mut run));
        let (reprompt, delivered) = inject(&mut run, "re_review");
        emitted.extend(reprompt);
        emitted.extend(delivered);
        emitted.extend(tick(&mut run).effects);
        emitted.extend(approve_and_merge(&mut run));
        assert_eq!(run.state, RunState::Completed);

        let spawned: Vec<&str> = emitted
            .iter()
            .filter_map(|effect| match effect {
                Effect::SpawnAgentNode { node_id, .. } => Some(node_id.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            spawned.is_empty(),
            "later rounds re-prompt the reviewer spawned before the loop: {spawned:?}"
        );
        assert!(
            !emitted
                .iter()
                .any(|effect| matches!(effect, Effect::CallGithub { .. })),
            "a review must never call GitHub itself; the source publishes and merges"
        );
        assert!(
            run.steps.iter().all(|step| step.agent_node_id != Some(42)),
            "the borrowed implementation agent is never adopted as an owned step"
        );
    }

    #[test]
    fn agent_review_exhaustion_never_claims_approval() {
        let mut run = reviewing(1);
        let effects = request_fixes(&mut run);
        assert!(effects.iter().any(
            |e| matches!(e, Effect::Notify { message } if message.contains("not been approved"))
        ));
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::SpawnAgentNode { .. })));
        assert!(
            run.step("merge").is_none(),
            "an unapproved review never asks for a merge"
        );
        assert_eq!(run.state, RunState::Failed);
    }

    #[test]
    fn agent_review_ambiguous_report_stops_for_attention() {
        let mut run = reviewing(3);
        let mut result = classified(&mut run, "verdict", Classification::Blocked);
        let repeated = classified(&mut run, "verdict", Classification::Blocked);
        let notifications = result
            .effects
            .iter()
            .chain(&repeated.effects)
            .filter(|effect| {
                matches!(effect, Effect::Notify { message } if message.contains("Review needs attention"))
            })
            .count();
        assert_eq!(
            notifications, 1,
            "parking the same inconclusive review notifies once"
        );
        result.effects.extend(tick(&mut run).effects);
        assert!(run.graph.node("close_blocked").is_none());
        assert!(run.graph.node("blocked").is_none());
        assert!(!result
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::CloseAgentNode { .. })));
        assert!(run.step("feedback").is_none());
        assert!(run.step("merge").is_none());
        assert_eq!(run.state, RunState::Running);
        assert_eq!(run.step("verdict").unwrap().status, StepStatus::Unverified);
        assert!(run.can_record_review("verdict", O::Completed));
    }

    #[test]
    fn human_review_findings_reach_the_next_feedback_prompt() {
        let mut run = reviewing(3);
        classified(&mut run, "verdict", Classification::Working);
        let attempt = run.step("verdict").unwrap().attempt.to_string();
        run.context
            .set("node.verdict.review_verdict_source", "operator");
        run.context
            .set("node.verdict.review_verdict_attempt", attempt);
        run.context.set(
            "node.verdict.operator_review_note",
            "Fix the unchecked null before merging.",
        );
        let (effects, _) = inject(&mut run, "feedback");
        let prompt = injected_prompt(&effects, "feedback", "$source");
        assert!(prompt
            .contains("Human review findings (operator):\nFix the unchecked null before merging."));
        assert!(prompt.contains("review report"));
    }

    #[test]
    fn human_review_findings_reach_feedback_without_a_report_placeholder() {
        let mut run = reviewing(3);
        let feedback = run
            .graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "feedback")
            .expect("feedback node");
        let K::InjectPty { prompt, .. } = &mut feedback.kind else {
            panic!("feedback must inject a prompt");
        };
        *prompt = "Address the requested changes and summarize your work.".into();

        classified(&mut run, "verdict", Classification::Working);
        let attempt = run.step("verdict").unwrap().attempt.to_string();
        run.context
            .set("node.verdict.review_verdict_source", "operator");
        run.context
            .set("node.verdict.review_verdict_attempt", attempt);
        run.context.set(
            "node.verdict.operator_review_note",
            "Fix the unchecked null before merging.",
        );
        let (effects, _) = inject(&mut run, "feedback");
        let prompt = injected_prompt(&effects, "feedback", "$source");
        assert!(prompt.contains("Address the requested changes"));
        assert!(prompt
            .contains("Human review findings (operator):\nFix the unchecked null before merging."));
        assert!(!prompt.contains("review report"));
    }

    #[test]
    fn request_changes_is_rejected_without_a_feedback_injection_route() {
        let mut run = reviewing(3);
        classified(&mut run, "verdict", Classification::Blocked);
        run.graph.edges.retain(|edge| edge.from != "verdict");
        run.graph.nodes.push(CircuitNode {
            id: "attention_only".into(),
            kind: K::Notify {
                message: "Inspect this review".into(),
            },
        });
        run.graph.edges.push(CircuitEdge {
            from: "verdict".into(),
            to: "attention_only".into(),
            condition: EdgeCondition::OnOutcome(O::Working),
        });

        assert!(!run.can_record_review("verdict", O::Working));
    }

    #[test]
    fn a_source_that_cannot_publish_waits_for_a_person() {
        let graph = CircuitGraph::agent_review(None, None, 3);
        let mut context = CircuitContext::new();
        context.set("source.agent_id", "42");
        let mut run = RunView {
            run_id: 1,
            graph,
            state: RunState::Pending,
            context,
            steps: vec![],
        };
        advance(&mut run, &CircuitEvent::Triggered);
        tick(&mut run);
        classified(&mut run, "await_source", Classification::Completed);
        tick(&mut run);
        inject(&mut run, "publish");
        classified(&mut run, "await_publish", Classification::Blocked);
        tick(&mut run);
        assert_eq!(
            run.step("confirm_publish").unwrap().status,
            StepStatus::Blocked
        );
        assert!(run.step("reviewer").is_none());
        advance(
            &mut run,
            &CircuitEvent::CollaboratorApproved {
                node_id: "confirm_publish".into(),
            },
        );
        tick(&mut run);
        tick(&mut run);
        assert_eq!(run.step("reviewer").unwrap().status, StepStatus::Running);
    }

    #[test]
    fn source_binding_cannot_be_used_to_delete_the_original_agent() {
        let mut run = reviewing(3);
        run.graph.nodes.push(CircuitNode {
            id: "unsafe_close".into(),
            kind: K::CloseAgentNode {
                target_node_id: Some("$source".into()),
            },
        });
        assert_eq!(run.resolve_target_agent("unsafe_close"), None);
    }
}

#[cfg(test)]
mod continuation_contract_tests {
    use super::*;

    #[test]
    fn review_copy_allows_prompt_and_settings_edits_but_rejects_changed_obligations() {
        let mut graph = CircuitGraph::agent_review(None, None, 3);
        if let K::SpawnAgentNode { prompt, model, .. } = &mut graph
            .nodes
            .iter_mut()
            .find(|n| n.id == "reviewer")
            .unwrap()
            .kind
        {
            *prompt = "Review the security boundaries".into();
            *model = Some("gpt-6-luna".into());
        }
        assert!(graph.has_local_review_contract());
        let mut unsafe_graph = graph.clone();
        unsafe_graph
            .edges
            .iter_mut()
            .find(|e| e.to == "close_approved")
            .unwrap()
            .condition = EdgeCondition::Always;
        assert!(
            !unsafe_graph.has_local_review_contract(),
            "approval cannot be bypassed"
        );
        let mut unsafe_graph = graph.clone();
        unsafe_graph
            .nodes
            .iter_mut()
            .find(|n| n.id == "feedback")
            .unwrap()
            .kind = K::InjectPty {
            prompt: "fix".into(),
            target_node_id: Some("reviewer".into()),
        };
        assert!(
            !unsafe_graph.has_local_review_contract(),
            "feedback must return to borrowed source"
        );
        let mut unsafe_graph = graph.clone();
        unsafe_graph
            .nodes
            .iter_mut()
            .find(|n| n.id == "retry")
            .unwrap()
            .kind = K::RetryLimit { max_retries: 0 };
        assert!(
            !unsafe_graph.has_local_review_contract(),
            "review must be bounded"
        );
        graph.nodes.push(CircuitNode {
            id: "publication".into(),
            kind: K::Notify {
                message: "additional action".into(),
            },
        });
        assert!(
            !graph.has_local_review_contract(),
            "continuation must not inherit extra actions"
        );
    }

    #[test]
    fn historical_inconclusive_branch_keeps_the_review_contract_strict() {
        let graph = CircuitGraph::from_json(include_str!(
            "../../tests/fixtures/legacy-local-review-circuit.json"
        ))
        .unwrap();
        assert!(graph.has_local_review_contract());

        let mut wrong_target = graph.clone();
        wrong_target
            .nodes
            .iter_mut()
            .find(|node| node.id == "close_blocked")
            .unwrap()
            .kind = K::CloseAgentNode {
            target_node_id: Some("$source".into()),
        };
        assert!(!wrong_target.has_local_review_contract());

        let mut wrong_outcome = graph.clone();
        wrong_outcome
            .edges
            .iter_mut()
            .find(|edge| edge.to == "close_blocked")
            .unwrap()
            .condition = EdgeCondition::Always;
        assert!(!wrong_outcome.has_local_review_contract());

        let mut extra_route = graph.clone();
        let route = extra_route
            .edges
            .iter()
            .find(|edge| edge.to == "close_blocked")
            .unwrap()
            .clone();
        extra_route.edges.push(route);
        assert!(!extra_route.has_local_review_contract());

        let mut extra_action = graph.clone();
        extra_action.edges.push(CircuitEdge {
            from: "blocked".into(),
            to: "merge".into(),
            condition: EdgeCondition::Always,
        });
        assert!(!extra_action.has_local_review_contract());

        let mut legacy = crate::circuit::test_support::pre_publication_local_review(3);
        for id in ["close_blocked", "blocked"] {
            legacy.nodes.push(graph.node(id).unwrap().clone());
        }
        legacy.edges.extend(
            graph
                .edges
                .iter()
                .filter(|edge| edge.to == "close_blocked" || edge.from == "close_blocked")
                .cloned(),
        );
        assert!(legacy.has_local_review_contract());
    }

    #[test]
    fn legacy_blocked_step_must_remain_a_notification() {
        let mut graph = CircuitGraph::from_json(include_str!(
            "../../tests/fixtures/legacy-local-review-circuit.json"
        ))
        .unwrap();
        graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "blocked")
            .unwrap()
            .kind = K::InjectPty {
            prompt: "Perform an additional action".into(),
            target_node_id: Some("$source".into()),
        };
        graph.validate().unwrap();
        assert!(
            !graph.has_local_review_contract(),
            "compatibility must not strip an executable action in place of the stock notification"
        );
    }

    #[test]
    fn legacy_close_blocked_step_must_remain_a_reviewer_close() {
        let mut graph = CircuitGraph::from_json(include_str!(
            "../../tests/fixtures/legacy-local-review-circuit.json"
        ))
        .unwrap();
        graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "close_blocked")
            .unwrap()
            .kind = K::InjectPty {
            prompt: "Perform an additional action".into(),
            target_node_id: Some("reviewer".into()),
        };
        graph.validate().unwrap();
        assert!(
            !graph.has_local_review_contract(),
            "compatibility must not strip an executable action in place of the stock reviewer close"
        );
    }

    #[test]
    fn runs_pinned_before_the_publication_flow_remain_continuable() {
        let legacy = crate::circuit::test_support::pre_publication_local_review(3);
        assert!(legacy.has_local_review_contract());
        let mut bypassed = legacy.clone();
        bypassed
            .edges
            .iter_mut()
            .find(|e| e.to == "close_approved")
            .unwrap()
            .condition = EdgeCondition::Always;
        assert!(
            !bypassed.has_local_review_contract(),
            "approval cannot be bypassed in the older shape either"
        );
    }
}
