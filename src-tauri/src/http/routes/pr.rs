//! `POST /api/meshes/{id}/pr` — create a GitHub PR for the mesh's current branch.

use crate::db;
use crate::http::request;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

#[derive(serde::Deserialize)]
struct CreatePrRequest {
    title: String,
    body: String,
    base_branch: String,
}

pub async fn create(
    lines: &mut tokio::io::BufStream<TcpStream>,
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
    ) {
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
