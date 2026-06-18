//! Mesh row reads for agent spawning — backed by the SQLite `meshes` table.
//!
//! **There is no `mesh.toml` file.** The mesh's user-tunable columns live
//! entirely on the `meshes` SQLite row (see `db::get_mesh_by_path`); the
//! `MeshRow` returned by [`mesh_row`] is a thin DTO built from that row by
//! `MeshRow::from(&mesh)`. (The `base_ref` field is additionally mirrored
//! into `.claude/settings.json` at the mesh root for Claude Code — see
//! `commands::mesh_properties` — but that mirror is the output of
//! `update_worktree_base_ref`, not an input to spawn-time resolution.)
//!
//! Spawning decisions read the fields they need (with their own defaults)
//! off the DTO; `spawn_agent_inner`'s auto-sync path is the only consumer
//! of `base_ref` from here, and it now detects the repo's actual default
//! branch (see `resolve_base_ref_for_spawn` in `agent::spawn`) instead of
//! trusting the COALESCE default.

use crate::db;
use crate::models::MeshRow;

/// Read the canonical `MeshRow` for the mesh at `mesh_path`.
/// Returns `None` if the mesh is not found.
pub fn mesh_row(mesh_path: &std::path::Path) -> Option<MeshRow> {
    let mesh = db::get_mesh_by_path(&mesh_path.to_string_lossy()).ok()?;
    Some(MeshRow::from(&mesh))
}
