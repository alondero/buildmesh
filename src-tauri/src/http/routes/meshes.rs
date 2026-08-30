//! `GET /api/meshes` — list all meshes.

use crate::db;

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
