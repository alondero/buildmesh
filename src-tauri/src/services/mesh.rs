//! Mesh service — creation with path/name extraction logic

use crate::db;
use crate::models::Mesh;

/// Error type for mesh service operations
#[derive(Debug)]
pub enum MeshError {
    Db(rusqlite::Error),
    InvalidLayout(String),
    Io(std::io::Error),
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeshError::Db(e) => write!(f, "{}", e),
            MeshError::InvalidLayout(l) => write!(f, "layout must be 'grid' or 'single', got '{}'", l),
            MeshError::Io(e) => write!(f, "{}", e),
        }
    }
}

impl From<rusqlite::Error> for MeshError {
    fn from(e: rusqlite::Error) -> Self {
        MeshError::Db(e)
    }
}

impl From<std::io::Error> for MeshError {
    fn from(e: std::io::Error) -> Self {
        MeshError::Io(e)
    }
}

/// Extract a human-readable mesh name from a filesystem path.
/// Uses the last path segment as the name.
pub fn name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            path.rsplit(|c| c == '/' || c == '\\')
                .next()
                .unwrap_or(path)
                .to_string()
        })
}

/// Create a test mesh in a temporary directory.
pub fn create_test(name: &str) -> Result<Mesh, MeshError> {
    let temp_dir = std::env::temp_dir();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let mesh_path = temp_dir.join(format!("buildmesh_test_{}_{}", name.replace(' ', "_"), timestamp));
    std::fs::create_dir_all(&mesh_path)?;
    let mesh = db::create_mesh(name, &mesh_path.to_string_lossy())?;
    Ok(mesh)
}

/// Validate and update a mesh's layout preference.
pub fn update_layout(mesh_id: i64, layout: &str) -> Result<(), MeshError> {
    if layout != "grid" && layout != "single" {
        return Err(MeshError::InvalidLayout(layout.to_string()));
    }
    db::update_mesh_layout(mesh_id, layout)?;
    Ok(())
}
