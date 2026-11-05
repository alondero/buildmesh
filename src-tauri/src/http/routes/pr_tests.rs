//! Status-code contract for the create-PR routes (#2190 review).
//!
//! These pin the seam the bug lived in. `pr_source` used to choose 403 by
//! matching `Err(e) if e.starts_with("Agent node")`, and three distinct
//! failures share that prefix — so a same-branch validation case answered
//! 403. The mobile client treats 403 as an expired session
//! (`isAuthError`) and responds by clearing the token and bouncing the
//! user to the pairing screen. Asserting the message alone would not have
//! caught it; the status is the contract.
//!
//! Two things every test here must do, both learned the hard way:
//! - attach `.with_ids(...)`, because `ParsedRequest::test_get` leaves
//!   `ids` unset, so `id0()` is 0 and each test would silently take the
//!   "mesh not found" path and pass for the wrong reason;
//! - drive the future on a current-thread runtime, because the per-test
//!   database is thread-local (`db::test_support::isolated`) and a
//!   multi-threaded `#[tokio::test]` runs the route on a worker with no
//!   install, which falls through to the global `OnceCell` and panics.

use super::*;
use crate::http::router::ParsedRequest;
use crate::models::EnvType;

fn block_on<F: std::future::Future<Output = Response>>(future: F) -> Response {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
        .block_on(future)
}

fn mesh_named(name: &str) -> crate::models::Mesh {
    // Distinct paths per mesh: `meshes.path` is unique, so reusing `"."`
    // would hand back the same row and a "cross-mesh" test would silently
    // become a same-mesh one.
    crate::db::create_mesh(name, &format!("/tmp/buildmesh-route-test/{name}")).expect("create mesh")
}

fn node_in(mesh_id: i64, name: &str) -> crate::models::AgentNode {
    crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id,
        name,
        path: &format!("/tmp/buildmesh-route-test/{mesh_id}/{name}"),
        branch: "main",
        env: EnvType::Windows,
        provider: "claude",
        worktree_name: Some(name),
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: true,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .expect("create agent node")
}

fn source_request(mesh_id: i64, query: &str) -> ParsedRequest {
    ParsedRequest::test_get(&format!("/api/meshes/{mesh_id}/pr/source{query}"))
        .with_ids(Some(mesh_id), None)
}

fn create_request(mesh_id: i64, body: serde_json::Value) -> ParsedRequest {
    ParsedRequest::test_post(
        &format!("/api/meshes/{mesh_id}/pr"),
        body.to_string().as_bytes(),
    )
    .with_ids(Some(mesh_id), None)
}

#[test]
fn pr_source_reports_a_missing_node_as_404() {
    let _db = crate::db::test_support::isolated();
    let mesh = mesh_named("route-missing-node");

    let response = block_on(pr_source(&source_request(mesh.id, "?node_id=987654321")));

    assert_eq!(
        response.status, "404 Not Found",
        "an unknown node id is a missing resource, not a server fault"
    );
}

#[test]
fn pr_source_reports_a_cross_mesh_node_as_403() {
    let _db = crate::db::test_support::isolated();
    let mesh_a = mesh_named("route-mesh-a");
    let mesh_b = mesh_named("route-mesh-b");
    let node = node_in(mesh_a.id, "agent-1");

    // Asked about mesh B, with a node that belongs to mesh A. Ownership
    // is the one case that genuinely warrants 403.
    let response = block_on(pr_source(&source_request(
        mesh_b.id,
        &format!("?node_id={}", node.id),
    )));

    assert_eq!(response.status, "403 Forbidden");
}

#[test]
fn pr_source_requires_a_node_id() {
    let _db = crate::db::test_support::isolated();
    let mesh = mesh_named("route-no-node-id");

    let response = block_on(pr_source(&source_request(mesh.id, "")));

    assert_eq!(response.status, "400 Bad Request");
}

#[test]
fn create_rejects_a_node_from_another_mesh_with_403() {
    let _db = crate::db::test_support::isolated();
    let mesh_a = mesh_named("route-create-a");
    let mesh_b = mesh_named("route-create-b");
    let node = node_in(mesh_a.id, "agent-1");

    let response = block_on(create(&create_request(
        mesh_b.id,
        serde_json::json!({ "title": "t", "body": "b", "node_id": node.id }),
    )));

    assert_eq!(
        response.status, "403 Forbidden",
        "a node from another mesh must be refused before any PR work"
    );
}

#[test]
fn create_rejects_an_unknown_node_with_404() {
    let _db = crate::db::test_support::isolated();
    let mesh = mesh_named("route-create-missing");

    let response = block_on(create(&create_request(
        mesh.id,
        serde_json::json!({ "title": "t", "body": "b", "node_id": 987654321 }),
    )));

    assert_eq!(response.status, "404 Not Found");
}

#[test]
fn pr_source_reports_a_same_branch_node_as_422_not_403() {
    // The exact regression (#2190 review). A node sitting on the mesh's
    // Base Ref is an ordinary validation failure. It used to answer 403
    // because the route matched `starts_with("Agent node")` and the
    // same-branch message shares that prefix — and the mobile client
    // answers 403 by wiping the session and bouncing to pairing. If this
    // ever returns to 403, a user with a node on `main` gets logged out.
    let _db = crate::db::test_support::isolated();
    let dir = crate::env::test_helpers::TestDir::new("route-same-branch");
    let repo =
        crate::env::test_helpers::init_repo_with_commit(dir.path(), &[("README.md", "init\n")]);
    let head = repo.head().expect("head after commit");
    let commit = head.peel_to_commit().expect("head commit");
    if repo.find_branch("main", git2::BranchType::Local).is_err() {
        repo.branch("main", &commit, true).expect("create main");
    }
    repo.set_head("refs/heads/main").expect("set HEAD to main");
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
        .expect("checkout main");

    // The node works directly in the checkout and is therefore on `main`,
    // which is also the mesh Base Ref — nothing to compare.
    let mesh = crate::db::create_mesh_with_base_ref_and_worktree(
        "route-same-branch",
        dir.path().to_str().expect("utf-8 path"),
        // No remote prefix: this repo has no configured remotes, and
        // `local_base_branch` only strips a prefix it can see as one.
        "main",
        true,
    )
    .expect("create mesh");
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "agent-1",
        path: dir.path().to_str().expect("utf-8 path"),
        branch: "main",
        env: EnvType::Windows,
        provider: "claude",
        worktree_name: Some("agent-1"),
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        // Not a worktree node, so the node's working path is the checkout.
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .expect("create agent node");

    let response = block_on(pr_source(&source_request(
        mesh.id,
        &format!("?node_id={}", node.id),
    )));

    assert_eq!(
        response.status, "422 Unprocessable Entity",
        "a same-branch node is a validation failure; 403 would log the mobile user out"
    );
}

#[test]
fn create_requires_a_node_id_in_the_body() {
    // The mesh-only request shape is the original defect (#1567): it could
    // only resolve the mesh root. It must now be a loud 400, not a
    // silently well-formed body that publishes the wrong branch.
    let _db = crate::db::test_support::isolated();
    let mesh = mesh_named("route-create-legacy");

    let response = block_on(create(&create_request(
        mesh.id,
        serde_json::json!({ "title": "t", "body": "b", "base_branch": "main" }),
    )));

    assert_eq!(response.status, "400 Bad Request");
}

fn pull_spawn_request(mesh_id: i64, pr_number: i64, body: &str) -> ParsedRequest {
    ParsedRequest::test_post(
        &format!("/api/meshes/{mesh_id}/pulls/{pr_number}/spawn"),
        body.as_bytes(),
    )
    .with_ids(Some(mesh_id), Some(pr_number))
}

async fn never_create(_: i64, _: i64, _: PullSpawnRequest) -> Result<serde_json::Value, String> {
    panic!("a rejected body must not create a node")
}

#[test]
fn pull_spawn_rejects_malformed_bodies_before_creating_a_node() {
    for (mesh, pr, body) in [
        (3, 42, r#"{}"#),
        (3, 42, r#"{"title":"t"}"#),
        (3, 42, r#"{"title":"t","head_ref":7}"#),
        (
            3,
            42,
            r#"{"title":"t","head_ref":"feat/x","prompt":"review it"}"#,
        ),
        (3, 0, r#"{"title":"t","head_ref":"feat/x"}"#),
        (0, 42, r#"{"title":"t","head_ref":"feat/x"}"#),
    ] {
        let response = block_on(spawn_with(
            &pull_spawn_request(mesh, pr, body),
            never_create,
        ));
        assert_eq!(
            response.status, "400 Bad Request",
            "body {body} on {mesh}/{pr}"
        );
    }
}

#[test]
fn pull_spawn_forwards_the_phone_body_and_answers_the_created_node() {
    // The literal body the Android client sends for a saved Launch
    // Configuration row (`PullSpawnTest` pins the client half).
    let body = r#"{"title":"Fix deadline","head_ref":"feat/deadline","head_sha":"abc123","head_repo_owner":"octo","head_repo_clone_url":"https://github.com/octo/r.git","provider":"claude","configuration_id":"launch/saved","login_id":"default"}"#;
    let response = block_on(spawn_with(
        &pull_spawn_request(3, 42, body),
        |mesh, pr, request| async move {
            assert_eq!((mesh, pr), (3, 42));
            assert_eq!(
                request,
                PullSpawnRequest {
                    title: "Fix deadline".into(),
                    head_ref: "feat/deadline".into(),
                    head_sha: "abc123".into(),
                    head_repo_owner: Some("octo".into()),
                    head_repo_clone_url: Some("https://github.com/octo/r.git".into()),
                    provider: Some("claude".into()),
                    configuration_id: Some("launch/saved".into()),
                    login_id: Some("default".into()),
                }
            );
            Ok(serde_json::json!({"id": 17, "mesh_id": 3, "status": "pending"}))
        },
    ));
    assert_eq!(response.status, "200 OK");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(response.body()).unwrap(),
        serde_json::json!({"id": 17, "mesh_id": 3, "status": "pending"})
    );
}

#[test]
fn pull_spawn_mesh_default_omits_provider_and_configuration() {
    let response = block_on(spawn_with(
        &pull_spawn_request(3, 42, r#"{"title":"t","head_ref":"feat/x"}"#),
        |_, _, request| async move {
            assert_eq!(
                (request.provider, request.configuration_id, request.head_sha),
                (None, None, String::new())
            );
            Ok(serde_json::json!({"id": 1}))
        },
    ));
    assert_eq!(response.status, "200 OK");
}

#[test]
fn pull_spawn_creation_failure_is_reported_with_its_message() {
    let response = block_on(spawn_with(
        &pull_spawn_request(3, 42, r#"{"title":"t","head_ref":"feat/x"}"#),
        |_, _, _| async {
            Err::<serde_json::Value, _>("Terminal cannot take an initial prompt".to_string())
        },
    ));
    assert_eq!(response.status, "500 Internal Server Error");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(response.body()).unwrap()["error"],
        "Terminal cannot take an initial prompt"
    );
}
