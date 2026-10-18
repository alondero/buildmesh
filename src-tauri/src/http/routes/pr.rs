//! Mobile HTTP routes for the PR Probe panel — list, mergeability, and merge.
//!
//! The backing Tauri commands live in `commands::pr` (issue #417). These
//! routes expose them over the mobile HTTP transport so a future mobile
//! PR screen can list, inspect, and merge PRs without round-tripping
//! through the desktop UI. The desktop `POST /api/meshes/{id}/pr`
//! (create-PR) endpoint already lives in this module and is unchanged.

use crate::db;
use crate::http::read_status::read_response;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;

/// `GET /api/meshes/{id}/pulls?state=open|closed` — list PRs for the
/// mesh's GitHub repo. The `state` query param is forwarded verbatim to
/// `commands::pr::get_repo_pulls`, which normalises any non-`"closed"`
/// value (including empty/absent) to `"open"` — so this handler does
/// no pre-processing of its own.
pub async fn list_pulls(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
    let state = req.query_param("state").unwrap_or_default();
    // Await the async wrapper so the blocking GitHub call runs on the blocking
    // pool, not this route's Tauri async-runtime worker (http/server.rs spawns
    // each connection on the runtime).
    match crate::commands::pr::get_repo_pulls(mesh_id, state).await {
        Ok(prs) => {
            let body = serde_json::to_string(&prs).unwrap_or_else(|_| "[]".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

/// `GET /api/meshes/{id}/pulls/{n}/mergeability` — per-PR mergeability.
/// Survives for mobile/older clients. The desktop panel no longer calls this:
/// issue #1529 returns `mergeable` inline on the list (`GET .../pulls`) via
/// the GraphQL summaries connection. `mergeable` is `null` while GitHub is
/// still computing (mirrors the desktop wire shape).
pub async fn get_mergeability(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
    let pr_number = req.id1();
    match crate::commands::pr::get_pr_mergeability(mesh_id, pr_number).await {
        Ok(m) => {
            let body = serde_json::to_string(&m).unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

/// Body shape for `POST /api/meshes/{id}/pulls/{n}/merge`. The mobile
/// client already has the PR's `url` (returned in the list payload), so
/// the handler accepts the full URL rather than re-deriving it from the
/// path's PR number — that's the only shape `commands::pr::merge_pr`
/// understands.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MergeRequest {
    url: String,
}

/// `POST /api/meshes/{id}/pulls/{n}/merge` — merge a PR (squash +
/// delete branch, matching the desktop panel's default strategy; the
/// desktop merge-strategy dropdown passes its choice through
/// `merge_pr`'s optional `merge_method` argument, which this route
/// leaves unset). The `pr_number` from the path is echoed back in the
/// response for client convenience, but the `url` in the body is the
/// authoritative argument — `merge_pr` only understands full PR URLs.
pub async fn merge(req: &ParsedRequest) -> Response {
    let pr_number = req.id1();

    let parsed: MergeRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    match crate::commands::pr::merge_pr(parsed.url, None).await {
        Ok(merged_url) => {
            let body = serde_json::to_string(&serde_json::json!({
                "url": merged_url,
                "pr_number": pr_number,
            }))
            .unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

/// Body for `POST /api/meshes/{id}/pulls/{n}/spawn`: the head fields come
/// from the phone's `GitHubPullRequest` row, exactly what the desktop PR probe
/// hands `create_pr_node`. There is no prompt field: the backend renders the
/// review prompt from the user's template, the same render the desktop uses.
/// Unknown fields are rejected so a client that tries to send one is told.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PullSpawnRequest {
    title: String,
    head_ref: String,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    head_repo_owner: Option<String>,
    #[serde(default)]
    head_repo_clone_url: Option<String>,
    /// Spawn Option; absent means the mesh default.
    #[serde(default)]
    provider: Option<String>,
    /// Saved Launch Configuration under `provider`.
    #[serde(default)]
    configuration_id: Option<String>,
}

/// `POST /api/meshes/{id}/pulls/{n}/spawn` — start an agent on the PR's head
/// branch through the desktop PR probe's `create_pr_node`. The node row is
/// committed and returned at once; the worktree and process start in the
/// background and report through the node's status.
pub async fn spawn(req: &ParsedRequest) -> Response {
    let Some(app) = crate::http::state::app_handle() else {
        return Response::json_error("503 Service Unavailable", "App not ready");
    };
    spawn_with(req, |mesh_id, pr_number, body| async move {
        crate::commands::run_blocking("http_spawn_pull_request", move || {
            crate::commands::agent::create_pr_node_for_request(
                app.clone(),
                // The HTTP route already holds the request fields the desktop
                // IPC payload deserializes into (issue #2449), so it calls the
                // body directly rather than rebuilding a wire wrapper.
                crate::commands::agent::CreatePrNodeRequest {
                    mesh_id,
                    pr_number,
                    pr_title: body.title,
                    head_ref: body.head_ref,
                    head_sha: body.head_sha,
                    provider: body.provider,
                    head_repo_owner: body.head_repo_owner,
                    head_repo_clone_url: body.head_repo_clone_url,
                    configuration_id: body.configuration_id,
                    reviewer: None,
                },
            )
            .map(|draft| draft.node)
        })
        .await
    })
    .await
}

async fn spawn_with<F, Fut, T>(req: &ParsedRequest, create: F) -> Response
where
    F: FnOnce(i64, i64, PullSpawnRequest) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
    T: serde::Serialize,
{
    let parsed: PullSpawnRequest = match serde_json::from_slice(&req.body) {
        Ok(value) => value,
        Err(e) => return Response::json_error("400 Bad Request", &format!("Invalid JSON: {e}")),
    };
    if req.id0() <= 0 || req.id1() <= 0 {
        return Response::json_error(
            "400 Bad Request",
            "A positive mesh and pull request number are required",
        );
    }
    match create(req.id0(), req.id1(), parsed).await {
        Ok(node) => match serde_json::to_string(&node) {
            Ok(body) => Response::json("200 OK", body),
            Err(e) => Response::json_error("500 Internal Server Error", &e.to_string()),
        },
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

/// `GET /api/meshes/{id}/pr/source?node_id=N` — preview the branches a
/// create-PR would use.
///
/// Resolves through the SAME function the create route calls
/// (`resolve_pr_source_for_node`), so the pair the sheet shows before
/// submitting is by construction the pair that gets published.
///
/// Introduced with issue #2024 rank 4 / #1567: the mobile sheet used to
/// hardcode `main` as the base and display the node's branch as the source
/// while the request itself resolved the mesh root — so preview and result
/// could disagree.
pub async fn pr_source(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
    let Some(node_id) = req
        .query_param("node_id")
        .and_then(|v| v.parse::<i64>().ok())
    else {
        return Response::json_error("400 Bad Request", "node_id query parameter is required");
    };
    match crate::commands::pr::resolve_pr_source_for_node(mesh_id, node_id) {
        Ok(resolved) => {
            // Serialise the derived `PrSource` struct directly — no
            // hand-assembled JSON, so the wire shape cannot drift from the
            // Rust type the mobile client imports.
            let body = serde_json::to_string(&resolved.source).unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        // The status comes from the typed error, never from the message text.
        // Matching on a prefix was a live bug: "Agent node branch ... is the
        // same as the mesh Base Ref" also starts with "Agent node", so a
        // harmless validation case answered 403 — which the mobile client
        // reads as an auth failure and answers by wiping the session and
        // bouncing the user to the pairing screen (#2190 review).
        Err(e) => Response::json_error(e.status(), &e.to_string()),
    }
}

/// `POST /api/meshes/{id}/pr` — create a GitHub PR from an agent node's
/// worktree.
///
/// `node_id` is REQUIRED. The previous mesh-scoped shape (no node id) could
/// only resolve `mesh.path`, so it published the mesh root's branch: a
/// `main -> main` PR for a root on `main`, and the wrong branch entirely for
/// a root sitting on an unrelated feature (issue #2024 rank 4 / #1567).
#[derive(serde::Deserialize)]
struct CreatePrRequest {
    title: String,
    body: String,
    /// Node whose worktree is the PR source. Scoped to `{id}`'s mesh.
    node_id: i64,
    /// Optional target branch. When absent the mesh's `base_ref` decides it
    /// — the client is never required to assume `main`.
    #[serde(default)]
    base_branch: Option<String>,
    /// Optional assertion that the worktree is still on the branch the
    /// client previewed. Mismatch is an error, not a silent substitution.
    #[serde(default)]
    head_branch: Option<String>,
}

pub async fn create(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();

    let parsed: CreatePrRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    // Issue #2330: `.is_err()` reported every failure as a deleted mesh. A failed
    // read must not invite the client to reconcile against a deletion that
    // never happened — only a successful query matching no row is a 404.
    let mesh_lookup =
        crate::read_outcome::ReadOutcome::<crate::models::Mesh>::absent_or_unavailable(
            db::get_mesh_by_id(mesh_id),
        );
    if let Some(failure) = mesh_lookup.failure() {
        return read_response(&failure, "Mesh not found");
    }

    // Ownership validation at the HTTP boundary, before any work is done,
    // so a node id belonging to a different mesh gets a 403 rather than a
    // PR minted against this mesh's repository.
    let node_lookup =
        crate::read_outcome::ReadOutcome::<crate::models::AgentNode>::absent_or_unavailable(
            db::get_agent_node_by_id(parsed.node_id),
        );
    match node_lookup.as_present() {
        // A read that could not be performed is unavailability, not a missing
        // node — a 403 here would also log the mobile user out.
        Some(node) if node.mesh_id != mesh_id => {
            return Response::json_error(
                "403 Forbidden",
                &format!(
                    "Agent node {} does not belong to mesh {}",
                    parsed.node_id, mesh_id
                ),
            );
        }
        _ => {
            if let Some(failure) = node_lookup.failure() {
                return read_response(&failure, "Agent node not found");
            }
        }
    }

    match crate::commands::pr::create_pr_for_node_source_http(
        mesh_id,
        parsed.node_id,
        parsed.title,
        parsed.body,
        parsed.base_branch,
        parsed.head_branch,
    )
    .await
    {
        Ok(result) => {
            let body = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        // Typed, so a same-branch or moved-worktree rejection answers 422
        // rather than being reported as a server fault (#2190 review).
        Err(e) => Response::json_error(e.status(), &e.to_string()),
    }
}
#[cfg(test)]
mod tests {
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
        crate::db::create_mesh(name, &format!("/tmp/buildmesh-route-test/{name}"))
            .expect("create mesh")
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

    async fn never_create(
        _: i64,
        _: i64,
        _: PullSpawnRequest,
    ) -> Result<serde_json::Value, String> {
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
        let body = r#"{"title":"Fix deadline","head_ref":"feat/deadline","head_sha":"abc123","head_repo_owner":"octo","head_repo_clone_url":"https://github.com/octo/r.git","provider":"claude","configuration_id":"launch/saved"}"#;
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
}
