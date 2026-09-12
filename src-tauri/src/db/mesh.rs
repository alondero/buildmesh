//! Mesh persistence: mesh rows, harness overrides, scratchpad/sandbox,
//! and the Autopilot run ledger keyed per mesh/node.

use rusqlite::{Connection, params};

use crate::models::*;
use crate::preferences::HarnessConfigValue;

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
        autopilot_enabled: row.get::<_, i32>(18)? != 0,
        autopilot_trigger_label: parse_str(row.get::<_, String>(19)?),
        autopilot_concurrency_limit: row.get::<_, i32>(20)?,
        autopilot_provider: parse_str(row.get::<_, String>(21)?),
        autopilot_action_on_success: parse_str(row.get::<_, String>(22)?),
        root_build_command: parse_str(row.get::<_, String>(23)?),
        root_run_command: parse_str(row.get::<_, String>(24)?),
        autopilot_mode: AutopilotMode::from_db_str(&row.get::<_, String>(25)?),
        loop_initial_prompt: parse_str(row.get::<_, String>(26)?),
        loop_suffix_prompt: parse_str(row.get::<_, String>(27)?),
        loop_max_iterations: row.get(28)?,
        loop_interval_seconds: row.get::<_, i32>(29)?,
        loop_consecutive_failures: row.get::<_, i32>(30)?,
        harness_overrides: parse_harness_overrides(&row.get::<_, String>(31)?),
        circuit_run_capacity: row.get::<_, i32>(32)?,
        worktree_directory: parse_str(row.get::<_, String>(33)?),
    })
}

/// Parse the `meshes.harness_overrides` JSON column into the typed
/// `HashMap<String, HarnessConfigValue>` (issue #1151, slice 2 of #1148).
///
/// The column is `TEXT NOT NULL DEFAULT '{}'` (schema v33), so the read
/// side never sees a NULL — the `COALESCE(.., '{}')` in
/// `migrations::mesh_columns_projection` shields the read path during
/// the brief window between ALTER-add and the column-walk pass finding
/// the new registry entry. A malformed JSON value (e.g. a hand-edited
/// `meshes.harness_overrides = "not json"`) degrades to an empty `HashMap`
/// rather than erroring the whole query — the same fail-safe contract
/// the pre-#1151 `parse_db_timestamp` helper uses. The empty map means
/// "no exceptions" at the resolver, which matches the legacy v32 zero-row
/// reading.
fn parse_harness_overrides(raw: &str) -> std::collections::HashMap<String, HarnessConfigValue> {
    if raw.trim().is_empty() {
        return std::collections::HashMap::new();
    }
    serde_json::from_str(raw).unwrap_or_default()
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

/// Per-test isolated variant of [`create_mesh`]. The public function locks
/// the process-global writer connection; this helper takes an explicit
/// `&Connection` so parallel tests can each operate against their own
/// in-memory DB without contending on the global mutex (issue #1691).
pub(crate) fn create_mesh_inner(db: &Connection, name: &str, path: &str) -> SqlResult<Mesh> {
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
         VALUES (?1, ?2, 'grid', ?3, 1, 'origin/main', 1)",
        params![name, path, next_position],
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

// --- Autopilot (issues #481/#482/#485, PRD #480) ---

/// Persist the full Autopilot Policy for a mesh in one write. Empty strings
/// for the optional TEXT columns store NULL so they read back as `None`
/// (matching `parse_str`). Returns the number of rows updated so the caller
/// can surface "mesh not found" (same zero-rows contract as
/// `update_mesh_pool_size`).
pub fn set_mesh_autopilot(
    id: i64,
    enabled: bool,
    trigger_label: Option<&str>,
    concurrency_limit: i32,
    provider: Option<&str>,
    action_on_success: Option<&str>,
) -> SqlResult<usize> {
    let db = write_conn();
    db.execute(
        "UPDATE meshes SET autopilot_enabled = ?1, autopilot_trigger_label = ?2, \
         autopilot_concurrency_limit = ?3, autopilot_provider = ?4, \
         autopilot_action_on_success = ?5 WHERE id = ?6",
        params![
            if enabled { 1 } else { 0 },
            trigger_label,
            concurrency_limit,
            provider,
            action_on_success,
            id
        ],
    )
}

/// Toggle ONLY the `autopilot_enabled` flag for a mesh — the Looping
/// Autopilot Start/Stop control (ticket #994). Deliberately narrow: unlike
/// [`set_mesh_autopilot`] (which rewrites the five issue-driven policy
/// columns), this writes a single column so the looping Start/Stop buttons
/// can't clobber a mesh's issue-driven trigger label / concurrency / provider
/// config. Returns rows updated so the command layer can surface "mesh not
/// found" (same zero-rows contract as `set_mesh_autopilot`).
pub fn set_mesh_autopilot_enabled(id: i64, enabled: bool) -> SqlResult<usize> {
    let db = write_conn();
    db.execute(
        "UPDATE meshes SET autopilot_enabled = ?1 WHERE id = ?2",
        params![if enabled { 1 } else { 0 }, id],
    )
}

/// Persist ONLY the `circuit_run_capacity` for a mesh (issue #1467) — the
/// narrow single-column writer for the per-mesh run-admission gate.
/// Deliberately separated from [`set_mesh_autopilot`] (which rewrites the
/// five legacy Autopilot policy columns atomically) so the new
/// "Max concurrent circuit runs" form-control in the Autopilot Probe tab
/// can't clobber the user's autopilot policy when toggling the run cap,
/// and so the legacy atomic-write regression tests are unaffected by the
/// new column. Range validation lives at the IPC boundary
/// (`commands::mesh_properties::update_mesh_circuit_run_capacity` —
/// clamped `1..=8`, same shape as `autopilot_concurrency_limit`). Returns
/// the rows updated so the command layer can surface "mesh not found"
/// (zero-rows contract, matches `set_mesh_autopilot_enabled`).
pub fn set_mesh_circuit_run_capacity(id: i64, capacity: i32) -> SqlResult<usize> {
    let db = write_conn();
    db.execute(
        "UPDATE meshes SET circuit_run_capacity = ?1 WHERE id = ?2",
        params![capacity, id],
    )
}

/// Persist the full Looping Autopilot configuration for a mesh in one write
/// (wayfinder #990 / ticket #991). Companion to `set_mesh_autopilot` — the
/// poller (ticket #992) reads `autopilot_mode` to decide which spawn strategy
/// to use, then reads the loop_* fields if mode == `Looping`. Empty strings
/// for the prompt TEXT columns store NULL so they read back as `None`
/// (matching `parse_str`). Range / nullability validation lives at the IPC
/// boundary (`commands::mesh_properties::update_mesh_loop_config`), not here
/// — the DB layer is the typed write, the command is the validation. Returns
/// the number of rows updated so the caller can surface "mesh not found"
/// (same zero-rows contract as `set_mesh_autopilot` and
/// `update_mesh_pool_size`).
#[allow(clippy::too_many_arguments)]
pub fn set_mesh_loop_config(
    id: i64,
    mode: AutopilotMode,
    initial_prompt: Option<&str>,
    suffix_prompt: Option<&str>,
    max_iterations: Option<i32>,
    interval_seconds: i32,
    consecutive_failures: i32,
) -> SqlResult<usize> {
    let db = write_conn();
    db.execute(
        "UPDATE meshes SET autopilot_mode = ?1, loop_initial_prompt = ?2, \
         loop_suffix_prompt = ?3, loop_max_iterations = ?4, \
         loop_interval_seconds = ?5, loop_consecutive_failures = ?6 \
         WHERE id = ?7",
        params![
            mode.as_db_str(),
            initial_prompt,
            suffix_prompt,
            max_iterations,
            interval_seconds,
            consecutive_failures,
            id,
        ],
    )
}

// --- Per-Mesh harness overrides (issue #1151 / slice 2 of #1148) ---
//
// Each Mesh owns a sparse `HashMap<String, HarnessConfigValue>` written to
// the `meshes.harness_overrides` JSON column at schema v33. The CRUD
// helpers below compose the new map on top of the current row state —
// they do NOT touch the legacy `meshes.model` / `meshes.effort` columns,
// which remain physically present for positional row compatibility but
// are no longer read as active configuration. The cascade order at the
// spawn seam is now:
//   explicit > mesh override > application default > native
// (the application slot is fed by `preferences::harness_default_for`).
//
// The wire-level validation (`is_known_harness_id`,
// `validate_harness_default`) lives in `preferences.rs` and is
// RE-INVOKED at the IPC boundary — these DB helpers are the typed
// write pass, not the validation gate. The helpers nevertheless re-read
// the current map via `get_mesh_harness_overrides_inner` so an upsert
// doesn't clobber a sibling harness's override (independent per-entry
// storage is the user-facing contract — see acceptance criteria 8-9).

/// Read the typed `harness_overrides` map for a Mesh. None on a missing
/// mesh (the IPC surface maps `None` to a "mesh not found" error).
pub fn get_mesh_harness_overrides(mesh_id: i64) -> SqlResult<Option<std::collections::HashMap<String, HarnessConfigValue>>> {
    let db = read_conn();
    let mut stmt = db
        .prepare("SELECT harness_overrides FROM meshes WHERE id = ?1")?;
    let result = stmt.query_row(params![mesh_id], |row| {
        let raw: String = row.get(0)?;
        Ok(parse_harness_overrides(&raw))
    });
    match result {
        Ok(map) => Ok(Some(map)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Internal read helper so the upsert/remove paths can join the
/// current-state read with the write under a single lock. Returns
/// `Ok(None)` for a missing mesh. The lock-once + `_inner(&Connection)`
/// pattern mirrors the rest of the DB module.
pub(crate) fn get_mesh_harness_overrides_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<Option<std::collections::HashMap<String, HarnessConfigValue>>> {
    let mut stmt = conn.prepare("SELECT harness_overrides FROM meshes WHERE id = ?1")?;
    let result = stmt.query_row(params![mesh_id], |row| {
        let raw: String = row.get(0)?;
        Ok(parse_harness_overrides(&raw))
    });
    match result {
        Ok(map) => Ok(Some(map)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Upsert one harness's entry in the mesh's `harness_overrides` map
/// (issue #1151 / slice 2 of #1148). Composes the new map on top of the
/// current row state so a sibling harness's override is preserved. An
/// empty `value` (every field collapsed by the validator / normalize
/// pass) removes the entry instead of writing `{model: null, effort:
/// null}` — the sparse-map invariant from issue #1148 acceptance criteria
/// 6 ("an empty harness configuration removes its sparse entry").
///
/// **Validation is the caller's responsibility** (issue #1148 AC #5: write
/// boundary rejects unknown ids / out-of-vocab effort). The DB helper
/// assumes the caller already validated. This mirrors the
/// `preferences::upsert_harness_default` pure-mutator split where the
/// validator lives in `preferences::validate_harness_default` and the
/// IPC command runs the load → mutate → save trio.
///
/// Returns the number of rows updated so the IPC surface can surface a
/// "mesh not found" error (same zero-rows contract as
/// `set_mesh_loop_config`, `set_mesh_autopilot`, `update_mesh_pool_size`).
pub fn upsert_mesh_harness_override(
    mesh_id: i64,
    harness_id: &str,
    value: HarnessConfigValue,
) -> SqlResult<usize> {
    let db = write_conn();
    upsert_mesh_harness_override_inner(&db, mesh_id, harness_id, value)
}

/// Lock-once + `_inner` companion for `upsert_mesh_harness_override` so
/// tests can drive the path with an in-memory connection.
pub(crate) fn upsert_mesh_harness_override_inner(
    conn: &Connection,
    mesh_id: i64,
    harness_id: &str,
    value: HarnessConfigValue,
) -> SqlResult<usize> {
    let mut map = match get_mesh_harness_overrides_inner(conn, mesh_id)? {
        Some(m) => m,
        None => return Ok(0),
    };
    if value.is_empty() {
        map.remove(harness_id);
    } else {
        map.insert(harness_id.to_string(), value);
    }
    let serialised = serde_json::to_string(&map)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    conn.execute(
        "UPDATE meshes SET harness_overrides = ?1 WHERE id = ?2",
        params![serialised, mesh_id],
    )
}

/// Remove one harness's entry from the mesh's `harness_overrides` map
/// (issue #1151). Idempotent — calling on a harness that was not already
/// overridden is a no-op (the IPC's "Reset" affordance never errors on
/// a UI button that was already in the cleared state). Returns the
/// number of rows updated so the IPC surface can surface "mesh not
/// found" (same zero-rows contract as the upsert).
pub fn remove_mesh_harness_override(mesh_id: i64, harness_id: &str) -> SqlResult<usize> {
    let db = write_conn();
    remove_mesh_harness_override_inner(&db, mesh_id, harness_id)
}

pub(crate) fn remove_mesh_harness_override_inner(
    conn: &Connection,
    mesh_id: i64,
    harness_id: &str,
) -> SqlResult<usize> {
    let mut map = match get_mesh_harness_overrides_inner(conn, mesh_id)? {
        Some(m) => m,
        None => return Ok(0),
    };
    // The `remove` is a no-op when the harness id was absent — the
    // row is still "touched" so a non-v33 schema (no `harness_overrides`
    // column) doesn't surface a 0-rows update as a misleading "mesh not
    // found" error. The column walk guarantees the column is present on
    // a v33+ DB; we still want a definitional behaviour for test fixtures
    // that omit column walk.
    map.remove(harness_id);
    let serialised = serde_json::to_string(&map)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    conn.execute(
        "UPDATE meshes SET harness_overrides = ?1 WHERE id = ?2",
        params![serialised, mesh_id],
    )
}

/// Clear every entry in the mesh's `harness_overrides` map (issue #1151) —
/// the secondary "Reset all" bulk action. Distinct from the per-harness
/// reset (`remove_mesh_harness_override`) — that preserves every other
/// entry; this one resets the entire map. Idempotent on a mesh that has
/// no overrides (the row is "touched" either way so the zero-rows
/// contract surfaces a real "mesh not found" rather than a confusing
/// "no rows updated" silent success).
pub fn clear_mesh_harness_overrides(mesh_id: i64) -> SqlResult<usize> {
    let db = write_conn();
    db.execute(
        "UPDATE meshes SET harness_overrides = '{}' WHERE id = ?1",
        params![mesh_id],
    )
}

/// Every mesh with Autopilot enabled — the poller's work list.
pub fn list_autopilot_enabled_meshes() -> SqlResult<Vec<Mesh>> {
    let db = read_conn();
    let mut stmt = db.prepare(&format!(
        "SELECT {} FROM meshes WHERE COALESCE(autopilot_enabled, 0) = 1 ORDER BY id",
        mesh_columns()
    ))?;
    let rows = stmt.query_map([], map_mesh_row)?;
    rows.collect()
}

/// Record an auto-spawned node in the `autopilot_runs` ledger (state
/// `implementing`). Idempotent per node (PRIMARY KEY node_id).
pub fn create_autopilot_run(node_id: i64, mesh_id: i64, issue_number: i64) -> SqlResult<()> {
    let db = write_conn();
    db.execute(
        "INSERT OR IGNORE INTO autopilot_runs (node_id, mesh_id, issue_number) \
         VALUES (?1, ?2, ?3)",
        params![node_id, mesh_id, issue_number],
    )?;
    Ok(())
}

/// Record a loop-iteration node (wayfinder #990 / ticket #992). Mirrors
/// [`create_autopilot_run`] but writes `loop_iteration = ?4` instead of
/// `issue_number`, so the Looping-mode poller can distinguish iteration
/// rows from issue-driven rows in the same ledger. `issue_number` is
/// stored as `0` (the column is NOT NULL on the table) — the
/// `loop_iteration IS NOT NULL` predicate is the authoritative
/// discriminator; the `0` sentinel is never read for loop rows.
///
/// Idempotent per node (`INSERT OR IGNORE` + PRIMARY KEY on node_id), so
/// a restart that replays the spawn doesn't double-write the iteration.
pub fn create_autopilot_loop_run(
    node_id: i64,
    mesh_id: i64,
    loop_iteration: i64,
) -> SqlResult<()> {
    let db = write_conn();
    db.execute(
        "INSERT OR IGNORE INTO autopilot_runs (node_id, mesh_id, issue_number, loop_iteration) \
         VALUES (?1, ?2, 0, ?3)",
        params![node_id, mesh_id, loop_iteration],
    )?;
    Ok(())
}

/// Typed view of the `autopilot_runs.state` column (migrates the stringly-
/// typed surface that issue #855 tracked). The DB column stays TEXT for
/// backward-compat; `to_db_str` matches the column constraint and every
/// existing row's stored value. Wire shape is the same snake-case union.
/// `suffix_pending` (issue #993) is a non-terminal Looping-mode state set
/// after deterministic wrap-up passes while the optional second-turn
/// `loop_suffix_prompt` runs on the same node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ts_rs::TS)]
#[ts(export, rename_all = "snake_case", export_to = "AutopilotRunStateKind.ts")]
pub enum AutopilotRunState {
    Implementing,
    Finishing,
    /// Deterministic wrap-up passed and the optional Looping-mode suffix was
    /// injected; the same node stays active until that second turn yields.
    SuffixPending,
    Completed,
    Failed,
    /// Terminal state set by the merged-PR auto-close sweep. The node row
    /// has also been `archived` by then — `Merged` is purely a pipeline
    /// marker so the sweep can fast-skip without re-fetching the GitHub
    /// merge endpoint. Distinct from `Completed` (the agent PR'd the work)
    /// and from `Failed`.
    Merged,
}

impl AutopilotRunState {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Implementing => "implementing",
            Self::Finishing => "finishing",
            Self::SuffixPending => "suffix_pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Merged => "merged",
        }
    }

    /// Parse back from the DB column. Unknown strings degrade to
    /// `Implementing` (the safest default; the sweep never re-fetches, so
    /// an unknown row simply costs one extra GitHub round-trip the next
    /// pass) rather than `None` — every call site wants a value.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "implementing" => Self::Implementing,
            "finishing" => Self::Finishing,
            "suffix_pending" => Self::SuffixPending,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "merged" => Self::Merged,
            _ => Self::Implementing,
        }
    }
}

impl serde::Serialize for AutopilotRunState {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_db_str())
    }
}

/// The pipeline row for a node, if it is Autopilot-managed:
/// `(issue_number, state, attempts, loop_iteration, pr_url)`. `Ok(None)` for
/// hand-spawned nodes. `loop_iteration` distinguishes the mode used to spawn
/// this run even if the mesh configuration changes while it is active;
/// `pr_url` survives the gap between verified wrap-up and a suffix turn.
/// `(issue_number, state, attempts, loop_iteration, pr_url)` for an Autopilot-managed node.
pub type AutopilotRun = (i64, AutopilotRunState, i32, Option<i64>, Option<String>);

pub fn get_autopilot_run(node_id: i64) -> SqlResult<Option<AutopilotRun>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        "SELECT issue_number, state, attempts, loop_iteration, pr_url \
         FROM autopilot_runs WHERE node_id = ?1",
    )?;
    let mut rows = stmt.query_map(params![node_id], |row| {
        let s: String = row.get(1)?;
        Ok((
            row.get(0)?,
            AutopilotRunState::from_db_str(&s),
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    rows.next().transpose()
}

/// Advance a node's Autopilot pipeline state, optionally bumping the
/// wrap-up attempt counter.
pub fn set_autopilot_run_state(
    node_id: i64,
    state: AutopilotRunState,
    attempts: Option<i32>,
) -> SqlResult<()> {
    let db = write_conn();
    let state_str = state.as_db_str();
    match attempts {
        Some(n) => db.execute(
            "UPDATE autopilot_runs SET state = ?1, attempts = ?2, \
             updated_at = datetime('now') WHERE node_id = ?3",
            params![state_str, n, node_id],
        )?,
        None => db.execute(
            "UPDATE autopilot_runs SET state = ?1, updated_at = datetime('now') \
             WHERE node_id = ?2",
            params![state_str, node_id],
        )?,
    };
    Ok(())
}
/// Record the wrap-up PR a completed run produced, so the merged-PR sweep
/// can later find and close the node without re-deriving the branch.
pub fn set_autopilot_run_pr(node_id: i64, pr_number: i64, pr_url: &str) -> SqlResult<()> {
    let db = write_conn();
    db.execute(
        "UPDATE autopilot_runs SET pr_number = ?1, pr_url = ?2, \
         updated_at = datetime('now') WHERE node_id = ?3",
        params![pr_number, pr_url, node_id],
    )?;
    Ok(())
}

/// Completed runs on this mesh whose wrap-up PR is known and whose node is
/// still on the grid — the merged-PR auto-close sweep's work list:
/// `(node_id, pr_number)`.
pub fn list_completed_autopilot_runs_with_pr(mesh_id: i64) -> SqlResult<Vec<(i64, i64)>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        "SELECT r.node_id, r.pr_number FROM autopilot_runs r \
         JOIN agent_nodes a ON a.id = r.node_id \
         WHERE r.mesh_id = ?1 AND r.state = 'completed' \
         AND r.pr_number IS NOT NULL AND a.status != 'archived'",
    )?;
    let rows = stmt.query_map(params![mesh_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Remove a node's Autopilot ledger row. Called from the node-delete path
/// (`services::agent_node::delete`). The table declares `ON DELETE
/// CASCADE` and the bundled SQLite build (rusqlite 0.32 / SQLite
/// 3.46.0) has FK enforcement on by default, so the parent `agent_nodes`
/// DELETE already cascades — we DELETE explicitly anyway as a defensive
/// belt against a future link against system libsqlite (where FK is
/// per-connection opt-in and defaults OFF). Deleting the row also
/// un-dedupes the issue (`list_known_autopilot_issue_numbers`), which is
/// the intended behaviour: closing a bad autopilot node while the issue
/// stays labelled lets the poller retry it.
pub fn delete_autopilot_run(node_id: i64) -> SqlResult<()> {
    let db = write_conn();
    db.execute(
        "DELETE FROM autopilot_runs WHERE node_id = ?1",
        params![node_id],
    )?;
    Ok(())
}

/// Shared "active autopilot node" count shape: runs still in the pipeline
/// (`implementing`/`finishing`/`suffix_pending`) whose node hasn't been
/// definition both counters below share, so the per-mesh gate and the
/// app-wide pool gate can never drift on what "active" means.
pub(crate) const COUNT_ACTIVE_AUTOPILOT_SQL: &str = "SELECT COUNT(*) FROM autopilot_runs r \
     JOIN agent_nodes a ON a.id = r.node_id \
     WHERE r.state IN ('implementing', 'finishing', 'suffix_pending') \
     AND a.status != 'archived'";

/// Number of *active* Autopilot nodes for a mesh. This is the count the
/// poller compares against `autopilot_concurrency_limit`; completed/failed
/// runs free their slot.
pub fn count_active_autopilot_nodes(mesh_id: i64) -> SqlResult<i64> {
    let db = read_conn();
    db.query_row(
        &format!("{} AND r.mesh_id = ?1", COUNT_ACTIVE_AUTOPILOT_SQL),
        params![mesh_id],
        |row| row.get(0),
    )
}

/// Number of *active* Autopilot nodes across **all** meshes — the same
/// active predicate as [`count_active_autopilot_nodes`] minus the mesh
/// filter. This is what the poller compares against the app-wide
/// `autopilot_pool_size` preference: per-mesh limits bound each mesh, but
/// only this total bounds the machine.
pub fn count_active_autopilot_nodes_total() -> SqlResult<i64> {
    let db = read_conn();
    count_active_autopilot_nodes_total_inner(&db)
}

pub(crate) fn count_active_autopilot_nodes_total_inner(conn: &Connection) -> SqlResult<i64> {
    conn.query_row(COUNT_ACTIVE_AUTOPILOT_SQL, [], |row| row.get(0))
}

/// Per-iteration snapshot consumed by the Looping-mode poller's pure
/// decision core (`services::autopilot::evaluate_loop_continuation`,
/// ticket #992). One row per loop iteration on this mesh, in
/// ascending-iteration order so the caller's trailing-failure walk is
/// a forward iteration from the front. `updated_at` is the raw SQLite
/// `datetime('now')` text the ledger stores on every state write —
/// `services::autopilot` parses it to a `SystemTime` for the
/// interval-delay check.
pub type LoopRunSnapshot = (i64, AutopilotRunState, String);

/// All loop-iteration rows for one mesh, in ascending iteration order
/// (ticket #992). Returns an empty `Vec` when the mesh has no loop
/// iterations yet (or the poller has just started). The empty case is
/// NOT an error — a fresh Looping-mode mesh with no prior spawns reads
/// back as `iteration_count = 0` and `trailing_failures = 0`, exactly
/// the state that allows the first spawn.
///
/// Pre-existing issue-driven rows are filtered by `loop_iteration IS
/// NOT NULL` so the two modes never cross-contaminate the ledger view.
pub fn list_loop_iterations(mesh_id: i64) -> SqlResult<Vec<LoopRunSnapshot>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        "SELECT loop_iteration, state, updated_at FROM autopilot_runs \
         WHERE mesh_id = ?1 AND loop_iteration IS NOT NULL \
         ORDER BY loop_iteration ASC",
    )?;
    let rows = stmt.query_map(params![mesh_id], |row| {
        let iteration: i64 = row.get(0)?;
        let state_str: String = row.get(1)?;
        let updated_at: String = row.get(2)?;
        Ok((iteration, AutopilotRunState::from_db_str(&state_str), updated_at))
    })?;
    rows.collect()
}

/// Node ids of `finishing` runs (all meshes) whose ledger row hasn't
/// advanced for at least `stale_minutes` — the poller re-drive's candidates.
/// The wrap-up pipeline is otherwise purely turn-driven, so a run whose
/// final Node Turn was lost (dropped by the in-flight guard, or a missed
/// attention callback) would stall in `finishing` forever, occupying a
/// concurrency slot (node 2328, 2026-07-17). Deliberately NOT scoped to
/// autopilot-enabled meshes: disabling a mesh's autopilot must not strand
/// its already-running wrap-ups. `updated_at` is bumped on every
/// state/attempt write, so "stale" means "no pipeline activity", not
/// "agent quiet".
pub fn list_stalled_finishing_autopilot_runs(stale_minutes: i64) -> SqlResult<Vec<i64>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        "SELECT r.node_id FROM autopilot_runs r \
         JOIN agent_nodes a ON a.id = r.node_id \
         WHERE r.state = 'finishing' AND a.status != 'archived' \
         AND r.updated_at <= datetime('now', '-' || ?1 || ' minutes')",
    )?;
    let rows = stmt.query_map(params![stale_minutes], |row| row.get(0))?;
    rows.collect()
}

/// Node ids of every run still in the pipeline, across all meshes. Startup
/// hydration for the evaluator's piloted-node registry — a restart must not
/// silently drop live autopilot nodes out of the wrap-up loop.
pub fn list_active_autopilot_node_ids() -> SqlResult<Vec<i64>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        "SELECT node_id FROM autopilot_runs \
         WHERE state IN ('implementing', 'finishing', 'suffix_pending')",
    )?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.collect()
}

/// Every active run's `(node_id, state)` across all meshes — the frontend's
/// autopilot-pill data (which nodes are piloted, and where in the pipeline
/// each one is). Excludes archived nodes: their cards aren't on the grid.
pub fn list_autopilot_run_states() -> SqlResult<Vec<(i64, AutopilotRunState)>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        "SELECT r.node_id, r.state FROM autopilot_runs r \
         JOIN agent_nodes a ON a.id = r.node_id \
         WHERE a.status != 'archived'",
    )?;
    let rows = stmt.query_map([], |row| {
        let s: String = row.get(1)?;
        Ok((row.get(0)?, AutopilotRunState::from_db_str(&s)))
    })?;
    rows.collect()
}

/// Every GitHub issue number this mesh already has a node for — union of the
/// Autopilot ledger and manually issue-spawned nodes — so the poller never
/// double-spawns an issue (including issues whose node completed or errored).
pub fn list_known_autopilot_issue_numbers(mesh_id: i64) -> SqlResult<Vec<i64>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        "SELECT issue_number FROM autopilot_runs WHERE mesh_id = ?1 \
         UNION \
         SELECT source_issue FROM agent_nodes \
         WHERE mesh_id = ?1 AND source_issue IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![mesh_id], |row| row.get(0))?;
    rows.collect()
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
    for (id, pos) in updates {
        db.execute(
            "UPDATE meshes SET position = ?1 WHERE id = ?2",
            params![pos, id],
        )?;
    }
    Ok(())
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
