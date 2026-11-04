//! Node creation folds the login picked in the spawn menu into the frozen
//! launch snapshot, so the node launches and resumes with that login.

use crate::preferences::harness_logins::HarnessLogin;
use crate::preferences::spawn_configurations::SpawnConfiguration;
use crate::preferences::PreferencesStore;

const WORK_HOME: &str = "C:/Users/u/.codex-accounts/work";

fn setup() -> (tempfile::TempDir, PreferencesStore, i64) {
    let root = tempfile::tempdir().unwrap();
    // The store is this test's own: the login and the saved configuration it
    // seeds are stored in *that* profile, never in an ambient one
    // (issue #1482).
    let store = PreferencesStore::new(root.path().join("prefs"));
    store
        .try_update(|prefs| {
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
    (root, store, mesh_id)
}

/// Create a node with the login picked in the spawn menu, over the store this
/// test owns.
///
/// [`super::create_agent_node_with_login_blocking`] resolves the saved
/// configuration and the login pick through the ambient façade, which a test
/// may no longer install (issue #1482). This drives the *same* building
/// blocks — [`resolve_saved_from`], [`with_login_pick`] and the store-scoped
/// create runner — over the test's own store, so the login this pick names
/// is the one the test seeded.
///
/// [`resolve_saved_from`]: crate::preferences::spawn_configurations::resolve_saved_from
/// [`with_login_pick`]: crate::preferences::spawn_configurations::with_login_pick
fn create_with_login(
    store: &PreferencesStore,
    mesh_id: i64,
    configuration_id: Option<&str>,
    login_id: Option<&str>,
) -> Result<crate::models::AgentNode, String> {
    let option = "codex";
    let configuration = crate::preferences::spawn_configurations::resolve_saved_from(
        store,
        option,
        configuration_id,
    )
    .map_err(|error| error.to_string())?;
    let configuration = match login_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(login) => {
            let prefs = store.load()?;
            crate::preferences::spawn_configurations::with_login_pick(
                &prefs,
                option,
                configuration,
                Some(login),
            )?
        }
        None => configuration,
    };
    crate::services::agent_node::create_blocking_configured_from(
        store,
        crate::services::agent_node::NodeCreateParams {
            mesh_id,
            // The runner reads the path from the mesh row it looks up.
            path: "",
            branch: "main",
            provider: Some(option),
            source_issue: None,
            source_pr: None,
            source_pr_pinned_sha: None,
            use_worktree_override: Some(false),
            name_override: None,
            head_repo_owner: None,
            head_repo_clone_url: None,
            configuration: configuration.as_ref(),
            initial_prompt: None,
            operation: None,
        },
        false,
    )
    .map(|created| created.node)
    .map_err(|error| error.to_string())
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
    let (_root, store, mesh_id) = setup();

    let defaults = create_with_login(&store, mesh_id, None, Some("work")).unwrap();
    assert_eq!(frozen_login(defaults.id).as_deref(), Some(WORK_HOME));

    let saved =
        create_with_login(&store, mesh_id, Some("launch/codex-high"), Some("work")).unwrap();
    assert_eq!(frozen_login(saved.id).as_deref(), Some(WORK_HOME));

    let unpicked = create_with_login(&store, mesh_id, Some("launch/codex-high"), None).unwrap();
    assert_eq!(frozen_login(unpicked.id), None, "no pick keeps the default");
}
