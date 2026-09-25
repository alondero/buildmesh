use super::model::{
    CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind as K, EdgeCondition, StepOutcome as O,
    CIRCUIT_GRAPH_VERSION,
};

impl CircuitGraph {
    /// Continuation accepts the review shape with editable prompts and launch settings.
    /// Changed control flow or additional actions need deliberate manual recovery.
    pub fn has_local_review_contract(&self) -> bool {
        let Some(K::RetryLimit { max_retries }) = self.node("retry").map(|node| &node.kind) else { return false; };
        if !(1..=10).contains(max_retries) { return false; }
        let expected = Self::agent_review(None, None, *max_retries);
        if self.nodes.len() != expected.nodes.len() || self.edges.len() != expected.edges.len() { return false; }
        for template in &expected.nodes {
            let Some(actual) = self.node(&template.id) else { return false; };
            let matches = match (&template.kind, &actual.kind) {
                (K::SpawnAgentNode { .. }, K::SpawnAgentNode { .. }) => true,
                (K::InjectPty { target_node_id: expected, .. }, K::InjectPty { target_node_id: actual, .. }) => expected == actual,
                (K::Notify { .. }, K::Notify { .. }) => true,
                (K::AwaitAgentTurn { target_node_id: expected }, K::LlmTurnClassifier { target_node_id: actual }) => expected == actual,
                (expected, actual) => expected == actual,
            };
            if !matches { return false; }
        }
        expected.edges.iter().all(|edge| self.edges.iter().filter(|actual| *actual == edge).count() == 1)
    }

    /// A borrowed source stays outside the owned-agent ledger. The reviewer
    /// reads its working directory, including uncommitted changes, from its
    /// own workspace and never writes to the source tree.
    pub fn agent_review(
        model: Option<String>,
        effort: Option<String>,
        max_rounds: i32,
    ) -> Self {
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
            ("reviewer", K::SpawnAgentNode {
                prompt: CircuitGraph::local_review_prompt(),
                name: Some("Code reviewer".into()), provider: provider.map(str::to_owned),
                model, effort, extra_args: None, timeout_seconds: None,
            }),
            ("verdict", K::ReviewVerdict { target_node_id: reviewer() }),
            ("feedback", K::InjectPty {
                prompt: CircuitGraph::review_feedback_prompt("An independent reviewer requested changes to your work."),
                target_node_id: target(),
            }),
            ("close_reviewer", K::CloseAgentNode { target_node_id: reviewer() }),
            ("await_fixes", K::AwaitAgentTurn { target_node_id: target() }),
            ("retry", K::RetryLimit { max_retries: max_rounds }),
            ("close_approved", K::CloseAgentNode { target_node_id: reviewer() }),
            ("approved", K::Notify { message: "Review approved for {{source.name}} (agent {{source.agent_id}}).".into() }),
            ("close_blocked", K::CloseAgentNode { target_node_id: reviewer() }),
            ("blocked", K::Notify { message: "Review needs attention for {{source.name}}: the reviewer could not give a verdict. See the review report in the run context.".into() }),
            ("exhausted", K::Notify { message: "Review limit reached for {{source.name}} after {{retry.max_retries}} rounds. Latest fixes have not been approved; inspect the report before continuing.".into() }),
        ];
        let edges = vec![
            ("trigger", "await_source", None),
            ("await_source", "source_ready", Some(O::Completed)),
            ("await_source", "confirm_source", Some(O::Blocked)),
            ("confirm_source", "source_ready", None),
            ("source_ready", "reviewer", None),
            ("reviewer", "verdict", None),
            ("verdict", "feedback", Some(O::Working)),
            ("feedback", "close_reviewer", None),
            ("close_reviewer", "await_fixes", None),
            ("await_fixes", "retry", Some(O::Completed)),
            // RetryLimit uses its first child as its loop re-entry.
            ("retry", "reviewer", Some(O::Completed)),
            ("retry", "exhausted", Some(O::Failed)),
            ("verdict", "close_approved", Some(O::Completed)),
            ("close_approved", "approved", None),
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
    use crate::autopilot::circuit::test_support::advance_with_report_evidence;
    use super::*;
    use crate::autopilot::circuit::{context::CircuitContext, stepper::*};
    use crate::autopilot::evaluator::Classification;

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
            &CircuitEvent::TurnClassified { binding: None,
                node_id: node.into(),
                classification: Some(classification),
                output: Some("review report".into()),
            },
        )
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
        crate::autopilot::circuit::test_support::advance_with_completion_evidence(
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

    fn request_fixes(run: &mut RunView) -> Vec<Effect> {
        classified(run, "verdict", Classification::Working);
        assert_eq!(run.resolve_target_agent("feedback"), Some(42));
        let feedback = advance(
            run,
            &CircuitEvent::AgentReady {
                node_id: "feedback".into(),
            },
        );
        assert!(feedback.effects.iter().any(
            |e| matches!(e, Effect::InjectPty { prompt, .. } if prompt.contains("review report"))
        ));
        assert!(feedback.effects.iter().all(|e| !matches!(e, Effect::CloseAgentNode { .. })));
        let attempt = run.step("feedback").unwrap().attempt;
        let delivered = advance(run, &CircuitEvent::PromptDelivered { node_id: "feedback".into(), attempt });
        assert!(delivered.effects.iter().any(|e| matches!(e, Effect::CloseAgentNode { target_node_id, .. } if target_node_id.as_deref() == Some("reviewer"))));
        run.step_mut("reviewer").unwrap().agent_node_id = None;
        tick(run);
        let result = classified(run, "await_fixes", Classification::Completed);
        let mut effects = result.effects;
        effects.extend(tick(run).effects);
        effects
    }

    #[test]
    fn agent_review_approval_stops_without_fixing_or_retrying() {
        let mut run = reviewing(3);
        let mut result = classified(&mut run, "verdict", Classification::Completed);
        result.effects.extend(tick(&mut run).effects);
        assert!(result.effects.iter().any(
            |e| matches!(e, Effect::CloseAgentNode { node_id, .. } if node_id == "close_approved")
        ));
        assert!(result
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Notify { message } if message.contains("approved"))));
        assert!(run.step("feedback").is_none());
        assert_eq!(run.state, RunState::Completed);
    }

    #[test]
    fn agent_review_findings_return_to_source_then_spawn_fresh_reviewer() {
        let mut run = reviewing(3);
        let effects = request_fixes(&mut run);
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::SpawnAgentNode { node_id } if node_id == "reviewer")));
        assert_eq!(run.step("reviewer").unwrap().attempt, 2);
        finish_review_turn(&mut run, 101);
        classified(&mut run, "verdict", Classification::Completed);
        tick(&mut run);
        assert_eq!(run.state, RunState::Completed);
        assert!(run.steps.iter().all(|s| s.agent_node_id != Some(42)));
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
        assert_eq!(run.state, RunState::Failed);
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
        if let K::SpawnAgentNode { prompt, model, .. } = &mut graph.nodes.iter_mut().find(|n| n.id == "reviewer").unwrap().kind {
            *prompt = "Review the security boundaries".into();
            *model = Some("gpt-6-luna".into());
        }
        assert!(graph.has_local_review_contract());
        let mut unsafe_graph = graph.clone();
        unsafe_graph.edges.iter_mut().find(|e| e.to == "close_approved").unwrap().condition = EdgeCondition::Always;
        assert!(!unsafe_graph.has_local_review_contract(), "approval cannot be bypassed");
        let mut unsafe_graph = graph.clone();
        unsafe_graph.nodes.iter_mut().find(|n| n.id == "feedback").unwrap().kind = K::InjectPty { prompt: "fix".into(), target_node_id: Some("reviewer".into()) };
        assert!(!unsafe_graph.has_local_review_contract(), "feedback must return to borrowed source");
        let mut unsafe_graph = graph.clone();
        unsafe_graph.nodes.iter_mut().find(|n| n.id == "retry").unwrap().kind = K::RetryLimit { max_retries: 0 };
        assert!(!unsafe_graph.has_local_review_contract(), "review must be bounded");
        graph.nodes.push(CircuitNode { id: "publication".into(), kind: K::Notify { message: "additional action".into() } });
        assert!(!graph.has_local_review_contract(), "continuation must not inherit extra actions");
    }
}
