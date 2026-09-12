//! `GET /api/meshes` — list all meshes.

use crate::db;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;

pub async fn list(_req: &ParsedRequest) -> Response {
    Response::json("200 OK", list_json().await)
}

pub async fn list_json() -> String {
    match crate::commands::run_blocking("http_list_meshes", || {
        db::list_meshes().map_err(|e| e.to_string())
    })
    .await
    {
        Ok(meshes) => serde_json::to_string(&meshes).unwrap_or_else(|_| "[]".to_string()),
        Err(_) => "[]".to_string(),
    }
}
