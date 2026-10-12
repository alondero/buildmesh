//! Canonical circuit blueprints and their stock prompts.

use super::model::{
    CircuitBlueprintKind, CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition,
    GithubActionKind, OpenPrPolicy, StepOutcome, CIRCUIT_GRAPH_VERSION,
};
use super::model::{CircuitNodeKind as K, StepOutcome as O};
use super::model_node_review::LOCAL_APPROVED_MESSAGE;

impl CircuitGraph {
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
                CircuitNode {
                    id: "trigger".to_string(),
                    kind: trigger,
                },
                CircuitNode {
                    id: "spawn".to_string(),
                    kind: CircuitNodeKind::SpawnAgentNode {
                        prompt: String::new(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                        timeout_seconds: None,
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
                CircuitEdge {
                    from: "trigger".to_string(),
                    to: "spawn".to_string(),
                    condition: EdgeCondition::Always,
                },
                CircuitEdge {
                    from: "spawn".to_string(),
                    to: "inject".to_string(),
                    condition: EdgeCondition::Always,
                },
                CircuitEdge {
                    from: "inject".to_string(),
                    to: "notify".to_string(),
                    condition: EdgeCondition::Always,
                },
            ],
        }
    }

    /// PR delivery and scope used by the issue-driven review blueprint. Keep
    /// this text stable because it is also the catalog's user-visible
    /// contract; [`Self::pr_review_prompt`] adds the shared review policy.
    pub const PR_REVIEW_PROMPT: &'static str = crate::review_contract::PR_REVIEW_DELIVERY;

    const AUTONOMOUS_IMPLEMENTATION_PROMPT: &'static str = "{{issue.prefill}}\nThis is an unattended implementation run. Carry the authorized issue through implementation and verification. Make routine implementation choices and record assumptions. Do not voluntarily enter interactive plan mode or stop after writing a plan. Ask for human input only when the task cannot proceed without a material decision, permission, or missing access; state that blocker clearly. Do not expand the issue's scope.";

    /// Shared review criteria used by both the local node-review loop and the
    /// issue-driven PR reviewer. The surrounding prompt supplies only the
    /// surface-specific scope and delivery mechanism.
    pub const REVIEW_POLICY: &'static str = crate::review_contract::REVIEW_POLICY;
    pub const REVIEW_FEEDBACK_POLICY: &'static str = crate::review_contract::REVIEW_FEEDBACK_POLICY;

    pub fn local_review_prompt() -> String {
        format!(
            "{}\nReview the work of agent {{{{source.agent_id}}}} in {{{{source.path}}}} directly: inspect committed changes from the merge-base {{{{source.base_ref}}}} plus all uncommitted and untracked changes. The source task is {{{{source.name}}}}. Its latest report is {{{{source.output}}}}. Do not modify files, commit, push, post comments, or open a PR. This is review round {{{{retry.attempt}}}} of {{{{retry.max_retries}}}}.",
            Self::REVIEW_POLICY
        )
    }

    pub fn pr_review_prompt() -> String {
        format!(
            "{}\n{} The pull request URL is {{{{pr.url}}}}. Provide the same findings and verdict in your final response.",
            Self::REVIEW_POLICY,
            Self::PR_REVIEW_PROMPT
        )
    }

    pub fn review_feedback_prompt(scope: &str) -> String {
        format!("{}\n{}", scope, Self::REVIEW_FEEDBACK_POLICY)
    }

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
    /// The reviewer is spawned once and re-prompted for later rounds, so it
    /// re-checks its own earlier findings. Approval closes the reviewer and
    /// asks the implementation agent to squash-merge; the run then ends and
    /// leaves that agent open.
    pub fn issue_driven_autopilot_review(trigger_label: &str) -> Self {
        use EdgeCondition::{Always, OnOutcome};
        use StepOutcome::Completed;

        fn node(id: &str, kind: CircuitNodeKind) -> CircuitNode {
            CircuitNode {
                id: id.to_string(),
                kind,
            }
        }
        fn edge(from: &str, to: &str) -> CircuitEdge {
            CircuitEdge {
                from: from.to_string(),
                to: to.to_string(),
                condition: Always,
            }
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
                        prompt: Self::AUTONOMOUS_IMPLEMENTATION_PROMPT.to_string(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                        timeout_seconds: None,
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
                        open_pr_policy: Some(OpenPrPolicy::RequireExisting),
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
                        prompt: Self::pr_review_prompt(),
                        name: None,
                        provider: None,
                        model: None,
                        effort: None,
                        extra_args: None,
                        timeout_seconds: None,
                    },
                ),
                node("review_round", CircuitNodeKind::AnyCompleted),
                node(
                    "review_classifier",
                    CircuitNodeKind::ReviewVerdict {
                        target_node_id: Some("reviewer".to_string()),
                    },
                ),
                node(
                    "follow_feedback",
                    CircuitNodeKind::InjectPty {
                        prompt: Self::review_feedback_prompt(crate::review_contract::PR_FEEDBACK_SCOPE),
                        target_node_id: Some("implementer".to_string()),
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
                    "re_review",
                    CircuitNodeKind::InjectPty {
                        prompt: Self::re_review_prompt(),
                        target_node_id: Some("reviewer".to_string()),
                    },
                ),
                node("close_approved", CircuitNodeKind::CloseAgentNode {
                    target_node_id: Some("reviewer".to_string()),
                }),
                node(
                    "merge",
                    CircuitNodeKind::InjectPty {
                        prompt: Self::pr_merge_prompt(),
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                // The implementation agent is closed only after GitHub confirms
                // the squash-merge. Every other ending leaves it open.
                node(
                    "merge_wait",
                    CircuitNodeKind::LlmTurnClassifier {
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                node(
                    "merge_verify",
                    CircuitNodeKind::GithubAction {
                        action: GithubActionKind::ConfirmPrMerged,
                        open_pr_policy: None,
                        label: None,
                        comment: None,
                    },
                ),
                node(
                    "close_implementer",
                    CircuitNodeKind::CloseAgentNode {
                        target_node_id: Some("implementer".to_string()),
                    },
                ),
                node(
                    "merge_blocked",
                    CircuitNodeKind::Notify {
                        message: Self::MERGE_BLOCKED_MESSAGE.to_string(),
                    },
                ),
                node(
                    "merge_unconfirmed",
                    CircuitNodeKind::Notify {
                        message: Self::MERGE_UNCONFIRMED_MESSAGE.to_string(),
                    },
                ),
                node("review_exhausted", CircuitNodeKind::Notify {
                    message: "Review limit reached for PR #{{pr.number}}. Latest fixes have not been approved. Resume the saved implementation session and request a fresh review.".to_string(),
                }),
                node(
                    "complete",
                    CircuitNodeKind::Notify {
                        message: Self::PR_MERGED_MESSAGE.to_string(),
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
                edge("reviewer", "review_round"),
                edge("review_round", "review_classifier"),
                outcome("review_classifier", "follow_feedback", StepOutcome::Working),
                outcome("review_classifier", "close_approved", Completed),
                edge("close_approved", "merge"),
                edge("merge", "merge_wait"),
                outcome("merge_wait", "merge_verify", Completed),
                outcome("merge_wait", "merge_blocked", StepOutcome::Blocked),
                outcome("merge_verify", "close_implementer", Completed),
                outcome("merge_verify", "merge_unconfirmed", StepOutcome::Failed),
                edge("close_implementer", "complete"),
                edge("follow_feedback", "feedback_classifier"),
                outcome("feedback_classifier", "review_retry", Completed),
                // A RetryLimit re-enters through its first child: later
                // rounds re-prompt the open reviewer instead of respawning it.
                outcome("review_retry", "re_review", Completed),
                outcome("review_retry", "review_exhausted", StepOutcome::Failed),
                edge("re_review", "review_round"),
            ],
        }
    }

    pub const PR_APPROVED_MESSAGE: &'static str = "Review approved for PR #{{pr.number}} ({{issue.title}}). The implementation agent was asked to squash-merge it and has been handed back to you.";
    /// What the run says once the PR was verified merged and the implementer closed.
    pub const PR_MERGED_MESSAGE: &'static str = "Review approved for PR #{{pr.number}} ({{issue.title}}) and it was squash-merged. The implementation agent has been closed.";
    /// The implementer reported it could not finish the merge. It stays open.
    pub const MERGE_BLOCKED_MESSAGE: &'static str = "Merge needs attention for PR #{{pr.number}} ({{issue.title}}): the implementation agent reported that it could not complete the squash-merge. It was left open so you can finish it.";
    /// GitHub did not report the PR as merged. The implementer stays open.
    pub const MERGE_UNCONFIRMED_MESSAGE: &'static str = "Merge needs attention for PR #{{pr.number}} ({{issue.title}}): the squash-merge was not confirmed ({{merge.unconfirmed_reason}}). The implementation agent was left open so you can finish it.";
    /// Later review rounds re-prompt the same reviewer. The policy is repeated
    /// because a long-lived reviewer session may have compacted its first turn.
    pub fn re_review_prompt() -> String {
        format!(
            "{}\n{}",
            crate::review_contract::RE_REVIEW_DELIVERY,
            Self::REVIEW_POLICY
        )
    }

    pub fn pr_merge_prompt() -> String {
        crate::review_contract::merge_approved_pr(
            "PR #{{pr.number}} ({{pr.url}})",
            " {{pr.number}}",
        )
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

    #[test]
    fn walking_skeleton_is_the_manual_spawn_inject_notify_chain() {
        let g = CircuitGraph::walking_skeleton("do the thing");
        assert_eq!(g.version, CIRCUIT_GRAPH_VERSION);
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["trigger", "spawn", "inject", "notify"]);
        assert_eq!(
            g.edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("trigger", "spawn"),
                ("spawn", "inject"),
                ("inject", "notify")
            ]
        );
        match &g.node("spawn").unwrap().kind {
            CircuitNodeKind::SpawnAgentNode {
                prompt,
                name,
                provider,
                model,
                effort,
                extra_args,
                timeout_seconds,
            } => {
                assert_eq!(
                    prompt, "",
                    "spawn starts fresh — the prompt rides InjectPty"
                );
                assert_eq!(*name, None);
                assert_eq!(provider, &None);
                assert_eq!(model, &None);
                assert_eq!(effort, &None);
                assert_eq!(extra_args, &None);
                assert_eq!(timeout_seconds, &None);
            }
            other => panic!("expected spawn node, got {:?}", other),
        }
        match &g.node("inject").unwrap().kind {
            CircuitNodeKind::InjectPty {
                prompt,
                target_node_id,
            } => {
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
            CircuitNodeKind::GithubIssueLabel {
                label: "buildmesh:run".into(),
            },
            CircuitNodeKind::GithubPullRequestLabel {
                label: "review-me".into(),
            },
            CircuitNodeKind::Interval {
                interval_seconds: 300,
            },
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
        g.validate()
            .expect("review blueprint must pass graph validation");
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
            g.node("open_pr").map(|n| &n.kind),
            Some(CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                open_pr_policy: Some(OpenPrPolicy::RequireExisting),
                ..
            })
        ));
        assert!(matches!(
            g.node("implementer").map(|n| &n.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt == CircuitGraph::AUTONOMOUS_IMPLEMENTATION_PROMPT
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
            Some(CircuitNodeKind::CollaboratorCheck {
                require_approval: true
            })
        ));
        assert!(
            g.node("review_prompt").is_none(),
            "the review instruction must be the reviewer's spawn-time first turn"
        );
        assert!(
            g.node("close_reviewer").is_none(),
            "the reviewer stays open between rounds"
        );
        assert!(matches!(
            g.node("re_review").map(|n| &n.kind),
            Some(CircuitNodeKind::InjectPty { target_node_id, prompt })
                if target_node_id.as_deref() == Some("reviewer")
                    && prompt.contains("{{retry.attempt}}")
                    && prompt.contains(CircuitGraph::REVIEW_POLICY)
        ));
        assert!(matches!(
            g.node("merge").map(|n| &n.kind),
            Some(CircuitNodeKind::InjectPty { target_node_id, prompt })
                if target_node_id.as_deref() == Some("implementer")
                    && prompt.contains("gh pr merge {{pr.number}} --squash")
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
        assert_eq!(
            g.children("review_retry").first().map(String::as_str),
            Some("re_review"),
            "review retry must re-enter by re-prompting the reviewer"
        );
        assert_eq!(g.children("close_approved"), vec!["merge".to_string()]);
        assert_eq!(g.children("merge"), vec!["merge_wait".to_string()]);
        assert_eq!(
            g.children("close_implementer"),
            vec!["complete".to_string()]
        );
        assert!(matches!(
            g.node("complete").map(|n| &n.kind),
            Some(CircuitNodeKind::Notify { message }) if message.contains("squash-merged")
        ));

        let parsed = CircuitGraph::from_json(&g.to_json().unwrap()).unwrap();
        assert_eq!(parsed, g);
    }

    /// Where the implementation agent can be closed from. The implementer is the
    /// user's work: only a squash-merge that GitHub confirmed may close it.
    #[test]
    fn issue_review_closes_the_implementer_only_after_a_verified_merge() {
        let g = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let closers: Vec<&str> = g
            .nodes
            .iter()
            .filter(|node| {
                matches!(&node.kind, CircuitNodeKind::CloseAgentNode { target_node_id }
                    if target_node_id.as_deref() == Some("implementer"))
            })
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(
            closers,
            vec!["close_implementer"],
            "one close, nowhere else"
        );

        let into_close: Vec<_> = g
            .edges
            .iter()
            .filter(|e| e.to == "close_implementer")
            .collect();
        assert_eq!(into_close.len(), 1);
        assert_eq!(into_close[0].from, "merge_verify");
        assert_eq!(
            into_close[0].condition,
            EdgeCondition::OnOutcome(StepOutcome::Completed),
            "only a confirmed merge reaches the close"
        );
        assert!(matches!(
            g.node("merge_verify").map(|n| &n.kind),
            Some(CircuitNodeKind::GithubAction {
                action: GithubActionKind::ConfirmPrMerged,
                ..
            })
        ));

        // The unhappy endings never reach the close and say the agent stays open.
        for id in ["merge_blocked", "merge_unconfirmed"] {
            assert!(g.children(id).is_empty(), "{id} ends the run");
            assert!(matches!(
                g.node(id).map(|n| &n.kind),
                Some(CircuitNodeKind::Notify { message }) if message.contains("left open")
            ));
        }
        let routed = |from: &str, to: &str, outcome: StepOutcome| {
            g.edges.iter().any(|e| {
                e.from == from && e.to == to && e.condition == EdgeCondition::OnOutcome(outcome)
            })
        };
        assert!(routed("merge_wait", "merge_blocked", StepOutcome::Blocked));
        assert!(routed(
            "merge_verify",
            "merge_unconfirmed",
            StepOutcome::Failed
        ));
    }

    #[test]
    fn review_prompt_keeps_pr_delivery_separate_from_shared_policy() {
        assert_eq!(
            CircuitGraph::PR_REVIEW_PROMPT,
            "Review PR {{pr.number}} and post the findings as a PR comment."
        );
        assert!(!CircuitGraph::PR_REVIEW_PROMPT.contains("grumpy senior"));
        assert!(CircuitGraph::REVIEW_POLICY.contains("reviewed commit or revision under review"));
        assert!(CircuitGraph::local_review_prompt()
            .contains("reviewed commit or revision under review"));
        assert!(CircuitGraph::pr_review_prompt().contains("post the findings as a PR comment"));
    }
}
