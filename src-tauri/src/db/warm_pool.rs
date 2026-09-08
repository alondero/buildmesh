//! Pre-spawn worktree pool persistence (issue #609, PRD #608).

use std::collections::HashSet;

use rusqlite::{Connection, params};

use super::{read_conn, write_conn, SqlResult};

// --- Pre-spawn Worktree Pool (issue #609, PRD #608) ---------------------------
//
// The pool is opt-in and best-effort: the spawn pipeline always falls back to a
// cold worktree creation when no `available` row exists for the mesh, so a
// corrupted / empty pool never blocks spawn. The DB row is just bookkeeping —
// the actual fast-checkout benefit comes from the on-disk directory the row
// points at. The row's only invariants are (a) `path` matches an existing
// directory when `status = 'available'` (or `spawn` will cold-fall-back),
// (b) `preassigned_name` is unique per mesh (so a claim never aliases an
// existing `agent_nodes.worktree_name`), and (c) `base_sha` is what `git rev-
// parse HEAD` returns inside the directory.

/// Lifecycle states for a `warm_worktrees` row.
///
/// `filling` is set while the background worker is mid-checkout — a concurrent
/// `claim_warm_entry_for_mesh` skips it and either takes the next `available`
/// row or returns `None` (cold spawn). `refreshing` is the analogous mid-flight
/// marker for the background SHA-refresh loop (PRD #608 §4 — declared here so
/// the reconcile + claim filters already recognise it; its producer is a
/// follow-up). `claimed` is the transient in-flight marker the claim flips the
/// row to; if `forget_after_spawn` then fails (DB error after a successful
/// spawn), the row sits at `claimed` with a live directory — the startup
/// reconcile prunes those (`status='claimed'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarmWorktreeStatus {
    Filling,
    Refreshing,
    Available,
    Claimed,
}

impl WarmWorktreeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            WarmWorktreeStatus::Filling => "filling",
            WarmWorktreeStatus::Refreshing => "refreshing",
            WarmWorktreeStatus::Available => "available",
            WarmWorktreeStatus::Claimed => "claimed",
        }
    }
}

// `ensure_warm_worktables_table` (v21) is subsumed by the baseline-table
// phase that every `db::migrations::evolve_to` call runs before columns.

/// Insert a new warm_worktrees row. The pool worker calls this AFTER cutting
/// the on-disk worktree so a `status = 'available'` row always points at a
/// real directory. `base_sha` is recorded for the spawn-time freshness check.
pub fn insert_warm_worktree(
    mesh_id: i64,
    path: &str,
    preassigned_name: &str,
    base_sha: Option<&str>,
    status: WarmWorktreeStatus,
) -> SqlResult<i64> {
    let db = write_conn();
    insert_warm_worktree_inner(&db, mesh_id, path, preassigned_name, base_sha, status)
}

pub(crate) fn insert_warm_worktree_inner(
    conn: &Connection,
    mesh_id: i64,
    path: &str,
    preassigned_name: &str,
    base_sha: Option<&str>,
    status: WarmWorktreeStatus,
) -> SqlResult<i64> {
    conn.execute(
        "INSERT INTO warm_worktrees (mesh_id, path, preassigned_name, status, base_sha)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            mesh_id,
            path,
            preassigned_name,
            status.as_str(),
            base_sha,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Mark a fresh row as `available` once the pool worker finishes cutting the
/// on-disk worktree. The flip is a single UPDATE — no race window where a
/// concurrent claim sees a `filling` row that doesn't yet point at a real
/// directory, because claimers always take the row to `claimed` BEFORE they
/// adopt its path.
pub fn mark_warm_worktree_available(id: i64, base_sha: Option<&str>) -> SqlResult<()> {
    let db = write_conn();
    mark_warm_worktree_available_inner(&db, id, base_sha)
}

pub(crate) fn mark_warm_worktree_available_inner(
    conn: &Connection,
    id: i64,
    base_sha: Option<&str>,
) -> SqlResult<()> {
    conn.execute(
        "UPDATE warm_worktrees SET status = ?1, base_sha = ?2, updated_at = datetime('now') WHERE id = ?3",
        params![WarmWorktreeStatus::Available.as_str(), base_sha, id],
    )?;
    Ok(())
}

/// Flip a warm entry to `refreshing` so a concurrent claim skips it while a
/// background `git reset --hard` is in flight (issue #613 ref-freshness). The
/// claim filter only matches `available` rows, so a row parked at `refreshing`
/// is invisible to `claim_warm_entry_for_mesh` until the freshness pass flips
/// it back to `available` via `mark_warm_worktree_available`. Symmetric with
/// `mark_warm_worktree_available` (which is what restores it).
pub fn mark_warm_worktree_refreshing(id: i64) -> SqlResult<()> {
    let db = write_conn();
    mark_warm_worktree_refreshing_inner(&db, id)
}

pub(crate) fn mark_warm_worktree_refreshing_inner(conn: &Connection, id: i64) -> SqlResult<()> {
    conn.execute(
        "UPDATE warm_worktrees SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![WarmWorktreeStatus::Refreshing.as_str(), id],
    )?;
    Ok(())
}

/// List every `available` warm entry for a mesh — the candidates the
/// ref-freshness pass (issue #613) checks against the freshly-fetched base
/// SHA. Only `available` rows are returned: `filling` rows aren't checked out
/// yet, `refreshing` rows are already mid-reset, and `claimed` rows belong to
/// a live spawn. Returns the same `WarmWorktree` projection a claim hands back
/// (`base_sha` is the field the freshness pass diffs against the new SHA).
pub fn list_available_warm_for_mesh(mesh_id: i64) -> SqlResult<Vec<WarmWorktree>> {
    let db = read_conn();
    list_available_warm_for_mesh_inner(&db, mesh_id)
}

pub(crate) fn list_available_warm_for_mesh_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<Vec<WarmWorktree>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, preassigned_name, base_sha
         FROM warm_worktrees
         WHERE mesh_id = ?1 AND status = ?2
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(
        params![mesh_id, WarmWorktreeStatus::Available.as_str()],
        |row| {
            Ok(WarmWorktree {
                id: row.get(0)?,
                path: row.get(1)?,
                preassigned_name: row.get(2)?,
                base_sha: row.get(3)?,
            })
        },
    )?;
    rows.collect()
}

/// Atomically claim the oldest available warm entry for `mesh_id`.
///
/// "Atomic" here means: a single `UPDATE ... RETURNING` flips status from
/// `available` to `claimed` and returns the row, so two concurrent manual
/// spawns on the same mesh can never both claim the same entry. SQLite
/// serialises the write inside the transaction; the spawn that loses the
/// race simply gets `None` and falls back to cold.
///
/// Returns `None` if no `available` row exists (empty / corrupted pool, or all
/// entries are mid-fill) — caller is expected to cold-spawn in that case.
pub fn claim_warm_entry_for_mesh(mesh_id: i64) -> SqlResult<Option<WarmWorktree>> {
    let db = write_conn();
    claim_warm_entry_for_mesh_inner(&db, mesh_id)
}

pub(crate) fn claim_warm_entry_for_mesh_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<Option<WarmWorktree>> {
    // Pick the oldest available row by `created_at` so the pool drains FIFO —
    // a long-lived warm entry is the one most likely to need a background
    // refresh, and adopting it now evens out the freshness.
    let mut stmt = conn.prepare(
        "SELECT id, path, preassigned_name, base_sha
         FROM warm_worktrees
         WHERE mesh_id = ?1 AND status = ?2
         ORDER BY created_at ASC
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![mesh_id, WarmWorktreeStatus::Available.as_str()])?;
    let row = match rows.next()? {
        Some(r) => r,
        None => return Ok(None),
    };
    let id: i64 = row.get(0)?;
    // Flip to `claimed` in a single statement. The transaction-scoped write
    // means a concurrent claimer that selected the same row will either see
    // status = 'claimed' (skipped by the WHERE filter) or be blocked behind
    // the in-flight transaction — never both read+write 'available'.
    let updated = conn.execute(
        "UPDATE warm_worktrees SET status = ?1, updated_at = datetime('now') WHERE id = ?2 AND status = ?3",
        params![
            WarmWorktreeStatus::Claimed.as_str(),
            id,
            WarmWorktreeStatus::Available.as_str(),
        ],
    )?;
    if updated == 0 {
        // Lost the race to a concurrent claimer; report no row.
        return Ok(None);
    }
    Ok(Some(WarmWorktree {
        id,
        path: row.get(1)?,
        preassigned_name: row.get(2)?,
        base_sha: row.get(3)?,
    }))
}

/// Delete a warm pool row by id. Called after a successful spawn (we don't
/// keep the row around as a 'claimed' tombstone — the directory itself
/// becomes the node's worktree and the row's bookkeeping purpose is done).
pub fn delete_warm_worktree(id: i64) -> SqlResult<()> {
    let db = write_conn();
    delete_warm_worktree_inner(&db, id)
}

/// Lock-free `_inner` so the use-site guard in
/// `services::warm_pool::recheck_after_claim` can drop a row against an
/// in-memory test connection without taking the global DB writer. The
/// `WHERE id = ?` is the primary-key index, so this is O(log n) regardless
/// of pool size. Idempotent: 0 rows affected on a missing id is not an
/// error (rusqlite returns `Ok(0)`), which is the property the
/// recheck_after_claim tests rely on for double-recheck safety.
pub(crate) fn delete_warm_worktree_inner(conn: &Connection, id: i64) -> SqlResult<()> {
    conn.execute("DELETE FROM warm_worktrees WHERE id = ?1", params![id])?;
    Ok(())
}

/// Delete every warm pool row for a mesh. Called by `delete_mesh` (foreign-key
/// cascade is off, so the rows must be removed explicitly). Returns the number
/// of rows deleted. Lock-free `_inner` so `delete_mesh` can run it under the
/// connection it already holds.
pub(crate) fn delete_warm_worktrees_for_mesh_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<usize> {
    let n = conn.execute("DELETE FROM warm_worktrees WHERE mesh_id = ?1", params![mesh_id])?;
    Ok(n)
}

/// List every warm pool directory path for a mesh, regardless of status
/// (issue #639 gap 3). The "everything" view — kept for diagnostic /
/// audit tools that want the full set of warm paths for a mesh,
/// INCLUDING `claimed` rows whose directories may back live agents.
/// Safe force-removal must exclude `claimed` rows (their directories may
/// back live agent processes); use [`list_warm_paths_for_mesh_droppable`]
/// for that path (#642.1).
///
/// Returns absolute host paths. Cheap: a `SELECT path` over a small index.
/// `#[allow(dead_code)]` because no production code path currently needs
/// the everything view; preserving the public surface so diagnostic tools
/// and the next issue can reach it without going through the `pub(crate)`
/// inner.
#[allow(dead_code)]
pub fn list_warm_paths_for_mesh(mesh_id: i64) -> SqlResult<Vec<String>> {
    let db = read_conn();
    list_warm_paths_for_mesh_inner(&db, mesh_id)
}

pub(crate) fn list_warm_paths_for_mesh_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM warm_worktrees WHERE mesh_id = ?1")?;
    let rows = stmt.query_map(params![mesh_id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// List warm pool directory paths for a mesh that are SAFE to force-remove —
/// every status EXCEPT `claimed`. This is the correct view for `delete_mesh`:
/// a `claimed` row's directory may back a live agent process, and force-
/// removing it would destroy the agent's working tree (#642.1). The mesh's
/// `agent_nodes` rows are also deleted by the cascade, so we can't ask the DB
/// "is there a live agent for this path?" — the conservative choice is to
/// skip every claimed row and leave the dir behind. The user opted to delete
/// the mesh; if they want the claimed dir gone too they can remove it by
/// hand. The dir is leaked (not data-lost) — `process_pending_removals` does
/// NOT help here because the mesh's `agent_nodes` rows are cascade-deleted
/// by the same transaction, so no `close` event ever fires for them.
pub fn list_warm_paths_for_mesh_droppable(mesh_id: i64) -> SqlResult<Vec<String>> {
    let db = read_conn();
    list_warm_paths_for_mesh_droppable_inner(&db, mesh_id)
}

pub(crate) fn list_warm_paths_for_mesh_droppable_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT path FROM warm_worktrees WHERE mesh_id = ?1 AND status != 'claimed'",
    )?;
    let rows = stmt.query_map(params![mesh_id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// A pool row the startup reconcile must tear down: its `id` (to delete the
/// SQLite row), its on-disk `path`, and whether that directory is still
/// `dir_present` (so the caller knows whether a Git worktree teardown is even
/// needed before dropping the row). See `list_warm_worktrees_to_reconcile_inner`
/// for which rows qualify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmReconcileEntry {
    pub id: i64,
    pub path: String,
    /// `true` when `path` still exists on disk at scan time — the reconcile
    /// must tear down the Git worktree before deleting the row. `false` ⇒ the
    /// directory is already gone (manual delete / crash before checkout), so
    /// there is nothing on disk to remove and the row can be dropped directly.
    pub dir_present: bool,
}

/// List every pool row the startup reconcile must clean up (issue #610). A row
/// qualifies when EITHER:
///   * it is an `available` row whose on-disk directory is missing — the user
///     hand-deleted `.claude/worktrees/<slug>` (an `available` row always had
///     its directory created, so a missing one is unambiguously broken,
///     regardless of age), OR
///   * it is stuck `filling` / `refreshing` AND is older than
///     `stale_after_minutes`. The age guard is load-bearing: `prewarm_one`
///     (run by this reconcile's own fill step AND by `refill_after_claim` on a
///     separate thread) inserts a `filling` row and then spends *seconds*
///     inside `create_git_worktree` before flipping it to `available`. Without
///     the age guard the reconcile could observe a row a worker is actively
///     mid-checkout on and destroy the in-flight worktree. A genuine
///     crash-orphan is always older than a few minutes (the app was closed and
///     relaunched in between); an in-flight fill is seconds old. The threshold
///     cleanly separates the two.
///
/// `claimed` rows are deliberately EXCLUDED: their directory may already back a
/// live agent node's worktree, so the caller must never tear it down — those
/// are pruned (row only) by `delete_orphaned_claimed_warm_worktrees`.
pub fn list_warm_worktrees_to_reconcile(
    stale_after_minutes: i64,
) -> SqlResult<Vec<WarmReconcileEntry>> {
    let db = read_conn();
    list_warm_worktrees_to_reconcile_inner(&db, stale_after_minutes)
}

pub(crate) fn list_warm_worktrees_to_reconcile_inner(
    conn: &Connection,
    stale_after_minutes: i64,
) -> SqlResult<Vec<WarmReconcileEntry>> {
    // SQLite computes the age flag (`created_at` older than the threshold); the
    // disk-existence check can't be pushed into SQL so the final
    // in-flight-vs-available decision is made in Rust. The modifier string is
    // assembled with `||` so the threshold binds as a parameter rather than
    // being interpolated into SQL.
    let mut stmt = conn.prepare(
        "SELECT id, path, status,
                (created_at <= datetime('now', '-' || ?1 || ' minutes')) AS age_stale
         FROM warm_worktrees
         WHERE status != 'claimed'",
    )?;
    let rows = stmt.query_map(params![stale_after_minutes], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)? != 0,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id, path, status, age_stale) = r?;
        let in_flight = status == WarmWorktreeStatus::Filling.as_str()
            || status == WarmWorktreeStatus::Refreshing.as_str();
        let dir_present = std::path::Path::new(&path).exists();
        // In-flight rows are reconciled only when old enough to be a
        // crash-orphan (never a row a worker is filling right now); a settled
        // `available` row is reconciled when its directory has vanished.
        let qualifies = if in_flight { age_stale } else { !dir_present };
        if qualifies {
            out.push(WarmReconcileEntry {
                id,
                path,
                dir_present,
            });
        }
    }
    Ok(out)
}

/// Delete rows that are stuck in `claimed` status. The only path that
/// produces a `claimed` row is `claim_warm_entry_for_mesh_inner`; the only
/// path that should remove it is `forget_after_spawn`, called from the
/// spawn's success branch. If that DELETE fails (DB error after a
/// successful spawn) the row sits at `claimed` forever — `claim_warm_entry`
/// won't pick it up again (the `status='available'` filter blocks it) and
/// the missing-dir scan (`list_warm_worktrees_to_reconcile` deliberately
/// excludes `claimed` rows because their directory may back a live node)
/// won't prune it either. This function closes that hole.
///
/// **#697 algorithm — per-row classification against the live session
/// snapshot it receives (production: `PROCESS_REGISTRY.session_ids()`):**
///
/// 1. **Adopted** — a live session exists on the warm row's mesh. The
///    spawn that claimed this row necessarily attached to that mesh, so
///    a live session there is sufficient evidence to refuse teardown.
///    Drop the row, **preserve** the directory. We deliberately do NOT
///    gate on `agent_nodes.worktree_name` matching `preassigned_name`
///    here — the #642.2 revert showed that gate fails open in the silent-
///    UPDATE-failure corner case (`set_agent_node_worktree_name` errored,
///    `agent_nodes.worktree_name` stays at the throwaway stage-1 slug,
///    GC misclassifies the row as orphan and tears down the live CWD).
///
/// 2. **Crashed-spawn orphan** — no live session on the mesh AND the
///    directory exists on disk. The spawn crashed mid-claim without
///    `forget_after_spawn` firing (so the row is stuck at `claimed` and
///    the directory is on disk with no agent to adopt it). Tear down the
///    git worktree metadata via `git::worktree::remove_one_worktree`,
///    then drop the row. This is the orphan leak #697 closes.
///
/// 3. **Mid-move / pre-missing** — no live session AND no directory on
///    disk. The Issue/PR spawn moves the directory onto a `gh{N}-`/`pr{N}-`
///    path (#612) before `forget_after_spawn` fires; if the spawn crashes
///    between the move and the row drop, the original pool path is empty
///    and the bookkeeping row is safe to drop with no fs action. The
///    pre-#697 row-only GC also handled this case (and still does here).
///
/// **Why "any live session on the mesh" rather than "live session whose
/// derived worktree path == warm.path":** the strict path-equality check
/// is the algorithm the issue body sketches, but it inherits the #642.2
/// data-loss bug — when `agent_nodes.worktree_name` is stale, the derived
/// path doesn't match the warm row's `path`, the strict check classifies
/// the row as orphan, and the GC tears down the LIVE agent's CWD. The
/// `any session on this mesh` predicate is a strictly looser (and
/// therefore safer) sufficient condition. The trade-off is more directory
/// leaks if a spawn is mid-flight on a different row of the same mesh
/// when GC runs — bounded by `claimed` rows per mesh, no data loss.
///
/// **No age guard:** a fresh `claimed` row is just as stuck as an old one
/// if `forget_after_spawn` delete fails. Called once from
/// `reconcile_on_startup` (step 1a, before the missing-dir scan).
pub fn delete_orphaned_claimed_warm_worktrees() -> SqlResult<usize> {
    // Snapshot live sessions ONCE before taking the DB lock. The call is
    // cheap (a `Vec` clone over the registry's session-id set) and the
    // snapshot is what the inner function uses to classify rows. Holding
    // the snapshot outside the lock closes a tiny but real race: a spawn
    // that register-then-dies between our snapshot and our iteration
    // would otherwise intermittently flip a row from "adopted" to
    // "orphan" mid-pass.
    let live_session_ids = crate::agent::process::PROCESS_REGISTRY.session_ids();

    // ---- Phase 1: snapshot rows + classify, under the global DB mutex ----
    //
    // The plan is a small `Vec` of `(row_id, path, tear_down)`. Building it
    // touches the DB twice (`SELECT id, mesh_id, path FROM warm_worktrees
    // WHERE status = 'claimed'`, then `live_mesh_ids_for`'s `SELECT DISTINCT
    // mesh_id FROM agent_nodes WHERE id IN (...)`). Both are cheap and
    // both finish before we drop the guard.
    let plan = {
        let db = write_conn();
        plan_orphan_cleanup(&db, &live_session_ids)?
    };
    // ↑↑↑ DB MUTEX DROPPED HERE ↑↑↑
    //
    // HARD RULE (issue #1228): the rest of this function MUST NOT touch
    // the DB or shell out to git while holding the writer mutex. The
    // FS teardown below is the slow part — `git worktree remove --force`
    // plus a recursive `remove_dir_all` fallback — and on Windows with
    // antivirus-inflated delete latency it can hold the lock for many
    // seconds. Every other DB touch in the app (attention flips, status
    // writes, HTTP auth token validation) queues behind that mutex during
    // startup reconcile, freezing the UI. It is also one refactor away
    // from a true deadlock: if `remove_one_worktree`'s subtree ever gains
    // a DB read it self-deadlocks against the guard we're holding.

    // ---- Phase 2: filesystem teardown, lock-free ----
    //
    // The plan carries everything the FS phase needs (no DB lookup
    // happens here). Failures are intentionally swallowed — a stuck
    // orphan row is strictly less harmful than blocking the row drop on
    // a flaky filesystem; the next reconcile pass retries the teardown.
    for (_id, path, tear_down) in &plan {
        if *tear_down {
            tear_down_warm_worktree_path(path);
        }
    }

    // ---- Phase 3: batch row DELETEs under the re-acquired mutex ----
    //
    // One short lock acquisition per row keeps the SQL plan trivial for
    // SQLite and the existing `idx_*` indexes unchanged. The DELETE phase
    // is purely a bookkeeping sweep: the FS work above already happened
    // and its outcomes don't influence what we commit here (Branch 1
    // "preserve dir" and Branch 3 "no dir on disk" both delete the row).
    let ids: Vec<i64> = plan.iter().map(|(id, _, _)| *id).collect();
    let db = write_conn();
    batch_delete_warm_worktrees_by_id(&db, &ids)
}

/// Test-facing entry point: snapshot + classify + FS teardown + DELETE, all
/// in one shot against a caller-supplied `&Connection`. Used by
/// `warm_pool_tests` to exercise the per-row classification branches
/// against in-memory fixtures without spinning up the global DB singleton.
///
/// Production goes through `delete_orphaned_claimed_warm_worktrees` (above),
/// which composes the same helpers but releases the DB mutex between the
/// classify and FS phases — see issue #1228.
#[allow(dead_code)] // Test-only consumer (`warm_pool_tests`); clippy's
                    // lib-build dead-code check doesn't see across the
                    // #[cfg(test)] boundary, so we have to opt out. Same
                    // pattern as `is_initialized` above.
pub(crate) fn delete_orphaned_claimed_warm_worktrees_inner(
    conn: &Connection,
    live_session_ids: &[i64],
) -> SqlResult<usize> {
    let plan = plan_orphan_cleanup(conn, live_session_ids)?;
    for (_id, path, tear_down) in &plan {
        if *tear_down {
            tear_down_warm_worktree_path(path);
        }
    }
    let ids: Vec<i64> = plan.iter().map(|(id, _, _)| *id).collect();
    batch_delete_warm_worktrees_by_id(conn, &ids)
}

/// Phase 1 helper: snapshot every `claimed` row and classify it against the
/// live-session snapshot. Pure read against the DB — no FS, no subprocess,
/// no row mutation. Returns `(row_id, path, tear_down)` per row; `tear_down`
/// is `true` only when the row's mesh has no live session (Branch 2 of the
/// algorithm documented on `delete_orphaned_claimed_warm_worktrees`).
///
/// Must be called with a connection whose DB mutex (if any) is held by the
/// caller. The FS phase that follows is lock-free by design.
fn plan_orphan_cleanup(
    conn: &Connection,
    live_session_ids: &[i64],
) -> SqlResult<Vec<(i64, String, bool)>> {
    // Read the full set of claimed rows up front, then drop the prepared
    // statement so the loop below doesn't keep a statement handle alive
    // across the FS phase (which runs lock-free after we return).
    let mut stmt = conn.prepare(
        "SELECT id, mesh_id, path FROM warm_worktrees WHERE status = 'claimed'",
    )?;
    let claimed: Vec<(i64, i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    if claimed.is_empty() {
        return Ok(Vec::new());
    }

    // Which meshes currently have a live session? Build the lookup set
    // from PROCESS_REGISTRY's session-id snapshot + agent_nodes.mesh_id.
    // Returns `None` if the agent_nodes query errored (rather than an
    // empty set) — an empty set would let the loop below tear down a
    // live agent's CWD because we "couldn't see" any live sessions. With
    // `None`, the loop falls back to row-only behaviour (the same shape
    // as the pre-#697 GC: drop rows, leave the filesystem alone).
    let live_mesh_ids = match live_mesh_ids_for(conn, live_session_ids) {
        Some(set) => Some(set),
        None => {
            tracing::warn!(
                "plan_orphan_cleanup: could not resolve live_mesh_ids \
                 ({} live session ids in snapshot); falling back to row-only \
                 behaviour (no filesystem teardown) to be safe",
                live_session_ids.len()
            );
            None
        }
    };

    Ok(claimed
        .into_iter()
        .map(|(id, mesh_id, path)| {
            let tear_down = match &live_mesh_ids {
                // Couldn't query live sessions → be conservative. Even
                // though there almost certainly is no live agent
                // (startup reconcile runs before user-facing spawn), a
                // transient DB error is not an excuse to destroy a
                // working tree. Drop the row but leave the dir intact —
                // the next reconcile (with a recovered DB) can retry
                // the teardown.
                None => false,
                Some(live) => !live.contains(&mesh_id),
            };
            (id, path, tear_down)
        })
        .collect())
}

/// Phase 2 helper: best-effort teardown of one warm pool worktree path.
/// Pure FS / subprocess — MUST NOT touch the DB. The caller is responsible
/// for holding no DB mutex across this call (issue #1228).
///
/// Two-step teardown: `remove_one_worktree` is the polite path that clears
/// `git worktree remove --force` metadata; if it returns `Err` (test
/// fixture was a plain tempdir, OR production hit a locked handle /
/// non-worktree state), the `remove_dir_all` fallback guarantees the
/// on-disk leak actually closes. Order matters — `remove_one_worktree`
/// first so a real git worktree loses its bookkeeping before the dir
/// goes (which is what unblocks a future `git worktree add` for the same
/// slug).
fn tear_down_warm_worktree_path(path: &str) {
    if crate::git::worktree::remove_one_worktree(path).is_err()
        && std::path::Path::new(path).exists()
    {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Phase 3 helper: DELETE the supplied row ids. Pure write against the DB.
/// Caller holds whatever mutex protects the connection. One statement per
/// id keeps the SQL plan trivial and the existing `idx_*` indexes in use;
/// the row count here is bounded by `claimed`-rows-per-mesh in practice.
fn batch_delete_warm_worktrees_by_id(conn: &Connection, ids: &[i64]) -> SqlResult<usize> {
    let mut deleted = 0;
    for id in ids {
        conn.execute("DELETE FROM warm_worktrees WHERE id = ?1", params![id])?;
        deleted += 1;
    }
    Ok(deleted)
}

/// Resolve `live_session_ids` (`PROCESS_REGISTRY.session_ids()` in
/// production) into the set of `mesh_id`s whose agents are currently live.
/// Returns `None` on any query error so the caller can choose the safe
/// fallback (row-only, no filesystem action). Returning an empty set
/// here would be ambiguous — "no live sessions" and "I couldn't tell"
/// are different facts and the caller's teardown decision depends on
/// which one is true.
///
/// `SELECT DISTINCT mesh_id FROM agent_nodes WHERE id IN (...)` is one
/// round-trip regardless of how many sessions are live. SQLite's variable
/// cap (999 by default) is generous compared to realistic session
/// counts; a future scale-up switch to a temp-table join would survive
/// this limit transparently.
fn live_mesh_ids_for(
    conn: &Connection,
    live_session_ids: &[i64],
) -> Option<HashSet<i64>> {
    if live_session_ids.is_empty() {
        // An empty PROCESS_REGISTRY snapshot IS a known fact (not an
        // error): no agents are currently running. Returning `Some(empty)`
        // here is what tells the caller it's safe to consider every
        // claimed row a candidate for orphan teardown.
        return Some(HashSet::new());
    }
    let placeholders = vec!["?"; live_session_ids.len()].join(",");
    let sql = format!(
        "SELECT DISTINCT mesh_id FROM agent_nodes WHERE id IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql).ok()?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(live_session_ids.iter()),
            |row| row.get::<_, i64>(0),
        )
        .ok()?;
    let mesh_ids: HashSet<i64> = rows.filter_map(Result::ok).collect();
    Some(mesh_ids)
}

/// How many `available` warm entries a mesh currently has. The pool worker
/// reads this before/after a fill so it knows whether to keep filling (pool
/// below `target`) or stand down (pool at or above `target`). Target is held
/// by the worker (hardcoded to 1 for the v21 tracer bullet) so we don't
/// plumb a config parameter through yet.
pub fn count_available_warm_for_mesh(mesh_id: i64) -> SqlResult<i64> {
    let db = read_conn();
    count_available_warm_for_mesh_inner(&db, mesh_id)
}

pub(crate) fn count_available_warm_for_mesh_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM warm_worktrees WHERE mesh_id = ?1 AND status = 'available'",
        params![mesh_id],
        |row| row.get(0),
    )?;
    Ok(n)
}

/// How many *droppable* warm entries a mesh has — every status EXCEPT
/// `claimed`. The downsize/idle drain computes `excess = droppable - target`
/// from this rather than from `count_warm_entries_for_mesh` (which includes
/// `claimed`): a `claimed` row is a worktree in transition to a live agent
/// node, NOT pool inventory, so it must neither inflate the excess nor be a
/// drop candidate (issue #613 review — the idle worker would otherwise
/// `git worktree remove --force` a live agent's worktree during the window
/// between claim and `forget_after_spawn`).
pub fn count_droppable_warm_entries_for_mesh(mesh_id: i64) -> SqlResult<i64> {
    let db = read_conn();
    count_droppable_warm_entries_for_mesh_inner(&db, mesh_id)
}

pub(crate) fn count_droppable_warm_entries_for_mesh_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM warm_worktrees WHERE mesh_id = ?1 AND status != 'claimed'",
        params![mesh_id],
        |row| row.get(0),
    )?;
    Ok(n)
}

/// Pick the oldest N *droppable* warm entries for a mesh so the downsize/idle
/// drain can delete them. Ordering: `filling` first (cheapest to drop —
/// worker's mid-checkout, will be GC'd on next reconcile anyway), then by
/// `created_at ASC` (FIFO). The status preference uses a `CASE` so a
/// brand-new `filling` row beats every older `available` row, but among
/// rows of the same status creation order wins.
///
/// **`claimed` rows are excluded** (`status != 'claimed'`): a claimed entry's
/// directory is being adopted as a live agent node's worktree, so force-
/// removing it would delete the agent's working tree out from under it (issue
/// #613 review). Claimed rows are reaped row-only by
/// `delete_orphaned_claimed_warm_worktrees`, never by the drain.
///
/// Returned tuple is `(id, path)` — the path is needed by the caller
/// (`services::warm_pool::drain_excess_warm_entries`) to invoke
/// `git::worktree::remove_one_worktree`. Returned ordered, so the caller
/// can `take(limit)` and the limit is just a row cap.
pub fn list_oldest_warm_entries_for_mesh(
    mesh_id: i64,
    limit: i64,
) -> SqlResult<Vec<(i64, String)>> {
    let db = read_conn();
    list_oldest_warm_entries_for_mesh_inner(&db, mesh_id, limit)
}

pub(crate) fn list_oldest_warm_entries_for_mesh_inner(
    conn: &Connection,
    mesh_id: i64,
    limit: i64,
) -> SqlResult<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, path FROM warm_worktrees \
         WHERE mesh_id = ?1 AND status != 'claimed' \
         ORDER BY CASE WHEN status = 'filling' THEN 0 ELSE 1 END ASC, \
                  created_at ASC \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![mesh_id, limit], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect()
}

/// List every droppable warm entry (all statuses except `claimed`) for a
/// mesh as `(id, path)` pairs (issue #1519). The directory-change rebuild
/// drains stale-location inventory regardless of count, so it needs the
/// full droppable set, not just the oldest-N excess window
/// `list_oldest_warm_entries_for_mesh` serves.
pub fn list_all_droppable_warm_entries_for_mesh(
    mesh_id: i64,
) -> SqlResult<Vec<(i64, String)>> {
    let db = read_conn();
    list_all_droppable_warm_entries_for_mesh_inner(&db, mesh_id)
}

pub(crate) fn list_all_droppable_warm_entries_for_mesh_inner(
    conn: &Connection,
    mesh_id: i64,
) -> SqlResult<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, path FROM warm_worktrees \
         WHERE mesh_id = ?1 AND status != 'claimed' \
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(params![mesh_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect()
}

/// True iff `path` corresponds to a row in `warm_worktrees`. The prune
/// pipeline queries this per worktree so the Worktree Manager tab can
/// badge pool entries and `delete_worktrees` can reject them. Cheap
/// (indexed on `path UNIQUE`) and side-effect free — safe to call from
/// `collect_prune_info` for every worktree.
pub fn is_warm_pool_path(path: &str) -> SqlResult<bool> {
    let db = read_conn();
    is_warm_pool_path_inner(&db, path)
}

pub(crate) fn is_warm_pool_path_inner(
    conn: &Connection,
    path: &str,
) -> SqlResult<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM warm_worktrees WHERE path = ?1)",
        params![path],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// Narrower sibling of `is_warm_pool_path_inner` used by the
/// `pending_worktree_removals` drain (issue #653). Returns true iff the path
/// is currently in `warm_worktrees` with `status = 'claimed'` — i.e. a live
/// spawn has just taken the row and is about to use (or is using) the
/// directory.
///
/// Why this is a separate predicate instead of a flag on
/// `is_warm_pool_path_inner`:
///   * The pending-removal drain needs to ASK a yes/no question with very
///     different semantics from `collect_prune_info`'s "is this a pool row?"
///     check. The prune info flow happily sees `available`/`filling`/
///     `refreshing` rows (those are pool inventory, not live spawns, so the
///     drain there must proceed); only `claimed` blocks the deletion. A
///     single flag would either over-block (drain stalls for healthy pool
///     rows) or under-block (drain deletes a live spawn's worktree).
///   * The narrower contract is the one that closes the race. The drain
///     must SKIP-and-DEQUEUE the pending removal when this returns true
///     (claim supersedes tombstone intent); it must NOT remove the
///     directory (that's a live agent's worktree). When the spawn
///     completes and `forget_after_spawn` drops the row, the next drain
///     sees `false` and proceeds.
///
/// `path` is the unique-key index (`warm_worktrees.path` is UNIQUE), so
/// this query is O(log n) regardless of pool size. Side-effect free; safe
/// to call from `services::agent_node::process_pending_removals` for every
/// pending entry.
pub fn warm_pool_claims_path(path: &str) -> SqlResult<bool> {
    let db = read_conn();
    warm_pool_claims_path_inner(&db, path)
}

pub(crate) fn warm_pool_claims_path_inner(
    conn: &Connection,
    path: &str,
) -> SqlResult<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM warm_worktrees WHERE path = ?1 AND status = 'claimed')",
        params![path],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// List every worktree-enabled mesh (use_worktree = 1) along with its id,
/// path, base_ref, and pre_spawn_pool_size. The pool worker iterates this
/// on startup and after each claim to reconcile downsize (drain) and
/// fill-up to the per-mesh target. Mirrors the projection `MeshRow` uses
/// for the spawn-time read so the two paths can't drift.
pub fn list_worktree_enabled_meshes_for_warm() -> SqlResult<Vec<WarmPoolMeshRow>> {
    let db = read_conn();
    list_worktree_enabled_meshes_for_warm_inner(&db)
}

pub(crate) fn list_worktree_enabled_meshes_for_warm_inner(
    conn: &Connection,
) -> SqlResult<Vec<WarmPoolMeshRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, base_ref, pre_spawn_pool_size, COALESCE(worktree_directory, '') FROM meshes WHERE use_worktree = 1",
    )?;
    let rows = stmt.query_map([], |row| {
        let raw_dir: String = row.get(4)?;
        let worktree_directory = if raw_dir.trim().is_empty() {
            None
        } else {
            Some(raw_dir.trim().to_string())
        };
        Ok(WarmPoolMeshRow {
            id: row.get(0)?,
            path: row.get(1)?,
            base_ref: row.get(2)?,
            pre_spawn_pool_size: row.get(3)?,
            worktree_directory,
        })
    })?;
    rows.collect()
}

/// Lightweight projection of `meshes` for the warm pool worker — only the
/// columns the worker needs. Kept private to the pool (not part of the
/// `MeshRow` typed view) so the pool worker can't accidentally widen its
/// dependency on the broader mesh config. `pre_spawn_pool_size` is the
/// per-mesh target the worker fills to (issue #611); `0` means "pool off
/// for this mesh".
#[derive(Debug, Clone)]
pub struct WarmPoolMeshRow {
    pub id: i64,
    pub path: String,
    pub base_ref: String,
    pub pre_spawn_pool_size: i64,
    /// Per-Mesh `worktree_directory` override (issue #1519). `None` means
    /// inherit the application default. The pool resolves the effective
    /// dir per mesh via `env::effective_worktree_dir_raw`.
    pub worktree_directory: Option<String>,
}

/// What a claim hands back to the spawn path: the four columns it actually
/// consumes. The other `warm_worktrees` columns (`mesh_id`, `status`,
/// `created_at`, `updated_at`) are bookkeeping the claimer doesn't read, so
/// they're deliberately omitted rather than carried as never-read fields.
#[derive(Debug, Clone)]
pub struct WarmWorktree {
    pub id: i64,
    pub path: String,
    pub preassigned_name: String,
    pub base_sha: Option<String>,
}
