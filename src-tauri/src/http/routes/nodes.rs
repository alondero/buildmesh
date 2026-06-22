//! `GET /api/nodes` and `POST /api/nodes/create`.

use tauri::Emitter;
use tokio::io::AsyncReadExt;
use crate::http::MaybeTls;

use crate::db;
use crate::http::request;

pub fn list_json() -> String {
    match db::list_agent_nodes() {
        Ok(nodes) => serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".to_string()),
        Err(_) => "[]".to_string(),
    }
}

pub async fn create(
    lines: &mut tokio::io::BufStream<MaybeTls>,
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

    #[derive(serde::Deserialize)]
    struct CreateNodeRequest {
        mesh_id: i64,
        provider: String,
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

    let req: CreateNodeRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Invalid JSON: {}", e);
            request::send_json_error(lines, "400 Bad Request", &msg).await;
            return;
        }
    };

    let mesh = match db::get_mesh_by_id(req.mesh_id) {
        Ok(m) => m,
        Err(_) => {
            request::send_json_error(lines, "400 Bad Request", "Mesh not found").await;
            return;
        }
    };

    let node = match crate::services::agent_node::create(
        req.mesh_id,
        &mesh.path,
        "main",
        Some(req.provider.as_str()),
        None, // source_issue
        None, // source_pr — generic mobile spawn, not PR-spawn (issue #450)
        None, // source_pr_pinned_sha — generic mobile spawn, no pin (issue #444)
        None, // use_worktree_override — None falls back to mesh default
        None, // name_override — none supplied on this route
    ) {
        Ok(n) => n,
        Err(e) => {
            let msg = format!("Failed to create node: {}", e);
            request::send_json_error(lines, "500 Internal Server Error", &msg).await;
            return;
        }
    };

    let Some(app) = crate::http::app_handle() else {
        request::send_json_error(lines, "503 Service Unavailable", "App not ready").await;
        return;
    };

    let node_id = node.id;
    let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());

    if let Err(e) = crate::agent::spawn::spawn_agent_inner(
        app,
        crate::agent::spawn::SpawnOptions {
            session_id: node_id,
            provider: crate::preferences::resolve_harness_provider(&req.provider),
            resume: None,
            rows: req.rows,
            cols: req.cols,
            prefill: None,
            node: Some(node),
        },
    )
    .await
    {
        let msg = format!("Failed to spawn agent: {}", e);
        request::send_json_error(lines, "500 Internal Server Error", &msg).await;
        return;
    }

    let _ = app.emit("node-created", serde_json::json!({ "id": node_id }));
    let _ = request::write_json(lines, "200 OK", &body).await;
}
