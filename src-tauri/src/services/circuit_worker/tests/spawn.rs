// Circuit worker spawn tests. Included in the existing tests scope.
// -----------------------------------------------------------------------
// `resolve_circuit_spawn_inputs` (issue #1358 / slice 3 of #1355)
//
// The pure seam that translates a `CircuitNodeKind::SpawnAgentNode` into
// the inputs the worker's impure wrapper threads into `create_pending`
// (for the `provider` column) and `SpawnRequest::with_explicit(...)`
// (for cascade layer-1 model/effort/extra_args). Mirrors the existing
// impure-wrapper / pure-core pattern this file uses for
// `reconcile_spawn_step` (line ~947) so the cascade + capability-mask
// contract can be tested without a Tauri runtime, AppHandle, or DB.
// -----------------------------------------------------------------------

fn spawn_kind(
    provider: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    extra_args: Option<&str>,
    timeout_seconds: Option<u32>,
) -> CircuitNodeKind {
    CircuitNodeKind::SpawnAgentNode {
        prompt: "implement the fix".to_string(),
        name: Some("implementer".to_string()),
        provider: provider.map(str::to_string),
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        extra_args: extra_args.map(str::to_string),
        timeout_seconds,
    }
}

#[test]
fn circuit_first_turn_uses_the_harness_submission_policy() {
    use crate::agent::launch::{initial_prompt_delivery, InitialPromptDelivery};
    assert_eq!(
        initial_prompt_delivery("claude", "implement the issue"),
        InitialPromptDelivery::AutoSubmit
    );
    assert_eq!(
        initial_prompt_delivery("codex:custom-provider", "review the PR"),
        InitialPromptDelivery::Prefill
    );
    let intent = super::spawn::circuit_spawn_intent(
        initial_prompt_delivery("claude", "implement the issue"),
        "implement the issue",
    );
    assert_eq!(
        intent,
        crate::agent::spawn::SpawnIntent::Loop {
            initial_prompt: "implement the issue".into(),
        }
    );
}

#[test]
fn circuit_first_turn_falls_back_to_pty_injection_without_prefill() {
    use crate::agent::launch::{initial_prompt_delivery, InitialPromptDelivery};
    assert_eq!(
        initial_prompt_delivery("kimi", "implement the issue"),
        InitialPromptDelivery::InjectAfterSpawn
    );
    assert_eq!(
        initial_prompt_delivery("dsh", "review the PR"),
        InitialPromptDelivery::InjectAfterSpawn
    );
}

#[test]
fn circuit_empty_first_turn_stays_fresh() {
    use crate::agent::launch::{initial_prompt_delivery, InitialPromptDelivery};
    assert_eq!(
        initial_prompt_delivery("claude", "  \n\t"),
        InitialPromptDelivery::Fresh
    );
}

/// A node-authored `provider: Some("codex")` flows through into the
/// row column AND into the resolved Provider the capability mask uses.
/// Without this seam the worker would never honour a per-step
/// provider override.
#[test]
fn circuit_spawn_resolves_provider_override() {
    let kind = spawn_kind(Some("codex"), None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).expect("valid spawn");
    assert_eq!(resolved.provider_str.as_deref(), Some("codex"));
}

#[test]
fn review_provider_selection_preserves_explicit_and_inherits_parent() {
    assert_eq!(
        inherited_review_provider(None, Some("codex")),
        Some("codex".to_string())
    );
    assert_eq!(
        inherited_review_provider(Some("  claude  "), Some("codex")),
        Some("claude".to_string())
    );
    assert_eq!(
        inherited_review_provider(Some(" \t"), Some("  kimi ")),
        Some("kimi".to_string())
    );
    assert_eq!(inherited_review_provider(None, None), None);
}

#[test]
fn circuit_review_spawn_uses_frozen_plan_before_graph_parent_and_defaults() {
    let mut context = CircuitContext::new();
    context.set("source.review_preset", "1");
    context.set("source.provider", "claude");
    context.set("review.provider", "kimi");
    let mut prefs = crate::preferences::AppPreferences::default();
    prefs.harness_defaults.insert(
        "codex".into(),
        crate::preferences::HarnessConfigValue {
            model: Some("gpt-6-luna".into()),
            effort: Some("low".into()),
            permission_mode: None,
        },
    );
    let plan =
        crate::preferences::launch_configurations::capture(&prefs, "codex", &Default::default())
            .unwrap();
    let snapshot = crate::preferences::launch_configurations::snapshot(plan);
    context.set(
        "review.launch.reviewer",
        serde_json::to_string(&snapshot).unwrap(),
    );
    let view = RunView {
        run_id: 1,
        graph: CircuitGraph::agent_review(None, None, 2),
        state: RunState::Running,
        context,
        steps: vec![],
    };
    let (provider, explicit) = resolve_review_spawn_inputs(
        &view,
        "reviewer",
        Some("claude".into()),
        ExplicitSpawnOverrides {
            model: Some("mutable-model".into()),
            effort: Some("high".into()),
            extra_args: Some("mutable-args".into()),
            timeout_seconds: Some(90),
        },
        Some("opencode"),
    );
    assert_eq!(provider.as_deref(), Some(snapshot.id.as_str()));
    assert_eq!(explicit.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(explicit.effort.as_deref(), Some("low"));
    assert_eq!(explicit.extra_args, None);
    assert_eq!(explicit.timeout_seconds, Some(90));
}

#[test]
fn review_spawn_cascade_applies_reviewer_precedence() {
    let mut context = CircuitContext::new();
    context.set("source.provider", "source-provider");
    context.set("source.model", "source-model");
    context.set("source.effort", "source-effort");
    let view = RunView {
        run_id: 1,
        graph: CircuitGraph::agent_review_with_provider(Some("graph-provider"), None, None, 2),
        state: RunState::Running,
        context,
        steps: vec![],
    };
    let (provider, explicit) = resolve_review_spawn_inputs(
        &view,
        "reviewer",
        Some("circuit-provider".into()),
        ExplicitSpawnOverrides {
            model: Some("circuit-model".into()),
            effort: Some("circuit-effort".into()),
            extra_args: None,
            timeout_seconds: None,
        },
        Some("parent-provider"),
    );
    assert_eq!(provider.as_deref(), Some("circuit-provider"));
    assert_eq!(explicit.model.as_deref(), Some("circuit-model"));
    assert_eq!(explicit.effort.as_deref(), Some("circuit-effort"));

    let mut preset_context = CircuitContext::new();
    preset_context.set("source.review_preset", "1");
    preset_context.set("source.provider", "source-provider");
    preset_context.set("source.model", "source-model");
    preset_context.set("source.effort", "source-effort");
    let preset_view = RunView {
        run_id: 2,
        graph: CircuitGraph::agent_review_with_provider(
            Some("stale-graph-provider"),
            None,
            None,
            2,
        ),
        state: RunState::Running,
        context: preset_context,
        steps: vec![],
    };
    let (provider, explicit) = resolve_review_spawn_inputs(
        &preset_view,
        "reviewer",
        Some("stale-graph-provider".into()),
        ExplicitSpawnOverrides::default(),
        Some("parent-provider"),
    );
    assert_eq!(provider.as_deref(), Some("source-provider"));
    assert_eq!(
        explicit.model, None,
        "preset metadata must not override the selected harness configuration"
    );
    assert_eq!(explicit.effort, None);

    let mut configured_context = CircuitContext::new();
    configured_context.set("source.review_preset", "1");
    configured_context.set("source.provider", "source-provider");
    configured_context.set("review.provider", "reviewer-provider");
    configured_context.set("source.model", "incompatible-source-model");
    configured_context.set("source.effort", "incompatible-source-effort");
    let configured_view = RunView {
        run_id: 3,
        graph: CircuitGraph::agent_review_with_provider(
            Some("stale-graph-provider"),
            None,
            None,
            2,
        ),
        state: RunState::Running,
        context: configured_context,
        steps: vec![],
    };
    let (provider, explicit) = resolve_review_spawn_inputs(
        &configured_view,
        "reviewer",
        Some("stale-graph-provider".into()),
        ExplicitSpawnOverrides {
            model: Some("stale-preset-model".into()),
            effort: Some("stale-preset-effort".into()),
            ..Default::default()
        },
        Some("parent-provider"),
    );
    assert_eq!(provider.as_deref(), Some("reviewer-provider"));
    assert_eq!(explicit.model, None);
    assert_eq!(explicit.effort, None);
}

#[test]
fn step_parent_resolution_handles_review_graphs_and_source_fallback() {
    let review = CircuitGraph::agent_review(None, None, 2);
    let mut context = CircuitContext::new();
    context.set("source.review_preset", "1");
    let view = RunView {
        run_id: 1,
        graph: review,
        state: RunState::Running,
        context,
        steps: vec![],
    };
    assert!(is_review_spawn_step(&view, "reviewer"));
    assert_eq!(resolve_step_parent_agent_id(&view, "reviewer"), None);

    let mut source_context = CircuitContext::new();
    source_context.set("source.review_preset", "1");
    source_context.set("source.agent_id", "77");
    let source_view = RunView {
        run_id: 3,
        graph: CircuitGraph::agent_review(None, None, 2),
        state: RunState::Running,
        context: source_context,
        steps: vec![],
    };
    assert_eq!(
        resolve_step_parent_agent_id(&source_view, "reviewer"),
        Some(77)
    );

    let issue_graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
    let issue_steps = issue_graph
        .nodes
        .iter()
        .map(|node| StepView {
            node_id: node.id.clone(),
            status: StepStatus::Completed,
            outcome: None,
            error: None,
            agent_node_id: (node.id == "implementer").then_some(42),
            attempt: 1,
        })
        .collect();
    let issue_view = RunView {
        run_id: 4,
        graph: issue_graph,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: issue_steps,
    };
    assert_eq!(
        resolve_step_parent_agent_id(&issue_view, "reviewer"),
        Some(42)
    );

    let ordinary = CircuitGraph::walking_skeleton("work");
    let plain = RunView {
        run_id: 2,
        graph: ordinary,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![],
    };
    assert!(!is_review_spawn_step(&plain, "spawn"));
}

#[test]
fn issue_review_spawn_seam_persists_parent_and_inherits_provider() {
    let _db = install_temp_db();
    let mesh = db::create_mesh(
        "issue-review-parent-provider",
        "/tmp/issue-review-parent-provider",
    )
    .unwrap();
    let source = db::create_agent_node(db::NewAgentNode {
        mesh_id: mesh.id,
        name: "Implementation",
        path: &mesh.path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "parent-provider",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: true,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let reviewer = db::create_agent_node(db::NewAgentNode {
        mesh_id: mesh.id,
        name: "Review",
        path: &mesh.path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "placeholder",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: true,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let graph = CircuitGraph::issue_driven_autopilot_review("ready-for-agent");
    let circuit = db::create_autopilot_circuit(
        mesh.id,
        "issue-review-parent-provider",
        "",
        &graph.to_json().unwrap(),
    )
    .unwrap();
    let run_id =
        db::create_circuit_run(circuit.id, mesh.id, "issue:42:ready-for-agent", "{}").unwrap();
    db::commit_circuit_advance(
        run_id,
        Some("running"),
        None,
        &[
            db::CircuitStepOp {
                node_id: "implementer".into(),
                status: "completed".into(),
                outcome: Some(Some("completed".to_string())),
                error: None,
                agent_node_id: Some(source.id),
                attempt: 1,
                fresh_attempt: false,
            },
            db::CircuitStepOp {
                node_id: "reviewer".into(),
                status: "running".into(),
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
                fresh_attempt: false,
            },
        ],
    )
    .unwrap();

    let mut view = RunView {
        run_id,
        graph,
        state: RunState::Running,
        context: CircuitContext::new(),
        steps: vec![
            StepView {
                node_id: "implementer".into(),
                status: StepStatus::Completed,
                outcome: Some(StepOutcome::Completed),
                error: None,
                agent_node_id: Some(source.id),
                attempt: 1,
            },
            StepView {
                node_id: "reviewer".into(),
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
            },
        ],
    };
    // Exercise the same production resolver used by spawn_step_agent:
    // parent discovery, conditional provider inheritance, and the
    // provider/model/effort cascade all happen inside this call.
    let ReviewSpawnResolution {
        parent_agent_node_id: parent_id,
        provider,
        explicit,
    } = resolve_review_spawn_configuration(
        &view,
        "reviewer",
        None,
        ExplicitSpawnOverrides::default(),
        Some("parent-provider"),
    );
    assert_eq!(parent_id, Some(source.id));
    assert_eq!(provider.as_deref(), Some("parent-provider"));
    assert!(explicit.model.is_none());
    assert!(explicit.effort.is_none());

    db::write_conn().execute("INSERT INTO circuit_effects(run_id,node_id,attempt,kind,state) VALUES (?1,'reviewer',1,'spawn','possible_dispatch')", [run_id]).unwrap();
    spawn::attach_spawned_agent(run_id, &mut view, "reviewer", reviewer.id, parent_id).unwrap();
    let persisted_parent = db::list_circuit_agent_ownerships()
        .unwrap()
        .into_iter()
        .find(|row| row.0 == reviewer.id && row.1 == run_id)
        .and_then(|row| row.5);
    assert_eq!(persisted_parent, Some(source.id));
}

/// The cascade layer-1 (explicit) override slot must carry the per-node
/// `model`. Empty / whitespace-only inputs collapse to absent so the
/// cascade falls through (issue #1148 AC #32).
#[test]
fn circuit_spawn_passes_model_through_explicit_override() {
    let kind = spawn_kind(Some("anthropic"), Some("opus-4-1"), None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.explicit.model.as_deref(), Some("opus-4-1"));
    assert_eq!(resolved.explicit.effort, None);
}

/// `effort` and `extra_args` ride the same explicit slot — pin both so
/// the seam doesn't accidentally drop one of them on its way through.
#[test]
fn circuit_spawn_passes_effort_and_extra_args_through_explicit_override() {
    let kind = spawn_kind(
        Some("anthropic"),
        None,
        Some("high"),
        Some("--dangerously-skip-permissions"),
        None,
    );
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.explicit.effort.as_deref(), Some("high"));
    assert_eq!(
        resolved.explicit.extra_args.as_deref(),
        Some("--dangerously-skip-permissions")
    );
}

/// Unknown provider strings stay as-is in the seam so the row
/// carries the user-authored value. The Anthropic fallback
/// (`Provider::from_db_str("...")` returning `Anthropic` for unknown
/// ids) runs downstream in `spawn_with_intent` against the row —
/// the seam does not parse it. Pin that contract so a future
/// refactor can't normalize the value too early and break the row's
/// author-visible identity.
#[test]
fn circuit_spawn_preserves_unknown_provider_string() {
    let kind = spawn_kind(Some("not-a-real-thing"), None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.provider_str.as_deref(), Some("not-a-real-thing"));
}

/// Whitespace-only model / effort / extra_args collapse to absent so
/// the cascade falls through.
#[test]
fn circuit_spawn_whitespace_overrides_collapse_to_absent() {
    let kind = spawn_kind(None, Some("   "), Some("\t\n"), Some("   \t  "), None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert!(resolved.explicit.model.is_none());
    assert!(resolved.explicit.effort.is_none());
    assert!(resolved.explicit.extra_args.is_none());
}

/// `name` is pass-through (no cascade layer owns it).
#[test]
fn circuit_spawn_name_passes_through_unchanged() {
    let kind = spawn_kind(Some("claude_code"), None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.name.as_deref(), Some("implementer"));
}

/// A non-SpawnAgentNode kind is a hard error — the seam narrows
/// before destructuring.
#[test]
fn circuit_spawn_rejects_non_spawn_kind() {
    let inject = CircuitNodeKind::InjectPty {
        prompt: "hi".into(),
        target_node_id: None,
    };
    let err = resolve_circuit_spawn_inputs(&inject).unwrap_err();
    assert!(
        err.contains("not a spawn"),
        "rejection message must name the kind: {err}"
    );
}

/// `provider: None` leaves the AST override empty. The worker then
/// resolves the effective provider through the shared explicit -> mesh ->
/// application default spawn chain before creating the
/// row, so this pure resolver remains free of database access.
#[test]
fn circuit_spawn_default_provider_is_none_when_unset() {
    let kind = spawn_kind(None, None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert!(
        resolved.provider_str.is_none(),
        "None -> resolve the mesh/application default at spawn time"
    );
}

/// #1219 review: a user-authored `timeout_seconds: Some(1800)` must
/// ride the explicit-override seam so the launch phase can log it (and
/// a future process-level watchdog can consume it without re-deriving
/// from the AST). The circuit-level watchdog reads the graph node
/// directly in `observe_waits`; this test pins the carrier behaviour
/// so the launch seam can trust `explicit.timeout_seconds`. The
/// inspector's contract is "0 or blank = inherit default".
#[test]
fn circuit_spawn_passes_timeout_seconds_through_explicit_override() {
    let kind = spawn_kind(Some("anthropic"), None, None, None, Some(1800));
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.explicit.timeout_seconds, Some(1800));
}

/// #1219 review: `Some(0)` collapses to `None` at the seam (the
/// inspector's "0 = inherit default" affordance) so the cascade
/// falls through. Without this collapse a zero-int overflow at save
/// time could request an instant expiry once the watchdog slice
/// lands. Pin the contract here.
#[test]
fn circuit_spawn_zero_timeout_seconds_collapses_to_none() {
    let kind = spawn_kind(Some("anthropic"), None, None, None, Some(0));
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert!(
        resolved.explicit.timeout_seconds.is_none(),
        "Some(0) must collapse to None so the cascade falls through"
    );
}

/// #1219 review: `None` carries through unchanged — the inspector's
/// "blank" affordance is semantically identical to "inherit the
/// orchestrator default" and must not become `Some(0)` at the seam.
#[test]
fn circuit_spawn_none_timeout_seconds_stays_none() {
    let kind = spawn_kind(Some("anthropic"), None, None, None, None);
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(resolved.explicit.timeout_seconds, None);
}

/// #1219 (round-2 review): the carrier seam must thread the value
/// end-to-end. `ResolvedCircuitSpawn` carries `explicit.timeout_seconds`
/// and is later flattened into `SpawnOptions::explicit_timeout_seconds`
/// by the orchestrator — this test asserts the carrier shape stays
/// populated so the launch phase can log the budget without
/// re-deriving from the AST. (The circuit watchdog itself reads the
/// graph node in `observe_waits`; see
/// `observe_waits_uses_spawn_step_timeout_seconds_for_wait_observed`.)
/// This exercises timeout resolution into the carrier; it does not launch a
/// process or prove the later flattening into `SpawnOptions`.
#[test]
fn circuit_spawn_carries_timeout_through_to_resolved_carrier() {
    let kind = spawn_kind(Some("anthropic"), None, None, None, Some(1800));
    let resolved = resolve_circuit_spawn_inputs(&kind).unwrap();
    assert_eq!(
        resolved.explicit.timeout_seconds,
        Some(1800),
        "carrier must thread timeout end-to-end so the orchestrator can \
         pass it into SpawnOptions"
    );
}
