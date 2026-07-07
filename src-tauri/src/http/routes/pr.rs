//! Mobile HTTP routes for the PR Probe panel — list, mergeability, and merge.
//!
//! The backing Tauri commands live in `commands::pr` (issue #417). These
//! routes expose them over the mobile HTTP transport so a future mobile
//! PR screen can list, inspect, and merge PRs without round-tripping
//! through the desktop UI. The desktop `POST /api/meshes/{id}/pr`
//! (create-PR) endpoint already lives in this module and is unchanged.

use tokio::io::AsyncReadExt;
use crate::http::MaybeTls;

use crate::db;
use crate::http::request;

/// `GET /api/meshes/{id}/pulls?state=open|closed` — list PRs for the
/// mesh's GitHub repo. The `state` query param is forwarded verbatim to
/// `commands::pr::get_repo_pulls`, which normalises any non-`"closed"`
/// value (including empty/absent) to `"open"` — so this handler does
/// no pre-processing of its own.
pub async fn list_pulls(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    mesh_id: i64,
    state: &str,
) {
    // Await the async wrapper so the blocking GitHub call runs on the blocking
    // pool, not this route's Tauri async-runtime worker (http/mod.rs spawns
    // each connection on the runtime).
    match crate::commands::pr::get_repo_pulls(mesh_id, state.to_string()).await {
        Ok(prs) => {
            let body = serde_json::to_string(&prs).unwrap_or_else(|_| "[]".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}

/// `GET /api/meshes/{id}/pulls/{n}/mergeability` — per-PR mergeability
/// enrichment. The list endpoint omits `mergeable`, so the panel calls
/// this once per PR after the list loads; `mergeable` is `null` while
/// GitHub is still computing (mirrors the desktop wire shape).
pub async fn get_mergeability(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    mesh_id: i64,
    pr_number: i64,
) {
    match crate::commands::pr::get_pr_mergeability(mesh_id, pr_number).await {
        Ok(m) => {
            let body = serde_json::to_string(&m).unwrap_or_else(|_| "{}".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
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
pub async fn merge(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    _mesh_id: i64,
    pr_number: i64,
    content_length: usize,
) {
    if content_length > 8 * 1024 {
        request::send_json_error(lines, "413 Content Too Large", "Body too large").await;
        return;
    }
    let mut body_bytes = vec![0u8; content_length];
    if content_length > 0 && lines.read_exact(&mut body_bytes).await.is_err() {
        let _ = request::write_status_only(lines, "400 Bad Request").await;
        return;
    }

    let req: MergeRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            request::send_json_error(
                lines,
                "400 Bad Request",
                &format!("Invalid JSON: {}", e),
            )
            .await;
            return;
        }
    };

    match crate::commands::pr::merge_pr(req.url).await {
        Ok(merged_url) => {
            let body = serde_json::to_string(&serde_json::json!({
                "url": merged_url,
                "pr_number": pr_number,
            }))
            .unwrap_or_else(|_| "{}".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}

/// `POST /api/meshes/{id}/pr` — create a GitHub PR for the mesh's current branch.

#[derive(serde::Deserialize)]
struct CreatePrRequest {
    title: String,
    body: String,
    base_branch: String,
}

pub async fn create(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    mesh_id: i64,
    content_length: usize,
) {
    if content_length > 64 * 1024 {
        request::send_json_error(lines, "413 Content Too Large", "Body too large").await;
        return;
    }

    let mut body_bytes = vec![0u8; content_length];
    if content_length > 0 && lines.read_exact(&mut body_bytes).await.is_err() {
        let _ = request::write_status_only(lines, "400 Bad Request").await;
        return;
    }

    let req: CreatePrRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            request::send_json_error(lines, "400 Bad Request", &format!("Invalid JSON: {}", e))
                .await;
            return;
        }
    };

    let mesh = match db::get_mesh_by_id(mesh_id) {
        Ok(m) => m,
        Err(_) => {
            request::send_json_error(lines, "404 Not Found", "Mesh not found").await;
            return;
        }
    };

    match crate::commands::pr::create_pr_for_mesh(
        mesh.path,
        req.title,
        req.body,
        req.base_branch,
    )
    .await
    {
        Ok(url) => {
            let body = serde_json::to_string(&serde_json::json!({ "url": url }))
                .unwrap_or_else(|_| "{}".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}
