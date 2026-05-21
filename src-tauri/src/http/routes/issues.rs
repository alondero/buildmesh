//! GitHub issue browsing + issue-driven agent spawning.

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

use crate::http::request;

pub async fn list(
    lines: &mut tokio::io::BufStream<TcpStream>,
    mesh_id: i64,
) {
    match crate::commands::pr::get_repo_issues(mesh_id) {
        Ok(issues) => {
            let body = serde_json::to_string(&issues).unwrap_or_else(|_| "[]".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}

#[derive(serde::Deserialize)]
struct SpawnRequest {
    title: String,
    body: String,
    provider: Option<String>,
}

pub async fn spawn(
    lines: &mut tokio::io::BufStream<TcpStream>,
    mesh_id: i64,
    issue_number: i64,
    content_length: usize,
) {
    if content_length > 256 * 1024 {
        request::send_json_error(lines, "413 Content Too Large", "Body too large").await;
        return;
    }
    let mut body_bytes = vec![0u8; content_length];
    if content_length > 0 && lines.read_exact(&mut body_bytes).await.is_err() {
        let _ = request::write_status_only(lines, "400 Bad Request").await;
        return;
    }

    let req: SpawnRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            request::send_json_error(lines, "400 Bad Request", &format!("Invalid JSON: {}", e))
                .await;
            return;
        }
    };

    let Some(app) = crate::http::app_handle() else {
        request::send_json_error(lines, "503 Service Unavailable", "App not ready").await;
        return;
    };

    // spawn_issue_agent is a #[tauri::command] but takes plain args except
    // for AppHandle which we already hold, so we call it directly.
    match crate::commands::agent::spawn_issue_agent(
        app.clone(),
        mesh_id,
        issue_number,
        req.title,
        req.body,
        req.provider,
    )
    .await
    {
        Ok(node) => {
            let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}
