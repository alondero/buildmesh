//! GitHub issue browsing + issue-driven agent spawning.

use crate::http::MaybeTls;

use crate::http::request;

pub async fn list(
    lines: &mut tokio::io::BufStream<MaybeTls>,
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
#[serde(deny_unknown_fields)]
// deny_unknown_fields is deliberate. The previous SpawnRequest carried a
// `body: String`; if a stale mobile bundle (served from `dist/mobile` via
// rust-embed and possibly cached on a phone for releases) keeps POSTing the
// legacy `{title, body, provider}` shape, we want a loud 400 - not a silent
// body-dropped success - so the user knows to refresh. See memory:
// buildmesh-serde-default-fragility.
struct SpawnRequest {
    title: String,
    provider: Option<String>,
}

pub async fn spawn(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    mesh_id: i64,
    issue_number: i64,
    content_length: usize,
) {
    let req: SpawnRequest = match request::read_json_body(lines, content_length, 256 * 1024).await {
        Ok(r) => r,
        Err(status) => {
            request::send_json_error(lines, &status, "Bad request").await;
            return;
        }
    };

    let Some(app) = crate::http::app_handle() else {
        request::send_json_error(lines, "503 Service Unavailable", "App not ready").await;
        return;
    };

    // spawn_issue_agent is a #[tauri::command] but takes plain args except
    // for AppHandle which we already hold, so we call it directly. The
    // backend derives the GitHub URL from the mesh's `origin` remote - we
    // only need the issue number and a title hint here.
    match crate::commands::agent::spawn_issue_agent(
        app.clone(),
        mesh_id,
        issue_number,
        req.title,
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
