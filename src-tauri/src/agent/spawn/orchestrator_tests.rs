use super::intent;
use super::orchestrator::{
    decide_startup_resume, intent_replaces_conversation, ResumeSkipDecision,
};
use super::{
    ExplicitSpawnOverrides, IssueContext, ResumeCause, SpawnIntent, SpawnRequest, TerminalSize,
    WorktreePolicy,
};

/// `SpawnRequest` must carry an `explicit` field (issue #1155 AC #1).
/// The struct shape pin protects `spawn_with_intent` from a future
/// refactor that drops the field — the orchestrator destructures
/// `explicit` out of the request and feeds it into `SpawnOptions`.
#[test]
fn spawn_request_carries_explicit_overrides() {
    let req = SpawnRequest {
        node_id: -1,
        intent: SpawnIntent::Fresh,
        terminal_size: TerminalSize { rows: 24, cols: 80 },
        explicit: ExplicitSpawnOverrides {
            model: Some("opus-4-1".into()),
            effort: Some("high".into()),
            extra_args: None,
            timeout_seconds: None,
        },
        worktree_policy: WorktreePolicy::RespectMesh,
        lifecycle_lease: false,
        cleanup_new_node_on_handover_failure: false,
    };
    assert_eq!(req.explicit.model.as_deref(), Some("opus-4-1"));
    assert_eq!(req.explicit.effort.as_deref(), Some("high"));

    // `Default` lets spawn sites opt out via `..Default::default()`.
    assert_eq!(ExplicitSpawnOverrides::default().model, None);
    assert_eq!(ExplicitSpawnOverrides::default().effort, None);
}

// -----------------------------------------------------------------------
// SpawnRequest constructor + intent dispatch pins.
// Resolver cascade coverage lives in command_tests.rs.
// -----------------------------------------------------------------------

/// Constructor contract pin (issue #1157): `SpawnRequest::new` must
/// set `explicit` to `Default::default()` so every existing call site
/// that doesn't wire layer-1 overrides gets the layer-1-empty
/// behaviour without re-declaring the field. Without this pin a
/// future refactor that returns `Self { ... explicit: <something> }`
/// silently changes the cascade behaviour at every call site.
#[test]
fn spawn_request_new_sets_explicit_default() {
    let req = SpawnRequest::new(42, SpawnIntent::Fresh, TerminalSize::default());
    assert_eq!(req.node_id, 42);
    assert_eq!(req.terminal_size, TerminalSize { rows: 24, cols: 80 });
    assert_eq!(req.explicit, ExplicitSpawnOverrides::default());
    assert_eq!(req.explicit.model, None);
    assert_eq!(req.explicit.effort, None);
    assert!(!req.cleanup_new_node_on_handover_failure);
    assert!(
        req.with_new_node_handover_cleanup()
            .cleanup_new_node_on_handover_failure
    );
}

#[test]
fn every_non_resume_intent_replaces_a_stored_conversation() {
    assert!(intent_replaces_conversation(&SpawnIntent::Fresh));
    assert!(intent_replaces_conversation(&SpawnIntent::Loop {
        initial_prompt: "continue".into(),
    }));
    assert!(intent_replaces_conversation(&SpawnIntent::Handover {
        selected_text: "context".into(),
    }));
    assert!(!intent_replaces_conversation(&SpawnIntent::Resume {
        cause: ResumeCause::Explicit,
    }));
}

/// Issue #1180 — `SpawnIntent::initial_prompt` is the single source
/// of truth for the GitHub-issue prefill. The spawn seam (`spawn_with_intent`)
/// routes through it; so does the desktop draft response and the
/// Circuit worker. Pin the wording here so any future drift would
/// surface as a unit-test failure before the agent gets the wrong
/// prompt.
#[test]
fn issue_intent_builds_its_prefill_at_the_spawn_seam() {
    let intent = SpawnIntent::Issue(IssueContext {
        owner: "alondero".into(),
        repo: "buildmesh".into(),
        number: 247,
        title: "Deepen spawn pipeline".into(),
        template: None,
    });

    assert_eq!(
        intent
            .initial_prompt()
            .as_ref()
            .map(intent::InitialPrompt::as_str),
        Some(
            "Please work on GitHub issue #247 — Deepen spawn pipeline\n\
https://github.com/alondero/buildmesh/issues/247"
        )
    );
}

#[test]
fn new_node_handover_uses_its_saved_codex_launch_configuration() {
    let _db = crate::db::test_support::isolated();
    let root = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(root.path().to_path_buf());
    let mesh = crate::db::create_mesh("new handover", "C:/handover-fixture").unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "handover",
        path: &mesh.path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "anthropic",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let mut node = node;
    node.launch_configuration = Some(
        serde_json::from_value(serde_json::json!({
            "id": "launch/saved-reviewer", "name": "Reviewer", "spawn_option_id": "custom-codex",
            "model": null, "effort": null, "extra_args": null,
            "resolved": {
                "configuration_id": "launch/saved-reviewer", "configuration_name": "Reviewer",
                "spawn_option_id": "custom-codex",
                "harness": { "id": "custom-codex", "name": "Codex", "harness": "codex" },
                "route": null, "model": null, "effort": null, "extra_args": null
            }
        }))
        .unwrap(),
    );
    let provider = crate::agent::provider::resolve_node_provider(&node);
    assert_eq!(provider, crate::models::Provider::Codex);
    let text = "REQUEST CHANGES.\r\nAddress every finding before starting work.\r\n".repeat(40);
    let request = SpawnRequest::new(
        node.id,
        SpawnIntent::Handover {
            selected_text: text.clone(),
        },
        TerminalSize::default(),
    )
    .with_new_node_handover_cleanup();
    let prompt = super::orchestrator::prepare_handover_prompt(
        &node,
        &request.intent,
        provider,
        request.cleanup_new_node_on_handover_failure,
        Some(root.path().to_path_buf()),
    )
    .unwrap()
    .unwrap();
    let files: Vec<_> = std::fs::read_dir(
        root.path()
            .join("handovers")
            .join(format!("node-{}", node.id)),
    )
    .unwrap()
    .map(|entry| entry.unwrap().path())
    .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(std::fs::read(&files[0]).unwrap(), text.as_bytes());
    assert_eq!(
        prompt,
        format!(
            "Read and follow the complete task brief in this file: \"{}\"",
            crate::circuit::handoff::agent_visible_path(&files[0], node.env)
        )
    );
    assert!(!crate::models::Provider::Codex
        .adapter()
        .prefill_requires_pty(&prompt));

    let ordinary_prompt = SpawnIntent::Prompt { text: text.clone() };
    assert_eq!(
        super::orchestrator::prepare_initial_prompt(
            &node,
            &ordinary_prompt,
            crate::models::Provider::Codex,
            None,
        )
        .unwrap(),
        Some(text)
    );
    crate::preferences::reset_for_tests();
}

#[tokio::test(flavor = "current_thread")]
async fn new_node_handover_staging_failure_deletes_the_idle_node() {
    let db = crate::db::test_support::isolated();
    let root = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(root.path().to_path_buf());
    let mesh = crate::db::create_mesh("failed handover", "C:/failed-handover-fixture").unwrap();
    let created = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "failed-handover",
        path: &mesh.path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "codex",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let node_id = created.id;
    let node = crate::db::get_agent_node_by_id(node_id).unwrap();
    std::fs::write(root.path().join("handovers"), "blocked").unwrap();
    let database = db.handle();
    let app_data_dir = Some(root.path().to_path_buf());
    let intent = SpawnIntent::Handover {
        selected_text: "review\n".repeat(100),
    };

    let result = crate::commands::run_blocking("prepare_handover_prompt_test", move || {
        let _adopted = crate::db::test_support::adopt(&database);
        super::orchestrator::prepare_handover_prompt(
            &node,
            &intent,
            crate::models::Provider::Codex,
            true,
            app_data_dir,
        )
    })
    .await;

    let error = result.unwrap_err();
    assert!(error.contains("Could not save the handover"), "{error}");
    assert!(matches!(
        crate::db::get_agent_node_by_id(node_id),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    crate::preferences::reset_for_tests();
}

#[tokio::test(flavor = "current_thread")]
async fn existing_node_handover_staging_failure_does_not_delete_the_node() {
    let db = crate::db::test_support::isolated();
    let root = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(root.path().to_path_buf());
    let mesh = crate::db::create_mesh("existing handover", "C:/existing-handover-fixture").unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "existing-handover",
        path: &mesh.path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "codex",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let node_id = node.id;
    std::fs::write(root.path().join("handovers"), "blocked").unwrap();
    let database = db.handle();
    let request = SpawnRequest::new(
        node_id,
        SpawnIntent::Handover {
            selected_text: "review\n".repeat(100),
        },
        TerminalSize::default(),
    );
    assert!(!request.cleanup_new_node_on_handover_failure);

    let result = crate::commands::run_blocking("prepare_existing_node_handover_test", move || {
        let _adopted = crate::db::test_support::adopt(&database);
        super::orchestrator::prepare_handover_prompt(
            &node,
            &request.intent,
            crate::models::Provider::Codex,
            request.cleanup_new_node_on_handover_failure,
            Some(root.path().to_path_buf()),
        )
    })
    .await;

    let error = result.unwrap_err();
    assert!(error.contains("Could not save the handover"), "{error}");
    assert_eq!(
        crate::db::get_agent_node_by_id(node_id).unwrap().id,
        node_id
    );
    crate::preferences::reset_for_tests();
}

#[tokio::test(flavor = "current_thread")]
async fn new_node_handover_reports_cleanup_failure_and_keeps_the_row() {
    let db = crate::db::test_support::isolated();
    let root = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(root.path().to_path_buf());
    let mesh = crate::db::create_mesh("cleanup failure", "C:/cleanup-failure-fixture").unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "cleanup-failure",
        path: &mesh.path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "codex",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let node_id = node.id;
    // The trigger forces agent_node::delete to fail after staging has already
    // failed, so this test exercises the combined-error cleanup branch.
    crate::db::write_conn()
        .execute_batch(
            "CREATE TRIGGER prevent_handover_test_node_delete \
             BEFORE DELETE ON agent_nodes \
             BEGIN SELECT RAISE(ABORT, 'forced test cleanup failure'); END;",
        )
        .unwrap();
    std::fs::write(root.path().join("handovers"), "blocked").unwrap();
    let database = db.handle();
    let app_data_dir = Some(root.path().to_path_buf());
    let intent = SpawnIntent::Handover {
        selected_text: "review\n".repeat(100),
    };

    let result =
        crate::commands::run_blocking("prepare_handover_cleanup_failure_test", move || {
            let _adopted = crate::db::test_support::adopt(&database);
            super::orchestrator::prepare_handover_prompt(
                &node,
                &intent,
                crate::models::Provider::Codex,
                true,
                app_data_dir,
            )
        })
        .await;

    let error = result.unwrap_err();
    assert!(error.contains("Could not save the handover"), "{error}");
    assert!(
        error.contains("could not remove the unstarted node"),
        "{error}"
    );
    assert!(error.contains("forced test cleanup failure"), "{error}");
    assert_eq!(
        crate::db::get_agent_node_by_id(node_id).unwrap().id,
        node_id
    );
    crate::preferences::reset_for_tests();
}

// -----------------------------------------------------------------------
// Resume-skip decision surface (issue #949 regression).
//
// Pins the PR #1121 fix: when a Startup resume is not viable, the
// caller must NOT write `Idle` to `agent_nodes.status` — the node
// stays `Suspended` so the user's Resume / Regenerate affordances
// remain reachable. `decide_startup_resume` is the single source of
// truth for that contract; `spawn_with_intent`'s Skip arms call no
// sink. A future refactor that re-introduces an `on_idle` write here
// fails review by virtue of the decision being a single enum variant.
// -----------------------------------------------------------------------

#[test]
fn decide_startup_resume_no_session_id_is_skipped() {
    let d = decide_startup_resume(None, ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::SkipSuspended);
}

#[test]
fn decide_startup_resume_empty_session_id_is_skipped() {
    // Empty-string defense — `db::list_suspended_nodes`'s SQL filter
    // only catches NULL; legacy writes could leave an empty string
    // behind, so the empty case must be filtered here.
    let d = decide_startup_resume(Some(""), ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::SkipSuspended);
}

#[test]
fn decide_startup_resume_when_adapter_declines_is_skipped() {
    let d = decide_startup_resume(
        Some("uuid"),
        ResumeCause::Startup,
        false, // OpenCode, Terminal — no --resume flag, no auto-resume
    );
    assert_eq!(
        d,
        ResumeSkipDecision::SkipAdapterDeclines,
        "OpenCode/Terminal Startup resume must skip without writing Idle"
    );
}

#[test]
fn decide_startup_resume_explicit_no_session_id_is_an_error() {
    // User clicked Resume on a node that never captured a session id.
    // This is a hard error — surfacing it is the user-driven recovery
    // path; the orchestrator-side Startup path silently skips.
    let d = decide_startup_resume(None, ResumeCause::Explicit, true);
    assert_eq!(d, ResumeSkipDecision::NoSessionId);
}

#[test]
fn decide_startup_resume_explicit_with_session_id_proceeds() {
    let d = decide_startup_resume(
        Some("uuid-7"),
        ResumeCause::Explicit,
        false, // explicit cause is unaffected by auto_resume_on_startup
    );
    assert_eq!(d, ResumeSkipDecision::Proceed("uuid-7".to_string()));
}

#[test]
fn decide_startup_resume_startup_with_session_id_and_adapter_accepts_proceeds() {
    let d = decide_startup_resume(Some("uuid-7"), ResumeCause::Startup, true);
    assert_eq!(d, ResumeSkipDecision::Proceed("uuid-7".to_string()));
}

/// Issue #1550 — `Regenerate` is user-driven exactly like `Explicit`: it
/// must proceed whenever a session id was captured, and must NOT be gated
/// on `auto_resume_on_startup()`. The cause only changes the flag shape at
/// argv composition (`resume_args_fork`), never whether the spawn happens.
#[test]
fn decide_startup_resume_regenerate_behaves_like_an_explicit_resume() {
    for auto_resume in [true, false] {
        assert_eq!(
            decide_startup_resume(Some("ses_abc"), ResumeCause::Regenerate, auto_resume),
            decide_startup_resume(Some("ses_abc"), ResumeCause::Explicit, auto_resume),
            "a Regenerate resume must reach the same decision as an Explicit \
             one (auto_resume_on_startup={auto_resume})"
        );
    }
    assert_eq!(
        decide_startup_resume(None, ResumeCause::Regenerate, true),
        ResumeSkipDecision::NoSessionId,
        "no captured id means nothing to branch off — fall through to a \
         fresh launch rather than erroring"
    );
}

/// The in-flight claim is a named local of spawn_agent_inner, held until
/// after the last phase. Binding it as `_claim` (or returning it from
/// prepare as a tuple a match can ignore) reopens the #650 race.
#[test]
fn spawn_intent_holds_named_claim_before_identity_mutation_and_across_phases() {
    let src = include_str!("orchestrator.rs");
    assert!(
        src.contains("let Some(claim) = SpawnInFlightClaim::try_claim"),
        "orchestrator must acquire the in-flight claim by name before the phase calls"
    );
    assert!(
        !src.contains("let _claim") && !src.contains(", _claim"),
        "binding the claim as _claim lets a future match drop it at the prepare seam"
    );
    assert!(
        src.contains("drop(claim)"),
        "claim must stay live until after the last phase (explicit drop is the use)"
    );
    assert!(
        src.find("SpawnInFlightClaim::try_claim(node_id)").unwrap()
            < src.find("db::clear_cli_session_id(node_id)").unwrap()
    );
}

/// spawn_with_intent is the sole owner of terminal spawn-failure reporting.
#[test]
fn spawn_with_intent_is_sole_node_spawn_failed_emitter() {
    let orch = include_str!("orchestrator.rs");
    let provision = include_str!("provision.rs");
    let launch = include_str!("launch.rs");
    let prepare = include_str!("prepare.rs");
    let streams = include_str!("streams.rs");
    assert!(
        orch.contains("\"node-spawn-failed\""),
        "spawn_with_intent must emit node-spawn-failed on Err"
    );
    for (name, src) in [
        ("provision.rs", provision),
        ("launch.rs", launch),
        ("prepare.rs", prepare),
        ("streams.rs", streams),
    ] {
        assert!(
            !src.contains("\"node-spawn-failed\""),
            "{name} must return Err; spawn_with_intent owns the failure toast"
        );
    }
    assert!(
        !provision.contains("session_lifecycle::on_error"),
        "provision must not stomp resume failures to Error via on_error"
    );
}

/// The spawn failure paths record **why** they failed.
///
/// The reasonless `on_error` fallback says only that something broke, and the
/// persisted lifecycle snapshot is what a restarted app and the phone client
/// read — a red node with no cause is a dead end. Each path that can fail a
/// spawn must therefore pass its reason through `on_spawn_failed`, and the
/// guard below checks each of those files for the reasonless fallback.
#[test]
fn every_spawn_failure_path_records_its_reason() {
    let orch = include_str!("orchestrator.rs");
    let commands_agent = include_str!("../../commands/agent.rs");
    let circuit_spawn = include_str!("../../services/circuit_worker/spawn.rs");
    // Compare against whitespace-normalised source, so a rustfmt re-wrap
    // does not change what the guard matches.
    let flatten = |src: &str| src.split_whitespace().collect::<Vec<_>>().join(" ");
    for (name, src) in [
        ("orchestrator.rs", orch),
        ("commands/agent.rs", commands_agent),
        ("services/circuit_worker/spawn.rs", circuit_spawn),
    ] {
        let flat = flatten(src);
        assert!(
            !flat.contains("session_lifecycle::on_error("),
            "{name} must record why the spawn failed (on_spawn_failed), not the reasonless fallback"
        );
        assert!(
            flat.contains("on_spawn_failed("),
            "{name} must report a failed spawn through on_spawn_failed so the reason is kept"
        );
    }
    // The orchestrator hands the spawn error itself to `on_spawn_failed` —
    // not a summary of it.
    assert!(
        flatten(orch).contains("on_spawn_failed(&sink, node_id, error)"),
        "the orchestrator must pass the spawn error through as it is"
    );
}
