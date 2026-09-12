//! GitHub issue browsing + issue-driven agent spawning.

use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::state;

pub async fn list(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
    // Await the async command wrapper (not the `*_blocking` core): this route
    // runs inside `tauri::async_runtime::spawn` (http/server.rs), so calling the
    // blocking core directly would park a Tauri worker. The wrapper offloads to
    // the blocking pool via `run_blocking`.
    match crate::commands::pr::get_repo_issues(mesh_id).await {
        Ok(issues) => {
            let body = serde_json::to_string(&issues).unwrap_or_else(|_| "[]".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
// deny_unknown_fields is deliberate. The previous SpawnRequest carried a
// `body: String`; if a stale mobile bundle (served from `dist/mobile` via
// rust-embed and possibly cached on a phone for releases) keeps POSTing the
// legacy `{title, body, provider}` shape, we want a loud 400 — not a silent
// body-dropped success — so the user knows to refresh. See memory:
// buildmesh-serde-default-fragility.
struct SpawnRequest {
    title: String,
    provider: Option<String>,
}

pub async fn spawn(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
    let issue_number = req.id1();

    let parsed: SpawnRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    let Some(app) = state::app_handle() else {
        return Response::json_error("503 Service Unavailable", "App not ready");
    };

    // spawn_issue_agent is a #[tauri::command] but takes plain args except
    // for AppHandle which we already hold, so we call it directly. The
    // backend derives the GitHub URL from the mesh's `origin` remote — we
    // only need the issue number and a title hint here.
    match crate::commands::agent::spawn_issue_agent(
        app.clone(),
        mesh_id,
        issue_number,
        parsed.title,
        parsed.provider,
    )
    .await
    {
        Ok(node) => {
            let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}
