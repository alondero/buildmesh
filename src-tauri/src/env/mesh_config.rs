//! Mesh config reads for agent spawning — backed by the SQLite `meshes` table.
//!
//! This is a thin lookup that returns the canonical [`MeshConfig`]; spawning
//! decisions read the fields they need (with their own defaults) off it.

use crate::db;
use crate::models::MeshConfig;

/// Read the canonical mesh config for the mesh at `mesh_path`.
/// Returns `None` if the mesh is not found.
pub fn read_mesh_config(mesh_path: &std::path::Path) -> Option<MeshConfig> {
    let mesh = db::get_mesh_by_path(&mesh_path.to_string_lossy()).ok()?;
    Some(MeshConfig::from(&mesh))
}
