//! Mesh spawn config — read from SQLite database.
//!
//! This module centralizes mesh config reads for agent spawning decisions.

use crate::db;

/// The subset of mesh config fields relevant to agent spawning decisions.
#[derive(Debug, Clone)]
pub struct MeshSpawnConfig {
    pub use_worktree: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub worktree_mode: Option<String>,
    pub default_provider: Option<String>,
}

impl Default for MeshSpawnConfig {
    fn default() -> Self {
        Self {
            use_worktree: true,
            model: None,
            effort: None,
            worktree_mode: None,
            default_provider: None,
        }
    }
}

/// Read mesh spawn config from the database.
/// Returns None if the mesh is not found.
pub fn read_mesh_spawn_config(mesh_path: &std::path::Path) -> Option<MeshSpawnConfig> {
    let mesh = db::get_mesh_by_path(&mesh_path.to_string_lossy()).ok()?;

    Some(MeshSpawnConfig {
        use_worktree: mesh.use_worktree,
        model: mesh.model,
        effort: mesh.effort,
        worktree_mode: mesh.worktree_mode,
        default_provider: mesh.default_provider,
    })
}