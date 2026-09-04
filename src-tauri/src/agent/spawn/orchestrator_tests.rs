use super::intent;
use super::orchestrator::{
    decide_startup_resume, intent_replaces_conversation, ResumeSkipDecision,
};
use super::{
    ExplicitSpawnOverrides, IssueContext, ResumeCause, SpawnIntent, SpawnRequest,
    TerminalSize, WorktreePolicy,
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
        },
        worktree_policy: WorktreePolicy::RespectMesh,
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
/// Autopilot watcher. Pin the wording here so any future drift would
/// surface as a unit-test failure before the agent gets the wrong
/// prompt.
#[test]
fn issue_intent_builds_its_prefill_at_the_spawn_seam() {
    let intent = SpawnIntent::Issue(IssueContext {
        owner: "alondero".into(),
        repo: "buildmesh".into(),
        number: 247,
        title: "Deepen spawn pipeline".into(),
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
    assert!(src.find("SpawnInFlightClaim::try_claim(node_id)").unwrap()
        < src.find("db::clear_cli_session_id(node_id)").unwrap());
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
