use super::*;
use crate::preferences::harness_logins::HarnessLogin;
use crate::preferences::spawn_configurations::SpawnConfiguration;
use crate::preferences::PreferencesStore;

fn fixture() -> (tempfile::TempDir, PreferencesStore, i64) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("repo");
    let repo = git2::Repository::init(&path).unwrap();
    repo.remote("origin", "https://github.com/example/spawn-menu.git")
        .unwrap();
    let mesh = crate::db::create_mesh("Spawn Menu", path.to_str().unwrap()).unwrap();
    let store = PreferencesStore::new(root.path().join("prefs"));
    store
        .try_update(|prefs| {
            prefs.harness_logins.push(HarnessLogin {
                id: "work".into(),
                harness_id: "codex".into(),
                label: "Work".into(),
                home: "C:/Users/u/.codex-accounts/work".into(),
            });
            prefs.spawn_configurations.push(SpawnConfiguration {
                id: "launch/codex-high".into(),
                name: "High".into(),
                spawn_option_id: "codex".into(),
                effort: Some("high".into()),
                login_id: Some("work".into()),
                ..Default::default()
            });
            Ok(())
        })
        .unwrap();
    (root, store, mesh.id)
}

fn frozen(node_id: i64) -> SpawnConfiguration {
    crate::db::node_spawn_configuration(node_id, "codex")
        .unwrap()
        .unwrap()
}

#[test]
fn issue_and_pr_spawns_freeze_the_login_and_configuration_from_the_common_menu() {
    let _db = crate::db::test_support::isolated();
    let (_root, store, mesh_id) = fixture();
    for (index, configuration_id) in [None, Some("launch/codex-high")].into_iter().enumerate() {
        for login_id in ["work", "default"] {
            let issue = create_issue_node_impl_with(
                Some(&store),
                mesh_id,
                index as i64 + 10,
                "Issue".into(),
                Some("codex".into()),
                configuration_id.map(str::to_string),
                Some(login_id.into()),
            )
            .unwrap()
            .0
            .node;
            let pr = create_pr_node_impl_configured_from(
                &store,
                CreatePrNodeRequest {
                    mesh_id,
                    pr_number: index as i64 + 20,
                    pr_title: "PR".into(),
                    head_ref: "main".into(),
                    head_sha: String::new(),
                    provider: Some("codex".into()),
                    head_repo_owner: None,
                    head_repo_clone_url: None,
                    configuration_id: configuration_id.map(str::to_string),
                    login_id: Some(login_id.into()),
                    reviewer: None,
                },
            )
            .unwrap()
            .0
            .node;
            for node in [issue, pr] {
                let configuration = frozen(node.id);
                let plan = configuration.resolved.unwrap();
                assert_eq!(
                    plan.harness_login.as_ref().map(|login| login.id.as_str()),
                    (login_id == "work").then_some("work")
                );
                assert_eq!(plan.effort.as_deref(), configuration_id.map(|_| "high"));
            }
        }
    }
    assert_eq!(
        store.load().unwrap().spawn_configurations[0]
            .login_id
            .as_deref(),
        Some("work")
    );
}

#[test]
fn review_circuit_freezes_the_login_pick_and_retry_keeps_the_first_selection() {
    let _db = crate::db::test_support::isolated();
    let (_root, store, mesh_id) = fixture();
    let source = create_issue_node_impl_with(
        Some(&store),
        mesh_id,
        42,
        "Review".into(),
        Some("codex".into()),
        None,
        None,
    )
    .unwrap()
    .0
    .node;
    crate::db::update_agent_node_status(source.id, crate::models::SessionStatus::Ready).unwrap();
    let run_id = crate::db::create_node_circuit_run_with_login(
        Some(&store),
        source.id,
        None,
        3,
        Some("launch/codex-high".into()),
        Some("default".into()),
    )
    .unwrap();
    let read = || {
        let run = crate::db::get_circuit_run(run_id).unwrap().unwrap();
        let context =
            crate::circuit::context::CircuitContext::from_json(&run.context_json).unwrap();
        serde_json::from_str::<SpawnConfiguration>(context.get("review.launch.reviewer").unwrap())
            .unwrap()
    };
    let first = read();
    assert_eq!(first.effort.as_deref(), Some("high"));
    assert!(
        first.resolved.as_ref().unwrap().harness_login.is_none(),
        "explicit Default overrides the saved Work pin"
    );
    let retry = crate::db::create_node_circuit_run_with_login(
        Some(&store),
        source.id,
        None,
        9,
        Some("codex".into()),
        Some("deleted".into()),
    )
    .unwrap();
    assert_eq!(retry, run_id);
    assert_eq!(read(), first);
    let source2 = create_issue_node_impl_with(
        Some(&store),
        mesh_id,
        43,
        "Other review".into(),
        Some("codex".into()),
        None,
        None,
    )
    .unwrap()
    .0
    .node;
    crate::db::update_agent_node_status(source2.id, crate::models::SessionStatus::Ready).unwrap();
    assert!(crate::db::create_node_circuit_run_with_login(
        Some(&store),
        source2.id,
        None,
        3,
        Some("codex".into()),
        Some("deleted".into())
    )
    .is_err());
    let accepted = crate::db::create_node_circuit_run_with_login(
        Some(&store),
        source2.id,
        None,
        3,
        Some("codex".into()),
        Some("work".into()),
    )
    .unwrap();
    let run = crate::db::get_circuit_run(accepted).unwrap().unwrap();
    let context = crate::circuit::context::CircuitContext::from_json(&run.context_json).unwrap();
    let configuration: SpawnConfiguration =
        serde_json::from_str(context.get("review.launch.reviewer").unwrap()).unwrap();
    assert_eq!(
        configuration.resolved.unwrap().harness_login.unwrap().id,
        "work"
    );
}
