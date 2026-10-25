// Circuit worker github tests. Included in the existing tests scope.
// -- worker panic isolation (issue #1235) -----------------------------
//
// Headline regression for the circuits worker: a single panic inside
// `run_pass` (e.g. a serde edge case on a freshly-loaded graph)
// unwinds the worker thread and the circuits stop advancing forever.
// The fix wraps each per-pass body in `run_worker_pass`. The test
// below drives the same call shape as `start_circuit_worker`'s loop
// body and asserts the second + third tick still run after the
// first panicked.

#[test]
fn circuit_worker_loop_survives_a_panicking_drive_pass() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let successes = AtomicUsize::new(0);
    for tick in 0..3 {
        let successes_ref = &successes;
        crate::process_util::run_worker_pass("circuits:drive", move || {
            if tick == 0 {
                panic!("fault injection: first drive pass panics");
            }
            successes_ref.fetch_add(1, Ordering::SeqCst);
        });
    }
    assert_eq!(
        successes.load(Ordering::SeqCst),
        2,
        "ticks 1 + 2 must succeed after tick 0 panicked"
    );
}

fn open_pr_run() -> RunView {
    let graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
    let steps = graph
        .nodes
        .iter()
        .map(|node| StepView {
            node_id: node.id.clone(),
            status: if node.id == "open_pr" {
                StepStatus::Running
            } else {
                StepStatus::Completed
            },
            outcome: None,
            error: None,
            agent_node_id: if node.id == "implementer" {
                Some(700)
            } else {
                None
            },
            attempt: 1,
        })
        .collect();
    RunView {
        run_id: 42,
        graph,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps,
    }
}

fn pushed_implementation(agent: i64) -> Result<crate::circuit::verification::WrapupState, String> {
    assert_eq!(agent, 700);
    Ok(crate::circuit::verification::WrapupState {
        dirty: false,
        pushed: true,
        branch: Some("renamed-implementation".into()),
        pr_url: None,
        pr_number: None,
        pr_required: false,
        repo_error: None,
    })
}

fn implementation_pr() -> crate::services::github::PullRequest {
    serde_json::from_value(serde_json::json!({
        "number": 314, "html_url": "https://github.com/example/repo/pull/314",
        "title": "Implementation", "head": { "ref": "renamed-implementation" }
    }))
    .unwrap()
}

fn set_open_pr_policy(run: &mut RunView, policy: Option<crate::circuit::model::OpenPrPolicy>) {
    if let Some(node) = run.graph.nodes.iter_mut().find(|node| node.id == "open_pr") {
        if let CircuitNodeKind::GithubAction { open_pr_policy, .. } = &mut node.kind {
            *open_pr_policy = policy;
        }
    }
}

#[test]
fn open_pr_acknowledges_existing_pr_and_publishes_review_context() {
    let mut run = open_pr_run();
    let event = github::ensure_open_pr(
        &run,
        "open_pr",
        Some(crate::circuit::model::OpenPrPolicy::RequireExisting),
        pushed_implementation,
        |head| {
            assert_eq!(head, "renamed-implementation");
            Ok(Some(implementation_pr()))
        },
        |_, _| panic!("existing PR must not be created again"),
    )
    .unwrap();
    let transition = advance(&mut run, &event);
    assert!(transition
        .step_writes
        .iter()
        .any(|write| write.node_id == "open_pr" && write.status == StepStatus::Completed));
    assert_eq!(run.context.get("pr.number"), Some("314"));
    assert_eq!(
        run.context.get("pr.url"),
        Some("https://github.com/example/repo/pull/314")
    );
    assert_eq!(
        run.context.get("pr.head_ref"),
        Some("renamed-implementation")
    );
}

#[test]
fn open_pr_target_is_recorded_before_lookup_or_create() {
    let run = open_pr_run();
    let calls = std::cell::RefCell::new(Vec::new());
    let event = github::ensure_open_pr_with_target(
        &run,
        "open_pr",
        None,
        pushed_implementation,
        |head| {
            assert_eq!(head, "renamed-implementation");
            calls.borrow_mut().push("target");
            Ok(())
        },
        |head| {
            assert_eq!(head, "renamed-implementation");
            assert_eq!(&*calls.borrow(), &["target"]);
            calls.borrow_mut().push("find");
            Ok(None)
        },
        |head, title| {
            assert_eq!(head, "renamed-implementation");
            assert_eq!(title, "Circuit run");
            assert_eq!(&*calls.borrow(), &["target", "find"]);
            calls.borrow_mut().push("create");
            Ok(implementation_pr())
        },
    )
    .unwrap();
    assert_eq!(&*calls.borrow(), &["target", "find", "create"]);
    assert!(matches!(
        event,
        CircuitEvent::GithubActionResult {
            success: true,
            pr_number: Some(314),
            ..
        }
    ));
}

#[test]
fn open_pr_review_requires_agent_pr_without_creating_one() {
    let error = github::ensure_open_pr(
        &open_pr_run(),
        "open_pr",
        Some(crate::circuit::model::OpenPrPolicy::RequireExisting),
        pushed_implementation,
        |_| Ok(None),
        |_, _| panic!("review blueprint delegates creation to its agent"),
    )
    .unwrap_err();
    assert!(error.contains("did not raise an open pull request"));
}

#[test]
fn open_pr_lookup_error_does_not_create_or_report_missing_pr() {
    let mut run = open_pr_run();
    set_open_pr_policy(&mut run, None);
    let error = github::ensure_open_pr(
        &run,
        "open_pr",
        None,
        pushed_implementation,
        |_| Err("GitHub unavailable".into()),
        |_, _| panic!("unknown is not absent"),
    )
    .unwrap_err();
    assert_eq!(error, "GitHub unavailable");
}

#[test]
fn open_pr_replay_after_creation_discovers_pr_without_duplicate() {
    let mut run = open_pr_run();
    set_open_pr_policy(&mut run, None);
    let created = std::cell::RefCell::new(None);
    let creates = std::cell::Cell::new(0);
    // Simulate losing the first result before its ledger commit: replay
    // the same Running snapshot against the now-existing remote PR.
    for _ in 0..2 {
        let event = github::ensure_open_pr(
            &run,
            "open_pr",
            None,
            pushed_implementation,
            |_| Ok(created.borrow().clone()),
            |head, _| {
                assert_eq!(head, "renamed-implementation");
                creates.set(creates.get() + 1);
                let pr = implementation_pr();
                let url = pr.clone();
                *created.borrow_mut() = Some(pr);
                Ok(url)
            },
        )
        .unwrap();
        assert!(matches!(
            event,
            CircuitEvent::GithubActionResult {
                success: true,
                pr_number: Some(314),
                ..
            }
        ));
    }
    assert_eq!(creates.get(), 1);
}

#[test]
fn open_pr_failed_git_observation_does_not_query_github() {
    let error = github::ensure_open_pr(
        &open_pr_run(),
        "open_pr",
        Some(crate::circuit::model::OpenPrPolicy::RequireExisting),
        |_| Err("worktree unavailable".into()),
        |_| panic!("branch identity is unknown"),
        |_, _| panic!("branch identity is unknown"),
    )
    .unwrap_err();
    assert_eq!(error, "worktree unavailable");
}

#[test]
fn determine_github_target_routes_correctly() {
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
    };
    use crate::circuit::stepper::{RunState, RunView};

    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![
            CircuitNode {
                id: "t".into(),
                kind: CircuitNodeKind::Manual,
            },
            CircuitNode {
                id: "pre_label".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::AddLabel,
                    open_pr_policy: None,
                    label: Some("in-progress".into()),
                    comment: None,
                },
            },
            CircuitNode {
                id: "open_pr".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::OpenPr,
                    open_pr_policy: Some(crate::circuit::model::OpenPrPolicy::RequireExisting),
                    label: None,
                    comment: None,
                },
            },
            CircuitNode {
                id: "post_label".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::AddLabel,
                    open_pr_policy: None,
                    label: Some("approved".into()),
                    comment: None,
                },
            },
        ],
        edges: vec![
            CircuitEdge {
                from: "t".into(),
                to: "pre_label".into(),
                condition: Default::default(),
            },
            CircuitEdge {
                from: "pre_label".into(),
                to: "open_pr".into(),
                condition: Default::default(),
            },
            CircuitEdge {
                from: "open_pr".into(),
                to: "post_label".into(),
                condition: Default::default(),
            },
        ],
    };

    let mut ctx = CircuitContext::new();
    ctx.set("issue.number", "42");
    ctx.set("pr.number", "1361");

    let view = RunView {
        run_id: 1,
        graph,
        state: RunState::Running,
        context: ctx,
        steps: vec![],
    };

    // Before OpenPr, AddLabel targets the issue (#42)
    let pre_target =
        determine_github_target(&view, "pre_label", GithubActionKind::AddLabel).unwrap();
    assert_eq!(pre_target, ("issue", 42));

    // After OpenPr, AddLabel targets the created PR (#1361)
    let post_target =
        determine_github_target(&view, "post_label", GithubActionKind::AddLabel).unwrap();
    assert_eq!(post_target, ("pr", 1361));

    // CloseIssue explicitly targets the issue (#42)
    let close_target =
        determine_github_target(&view, "post_label", GithubActionKind::CloseIssue).unwrap();
    assert_eq!(close_target, ("issue", 42));
}
