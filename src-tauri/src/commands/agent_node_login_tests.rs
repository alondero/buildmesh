//! Node creation folds the login picked in the spawn menu into the frozen
//! launch snapshot, so the node launches and resumes with that login.

use crate::preferences::harness_logins::HarnessLogin;
use crate::preferences::spawn_configurations::SpawnConfiguration;

const WORK_HOME: &str = "C:/Users/u/.codex-accounts/work";

fn setup() -> (tempfile::TempDir, i64) {
    let root = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(root.path().join("prefs"));
    crate::preferences::storage::try_update(|prefs| {
        prefs.harness_logins.push(HarnessLogin {
            id: "work".into(),
            harness_id: "codex".into(),
            label: "Work".into(),
            home: WORK_HOME.into(),
        });
        prefs.spawn_configurations.push(SpawnConfiguration {
            id: "launch/codex-high".into(),
            name: "Codex high".into(),
            spawn_option_id: "codex".into(),
            effort: Some("high".into()),
            ..Default::default()
        });
        Ok(())
    })
    .unwrap();
    let mesh_id = crate::db::create_mesh(
        "login-fold",
        &format!("/tmp/buildmesh_login_fold_{}", std::process::id()),
    )
    .unwrap()
    .id;
    (root, mesh_id)
}

fn frozen_login(node_id: i64) -> Option<String> {
    crate::db::node_spawn_configuration(node_id, "codex")
        .unwrap()
        .and_then(|configuration| configuration.resolved)
        .and_then(|plan| plan.harness_login)
        .map(|login| login.home)
}

#[test]
fn a_login_pick_is_frozen_on_the_node_for_defaults_and_saved_configurations() {
    let _db = crate::db::test_support::shared_global_db();
    let (_root, mesh_id) = setup();

    let defaults = super::create_agent_node_with_login_blocking(
        mesh_id,
        "main",
        Some("codex"),
        Some(false),
        None,
        None,
        Some("work"),
    )
    .unwrap();
    assert_eq!(frozen_login(defaults.id).as_deref(), Some(WORK_HOME));

    let saved = super::create_agent_node_with_login_blocking(
        mesh_id,
        "main",
        Some("codex"),
        Some(false),
        Some("launch/codex-high"),
        None,
        Some("work"),
    )
    .unwrap();
    assert_eq!(frozen_login(saved.id).as_deref(), Some(WORK_HOME));

    let unpicked = super::create_agent_node_with_login_blocking(
        mesh_id,
        "main",
        Some("codex"),
        Some(false),
        Some("launch/codex-high"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(frozen_login(unpicked.id), None, "no pick keeps the default");
}
