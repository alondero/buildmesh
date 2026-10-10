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
    let Some(node_id) = req.query_param("node_id").and_then(|v| v.parse::<i64>().ok()) else {
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
        // The resolver's only node-aware failure is ownership. A node from
        // another mesh is a client error, not a server fault.
        Err(e) if e.starts_with("Agent node") => Response::json_error("403 Forbidden", &e),
        Err(e) => Response::json_error("500 Internal Server Error", &e),
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

    match crate::commands::pr::create_pr_for_node_source(
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
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}
