//! Agent-node discovery routes for the mobile SPA.
//!
//! "Discovered nodes" are external CLI runs (Claude Code, Gemini, ...)
//! that buildmesh found on disk but doesn't yet track. The mobile flow:
//!   1. GET /api/meshes/{id}/agent-nodes/discover -> list them
//!   2. POST /api/meshes/{id}/agent-nodes/import-and-resume -> create the
//!      agent node, store the cli_session_id, and immediately spawn the
//!      agent with `--resume` so the user lands on a live terminal.
//!
//! Renamed from `sessions.rs` in issue #490: the public HTTP surface uses
//! "Agent Node" vocabulary. The old `/sessions/*` paths are kept alive in
//! the dispatcher (src/http/mod.rs) for one release as a deprecation shim
//! with a `Deprecation: true` response header.

use crate::http::MaybeTls;

use crate::db;
use crate::http::request;

pub async fn discover(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    mesh_id: i64,
) {
    let mesh = match db::get_mesh_by_id(mesh_id) {
        Ok(m) => m,
        Err(_) => {
            request::send_json_error(lines, "404 Not Found", "Mesh not found").await;
            return;
        }
    };
    match crate::services::agent_node_discovery::discover(mesh_id, &mesh.path) {
        Ok(nodes) => {
            let body = serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}

#[derive(serde::Deserialize)]
struct ImportAndResumeRequest {
    cli_session_id: String,
    branch: String,
    worktree_name: Option<String>,
    provider: Option<String>,
    #[serde(default = "default_rows")]
    rows: u16,
    #[serde(default = "default_cols")]
    cols: u16,
}
fn default_rows() -> u16 {
    24
}
fn default_cols() -> u16 {
    80
}

pub async fn import_and_resume(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    mesh_id: i64,
    content_length: usize,
) {
    let req: ImportAndResumeRequest = match request::read_json_body(lines, content_length, 64 * 1024).await {
        Ok(r) => r,
        Err(status) => {
            request::send_json_error(lines, &status, "Bad request").await;
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

    let session_name = crate::session_naming::on_spawn();
    let resolved = crate::env::resolve_agent_path(&mesh.path, None);
    let env_type = resolved.env_type;
    // Store the harness/profile id verbatim (issue #535); resolve to a concrete
    // executor only at the spawn seam. Absent provider defaults to "anthropic".
    let provider_id = req.provider.as_deref().unwrap_or("anthropic");

    let use_worktree = req.worktree_name.is_some();
    let node = match db::create_agent_node(
        mesh_id,
        &session_name,
        &mesh.path,
        &req.branch,
        env_type,
        provider_id,
        req.worktree_name.as_deref(),
        None,
        None,
        None, // source_pr_pinned_sha - HTTP route doesn't accept a pinned SHA
        use_worktree,
        None,
        None,
    ) {
        Ok(n) => n,
        Err(e) => {
            request::send_json_error(
                lines,
                "500 Internal Server Error",
                &format!("create_agent_node failed: {}", e),
            )
            .await;
            return;
        }
    };

    if let Err(e) = db::update_cli_session_id(node.id, &req.cli_session_id) {
        request::send_json_error(
            lines,
            "500 Internal Server Error",
            &format!("update_cli_session_id failed: {}", e),
        )
        .await;
        return;
    }

    let Some(app) = crate::http::app_handle() else {
        request::send_json_error(lines, "503 Service Unavailable", "App not ready").await;
        return;
    };

    if let Err(e) = crate::agent::spawn::spawn_agent_inner(
        app,
        crate::agent::spawn::SpawnOptions {
            session_id: node.id,
            provider: crate::preferences::resolve_harness_provider(provider_id),
            resume: Some(req.cli_session_id.clone()),
            rows: req.rows,
            cols: req.cols,
            prefill: None,
            node: Some(node.clone()),
        },
    )
    .await
    {
        // Don't surface as 500 - the node row exists and could still be
        // used; instead return the node + the spawn error in one payload
        // so the mobile UI can decide.
        let body = serde_json::to_string(&serde_json::json!({
            "node": node,
            "spawn_error": e,
        }))
        .unwrap_or_else(|_| "{}".to_string());
        let _ = request::write_json(lines, "207 Multi-Status", &body).await;
        return;
    }

    let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());
    let _ = request::write_json(lines, "200 OK", &body).await;
}
