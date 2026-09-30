//! Mesh persistence: mesh rows, Circuit admission capacity, scratchpad and sandbox.

use std::fmt::Write as _;

use rusqlite::{Connection, params};

use crate::models::*;

use super::{read_conn, write_conn, SqlResult};

// --- Internal Helpers (no locking) ---

/// Canonical column projection for reading a `Mesh` row, built from
/// `migrations::mesh_columns_projection()` (the registry's
/// `ColumnSpec::read_default` for each mesh column). The projection
/// order pins the `map_mesh_row` positional reads below — reordering
/// the registry reshuffles them in lockstep. See the registry's
/// `mesh_column_specs` doc for the invariants and the regression
/// tests in `db::migrations` for the shape pins.
///
/// Replaces the pre-#249 hand-written `const MESH_COLUMNS: &str`
/// (every `COALESCE(col, default)` literal) — the COALESCE defaults
/// now live next to the [`migrations::ColumnSpec`] entry that
/// introduces the column, so a new column is one entry in the
/// registry, not three edits across `init()`, the safety net, and
/// the projection string.
fn mesh_columns() -> &'static str {
    super::migrations::mesh_columns_projection()
}

/// Map a row selected with [`mesh_columns`] into a `Mesh`. Single
/// place that normalizes empty config strings to `None` (via
/// `parse_str`). The positional `row.get(N)` indices track the
/// `migrations::mesh_column_specs` iteration order — don't reorder
/// the registry without re-mapping this function.
fn map_mesh_row(row: &rusqlite::Row) -> rusqlite::Result<Mesh> {
    Ok(Mesh {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        layout: row.get::<_, String>(3)?,
        position: row.get(4)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
        build_command: parse_str(row.get::<_, String>(6)?),
        run_command: parse_str(row.get::<_, String>(7)?),
        model: parse_str(row.get::<_, String>(8)?),
        effort: parse_str(row.get::<_, String>(9)?),
        use_worktree: row.get::<_, i32>(10)? != 0,
        worktree_mode: parse_str(row.get::<_, String>(11)?),
        default_provider: parse_str(row.get::<_, String>(12)?),
        base_ref: row.get::<_, String>(13)?,
        scratchpad: row.get(14)?,
        sandbox: row.get::<_, i32>(15)? != 0,
        pre_spawn_pool_size: row.get::<_, i32>(16)?,
        color: parse_str(row.get::<_, String>(17)?),

        root_build_command: parse_str(row.get::<_, String>(23)?),
        root_run_command: parse_str(row.get::<_, String>(24)?),

        circuit_run_capacity: row.get::<_, i32>(31)?,
        worktree_directory: parse_str(row.get::<_, String>(32)?),
    })
}

pub(crate) fn get_mesh_by_id_inner(conn: &Connection, id: i64) -> SqlResult<Mesh> {
    let mut stmt = conn.prepare(
        &format!("SELECT {} FROM meshes WHERE id = ?1", mesh_columns())
    )?;
    stmt.query_row(params![id], map_mesh_row)
}

fn parse_str(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}
// --- Mesh operations ---

pub fn create_mesh(name: &str, path: &str) -> SqlResult<Mesh> {
    let db = write_conn();
    create_mesh_inner(&db, name, path)
}

/// Create a mesh whose **Base Ref** is not the `origin/main` default. Used by
/// the GitHub clone flow, which resolves the cloned repo's real default branch
/// so worktrees are cut from the right ref instead of booting the mesh up as a
/// **drifted root**.
pub fn create_mesh_with_base_ref(name: &str, path: &str, base_ref: &str) -> SqlResult<Mesh> {
    let db = write_conn();
    create_mesh_with_base_ref_inner(&db, name, path, base_ref)
}

/// Per-test isolated variant of [`create_mesh`]. The public function locks
/// the process-global writer connection; this helper takes an explicit
/// `&Connection` so parallel tests can each operate against their own
/// in-memory DB without contending on the global mutex (issue #1691).
pub(crate) fn create_mesh_inner(db: &Connection, name: &str, path: &str) -> SqlResult<Mesh> {
    create_mesh_with_base_ref_inner(db, name, path, "origin/main")
}

pub(crate) fn create_mesh_with_base_ref_inner(
    db: &Connection,
    name: &str,
    path: &str,
    base_ref: &str,
) -> SqlResult<Mesh> {
    // Check if mesh with this path already exists (idempotent upsert)
    let existing: Option<i64> = db.query_row(
        "SELECT id FROM meshes WHERE path = ?1",
        params![path],
        |row| row.get(0),
    ).ok();

    if let Some(id) = existing {
        return get_mesh_by_id_inner(db, id);
    }

    // Append at end of position list
    let next_position: i64 = db.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM meshes",
        [],
        |row| row.get(0),
    )?;

    // `pre_spawn_pool_size = 1` is written explicitly (not left to the
    // column default) because a DB upgraded from pre-v24 still carries the
    // ALTER-time `DEFAULT 0` — new meshes must get the pool-on default
    // regardless of when the DB was created (ADR 0020).
    db.execute(
        "INSERT INTO meshes (name, path, layout, position, use_worktree, base_ref, pre_spawn_pool_size)
         VALUES (?1, ?2, 'grid', ?3, 1, ?4, 1)",
        params![name, path, next_position, base_ref],
    )?;
    let id = db.last_insert_rowid();
    get_mesh_by_id_inner(db, id)
}

pub fn get_mesh_by_id(id: i64) -> SqlResult<Mesh> {
    let db = read_conn();
    get_mesh_by_id_inner(&db, id)
}

/// Set (or clear) a mesh's accent colour. `Some(hex)` stores the `#rrggbb`
/// string; `None` clears it back to the deterministic-palette fallback.
/// Returns the number of rows updated so callers can surface a "mesh not
/// found" error rather than a silent no-op (matches the zero-rows contract
/// used by `set_mesh_sandbox` / `update_mesh_pool_size`).
pub fn set_mesh_color(id: i64, color: Option<&str>) -> SqlResult<usize> {
    let db = write_conn();
    db.execute(
        "UPDATE meshes SET color = ?1 WHERE id = ?2",
        params![color, id],
    )
}

/// Persist the per-mesh Circuit Run admission limit; IPC validates its range.
pub fn set_mesh_circuit_run_capacity(id: i64, capacity: i32) -> SqlResult<usize> {
    let db = write_conn();
    db.execute(
        "UPDATE meshes SET circuit_run_capacity = ?1 WHERE id = ?2",
        params![capacity, id],
    )
}

pub fn update_mesh_layout(id: i64, layout: &str) -> SqlResult<()> {
    let db = write_conn();
    db.execute(
        "UPDATE meshes SET layout = ?1 WHERE id = ?2",
        params![layout, id],
    )?;
    Ok(())
}

/// Read the scratch pad text for a mesh. Returns the empty string (not
/// an error) for an unknown mesh id so the frontend can mount a blank
/// editor without a second round-trip — Scratch Pad is a "type whatever
/// you want" surface and the absence of notes is the common case.
pub fn get_mesh_scratchpad(id: i64) -> SqlResult<String> {
    let db = read_conn();
    get_mesh_scratchpad_inner(&db, id)
}

pub(crate) fn get_mesh_scratchpad_inner(conn: &Connection, id: i64) -> SqlResult<String> {
    // COALESCE keeps the contract even on a pre-v17 DB whose safety net
    // hasn't run yet (e.g. unit tests that construct the schema in
    // memory) — empty string instead of NULL.
    conn.query_row(
        "SELECT COALESCE(scratchpad, '') FROM meshes WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )
}

/// Overwrite a mesh's scratch pad text. Empty string is a normal value
/// (cleared notes), not a deletion. Returns an error if the mesh id
/// doesn't exist — the call site surfaces that to the frontend so a
/// debounced save that fires after the mesh was deleted doesn't silently
/// report "Saved" for a write that affected zero rows.
pub fn set_mesh_scratchpad(id: i64, content: &str) -> SqlResult<()> {
    let db = write_conn();
    set_mesh_scratchpad_inner(&db, id, content)
}

pub(crate) fn set_mesh_scratchpad_inner(
    conn: &Connection,
    id: i64,
    content: &str,
) -> SqlResult<()> {
    let rows = conn.execute(
        "UPDATE meshes SET scratchpad = ?1 WHERE id = ?2",
        params![content, id],
    )?;
    if rows == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// Set a mesh's `sandbox` flag. A write matching zero rows is an error so
/// a save that fires after the mesh was deleted doesn't silently report
/// success — same contract as `set_mesh_scratchpad`. Shared by the macOS
/// Seatbelt (#497) and Windows AppContainer (#498) toggles: the column is
/// one, the consumer OS-sandbox policy is decided at spawn time.
pub fn set_mesh_sandbox(id: i64, sandbox: bool) -> SqlResult<()> {
    let db = write_conn();
    set_mesh_sandbox_inner(&db, id, sandbox)
}

pub(crate) fn set_mesh_sandbox_inner(
    conn: &Connection,
    id: i64,
    sandbox: bool,
) -> SqlResult<()> {
    let rows = conn.execute(
        "UPDATE meshes SET sandbox = ?1 WHERE id = ?2",
        params![sandbox as i32, id],
    )?;
    if rows == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// Narrow single-column write for the per-Mesh `worktree_directory`
/// override (issue #1519). `None` (or blank) clears the override so the
/// Mesh inherits the application default; `Some(dir)` stores the trimmed
/// raw input verbatim (no shell/`~` expansion — resolution joins it at
/// read time via `env::effective_worktree_dir_raw`). Validation
/// (absolute-path environment match) lives at the IPC boundary
/// (`commands::mesh_properties::update_mesh_worktree_directory`), not
/// here — this helper is the typed write pass. Zero-rows-is-an-error
/// contract matches `set_mesh_sandbox`.
pub fn set_mesh_worktree_directory(id: i64, directory: Option<&str>) -> SqlResult<usize> {
    let db = write_conn();
    set_mesh_worktree_directory_inner(&db, id, directory)
}

pub(crate) fn set_mesh_worktree_directory_inner(
    conn: &Connection,
    id: i64,
    directory: Option<&str>,
) -> SqlResult<usize> {
    let cleaned = directory
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    conn.execute(
        "UPDATE meshes SET worktree_directory = ?1 WHERE id = ?2",
        params![cleaned, id],
    )
}

pub fn update_mesh_positions_batch(updates: &[(i64, i64)]) -> SqlResult<()> {
    if updates.is_empty() { return Ok(()); }
    let db = write_conn();
    update_mesh_positions_batch_inner(&db, updates)
}

/// Test-facing entry point mirroring [`update_mesh_positions_batch`]; takes
/// a caller-supplied `&Connection` so the per-row vs bulk-shape contract can
/// be exercised against an in-memory fixture (issue #1746). The public
/// function locks the process-global writer; this helper takes an explicit
/// connection so parallel tests can each operate against their own DB.
pub(crate) fn update_mesh_positions_batch_inner(
    conn: &Connection,
    updates: &[(i64, i64)],
) -> SqlResult<()> {
    if updates.is_empty() { return Ok(()); }
    // One bulk UPDATE per chunk, all chunks inside a single transaction.
    // Pre-issue-#1746 the loop paid N commits / fsyncs while holding the
    // process-global writer Mutex — every other DB user (UI reads via the
    // 8-conn reader pool, HTTP, worker pollers) stalled behind it. After
    // #1746 there is **one commit** for the whole batch (irrespective of
    // how many chunks the parameter cap forces); the chunks exist solely
    // to stay below `SQLITE_MAX_VARIABLE_NUMBER` (default 999) per
    // prepared statement. SQLite still does the per-row lookup against
    // the primary key — the `EXPLAIN QUERY PLAN` shape is unchanged.
    //
    // The CASE-WHEN shape binds 2 parameters per row (id + position).
    // The IN list reuses the same `?2i+1` placeholders the CASE already
    // referenced — SQLite lets a placeholder be referenced any number of
    // times. Chunked at 300 → 600 binds per statement, well below the
    // 999 cap with headroom for the prepare cache to add bind metadata.
    const CHUNK_SIZE: usize = 300;
    let tx = conn.unchecked_transaction()?;
    for chunk in updates.chunks(CHUNK_SIZE) {
        let mut case_sql = String::with_capacity(64 + chunk.len() * 24);
        case_sql.push_str("UPDATE meshes SET position = CASE id ");
        for i in 0..chunk.len() {
            let _ = write!(
                case_sql,
                "WHEN ?{} THEN ?{} ",
                2 * i + 1,
                2 * i + 2,
            );
        }
        case_sql.push_str("END WHERE id IN (");
        for i in 0..chunk.len() {
            if i > 0 { case_sql.push(','); }
            let _ = write!(case_sql, "?{}", 2 * i + 1);
        }
        case_sql.push(')');

        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() * 2);
        for (id, pos) in chunk {
            params_vec.push(id);
            params_vec.push(pos);
        }
        tx.execute(&case_sql, params_vec.as_slice())?;
    }
    tx.commit()
}

pub fn list_meshes() -> SqlResult<Vec<Mesh>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM meshes ORDER BY position ASC, name ASC", mesh_columns())
    )?;
    let rows = stmt.query_map([], map_mesh_row)?;
    rows.collect()
}

/// Look up a mesh by its path.
pub fn get_mesh_by_path(path: &str) -> SqlResult<Mesh> {
    let db = read_conn();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM meshes WHERE path = ?1", mesh_columns())
    )?;
    stmt.query_row(params![path], map_mesh_row)
}

pub fn delete_mesh(id: i64) -> SqlResult<()> {
    let db = write_conn();
    delete_mesh_inner(&db, id)
}

/// Per-test isolated variant of [`delete_mesh`] (issue #1691). The
/// public function locks the process-global writer; this helper takes
/// an explicit `&Connection` so parallel tests can each operate
/// against their own in-memory DB.
pub(crate) fn delete_mesh_inner(db: &Connection, id: i64) -> SqlResult<()> {
    // Autopilot Circuits ledger (spec #1205): explicit child deletes —
    // same defensive rule as the warm pool below.
    crate::db::circuit::delete_circuits_for_mesh_inner(db, id)?;
    // Autopilot Runs ledger (issue #1231): explicit child delete. The
    // schema declares `ON DELETE CASCADE` on `autopilot_runs.node_id`,
    // and the bundled SQLite build (rusqlite 0.32 / SQLite 3.46.0)
    // has FK enforcement on by default, so the agent_nodes DELETE
    // below already cascades. We DELETE explicitly anyway as a
    // defensive belt against a future link against system libsqlite
    // (where FK is per-connection opt-in and defaults OFF): an orphan
    // row here would feed `list_active_autopilot_node_ids` and
    // `list_known_autopilot_issue_numbers` with ghost node ids /
    // issue numbers the poller would never respawn.
    db.execute("DELETE FROM autopilot_runs WHERE mesh_id = ?1", params![id])?;
    db.execute("DELETE FROM agent_nodes WHERE mesh_id = ?1", params![id])?;
    // The `warm_worktrees.mesh_id` FK declares ON DELETE CASCADE, and
    // the bundled SQLite build has FK enforcement on by default, so
    // the `meshes` DELETE at the bottom would cascade — we DELETE
    // explicitly anyway as a defensive belt against a future system
    // libsqlite link (issue #609). Same pattern as
    // `delete_autopilot_run` above.
    crate::db::warm_pool::delete_warm_worktrees_for_mesh_inner(db, id)?;
    db.execute("DELETE FROM meshes WHERE id = ?1", params![id])?;
    Ok(())
}

/// Safety net: re-apply the **mesh-default** Spawn Option composite-id
/// rewrite. The v19 first-class block in
/// `db::migrations::migrate_agent_node_provider_id_to_composite` only
/// rewrites `agent_nodes.provider` — `meshes.default_provider` was
/// missed, and a pre-#575 user still has bare `"minimax"` / `"kimi"`
/// values in the per-mesh column after upgrade. Without this safety
/// net, the bare form routes through `resolve_provider_env` to the
/// keyed **account** instead of the post-#575 proxied pairing — the
/// same trap the `preferences::ensure_default_provider_normalized`
/// helper closes for the app-wide default.
///
/// Called from `lib.rs::setup` immediately after `preferences::init`.
/// **Idempotent**: the `WHERE default_provider = 'minimax'` guard
/// skips already-composite rows, so re-running on a healthy v19+ DB
/// is a no-op.
///
/// Moved from `db/mod.rs` as part of issue #1655: `mod.rs` must own
/// connection + init + baseline DDL only — every domain mutation
/// belongs in the module that owns the table.
pub(crate) fn ensure_mesh_default_provider_normalized(conn: &Connection) -> SqlResult<()> {
    // Table-exists guard mirrors `ensure_agent_node_provider_id_migrated`:
    // a fresh DB creates the table above, so this is a no-op there.
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meshes'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(());
    }
    let rows_minimax = conn.execute(
        "UPDATE meshes SET default_provider = 'claude:minimax'
         WHERE default_provider = 'minimax'",
        [],
    )?;
    // `kimi` is intentionally absent from this migration: post-#918, bare
    // `kimi` resolves to the native Kimi Code harness via
    // `Provider::from_db_str("kimi") == Provider::Kimi`, so rewriting to
    // `claude:kimi` would land the mesh in a state with no matching Proxied
    // row. (Follow-up: post-#918 migration that re-rewrites `claude:kimi` →
    // `kimi` for users who already passed through v19.)
    if rows_minimax > 0 {
        tracing::info!(
            "ensure_mesh_default_provider_normalized: rewrote {} minimax mesh defaults to composite form",
            rows_minimax
        );
    }
    Ok(())
}
