// Circuit worker naming tests. Included in the existing tests scope.
// --- circuit_step_node_name ---

fn naming_view(graph: CircuitGraph, context: &[(&str, &str)]) -> RunView {
    let mut view_context = CircuitContext::new();
    for (key, value) in context {
        view_context.set(key, *value);
    }
    RunView {
        run_id: 1,
        graph,
        state: RunState::Running,
        context: view_context,
        steps: vec![],
    }
}

fn manual_spawn_graph() -> CircuitGraph {
    CircuitGraph {
        version: CIRCUIT_GRAPH_VERSION,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: "spawn".into(),
            kind: CircuitNodeKind::SpawnAgentNode {
                prompt: "work".into(),
                name: None,
                provider: None,
                model: None,
                effort: None,
                extra_args: None,
                timeout_seconds: None,
            },
        }],
        edges: vec![],
    }
}

#[test]
fn circuit_step_node_name_names_the_implementer_from_the_issue() {
    let view = naming_view(
        CircuitGraph::issue_driven_autopilot_review("autopilot"),
        &[("issue.number", "123"), ("issue.title", "Fix the widget")],
    );
    assert_eq!(
        circuit_step_node_name(&view, "implementer", None, &HashSet::new()),
        Some("gh123-fix-the-widget".to_string())
    );
}

#[test]
fn circuit_step_node_name_names_the_reviewer_from_the_pull_request() {
    let view = naming_view(
        CircuitGraph::issue_driven_autopilot_review("autopilot"),
        &[
            ("issue.number", "123"),
            ("issue.title", "Fix the widget"),
            ("pr.number", "456"),
            ("pr.title", "fix: the widget"),
        ],
    );
    assert_eq!(
        circuit_step_node_name(&view, "reviewer", None, &HashSet::new()),
        Some("pr456-review-fix-the-widget".to_string())
    );
}

#[test]
fn circuit_step_node_name_names_the_reviewer_from_the_issue_without_a_pull_request() {
    let view = naming_view(
        CircuitGraph::issue_driven_autopilot_review("autopilot"),
        &[("issue.number", "123"), ("issue.title", "Fix the widget")],
    );
    assert_eq!(
        circuit_step_node_name(&view, "reviewer", None, &HashSet::new()),
        Some("gh123-review-fix-the-widget".to_string())
    );
}

#[test]
fn circuit_step_node_name_names_an_agent_review_reviewer_from_its_source() {
    let view = naming_view(
        CircuitGraph::agent_review(None, None, 3),
        &[
            ("source.review_preset", "1"),
            ("source.name", "gh7-add-chip"),
        ],
    );
    // The preset graph authors a role label on its reviewer step; it must not
    // become the node (and worktree directory) name.
    let authored = resolve_circuit_spawn_inputs(&view.graph.node("reviewer").unwrap().kind)
        .unwrap()
        .name;
    assert_eq!(authored.as_deref(), Some("Code reviewer"));
    assert_eq!(
        circuit_step_node_name(&view, "reviewer", authored.as_deref(), &HashSet::new()),
        Some("review-gh7-add-chip".to_string())
    );
}

#[test]
fn circuit_step_node_name_disambiguates_against_taken_worktree_names() {
    let view = naming_view(
        CircuitGraph::issue_driven_autopilot_review("autopilot"),
        &[("issue.number", "123"), ("issue.title", "Fix the widget")],
    );
    let taken = HashSet::from(["gh123-fix-the-widget".to_string()]);
    assert_eq!(
        circuit_step_node_name(&view, "implementer", None, &taken),
        Some("gh123-fix-the-widget-2".to_string())
    );
}

#[test]
fn circuit_step_node_name_keeps_an_authored_name_over_issue_context() {
    let view = naming_view(
        CircuitGraph::issue_driven_autopilot_review("autopilot"),
        &[("issue.number", "123"), ("issue.title", "Fix the widget")],
    );
    assert_eq!(
        circuit_step_node_name(&view, "implementer", Some("my-step"), &HashSet::new()),
        Some("my-step".to_string())
    );
}

#[test]
fn circuit_step_node_name_keeps_an_authored_reviewer_name_outside_the_review_preset() {
    let view = naming_view(
        CircuitGraph::issue_driven_autopilot_review("autopilot"),
        &[("issue.number", "123"), ("pr.number", "456")],
    );
    assert_eq!(
        circuit_step_node_name(&view, "reviewer", Some("my-reviewer"), &HashSet::new()),
        Some("my-reviewer".to_string())
    );
}

#[test]
fn circuit_step_node_name_leaves_manual_circuits_to_the_random_default() {
    let view = naming_view(manual_spawn_graph(), &[]);
    assert_eq!(
        circuit_step_node_name(&view, "spawn", None, &HashSet::new()),
        None
    );
}

#[test]
fn circuit_step_node_name_names_a_manual_step_from_pull_request_context_alone() {
    let view = naming_view(
        manual_spawn_graph(),
        &[("pr.number", "9"), ("pr.title", "Bump deps")],
    );
    assert_eq!(
        circuit_step_node_name(&view, "spawn", None, &HashSet::new()),
        Some("pr9-bump-deps".to_string())
    );
}
