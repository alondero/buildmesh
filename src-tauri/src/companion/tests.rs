use super::*;

#[test]
fn one_task_owns_the_runner_until_it_finishes_even_after_cancel() {
    let state = CompanionState::default();
    let (task, cancellation) = state.start("status".into()).unwrap();
    assert!(state.start("another".into()).is_err());
    let cancelled = state.cancel(&task.id).unwrap();
    assert_eq!(cancelled.status, "cancelling");
    assert_eq!(cancelled.revision, task.revision + 1);
    assert_eq!(state.snapshot().unwrap().revision, cancelled.revision);
    assert!(*cancellation.borrow());
    assert!(state.start("another".into()).is_err());
    state.0.lock().unwrap().as_mut().unwrap().snapshot.status = "cancelled".into();
    assert!(state.cancel(&task.id).is_err());
    assert_eq!(state.snapshot().unwrap().status, "cancelled");
    let (next, _) = state.start("next".into()).unwrap();
    assert!(next.generation > task.generation);
    assert!(state.cancel(&task.id).is_err());
}

#[tokio::test]
async fn confirmation_is_bound_to_task_revision_and_consumed_once() {
    let state = CompanionState::default();
    let (task, _) = state.start("stop failed nodes".into()).unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    state.0.lock().unwrap().as_mut().unwrap().confirmation = Some(sender);
    assert!(state.respond("retired", 0, true).is_err());
    assert!(state.respond(&task.id, 1, true).is_err());
    state.respond(&task.id, 0, true).unwrap();
    assert!(receiver.await.unwrap());
    assert!(state.respond(&task.id, 0, true).is_err());
}

#[tokio::test]
async fn cancellation_drops_pending_confirmation_without_approval() {
    let state = CompanionState::default();
    let (task, _) = state.start("stop nodes".into()).unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    state.0.lock().unwrap().as_mut().unwrap().confirmation = Some(sender);
    state.cancel(&task.id).unwrap();
    assert!(receiver.await.is_err());
    assert!(state.respond(&task.id, 0, true).is_err());
}

#[test]
fn spawn_budget_cannot_be_evaded_with_multiple_calls_or_oversized_settings() {
    let mut used = 0;
    tools::reserve_spawns(&mut used, 3, 5).unwrap();
    assert!(tools::reserve_spawns(&mut used, 3, 5).is_err());
    assert_eq!(used, 3);
    tools::reserve_spawns(&mut used, 2, 5).unwrap();
    assert!(tools::reserve_spawns(&mut used, 1, 5).is_err());
    assert!(tools::reserve_spawns(&mut 0, 11, 100).is_err());
    assert!(tools::reserve_spawns(&mut 0, 0, 5).is_err());
}

#[test]
fn model_cannot_supply_approval_or_unknown_actions() {
    assert!(serde_json::from_str::<tools::Tool>(
        r#"{"tool":"terminate_nodes","node_ids":[1],"approved":true}"#
    )
    .is_err());
    assert!(
        serde_json::from_str::<tools::Tool>(r#"{"tool":"shell","command":"echo bad"}"#).is_err()
    );
    assert!(serde_json::from_str::<tools::Tool>(r#"{"tool":"terminate_nodes"}"#).is_err());
}

#[test]
fn invalid_prompts_do_not_claim_the_runner() {
    let state = CompanionState::default();
    assert!(state.start(" ".into()).is_err());
    assert!(state.start("x".repeat(16_001)).is_err());
    assert!(state.snapshot().is_none());
}

#[test]
fn legacy_preferences_default_to_five_spawns() {
    let prefs: crate::preferences::AppPreferences = serde_json::from_str("{}").unwrap();
    assert_eq!(prefs.companion.spawn_limit, 5);
}

#[test]
fn companion_creation_preserves_frozen_launch_overrides() {
    use crate::preferences::{
        launch_configurations::{snapshot, ResolvedLaunchPlan},
        HarnessProfile,
    };
    let _isolation = crate::db::test_support::isolated();
    crate::db::write_conn().execute_batch("INSERT INTO meshes(id,name,path,use_worktree) VALUES(1,'test','/companion-fixture',0);").unwrap();
    let configuration = snapshot(ResolvedLaunchPlan {
        configuration_id: "launch/companion-test".into(),
        configuration_name: "Configured".into(),
        spawn_option_id: "codex".into(),
        harness: HarnessProfile {
            id: "codex".into(),
            name: "Codex".into(),
            harness: "codex".into(),
            runtime: Some(crate::models::EnvType::Windows),
            wsl_distro: None,
            executable: None,
        },
        route: None,
        verification: None,
        model: Some("gpt-5.6-sol".into()),
        effort: Some("high".into()),
        extra_args: None,
    });
    let request = tools::SpawnInput {
        issue_number: None,
        initial_prompt: Some("test".into()),
        launch_config_id: Some(configuration.id.clone()),
    };
    let intent = crate::agent::spawn::SpawnIntent::Prompt {
        text: "test".into(),
    };
    let node = tools::create_node(1, &request, None, &intent, Some(&configuration)).unwrap();
    let saved = crate::db::node_spawn_configuration(node.id, "codex")
        .unwrap()
        .unwrap();
    assert_eq!(saved.id, configuration.id);
    assert_eq!(saved.model, configuration.model);
    assert_eq!(saved.effort, configuration.effort);
    assert_eq!(node.env, crate::models::EnvType::Windows);
    assert!(
        node.use_worktree,
        "Companion forces a Worktree Node even on a root-node Mesh"
    );
}
