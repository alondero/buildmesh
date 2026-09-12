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
/// delete branch, matching the desktop panel). The `pr_number` from the
/// path is echoed back in the response for client convenience, but the
/// `url` in the body is the authoritative argument — `merge_pr` only
/// understands full PR URLs.
pub async fn merge(req: &ParsedRequest) -> Response {
    let pr_number = req.id1();

    let parsed: MergeRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    match crate::commands::pr::merge_pr(parsed.url).await {
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

/// `POST /api/meshes/{id}/pr` — create a GitHub PR for the mesh's current branch.

#[derive(serde::Deserialize)]
struct CreatePrRequest {
    title: String,
    body: String,
    base_branch: String,
}

pub async fn create(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();

    let parsed: CreatePrRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    let mesh = match db::get_mesh_by_id(mesh_id) {
        Ok(m) => m,
        Err(_) => {
            return Response::json_error("404 Not Found", "Mesh not found");
        }
    };

    match crate::commands::pr::create_pr_for_mesh(
        mesh.path,
        parsed.title,
        parsed.body,
        parsed.base_branch,
    )
    .await
    {
        Ok(url) => {
            let body = serde_json::to_string(&serde_json::json!({ "url": url }))
                .unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}
