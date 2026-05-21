//! `GET /api/meshes` — list all meshes.

use crate::db;

pub fn list_json() -> String {
    match db::list_meshes() {
        Ok(meshes) => serde_json::to_string(&meshes).unwrap_or_else(|_| "[]".to_string()),
        Err(_) => "[]".to_string(),
    }
}
