use super::model::{
    CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind as K, EdgeCondition, StepOutcome as O,
    CIRCUIT_GRAPH_VERSION,
};

impl CircuitGraph {
    /// A borrowed source stays outside the owned-agent ledger. The reviewer
    /// reads its working directory, including uncommitted changes, from its
    /// own workspace and never writes to the source tree.
    pub fn agent_review(
        provider: &str,
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
                prompt: concat!(
                    "Review the work of agent {{source.agent_id}} in {{source.path}}. ",
                    "Read that directory directly: review committed changes from the merge-base with ",
                    "{{source.base_ref}} and all uncommitted/untracked changes. The source task is {{source.name}}. ",
                    "Its latest report is: {{source.output}}\n",
                    "Inspect the code and relevant project instructions. Do not modify files, commit, push, ",
                    "post comments, or open a PR. Report actionable findings with file locations in your final response. ",
                    "If the work is satisfactory, explicitly state that you approve and have no remaining findings. ",
                    "Otherwise explicitly state that changes are requested. If you cannot assess the work, explain ",
                    "the blocker. This is review round {{retry.attempt}} of {{retry.max_retries}}."
                ).into(),
                name: Some("Code reviewer".into()), provider: Some(provider.into()),
                model, effort, extra_args: None, timeout_seconds: None,
            }),
            ("verdict", K::ReviewVerdict { target_node_id: reviewer() }),
            ("feedback", K::InjectPty {
                prompt: "An independent reviewer requested changes to your work. Review report:\n{{node.reviewer.output}}\nAddress every valid finding, run relevant checks, and report your changes. Explain any finding you disagree with. Another independent review will follow. Do not start another review loop yourself.".into(),
                target_node_id: target(),
            }),
            ("close_reviewer", K::CloseAgentNode { target_node_id: reviewer() }),
            ("await_fixes", K::LlmTurnClassifier { target_node_id: target() }),
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
    use super::*;
    use crate::autopilot::circuit::{context::CircuitContext, stepper::*};
    use crate::autopilot::evaluator::Classification;

    fn tick(run: &mut RunView) -> Transition {
        advance(
            run,
            &CircuitEvent::Tick(Capacity {
                circuit_free_slots: 2,
                mesh_agent_free_slots: 1,
            }),
        )
    }

    fn classified(run: &mut RunView, node: &str, classification: Classification) -> Transition {
        advance(
            run,
            &CircuitEvent::TurnClassified {
                node_id: node.into(),
                classification: Some(classification),
                output: Some("review report".into()),
            },
        )
    }

    fn reviewing(rounds: i32) -> RunView {
        let graph = CircuitGraph::agent_review("claude", None, None, rounds);
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

    fn finish_review_turn(run: &mut RunView, id: i64) {
        run.attach_agent_node("reviewer", id);
        advance(
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
        assert!(feedback.effects.iter().any(|e| matches!(e, Effect::CloseAgentNode { target_node_id, .. } if target_node_id.as_deref() == Some("reviewer"))));
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
        assert_eq!(run.state, RunState::Completed);
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
