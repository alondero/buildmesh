//! Agent Node persistence, coordinator node digest rows, and the pending
//! worktree-removal queue.

use rusqlite::{Connection, params};

use crate::models::*;

use super::{read_conn, write_conn, SqlResult};

const AGENT_NODE_COLUMNS: &str =
    "id, mesh_id, name, path, branch, env, provider, status, cli_session_id, worktree_name, created_at, source_issue, use_worktree, is_pinned, position, source_pr, head_repo_owner, head_repo_clone_url, source_pr_pinned_sha, signal_health, worktree_path";

fn map_agent_node_row(row: &rusqlite::Row) -> rusqlite::Result<AgentNode> {
    Ok(AgentNode {
        id: row.get(0)?,
        mesh_id: row.get(1)?,
        name: row.get(2)?,
        path: row.get(3)?,
        branch: row.get(4)?,
        env: EnvType::from_db_str(&row.get::<_, String>(5)?),
        // Stored verbatim (issue #535): the harness/profile id round-trips as
        // an opaque String; resolution to a concrete executor happens at the
        // spawn seam via `preferences::resolve_harness_provider`.
        provider: row.get::<_, String>(6)?,
        status: SessionStatus::from_db_str(&row.get::<_, String>(7)?),
        cli_session_id: row.get(8)?,
        worktree_name: row.get(9)?,
        use_worktree: row.get::<_, i32>(12)? != 0,
        // is_pinned is at index 13 (wayfinder #982 / ticket #984). Same
        // NOT NULL + DEFAULT 0 storage as `use_worktree` — a pre-v29 row
        // reads back as `false` via the ALTER-added default, and the
        // coordinator digest / list path branches on this to render the
        // Pinned Grid view (ticket #986).
        is_pinned: row.get::<_, i32>(13)? != 0,
        source_issue: row.get(11)?,
        position: row.get(14)?,
        // source_pr is at index 15. Read as Option: the safety net adds the
        // column nullable for pre-v15 DBs, and rusqlite's typed read errors
        // the row on NULL otherwise. (v16 added head_repo_owner +
        // head_repo_clone_url at 16/17, source_pr_pinned_sha at 18 — see
        // AGENT_NODE_COLUMNS.)
        source_pr: row.get(15)?,
        head_repo_owner: row.get(16)?,
        head_repo_clone_url: row.get(17)?,
        // source_pr_pinned_sha is at index 18 (issue #444). Same nullable
        // pattern as `source_pr`: a pre-v16 row that didn't store a SHA
        // reads back as `None`, and the drift-check path treats `None` as
        // "skip the comparison" rather than failing.
        source_pr_pinned_sha: row.get(18)?,
        // signal_health is at index 19 (issue #1364). Nullable TEXT — NULL
        // means "no provisioning outcome / no callback yet"; the typed read
        // maps unknown strings to `None` so a stale value never panics.
        signal_health: row
            .get::<_, Option<String>>(19)?
            .and_then(|s| crate::agent::session_lifecycle::SignalHealth::from_db_str(&s)),
        // worktree_path is at index 20 (issue #1519). Nullable TEXT — NULL
        // means legacy `<mesh>/.claude/worktrees/<name>` fallback
        // (pre-#1519 rows + Root Nodes). Empty string degrades to `None`
        // so a hand-edited blank doesn't resolve to a bare empty dir.
        worktree_path: row
            .get::<_, Option<String>>(20)?
            .and_then(|s| {
                let t = s.trim();
                if t.is_empty() { None } else { Some(t.to_string()) }
            }),
        created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(10)?)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
    })
}

/// Parse a timestamp column that may be either RFC3339 (what Rust writes, e.g.
/// `update_agent_node_status`) or SQLite's `datetime('now')` form
/// (`YYYY-MM-DD HH:MM:SS`, what a column DEFAULT or backfill writes). Falls back
/// to "now" on an unparseable value so a malformed row degrades to a fresh
/// timestamp rather than erroring the whole query.
fn parse_db_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&chrono::Utc);
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return chrono::Utc.from_utc_datetime(&naive);
    }
    chrono::Utc::now()
}

/// Coordinator read API (ADR-0008): every non-archived Agent Node across all
/// Meshes, joined with its Mesh name and `status_changed_at`, in the same
/// order the grid renders. The two extra fields aren't on `AgentNode`, so we
/// return them alongside it; `coordinator::node_digest::spine` turns each tuple
/// into a Node Digest. Spine-only — no transcript enrichment in this slice.
pub fn list_coordinator_node_rows()
-> SqlResult<Vec<(AgentNode, String, chrono::DateTime<chrono::Utc>)>> {
    let db = read_conn();
    list_coordinator_node_rows_inner(&db)
}

pub fn list_coordinator_node_rows_inner(
    conn: &Connection,
) -> SqlResult<Vec<(AgentNode, String, chrono::DateTime<chrono::Utc>)>> {
    // Qualify AGENT_NODE_COLUMNS with the `a.` alias (derived, never drifts)
    // so the join with `meshes` has no ambiguous `name`/`created_at`/`position`.
    let qualified: String = AGENT_NODE_COLUMNS
        .split(", ")
        .map(|c| format!("a.{}", c))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {qualified}, m.name, a.status_changed_at \
         FROM agent_nodes a JOIN meshes m ON a.mesh_id = m.id \
         WHERE a.status != 'archived' \
         ORDER BY a.mesh_id ASC, a.position ASC, a.created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        // map_agent_node_row reads positional indices 0..20, which match the
        // AGENT_NODE_COLUMNS order we selected first; mesh name and
        // status_changed_at follow at 21 and 22 (v16 added head_repo_owner +
        // head_repo_clone_url at 16/17, source_pr_pinned_sha at 18, v29
        // added is_pinned at 13, v35 added signal_health at 19, v37 added
        // worktree_path at 20 — see AGENT_NODE_COLUMNS).
        let node = map_agent_node_row(row)?;
        let mesh_name: String = row.get(21)?;
        // Read as Option: a DB migrated from a pre-v14 schema added the column
        // nullable, so any row inserted before `create_agent_node` started
        // stamping it (or via some other path) can be NULL. A non-Option read
        // would make rusqlite error the whole query on a single NULL row,
        // blanking the endpoint. Fall back to the node's creation time.
        let status_changed_at: Option<String> = row.get(22)?;
        let status_changed_at = status_changed_at
            .map(|s| parse_db_timestamp(&s))
            .unwrap_or(node.created_at);
        Ok((node, mesh_name, status_changed_at))
    })?;
    rows.collect()
}

pub(crate) fn get_agent_node_by_id_inner(conn: &Connection, id: i64) -> SqlResult<AgentNode> {
    let mut stmt = conn.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE id = ?1", AGENT_NODE_COLUMNS)
    )?;
    stmt.query_row(params![id], map_agent_node_row)
}
// --- Agent Node operations ---

#[allow(clippy::too_many_arguments)]
pub fn create_agent_node(
    mesh_id: i64,
    name: &str,
    path: &str,
    branch: &str,
    env: EnvType,
    provider: &str,
    worktree_name: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree: bool,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
    worktree_path: Option<&str>,
) -> SqlResult<AgentNode> {
    let env = resolve_spawn_env(provider, worktree_path, use_worktree, env);
    let db = write_conn();
    create_agent_node_inner(
        &db, mesh_id, name, path, branch, env, provider,
        worktree_name, source_issue, source_pr, source_pr_pinned_sha,
        use_worktree, head_repo_owner, head_repo_clone_url, worktree_path,
    )
}

/// Resolve the spawn `EnvType` once per call so the public function and
/// the per-test helper share the same logic instead of duplicating the
/// harness-runtime fallback (issue #1691 review cleanup).
fn resolve_spawn_env(
    provider: &str,
    worktree_path: Option<&str>,
    use_worktree: bool,
    default: EnvType,
) -> EnvType {
    crate::preferences::harness_runtime(provider).unwrap_or_else(|| {
        worktree_path
            .filter(|path| use_worktree && !path.trim().is_empty())
            .map(|path| crate::env::resolve_raw_path(path).env_type)
            .unwrap_or(default)
    })
}

/// Per-test isolated variant of [`create_agent_node`] (issue #1691).
/// The public function locks the process-global writer; this helper
/// takes an explicit `&Connection` so parallel tests can each operate
/// against their own in-memory DB.
///
/// `env` is the *resolved* spawn environment — the caller must run
/// [`resolve_spawn_env`] first so the helper does not duplicate the
/// harness-runtime fallback.
pub(crate) fn create_agent_node_inner(
    db: &Connection,
    mesh_id: i64,
    name: &str,
    path: &str,
    branch: &str,
    env: EnvType,
    provider: &str,
    worktree_name: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree: bool,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
    worktree_path: Option<&str>,
) -> SqlResult<AgentNode> {
    // Append at the end of this mesh's grid order. New nodes land last so an
    // existing arrangement isn't disturbed by a fresh spawn.
    let next_position: i64 = db.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM agent_nodes WHERE mesh_id = ?1",
        params![mesh_id],
        |row| row.get(0),
    )?;
    // Stamp `status_changed_at` explicitly rather than leaning on the column
    // DEFAULT: a DB migrated from pre-v14 added the column nullable with NO
    // default (SQLite can't ALTER-add a non-constant default), so an INSERT that
    // omitted it would store NULL and break the coordinator digest query.
    // Normalize blank worktree_path to NULL — a hand-edited empty string
    // must read back as `None` (legacy fallback), not as a bare empty dir
    // (issue #1519).
    let worktree_path = worktree_path
        .map(str::trim)
        .filter(|s| !s.is_empty());
    db.execute(
        "INSERT INTO agent_nodes (mesh_id, name, path, branch, env, provider, status, worktree_name, source_issue, source_pr, source_pr_pinned_sha, use_worktree, position, status_changed_at, head_repo_owner, head_repo_clone_url, worktree_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            mesh_id,
            name,
            path,
            branch,
            env.to_string(),
            provider,
            worktree_name,
            source_issue,
            source_pr,
            source_pr_pinned_sha,
            if use_worktree { 1 } else { 0 },
            next_position,
            chrono::Utc::now().to_rfc3339(),
            head_repo_owner,
            head_repo_clone_url,
            worktree_path,
        ],
    )?;
    let id = db.last_insert_rowid();
    get_agent_node_by_id_inner(db, id)
}

/// Persist new grid positions for a batch of agent nodes (drag-to-reorder).
/// Callers send the full new ordering for the affected mesh so the DB stays in
/// sync with the frontend's optimistic update. Mirrors `update_mesh_positions_batch`.
pub fn update_agent_node_positions_batch(updates: &[(i64, i64)]) -> SqlResult<()> {
    if updates.is_empty() { return Ok(()); }
    let db = write_conn();
    for (id, pos) in updates {
        db.execute(
            "UPDATE agent_nodes SET position = ?1 WHERE id = ?2",
            params![pos, id],
        )?;
    }
    Ok(())
}

/// Rename IPC only — writes `name` alone and is **not** the spawn-path
/// slug adoption. The spawn path uses `adopt_manual_pool_slug_with_path`,
/// which writes `name`, `worktree_name`, and `worktree_path` together so
/// the close path's removal directory cannot drift from the displayed
/// name (#1080, #1519).
/// Mixing the two would reintroduce the bug.
pub fn update_agent_node_name(id: i64, name: &str) -> SqlResult<()> {
    let db = write_conn();
    db.execute(
        "UPDATE agent_nodes SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    Ok(())
}

/// Legacy two-half adoption (`name` + `worktree_name`, issue #1080) kept for
/// the `db::tests` pins. Production spawns use
/// [`adopt_manual_pool_slug_with_path`], which adds the third half
/// (`worktree_path`, issue #1519). Test-only, like the pins that call it.
#[cfg(test)]
pub(crate) fn adopt_manual_pool_slug_inner(conn: &Connection, id: i64, slug: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_nodes SET name = ?1, worktree_name = ?1 WHERE id = ?2",
        params![slug, id],
    )?;
    Ok(())
}

/// Adopt the pool's slug AND persist the exact resolved `worktree_path`
/// (issue #1519). Manual warm claims resolve onto the already-on-disk
/// pool directory (`entry.path`), which differs from the stage-1
/// throwaway `worktree_path` the row was created with — without this
/// third half of the adoption the close path would derive its removal
/// directory from a stale path and leak the live worktree. Issue/PR
/// spawns don't call this (their pool dir is moved onto the node's own
/// `gh{N}-`/`pr{N}-` target, which the row already stores).
pub fn adopt_manual_pool_slug_with_path(
    id: i64,
    slug: &str,
    worktree_path: Option<&str>,
) -> SqlResult<()> {
    let db = write_conn();
    adopt_manual_pool_slug_with_path_inner(&db, id, slug, worktree_path)
}

pub(crate) fn adopt_manual_pool_slug_with_path_inner(
    conn: &Connection,
    id: i64,
    slug: &str,
    worktree_path: Option<&str>,
) -> SqlResult<()> {
    let worktree_path = worktree_path
        .map(str::trim)
        .filter(|s| !s.is_empty());
    conn.execute(
        "UPDATE agent_nodes SET name = ?1, worktree_name = ?1, worktree_path = ?2 WHERE id = ?3",
        params![slug, worktree_path, id],
    )?;
    Ok(())
}

/// Update an agent node's `provider` column. Used by the Regenerate
/// command (issue #774 / #775) to swap a node's Model Provider on
/// respawn. Stores the opaque harness/profile id verbatim — the
/// resolver shim normalises to a `Provider` enum at the spawn seam,
/// so the caller can pass either a bare `harness` id or a composite
/// `<harness>:<provider_id>` Spawn Option id (issue #575). The
/// underlying SQL is a plain one-column UPDATE; passing the same
/// value the column already carries rewrites the row to itself,
/// which is harmless (the trigger is a Regenerate, not a hot loop)
/// and avoids the need for an `AND provider <> ?1` guard that could
/// silently drop a real rewrite if the comparison string ever drifted
/// from the column's storage form.
pub fn set_agent_node_provider(id: i64, provider: &str) -> SqlResult<()> {
    let runtime = match crate::preferences::harness_runtime(provider) {
        Some(runtime) => runtime,
        None => match get_agent_node_by_id(id) {
            Ok(node) => crate::env::resolve_raw_path(&crate::env::node_working_path(&node).raw_path).env_type,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
            Err(error) => return Err(error),
        },
    }.to_string();
    let db = write_conn();
    db.execute(
        "UPDATE agent_nodes SET provider = ?1, env = ?3 WHERE id = ?2",
        params![provider, id, runtime],
    )?;
    Ok(())
}

/// Set (or clear) an agent node's `is_pinned` flag (wayfinder #982 /
/// ticket #984). The Pinned Grid view mode (ticket #986) renders every
/// node whose `is_pinned = true`, regardless of mesh or status, so the
/// user can build a curated cross-mesh focus list. Returns the number of
/// rows updated so the caller can distinguish "node not found" (zero) from
/// "successfully persisted" (one) — same contract as
/// `update_agent_node_positions` and `set_agent_node_provider`. Mirrors
/// the unconditional single-column UPDATE shape (no `AND is_pinned <> ?1`
/// guard) because writing the same value the column already carries is
/// harmless — the trigger is a UI toggle, not a hot loop.
pub fn set_agent_node_pinned(id: i64, pinned: bool) -> SqlResult<usize> {
    let db = write_conn();
    set_agent_node_pinned_inner(&db, id, pinned)
}

/// Lock-free `_inner` so the migration tests in `db::migration_tests` can
/// exercise the production SQL against an in-memory fixture; duplicating
/// the SQL in the test silently drifts when this path changes.
pub(crate) fn set_agent_node_pinned_inner(
    conn: &Connection,
    id: i64,
    pinned: bool,
) -> SqlResult<usize> {
    conn.execute(
        "UPDATE agent_nodes SET is_pinned = ?1 WHERE id = ?2",
        params![if pinned { 1 } else { 0 }, id],
    )
}

/// Flip an agent node's `is_pinned` flag and return the new value
/// (wayfinder #982 / ticket #984). The return type carries the post-flip
/// state so the frontend store can patch the local entry directly without
/// a follow-up `get_agent_node_by_id` round-trip — same shape as
/// `regenerate_agent_node` (issue #774), which also returns the
/// post-write `AgentNode`.
///
/// The flip is atomic in SQLite: a single `UPDATE ... SET is_pinned = 1 -
/// is_pinned ... RETURNING is_pinned` writes and reads back the new value
/// in one statement. `RETURNING` requires SQLite ≥ 3.35 (March 2021),
/// which every supported Buildmesh target carries (rusqlite 0.32 bundles
/// SQLite ≥ 3.46, see issue #535 baseline). On a missing id the statement
/// still succeeds (0 rows), but `RETURNING` then yields no row — we map
/// that to `Ok(None)` and the caller surfaces "node not found" from the
/// surrounding `#[command]` wrapper rather than faking a flipped boolean.
pub fn toggle_agent_node_pinned(id: i64) -> SqlResult<Option<bool>> {
    let db = write_conn();
    toggle_agent_node_pinned_inner(&db, id)
}

/// Lock-free `_inner` so the migration tests in `db::migration_tests` can
/// exercise the production SQL against an in-memory fixture.
pub(crate) fn toggle_agent_node_pinned_inner(
    conn: &Connection,
    id: i64,
) -> SqlResult<Option<bool>> {
    let mut stmt = conn.prepare(
        "UPDATE agent_nodes SET is_pinned = 1 - is_pinned \
         WHERE id = ?1 \
         RETURNING is_pinned",
    )?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => {
            let new: i32 = row.get(0)?;
            Ok(Some(new != 0))
        }
        None => Ok(None),
    }
}

pub fn get_agent_node_by_id(id: i64) -> SqlResult<AgentNode> {
    let db = read_conn();
    get_agent_node_by_id_inner(&db, id)
}

pub fn list_agent_nodes() -> SqlResult<Vec<AgentNode>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE status != 'archived' ORDER BY mesh_id ASC, position ASC, created_at ASC", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map([], map_agent_node_row)?;
    rows.collect()
}

pub fn list_agent_nodes_by_mesh(mesh_id: i64) -> SqlResult<Vec<AgentNode>> {
    let db = read_conn();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE mesh_id = ?1 ORDER BY position ASC, created_at ASC", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map(params![mesh_id], map_agent_node_row)?;
    rows.collect()
}

pub fn update_agent_node_status(id: i64, status: SessionStatus) -> SqlResult<()> {
    let db = write_conn();
    // Single choke point for status transitions; the coordinator digest
    // reads `status_changed_at` for `last_activity`. Stored as RFC3339
    // rather than SQLite's `datetime('now')` (timezone-aware, sortable).
    update_agent_node_status_inner(&db, id, status)
}

/// `_inner` form of [`update_agent_node_status`]. Exists so the race-fix
/// test in `db/agent_node_tests.rs` can exercise the production SQL against
/// an in-memory fixture; duplicating the SQL in the test silently drifts
/// when this path changes (timestamp format, extra column, transaction).
pub fn update_agent_node_status_inner(
    conn: &Connection,
    id: i64,
    status: SessionStatus,
) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_nodes SET status = ?1, status_changed_at = ?2 WHERE id = ?3",
        params![status.to_db_str(), chrono::Utc::now().to_rfc3339(), id],
    )?;
    Ok(())
}

/// Conditional `update_agent_node_status`. Returns whether the row matched.
///
/// Issue #654 — the orchestrator's delayed `Spawning → Running` promotion;
/// no-op if the reader thread's early-exit Error write already won.
pub fn update_agent_node_status_if(
    id: i64,
    new: SessionStatus,
    expected: SessionStatus,
) -> SqlResult<bool> {
    let db = write_conn();
    update_agent_node_status_if_inner(&db, id, new, expected)
}

/// `_inner` form of [`update_agent_node_status_if`].
///
/// The `AND status = ?4` predicate means a no-op match leaves
/// `status_changed_at` untouched, so the coordinator's `last_activity`
/// keeps reporting the real event (e.g. the reader's Error write) rather
/// than a phantom orchestrator activity timestamp.
pub fn update_agent_node_status_if_inner(
    conn: &Connection,
    id: i64,
    new: SessionStatus,
    expected: SessionStatus,
) -> SqlResult<bool> {
    let changed = conn.execute(
        "UPDATE agent_nodes SET status = ?1, status_changed_at = ?2 \
         WHERE id = ?3 AND status = ?4",
        params![
            new.to_db_str(),
            chrono::Utc::now().to_rfc3339(),
            id,
            expected.to_db_str(),
        ],
    )?;
    Ok(changed > 0)
}

/// Inverse of [`update_agent_node_status_if`]: write `new` UNLESS current
/// status is in `forbidden`. Issue #654 — both writers (orchestrator's
/// `Spawning`, reader's `Error`) forbid the terminal set so whichever
/// fires first sticks and the other becomes a no-op.
pub fn update_agent_node_status_unless_in(
    id: i64,
    new: SessionStatus,
    forbidden: &[SessionStatus],
) -> SqlResult<bool> {
    let db = write_conn();
    update_agent_node_status_unless_in_inner(&db, id, new, forbidden)
}

pub fn update_agent_node_status_unless_in_inner(
    conn: &Connection,
    id: i64,
    new: SessionStatus,
    forbidden: &[SessionStatus],
) -> SqlResult<bool> {
    if forbidden.is_empty() {
        // Disjoint surface from `update_agent_node_status_inner`: an empty
        // forbidden list would match every row, which is exactly that
        // primitive's job.
        return Err(rusqlite::Error::InvalidQuery);
    }
    // Positional placeholders (`?N`) so SQLite parameterises every value —
    // no SQL injection surface, no enum-name interpolation.
    let placeholders = forbidden
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 4))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE agent_nodes SET status = ?1, status_changed_at = ?2 \
         WHERE id = ?3 AND status NOT IN ({placeholders})"
    );
    let now = chrono::Utc::now().to_rfc3339();
    // `SessionStatus::to_db_str` returns `&'static str`, so the slice of
    // refs is `&'static [&'static str]` — no lifetime juggling required,
    // just collect the static refs into a Vec and pass to execute.
    let new_str: &'static str = new.to_db_str();
    let forbidden_strs: Vec<&'static str> = forbidden.iter().map(|f| f.to_db_str()).collect();
    let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(3 + forbidden.len());
    params_vec.push(&new_str);
    params_vec.push(&now);
    params_vec.push(&id);
    for s in &forbidden_strs {
        params_vec.push(s);
    }
    let changed = conn.execute(&sql, params_vec.as_slice())?;
    Ok(changed > 0)
}

pub fn archive_agent_node(id: i64) -> SqlResult<()> {
    update_agent_node_status(id, SessionStatus::Archived)
}

/// Update the persisted hook/attention signal health for an agent node
/// (issue #1364 §3). `None` clears the column (back to "no provisioning
/// outcome / no callback yet").
pub fn update_agent_node_signal_health(
    id: i64,
    health: Option<crate::agent::session_lifecycle::SignalHealth>,
) -> SqlResult<()> {
    let db = write_conn();
    update_agent_node_signal_health_inner(&db, id, health)
}

pub(crate) fn update_agent_node_signal_health_inner(
    conn: &Connection,
    id: i64,
    health: Option<crate::agent::session_lifecycle::SignalHealth>,
) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_nodes SET signal_health = ?1 WHERE id = ?2",
        params![health.map(|h| h.to_db_str()), id],
    )?;
    Ok(())
}

/// Update the persisted CLI session id for an agent node. For the fill-only
/// variant used by attention-hook fallback, see `set_cli_session_id_if_missing`.
pub fn update_cli_session_id(id: i64, cli_id: &str) -> SqlResult<()> {
    let db = write_conn();
    db.execute("UPDATE agent_nodes SET cli_session_id = ?1 WHERE id = ?2", params![cli_id, id])?;
    Ok(())
}

/// Remove the identity of a conversation that is deliberately being replaced
/// with a fresh spawn (for example, after a cross-harness regenerate).
pub fn clear_cli_session_id(id: i64) -> SqlResult<()> {
    let db = write_conn();
    clear_cli_session_id_inner(&db, id)
}

pub(crate) fn clear_cli_session_id_inner(conn: &Connection, id: i64) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_nodes SET cli_session_id = NULL, session_started_at = ?1 WHERE id = ?2",
        params![chrono::Utc::now().timestamp_millis(), id],
    )?;
    Ok(())
}

pub fn session_started_at_ms(id: i64) -> SqlResult<Option<i64>> {
    use rusqlite::OptionalExtension;
    let conn = read_conn();
    conn.query_row("SELECT session_started_at FROM agent_nodes WHERE id = ?1",
        params![id], |row| row.get(0))
        .optional()
}

/// Persist a provider-assigned session id without overwriting an id captured
/// by an earlier, more immediate source. Codex hook callbacks use this as a
/// structured fallback when PTY output did not expose the UUID (issue #1089).
pub fn set_cli_session_id_if_missing(id: i64, cli_id: &str) -> SqlResult<bool> {
    let db = write_conn();
    set_cli_session_id_if_missing_inner(&db, id, cli_id)
}

/// Identity of the process and its last lifecycle transition. Continuations
/// observed before user input/regeneration must not write into the new turn.
pub(crate) fn agent_turn_stamp(id: i64) -> SqlResult<Option<String>> {
    read_conn().query_row("SELECT session_started_at, status_changed_at FROM agent_nodes WHERE id = ?1",
        params![id], |row| {
            let generation: Option<i64> = row.get(0)?;
            let changed: Option<String> = row.get(1)?;
            Ok(generation.map(|g| format!("{g}:{}", changed.unwrap_or_default())))
        })
}

pub(crate) fn set_cli_session_id_if_missing_inner(
    conn: &Connection,
    id: i64,
    cli_id: &str,
) -> SqlResult<bool> {
    let changed = conn.execute(
        "UPDATE agent_nodes SET cli_session_id = ?1 \
         WHERE id = ?2 AND (cli_session_id IS NULL OR cli_session_id = '')",
        params![cli_id, id],
    )?;
    Ok(changed > 0)
}

/// Flip any nodes that cannot be running on startup to `suspended`.
///
/// Covers three states that all mean "we expected this node to be live but
/// its process is gone":
/// - `running` / `awaiting_input`: the agent process died with the app.
/// - `pending`: the two-stage spawn flow created the row in stage-1 but the
///   app crashed before stage-2 (`start_node_background`) could spawn the
///   process. Without this, a stuck `pending` row would render as a
///   perpetual "◌ Starting…" badge with no way to recover.
pub fn mark_running_nodes_suspended() -> SqlResult<usize> {
    let db = write_conn();
    // Deliberately does NOT touch `status_changed_at`: a restart-time suspend is
    // bookkeeping, not agent activity, so the coordinator digest's
    // `last_activity` should keep reporting when the node *actually* last did
    // work (pre-crash), not the moment the app reopened. (See ADR-0008 spine.)
    // `spawning` (issue #654) is included so a crash between process launch
    // and the 3s Running promotion leaves a recoverable `suspended` row.
    let count = db.execute(
        "UPDATE agent_nodes SET status = 'suspended' \
         WHERE status IN ('running', 'awaiting_input', 'pending', 'spawning', 'ready')",
        [],
    )?;
    Ok(count)
}

pub fn list_suspended_nodes() -> SqlResult<Vec<AgentNode>> {
    let db = read_conn();
    list_suspended_nodes_inner(&db)
}

pub(crate) fn list_suspended_nodes_inner(db: &Connection) -> SqlResult<Vec<AgentNode>> {
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE status = 'suspended'", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map([], map_agent_node_row)?;
    rows.collect()
}

pub fn recover_suspended_cli_session_id(node: &AgentNode, cli_id: &str, generation: Option<i64>) -> SqlResult<bool> {
    let conn = write_conn();
    recover_suspended_cli_session_id_inner(&conn, node, cli_id, generation)
}

pub(crate) fn recover_suspended_cli_session_id_inner(
    conn: &Connection, node: &AgentNode, cli_id: &str, generation: Option<i64>,
) -> SqlResult<bool> {
    // The disk scan runs without a DB lock. A user may have launched,
    // regenerated, or deleted the node meanwhile; never write into that run.
    let changed = conn.execute("UPDATE agent_nodes SET cli_session_id = ?1
        WHERE id = ?2 AND status = 'suspended' AND provider = ?3 AND path = ?4
        AND worktree_name IS ?5 AND worktree_path IS ?6
        AND session_started_at IS ?7
        AND (cli_session_id IS NULL OR cli_session_id = '')
        AND NOT EXISTS (SELECT 1 FROM agent_nodes WHERE id != ?2 AND cli_session_id = ?1)",
        params![cli_id, node.id, node.provider, node.path, node.worktree_name, node.worktree_path,
            generation])?;
    Ok(changed > 0)
}

/// Live discovery uses the durable process generation, not the time of the
/// recovery probe. A delayed disk scan must never claim a replacement session.
pub(crate) fn recover_live_cli_session_id(node: &AgentNode, cli_id: &str, generation: i64) -> SqlResult<bool> {
    recover_live_cli_session_id_inner(&write_conn(), node, cli_id, generation)
}

pub(crate) fn recover_live_cli_session_id_inner(
    conn: &Connection, node: &AgentNode, cli_id: &str, generation: i64,
) -> SqlResult<bool> {
    let changed = conn.execute("UPDATE agent_nodes SET cli_session_id = ?1
        WHERE id = ?2 AND status IN ('running', 'ready', 'awaiting_input', 'completed', 'spawning')
        AND provider = ?3 AND path = ?4 AND worktree_name IS ?5 AND worktree_path IS ?6
        AND session_started_at = ?7 AND use_worktree = ?8 AND env = ?9
        AND (cli_session_id IS NULL OR cli_session_id = '')
        AND NOT EXISTS (SELECT 1 FROM agent_nodes WHERE id != ?2 AND cli_session_id = ?1)",
        params![cli_id, node.id, node.provider, node.path, node.worktree_name, node.worktree_path,
            generation, node.use_worktree, node.env.to_string()])?;
    Ok(changed > 0)
}

/// Lean existence check for a node's CLI session id (issue #1499). The
/// capture pollers run this on every retry; selecting the full
/// `AGENT_NODE_COLUMNS` projection plus an `AgentNode` allocation (as
/// `get_agent_node_by_id` does) on a hot loop is wasteful. The predicate
/// mirrors `set_cli_session_id_if_missing_inner`'s write guard exactly —
/// present here means a conditional write would be a no-op there.
pub fn cli_session_id_present(id: i64) -> SqlResult<bool> {
    let db = read_conn();
    cli_session_id_present_inner(&db, id)
}

pub(crate) fn cli_session_id_present_inner(conn: &Connection, id: i64) -> SqlResult<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_nodes WHERE id = ?1 \
         AND cli_session_id IS NOT NULL AND cli_session_id != '')",
        params![id],
        |row| row.get(0),
    )
}

// --- Pending worktree removal queue ---
//
// Closing a node deletes its row immediately so the UI can drop it at once, but
// the worktree directory removal is slow and retry-prone. We record the intent
// here (atomically with the row delete) so a background task — or the next app
// launch — can finish the removal. `worktree_path` is UNIQUE so re-enqueuing the
// same path is a no-op rather than a duplicate.

pub(crate) fn enqueue_worktree_removal_inner(conn: &Connection, path: &str, node_name: &str) -> SqlResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pending_worktree_removals (worktree_path, node_name) VALUES (?1, ?2)",
        params![path, node_name],
    )?;
    Ok(())
}

pub(crate) fn list_pending_worktree_removals_inner(conn: &Connection) -> SqlResult<Vec<PendingWorktreeRemoval>> {
    let mut stmt = conn.prepare(
        "SELECT worktree_path, node_name FROM pending_worktree_removals ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PendingWorktreeRemoval {
            worktree_path: row.get(0)?,
            node_name: row.get(1)?,
        })
    })?;
    rows.collect()
}

pub(crate) fn delete_pending_worktree_removal_inner(conn: &Connection, path: &str) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM pending_worktree_removals WHERE worktree_path = ?1",
        params![path],
    )?;
    Ok(())
}

pub(crate) fn delete_agent_node_enqueueing_removal_inner(
    conn: &Connection,
    id: i64,
    removal: Option<(&str, &str)>,
) -> SqlResult<()> {
    conn.execute("DELETE FROM agent_nodes WHERE id = ?1", params![id])?;
    conn.execute(
        "DELETE FROM app_settings WHERE key = ?1",
        params![format!("{}{id}", crate::db::semantic_turns::SEMANTIC_TURN_KEY_PREFIX)],
    )?;
    if let Some((path, node_name)) = removal {
        enqueue_worktree_removal_inner(conn, path, node_name)?;
    }
    Ok(())
}

/// Delete an agent node row and, in the same transaction, enqueue its worktree
/// for background removal. Doing both atomically is what makes the optimistic
/// close honest: the system can never forget a worktree it owes a cleanup, even
/// if it's killed between the two writes.
pub fn delete_agent_node_enqueueing_removal(
    id: i64,
    removal: Option<(&str, &str)>,
) -> SqlResult<()> {
    let mut db = write_conn();
    let tx = db.transaction()?;
    delete_agent_node_enqueueing_removal_inner(&tx, id, removal)?;
    tx.commit()
}

pub fn list_pending_worktree_removals() -> SqlResult<Vec<PendingWorktreeRemoval>> {
    let db = read_conn();
    list_pending_worktree_removals_inner(&db)
}

pub fn delete_pending_worktree_removal(path: &str) -> SqlResult<()> {
    let db = write_conn();
    delete_pending_worktree_removal_inner(&db, path)
}

/// Safety net (v19): re-apply the **custom-account** Spawn Option
/// composite-id migration. Called from `lib.rs::setup` after
/// `preferences::init` (because it needs the live
/// `Vec<ProviderAccount>`). The companion to
/// `db::migrations::ensure_agent_node_provider_id_migrated` — together
/// they guarantee a v19+ DB never has a bare proxied-provider id in
/// `agent_nodes.provider` that should have been rewritten.
///
/// **Idempotent**: the underlying migration's `WHERE provider NOT
/// LIKE '%:%'` guard skips rows already in composite form, and the
/// `provider NOT IN (...)` whitelist protects bare `HarnessProfile`
/// ids from being rewritten.
///
/// Moved from `db/mod.rs` as part of issue #1655: `mod.rs` must own
/// connection + init + baseline DDL only — every domain mutation
/// belongs in the module that owns the table.
pub(crate) fn ensure_agent_node_provider_id_custom_accounts_migrated(
    conn: &Connection,
    accounts: &[crate::preferences::ProviderAccount],
) -> SqlResult<()> {
    migrate_agent_node_provider_id_custom_accounts(conn, accounts)
}

/// v19 Spawn Option composite-id migration, **custom-account block**
/// (issue #575 / ADR-0016 §6). Rewrites any bare id that names a
/// `claude_compatible` `ProviderAccount` (a user-configured custom
/// endpoint) to `claude:<id>`. Split from the always-run version so it
/// can be called from `lib.rs::setup` *after* `preferences::init` —
/// the first-class block has no preferences dependency and is
/// therefore safe to run from `db::init`, but the custom-account
/// block needs the live `Vec<ProviderAccount>` (the user's stored
/// `preferences.json` merged with the code-defined defaults).
///
/// **Idempotent**: `WHERE provider NOT LIKE '%:%'` skips already-
/// migrated rows. The `provider NOT IN (...)` whitelist of built-in
/// harness ids protects against accidentally rewriting a custom
/// `HarnessProfile` row whose `id` happens to match a proxied
/// provider id (the two lists are separate, but the SQL guard
/// guarantees the rewrite only fires for rows that look like bare
/// `ProviderAccount` ids, never bare `HarnessProfile` ids).
///
/// **Filter on `enabled`**: a disabled custom account is left bare
/// so the resolver falls through to the Anthropic default at spawn
/// time. This is intentional — the user's archived node "remembers"
/// they disabled the account, and silently re-enabling it on the
/// node would be surprising. Re-enabling the account + restart
/// triggers another migration run via
/// `ensure_agent_node_provider_id_custom_accounts_migrated`.
pub(crate) fn migrate_agent_node_provider_id_custom_accounts(
    conn: &Connection,
    accounts: &[crate::preferences::ProviderAccount],
) -> SqlResult<()> {
    // Same table-exists guard as the main migration.
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='agent_nodes'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(());
    }
    // Collect first — `UPDATE ... WHERE provider IN (...)` with
    // a Rust-built IN list keeps the migration a single SQL
    // statement and the bound parameters are bound, not
    // string-interpolated.
    let custom_ids: Vec<String> = accounts
        .iter()
        .filter(|a| {
            a.claude_compatible
                && a.enabled
                && !a.id.is_empty()
                && !a.id.contains(':')
        })
        .map(|a| a.id.clone())
        .collect();
    if !custom_ids.is_empty() {
        let placeholders = custom_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        // The `NOT IN` whitelist of built-in harness ids uses the shared
        // `BUILTIN_HARNESS_IDS` const so the SQL guard and the wire-shape
        // doc in `agent::provider::ProviderInfo` can't drift apart
        // (issue #583 cleanup — the previous hardcoded list of six
        // literals had no single source of truth).
        let builtin_placeholders = crate::agent::provider::BUILTIN_HARNESS_IDS
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE agent_nodes \
                SET provider = 'claude:' || provider \
              WHERE provider NOT LIKE '%:%' \
                AND provider NOT IN ({builtin_placeholders}) \
                AND provider IN ({placeholders})",
        );
        // Bind the whitelist literals first, then the custom account ids —
        // the placeholders appear in the SQL in that order.
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = crate::agent::provider::BUILTIN_HARNESS_IDS
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        params_vec.extend(custom_ids.iter().map(|s| s as &dyn rusqlite::ToSql));
        let rows_custom = conn.execute(&sql, params_vec.as_slice())?;
        if rows_custom > 0 {
            tracing::info!(
                "migrate_agent_node_provider_id_custom_accounts: rewrote {} agent_nodes from custom bare account ids",
                rows_custom
            );
        }
    }

    Ok(())
}
