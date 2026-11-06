//! Mesh property writes: the user-tunable columns on the `meshes` row.
//!
//! **There is no `mesh.toml` file.** The "properties" / "config" lives on the
//! `meshes` SQLite row (see `db::get_mesh_by_id`), not in any file at the mesh
//! root. `Worktree.baseRef` is additionally written to `.claude/settings.json`
//! at the mesh root so Claude Code can read it; that mirror is an output, not
//! a source of truth — the DB column is the source, and the mirror travels
//! with the command that owns the write
//! (`commands::mesh_properties::update_worktree_base_ref`).
//!
//! Every statement here runs on one connection: a public function checks the
//! writer out and hands it to an `_inner(&Connection)` helper, so a compound
//! write never nests a second checkout (the deadlock class issue #1533's
//! reader-pool exhaustion belongs to). The IPC layer is a thin adapter over
//! these functions — it validates the wire value and maps the error to
//! `String`, and it issues no SQL of its own (issue #2527).

use rusqlite::{params, Connection};

use super::{write_conn, SqlResult};

#[cfg(test)]
#[path = "mesh_properties_tests.rs"]
mod tests;

/// The `meshes` columns a user may write from the UI.
///
/// `name` has its own command ([`set_mesh_name`]), and `base_ref` /
/// `use_worktree` carry side effects (the `.claude/settings.json` mirror and
/// the worktree layout) and route through their own commands.
///
/// The list is the only thing standing between a wire-supplied column name
/// and the SQL: a column name cannot be a bound parameter, so it is
/// interpolated — and therefore checked here, in the module that owns the
/// statement. The command boundary checks the same list, so an unknown column
/// is rejected before the write is even attempted.
pub const WRITABLE_COLUMNS: &[&str] = &[
    "build_command",
    "run_command",
    // Per-context build/run commands (issue #802). Nullable siblings of
    // build_command/run_command; a Root Node prefers these and falls back
    // to the plain columns when they're unset.
    "root_build_command",
    "root_run_command",
    "model",
    "effort",
    "worktree_mode",
    "default_provider",
];

/// Set a mesh's display name.
pub fn set_mesh_name(mesh_id: i64, name: &str) -> SqlResult<usize> {
    let db = write_conn();
    set_mesh_name_inner(&db, mesh_id, name)
}

pub(crate) fn set_mesh_name_inner(
    conn: &Connection,
    mesh_id: i64,
    name: &str,
) -> SqlResult<usize> {
    conn.execute(
        "UPDATE meshes SET name = ?1 WHERE id = ?2",
        params![name, mesh_id],
    )
}

/// Set one user-tunable column.
///
/// `column` must be in [`WRITABLE_COLUMNS`]; anything else is rejected
/// before the statement is built, because the name is interpolated rather
/// than bound.
pub fn set_mesh_column(mesh_id: i64, column: &str, value: &str) -> SqlResult<usize> {
    let db = write_conn();
    set_mesh_column_inner(&db, mesh_id, column, value)
}

pub(crate) fn set_mesh_column_inner(
    conn: &Connection,
    mesh_id: i64,
    column: &str,
    value: &str,
) -> SqlResult<usize> {
    if !WRITABLE_COLUMNS.contains(&column) {
        return Err(rusqlite::Error::InvalidParameterName(column.to_string()));
    }
    conn.execute(
        &format!("UPDATE meshes SET {} = ?1 WHERE id = ?2", column),
        params![value, mesh_id],
    )
}

/// Set whether a mesh's agent nodes run in worktrees.
pub fn set_mesh_use_worktree(mesh_id: i64, use_worktree: bool) -> SqlResult<usize> {
    let db = write_conn();
    set_mesh_use_worktree_inner(&db, mesh_id, use_worktree)
}

pub(crate) fn set_mesh_use_worktree_inner(
    conn: &Connection,
    mesh_id: i64,
    use_worktree: bool,
) -> SqlResult<usize> {
    conn.execute(
        "UPDATE meshes SET use_worktree = ?1 WHERE id = ?2",
        params![use_worktree as i32, mesh_id],
    )
}

/// Set the per-mesh target for the pre-spawn Worktree Pool (`0` disables it,
/// `1..=5` is the count the worker fills to). The range is clamped at the IPC
/// boundary, so a misbehaving frontend cannot write a garbage value into the
/// column and break the worker's count comparisons.
pub fn set_mesh_pre_spawn_pool_size(mesh_id: i64, pool_size: i32) -> SqlResult<usize> {
    let db = write_conn();
    set_mesh_pre_spawn_pool_size_inner(&db, mesh_id, pool_size)
}

pub(crate) fn set_mesh_pre_spawn_pool_size_inner(
    conn: &Connection,
    mesh_id: i64,
    pool_size: i32,
) -> SqlResult<usize> {
    conn.execute(
        "UPDATE meshes SET pre_spawn_pool_size = ?1 WHERE id = ?2",
        params![pool_size, mesh_id],
    )
}

/// Set a mesh's base ref — the ref a node's change set is measured against.
pub fn set_mesh_base_ref(mesh_id: i64, base_ref: &str) -> SqlResult<usize> {
    let db = write_conn();
    set_mesh_base_ref_inner(&db, mesh_id, base_ref)
}

pub(crate) fn set_mesh_base_ref_inner(
    conn: &Connection,
    mesh_id: i64,
    base_ref: &str,
) -> SqlResult<usize> {
    conn.execute(
        "UPDATE meshes SET base_ref = ?1 WHERE id = ?2",
        params![base_ref, mesh_id],
    )
}

/// Reset a mesh's base ref to the default (`origin/main`).
pub fn reset_mesh_base_ref(mesh_id: i64) -> SqlResult<usize> {
    let db = write_conn();
    reset_mesh_base_ref_inner(&db, mesh_id)
}

pub(crate) fn reset_mesh_base_ref_inner(conn: &Connection, mesh_id: i64) -> SqlResult<usize> {
    conn.execute(
        "UPDATE meshes SET base_ref = 'origin/main' WHERE id = ?1",
        params![mesh_id],
    )
}
