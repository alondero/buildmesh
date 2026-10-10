//! Mobile HTTP routes for the PR Probe panel — list, mergeability, and merge.
//!
//! The backing Tauri commands live in `commands::pr` (issue #417). These
//! routes expose them over the mobile HTTP transport so a future mobile
//! PR screen can list, inspect, and merge PRs without round-tripping
//! through the desktop UI. The desktop `POST /api/meshes/{id}/pr`
//! (create-PR) endpoint already lives in this module and is unchanged.

use crate::db;
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

    if db::get_mesh_by_id(mesh_id).is_err() {
        return Response::json_error("404 Not Found", "Mesh not found");
    }

    // Ownership validation at the HTTP boundary, before any work is done,
    // so a node id belonging to a different mesh gets a 403 rather than a
    // PR minted against this mesh's repository.
    match db::get_agent_node_by_id(parsed.node_id) {
        Err(_) => return Response::json_error("404 Not Found", "Agent node not found"),
        Ok(node) if node.mesh_id != mesh_id => {
            return Response::json_error(
                "403 Forbidden",
                &format!(
                    "Agent node {} does not belong to mesh {}",
                    parsed.node_id, mesh_id
                ),
            );
        }
        Ok(_) => {}
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
        crate::db::create_agent_node(
            mesh_id,
            name,
            &format!("/tmp/buildmesh-route-test/{mesh_id}/{name}"),
            "main",
            EnvType::Windows,
            "claude",
            Some(name),
            None,
            None,
            None,
            true,
            None,
            None,
            None,
        )
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
        let mesh = crate::db::create_mesh_with_base_ref(
            "route-same-branch",
            dir.path().to_str().expect("utf-8 path"),
            // No remote prefix: this repo has no configured remotes, and
            // `local_base_branch` only strips a prefix it can see as one.
            "main",
        )
        .expect("create mesh");
        let node = crate::db::create_agent_node(
            mesh.id,
            "agent-1",
            dir.path().to_str().expect("utf-8 path"),
            "main",
            EnvType::Windows,
            "claude",
            Some("agent-1"),
            None,
            None,
            None,
            // Not a worktree node, so the node's working path is the checkout.
            false,
            None,
            None,
            None,
        )
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
}
