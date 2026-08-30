//! `GET /api/nodes` and `POST /api/nodes/create`.

use tauri::Emitter;
use crate::http::MaybeTls;

use crate::db;
use crate::http::request;

pub async fn list_json() -> String {
    match crate::commands::run_blocking("http_list_nodes", || {
        db::list_agent_nodes().map_err(|e| e.to_string())
    })
    .await
    {
        Ok(nodes) => serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".to_string()),
        Err(_) => "[]".to_string(),
    }
}

pub async fn create(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    content_length: usize,
) {
    let Some(body_bytes) =
        request::read_body_or_send_error(lines, content_length, 64 * 1024).await
    else {
        return;
    };

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

    let mesh_id = req.mesh_id;
    let provider = req.provider;
    let mesh = match crate::commands::run_blocking("http_create_node_mesh", move || {
        db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(m) => m,
        Err(_) => {
            request::send_json_error(lines, "400 Bad Request", "Mesh not found").await;
            return;
        }
    };

    let mesh_path = mesh.path.clone();
    let node = match crate::commands::run_blocking("http_create_node", move || {
        crate::services::agent_node::create(
            mesh_id,
            &mesh_path,
            "main",
            Some(provider.as_str()),
            None, // source_issue
            None, // source_pr — generic mobile spawn, not PR-spawn (issue #450)
            None, // source_pr_pinned_sha — generic mobile spawn, no pin (issue #444)
            None, // use_worktree_override — None falls back to mesh default
            None, // name_override — none supplied on this route
        )
        .map_err(|e| e.to_string())
    })
    .await
    {
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

    if let Err(e) = crate::agent::spawn::spawn_with_intent(
        app,
        crate::agent::spawn::SpawnRequest::new(
            node_id,
            crate::agent::spawn::SpawnIntent::Fresh,
            crate::agent::spawn::TerminalSize {
                rows: req.rows,
                cols: req.cols,
            },
        ),
    )
    .await
    {
        let msg = format!("Failed to spawn agent: {}", e);
        request::send_json_error(lines, "500 Internal Server Error", &msg).await;
        return;
    }

    let node = match crate::commands::run_blocking("http_reload_node", move || {
        db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(node) => node,
        Err(e) => {
            request::send_json_error(
                lines,
                "500 Internal Server Error",
                &format!("Failed to reload node: {}", e),
            )
            .await;
            return;
        }
    };
    let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());

    let _ = app.emit(
        "node-created",
        crate::commands::agent::NodeCreatedPayload { id: node_id },
    );
    let _ = request::write_json(lines, "200 OK", &body).await;
}
