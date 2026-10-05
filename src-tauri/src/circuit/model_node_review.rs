use super::model::{
    CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind as K, EdgeCondition, StepOutcome as O,
    CIRCUIT_GRAPH_VERSION,
};

pub(crate) const LOCAL_APPROVED_MESSAGE: &str = "Review approved for {{source.name}} (agent {{source.agent_id}}). It was asked to squash-merge its pull request and has been handed back to you.";
pub(crate) const LEGACY_LOCAL_APPROVED_MESSAGE: &str =
    "Review approved for {{source.name}} (agent {{source.agent_id}}).";

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
        upgraded.upgrade_local_review_publication_flow()
            && upgraded.has_review_topology_of(&expected)
    }

    /// Move a stored local review graph from the per-round reviewer to the
    /// publication flow. Only the exact pre-publication topology is rewritten;
    /// the reviewer definition and customized texts are kept.
    pub(crate) fn upgrade_local_review_publication_flow(&mut self) -> bool {
        if self.node("close_reviewer").is_none() || self.node("publish").is_some() {
            return false;
        }
        let Some(K::RetryLimit { max_retries }) = self.node("retry").map(|node| &node.kind) else {
            return false;
        };
        let canonical = Self::agent_review(None, None, *max_retries);
        let mut upgraded = self.clone();
        let rewired = upgraded.retarget_edge("source_ready", "reviewer", "publish")
            && upgraded.retarget_edge("reviewer", "verdict", "review_round")
            && upgraded.retarget_edge("feedback", "close_reviewer", "await_fixes")
            && upgraded.remove_edge("close_reviewer", "await_fixes")
            && upgraded.retarget_edge("retry", "reviewer", "re_review")
            && upgraded.retarget_edge("close_approved", "approved", "merge");
        if !rewired {
            return false;
        }
        upgraded.nodes.retain(|node| node.id != "close_reviewer");
        for id in [
            "publish",
            "await_publish",
            "confirm_publish",
            "publish_ready",
            "review_round",
            "re_review",
            "merge",
        ] {
            upgraded
                .nodes
                .push(canonical.node(id).expect("canonical review node").clone());
        }
        let added = canonical.edges.iter().filter(|edge| {
            matches!(
                edge.from.as_str(),
                "publish"
                    | "await_publish"
                    | "confirm_publish"
                    | "publish_ready"
                    | "review_round"
                    | "re_review"
                    | "merge"
            )
        });
        upgraded.edges.extend(added.cloned());
        upgraded.replace_stock_text(
            "feedback",
            &Self::review_feedback_prompt(crate::review_contract::LEGACY_LOCAL_FEEDBACK_SCOPE),
            &Self::review_feedback_prompt(crate::review_contract::LOCAL_FEEDBACK_SCOPE),
        );
        upgraded.replace_stock_text(
            "approved",
            LEGACY_LOCAL_APPROVED_MESSAGE,
            LOCAL_APPROVED_MESSAGE,
        );
        if !upgraded.has_review_topology_of(&canonical) {
            return false;
        }
        *self = upgraded;
        true
    }

    /// A borrowed source stays outside the owned-agent ledger. The reviewer
    /// reads its working directory, including uncommitted changes, from its
    /// own workspace and never writes to the source tree.
    pub fn agent_review(model: Option<String>, effort: Option<String>, max_rounds: i32) -> Self {
        Self::agent_review_with_provider(None, model, effort, max_rounds)
    }

    /// Build a review graph with an explicit reviewer provider. The built-in
    /// title-bar preset uses [`Self::agent_review`] so its shared graph does
    /// not retain the provider of whichever source agent created it first.
    pub fn agent_review_with_provider(
        provider: Option<&str>,
        model: Option<String>,
        effort: Option<String>,
        max_rounds: i32,
    ) -> Self {
        let target = || Some("$source".to_string());
        let reviewer = || Some("reviewer".to_string());
        let nodes = vec![
            ("trigger", K::Manual),
            ("await_source", K::AwaitAgentTurn { target_node_id: target() }),
            ("confirm_source", K::CollaboratorCheck { require_approval: true }),
            ("source_ready", K::AnyCompleted),
            // The source publishes a PR before review so approval can end
            // with a merge of exactly what was reviewed.
            ("publish", K::InjectPty {
                prompt: crate::review_contract::PUBLISH_FOR_REVIEW.into(),
                target_node_id: target(),
            }),
            ("await_publish", K::AwaitAgentTurn { target_node_id: target() }),
            ("confirm_publish", K::CollaboratorCheck { require_approval: true }),
            ("publish_ready", K::AnyCompleted),
            ("reviewer", K::SpawnAgentNode {
                prompt: CircuitGraph::local_review_prompt(),
                name: Some("Code reviewer".into()), provider: provider.map(str::to_owned),
                model, effort, extra_args: None, timeout_seconds: None,
            }),
            ("review_round", K::AnyCompleted),
            ("verdict", K::ReviewVerdict { target_node_id: reviewer() }),
            ("feedback", K::InjectPty {
                prompt: CircuitGraph::review_feedback_prompt(crate::review_contract::LOCAL_FEEDBACK_SCOPE),
                target_node_id: target(),
            }),
            ("await_fixes", K::AwaitAgentTurn { target_node_id: target() }),
            ("retry", K::RetryLimit { max_retries: max_rounds }),
            ("re_review", K::InjectPty { prompt: CircuitGraph::re_review_prompt(), target_node_id: reviewer() }),
            ("close_approved", K::CloseAgentNode { target_node_id: reviewer() }),
            ("merge", K::InjectPty {
                prompt: crate::review_contract::merge_approved_pr("your pull request for this work", ""),
                target_node_id: target(),
            }),
            ("approved", K::Notify { message: LOCAL_APPROVED_MESSAGE.into() }),
            ("close_blocked", K::CloseAgentNode { target_node_id: reviewer() }),
            ("blocked", K::Notify { message: "Review needs attention for {{source.name}}: the reviewer could not give a verdict. See the review report in the run context.".into() }),
            ("exhausted", K::Notify { message: "Review limit reached for {{source.name}} after {{retry.max_retries}} rounds. Latest fixes have not been approved; inspect the report before continuing.".into() }),
        ];
        let edges = vec![
            ("trigger", "await_source", None),
            ("await_source", "source_ready", Some(O::Completed)),
            ("await_source", "confirm_source", Some(O::Blocked)),
            ("confirm_source", "source_ready", None),
            ("source_ready", "publish", None),
            ("publish", "await_publish", None),
            ("await_publish", "publish_ready", Some(O::Completed)),
            ("await_publish", "confirm_publish", Some(O::Blocked)),
            ("confirm_publish", "publish_ready", None),
            ("publish_ready", "reviewer", None),
            ("reviewer", "review_round", None),
            ("review_round", "verdict", None),
            ("verdict", "feedback", Some(O::Working)),
            ("feedback", "await_fixes", None),
            ("await_fixes", "retry", Some(O::Completed)),
            // RetryLimit uses its first child as its loop re-entry: later
            // rounds re-prompt the open reviewer instead of respawning it.
            ("retry", "re_review", Some(O::Completed)),
            ("retry", "exhausted", Some(O::Failed)),
            ("re_review", "review_round", None),
            ("verdict", "close_approved", Some(O::Completed)),
            ("close_approved", "merge", None),
            ("merge", "approved", None),
            ("verdict", "close_blocked", Some(O::Blocked)),
            ("close_blocked", "blocked", None),
        ];
        Self {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: nodes
                .into_iter()
                .map(|(id, kind)| CircuitNode {
                    id: id.into(),
                    kind,
                })
                .collect(),
            edges: edges
                .into_iter()
                .map(|(from, to, outcome)| CircuitEdge {
                    from: from.into(),
                    to: to.into(),
                    condition: outcome
                        .map(EdgeCondition::OnOutcome)
                        .unwrap_or(EdgeCondition::Always),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit::evaluator::Classification;
    use crate::circuit::test_support::advance_with_report_evidence;
    use crate::circuit::{context::CircuitContext, stepper::*};

    fn tick(run: &mut RunView) -> Transition {
        advance(
            run,
            &CircuitEvent::Tick(Capacity {
                circuit_free_slots: 2,
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
        result.effects.extend(tick(&mut run).effects);
        assert!(result.effects.iter().any(
            |e| matches!(e, Effect::Notify { message } if message.contains("needs attention"))
        ));
        assert!(run.step("feedback").is_none());
        assert!(run.step("merge").is_none());
        assert_eq!(run.state, RunState::Failed);
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

    #[test]
    fn stored_preset_upgrades_to_the_publication_flow_keeping_its_reviewer() {
        let mut stored = crate::circuit::test_support::pre_publication_local_review(4);
        if let K::SpawnAgentNode { prompt, .. } = &mut stored
            .nodes
            .iter_mut()
            .find(|n| n.id == "reviewer")
            .unwrap()
            .kind
        {
            *prompt = "Review the security boundaries".into();
        }
        assert!(stored.upgrade_local_review_publication_flow());
        stored.validate().unwrap();
        let canonical = CircuitGraph::agent_review(None, None, 4);
        assert!(stored.has_review_topology_of(&canonical));
        assert_eq!(
            stored.children("retry").first().map(String::as_str),
            Some("re_review")
        );
        for id in ["feedback", "publish", "re_review", "merge", "approved"] {
            assert_eq!(
                stored.node(id),
                canonical.node(id),
                "{id} uses the current stock text"
            );
        }
        assert!(matches!(&stored.node("reviewer").unwrap().kind,
            K::SpawnAgentNode { prompt, .. } if prompt == "Review the security boundaries"));
        assert!(
            !stored.upgrade_local_review_publication_flow(),
            "the upgrade is idempotent"
        );
    }
}
