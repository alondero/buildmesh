//! Database module using rusqlite for local SQLite storage.
//!
//! ## Module seam (issue #1655)
//!
//! This file owns the process-wide connection pool, [`init`], and baseline
//! `CREATE TABLE` / canonical-index DDL. Query functions live in domain
//! modules (`mesh`, `agent_node`, `warm_pool`, `auth`, `drive`,
//! `semantic_turns`, `circuit`) and are re-exported here so existing
//! `db::foo` call sites keep compiling until follow-up imports switch to
//! `db::mesh::…` / `db::circuit::queue::…`.
//!
//! Do not add new query functions here. Put them in the domain module that
//! owns the table. The freeze is pinned by `db_mod_owns_connection_and_init_only`.
//!
//! ## Schema evolution (issue #249)
//!
//! All schema migration lives in the single entry point
//! [`migrations::evolve_to`]. It owns:
//!
//! - the version-by-version migration steps (column adds, one-shot backfills),
//! - the read-side `COALESCE` defaults for the `meshes` projection
//!   (via [`migrations::mesh_columns_projection`]),
//! - the `schema_version` probe and the post-migration bump,
//! - baseline table creation before columns evolve, and
//! - canonical index creation after every migrated column exists.
//!
//! A new column becomes "add a [`migrations::ColumnSpec`] entry to
//! `migrations::SPECS` and you're done" — one place, not three. See
//! the module-level doc on `db::migrations` for the full design and
//! the bug class it closes (the v8→v9 `source_issue` regression is
//! the canonical pin).

pub(crate) mod migrations;

/// Shared test infrastructure for backend tests that need a real
/// SQLite database. See `db::test_support` for the helper.
#[cfg(test)]
pub mod test_support;

pub mod mesh;
pub mod agent_node;
pub mod warm_pool;
pub mod auth;
pub mod drive;
pub mod semantic_turns;
pub mod circuit;

pub use mesh::*;
pub use agent_node::*;
pub use warm_pool::*;
pub use auth::*;
pub use drive::*;
pub use semantic_turns::*;
pub use circuit::*;

pub(crate) use auth::{COORDINATOR_DRIVE_TOKEN_KEY, COORDINATOR_READ_TOKEN_KEY};

#[allow(unused_imports)]
pub(crate) use mesh::{
    get_mesh_by_id_inner,
    get_mesh_harness_overrides_inner,
    upsert_mesh_harness_override_inner,
    remove_mesh_harness_override_inner,
    count_active_autopilot_nodes_total_inner,
    get_mesh_scratchpad_inner,
    set_mesh_scratchpad_inner,
    set_mesh_sandbox_inner,
    set_mesh_worktree_directory_inner,
    COUNT_ACTIVE_AUTOPILOT_SQL
};

#[allow(unused_imports)]
pub(crate) use agent_node::{
    get_agent_node_by_id_inner,
    adopt_manual_pool_slug_with_path_inner,
    set_agent_node_pinned_inner,
    toggle_agent_node_pinned_inner,
    update_agent_node_signal_health_inner,
    clear_cli_session_id_inner,
    agent_turn_stamp,
    set_cli_session_id_if_missing_inner,
    list_suspended_nodes_inner,
    recover_suspended_cli_session_id_inner,
    recover_live_cli_session_id,
    recover_live_cli_session_id_inner,
    cli_session_id_present_inner,
    enqueue_worktree_removal_inner,
    list_pending_worktree_removals_inner,
    delete_pending_worktree_removal_inner,
    delete_agent_node_enqueueing_removal_inner
};

#[cfg(test)]
pub(crate) use agent_node::{
    adopt_manual_pool_slug_inner,
    create_agent_node_inner,
};

#[cfg(test)]
pub(crate) use mesh::{
    create_mesh_inner,
    delete_mesh_inner,
};

#[allow(unused_imports)]
pub(crate) use warm_pool::{
    insert_warm_worktree_inner,
    mark_warm_worktree_available_inner,
    mark_warm_worktree_refreshing_inner,
    list_available_warm_for_mesh_inner,
    claim_warm_entry_for_mesh_inner,
    delete_warm_worktree_inner,
    delete_warm_worktrees_for_mesh_inner,
    list_warm_paths_for_mesh_inner,
    list_warm_paths_for_mesh_droppable_inner,
    list_warm_worktrees_to_reconcile_inner,
    delete_orphaned_claimed_warm_worktrees_inner,
    count_available_warm_for_mesh_inner,
    count_droppable_warm_entries_for_mesh_inner,
    list_oldest_warm_entries_for_mesh_inner,
    list_all_droppable_warm_entries_for_mesh_inner,
    is_warm_pool_path_inner,
    warm_pool_claims_path_inner,
    list_worktree_enabled_meshes_for_warm_inner
};

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod mesh_tests;

#[cfg(test)]
mod scratchpad_tests;

#[cfg(test)]
mod sandbox_tests;

#[cfg(test)]
mod device_session_tests;

#[cfg(test)]
mod drive_idempotency_tests;

#[cfg(test)]
mod warm_pool_tests;

#[cfg(test)]
mod agent_node_tests;

#[cfg(test)]
mod harness_overrides_tests;

#[cfg(test)]
mod circuit_tests;

#[cfg(test)]
mod circuit_prune_tests;

#[cfg(test)]
mod seam_tests;

use rusqlite::{Connection, OpenFlags};
pub use rusqlite::Result as SqlResult;
use once_cell::sync::OnceCell;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;


// Eight handles cover the UI, HTTP, and worker polling fan-out while keeping
// SQLite's file-descriptor and cache footprint bounded.
const READER_POOL_SIZE: usize = 8;
// Synchronous callers retain the historical accessor, but no checkout may
// park a thread forever. Async callers use `try_read_conn()` to surface this
// timeout as an error instead of blocking a runtime worker indefinitely.
const READER_CHECKOUT_TIMEOUT: Duration = Duration::from_secs(1);
static INIT_LOCK: Mutex<()> = Mutex::new(());

struct Database {
    // `std::sync::Mutex` is intentional: issue #1224 requires poison recovery
    // after a panic in a writer, while reader-pool bookkeeping uses
    // `parking_lot` because its guards are never exposed across panics.
    writer: Mutex<Connection>,
    readers: ReaderPool,
}

/// Fixed-size pool of read-only SQLite connections. The bookkeeping mutex is
/// held only while checking a connection in or out, never while it runs SQL.
struct ReaderPool {
    available: parking_lot::Mutex<Vec<Connection>>,
    ready: parking_lot::Condvar,
    db_path: PathBuf,
    flags: OpenFlags,
}

impl ReaderPool {
    fn open(db_path: &Path) -> SqlResult<Self> {
        let mut flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if db_path.to_string_lossy().starts_with("file:") {
            flags |= OpenFlags::SQLITE_OPEN_URI;
        }
        let mut connections = Vec::with_capacity(READER_POOL_SIZE);
        for _ in 0..READER_POOL_SIZE {
            let conn = Connection::open_with_flags(db_path, flags)?;
            apply_connection_pragmas(&conn, true)?;
            connections.push(conn);
        }
        Ok(Self {
            available: parking_lot::Mutex::new(connections),
            ready: parking_lot::Condvar::new(),
            db_path: db_path.to_path_buf(),
            flags,
        })
    }

    fn checkout(&self) -> SqlResult<ReadConnection<'_>> {
        let mut available = self.available.lock();
        let started = std::time::Instant::now();
        loop {
            if let Some(conn) = available.pop() {
                if started.elapsed() >= Duration::from_millis(10) {
                    tracing::debug!(elapsed_ms = started.elapsed().as_millis(), "database reader pool contention ended");
                }
                return Ok(ReadConnection {
                    pool: self,
                    conn: Some(conn),
                });
            }
            let wait = self.ready.wait_for(&mut available, READER_CHECKOUT_TIMEOUT);
            if wait.timed_out() {
                tracing::warn!(elapsed_ms = started.elapsed().as_millis(), "database reader pool checkout timed out");
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
    }
}

pub struct ReadConnection<'a> {
    pool: &'a ReaderPool,
    conn: Option<Connection>,
}

impl Deref for ReadConnection<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn
            .as_ref()
            .expect("checked-out reader connection must be present")
    }
}

impl Drop for ReadConnection<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let conn = if conn.is_autocommit() {
                conn
            } else {
                let conn = conn;
                if conn.execute_batch("ROLLBACK").is_ok() && conn.is_autocommit() {
                    conn
                } else {
                    match Connection::open_with_flags(&self.pool.db_path, self.pool.flags)
                        .and_then(|replacement| {
                            apply_connection_pragmas(&replacement, true)?;
                            Ok(replacement)
                        }) {
                        Ok(replacement) => replacement,
                        Err(error) => {
                            tracing::error!(%error, "failed to recycle database reader connection");
                            return;
                        }
                    }
                }
            };
            self.pool.available.lock().push(conn);
            self.pool.ready.notify_one();
        }
    }
}

static DB: OnceCell<Database> = OnceCell::new();

/// Current schema version.
///
// v24 — Warm pool on by default (ADR 0020, spawn-latency work): a new
// mesh now defaults to `pre_spawn_pool_size = 1` (one pre-warmed worktree)
// instead of 0, and a ONE-TIME backfill flips existing worktree-enabled
// meshes still at 0 to 1. The pool is the single biggest click-to-terminal
// win (sub-500ms adopt vs multi-second cold checkout) and its lifecycle
// has been hardened across #609–#653, so opt-out is the right polarity.
// Deliberate trade-off: a user who explicitly set 0 pre-v24 is
// indistinguishable from one who never touched the field, so the backfill
// overrides both — once. The Worktrees Probe still sets it back to 0.
// Ordering constraint: the `pre_spawn_pool_size` column is added by an
// `ensure_*` net AFTER `migrate_if_needed` runs, so the backfill lives in
// [`ensure_pool_default_backfill`] gated on its own app_settings flag
// (crash-safe: the flag is written only after the UPDATE commits), not in
// the version-gated migration ladder.
//
// v23 — Coordinator drive idempotency ledger (issue #320, ADR-0008 §6):
// add the `coordinator_drive_prompts` table so a Coordinator retrying a
// timed-out `POST /nodes/{id}/prompt` over a flaky network never lands the
// prompt twice. Each row records the honest verdict under a caller-supplied
// idempotency key, scoped to the node it drove; a duplicate `(node_id, key)`
// replays the recorded verdict instead of re-sending. A brand-new table needs
// no data migration — `CREATE TABLE IF NOT EXISTS` in `init` materializes it
// for every DB; the version bump just records the shape moved forward.
//
// v32 — Coordinator drive idempotency hardening (issue #750): three additive
// columns on `coordinator_drive_prompts` close the deferred gaps from the
// #320 review (PR #749):
//   * `status` (`TEXT NOT NULL DEFAULT 'pending'`) — the drive's progress:
//     `pending` → `delivered` | `unverified`. A `pending` row means a drive
//     is in flight or crashed mid-send. The atomic claim step
//     (`claim_drive_prompt_inner`, issue #750 item 1) inserts the row in
//     `pending` state, then `finalize_drive_prompt_inner` flips it to the
//     verdict after a successful send. The route rejects `pending` rows with
//     `409 + Retry-After` rather than sending a second prompt.
//   * `claimed_at` (`TEXT NOT NULL DEFAULT datetime('now')`) — when the
//     `pending` claim was inserted. Two consumers: the orphan-recovery pass
//     inside the claim transaction reclaims any `pending` row older than
//     `PENDING_CLAIM_TIMEOUT_SECS` (a crashed-mid-send row must not block the
//     key forever — a retry can re-send), and the GC sweep uses `created_at`
//     rather than `claimed_at` for the bounded-age prune.
//   * `prompt_hash` (`TEXT NOT NULL DEFAULT ''`) — SHA-256 hex of the prompt
//     body, computed by the route from the same bytes the driver writes. The
//     claim step compares incoming vs stored hash and returns `Mismatch` when
//     the same key is reused with a different payload (issue #750 item 2 —
//     Stripe-style, prevents a silent 200-replay-of-different-prompt).
// `verdict` is loosened from `TEXT NOT NULL` to `TEXT NOT NULL DEFAULT ''` so
// the `INSERT OR IGNORE` claim write doesn't need to set it. No data
// migration: pre-v32 rows read back as `status='pending', claimed_at=<old
// created_at>, prompt_hash='', verdict=<existing>`; the `prompt_hash=''`
// default will surface as a `Mismatch` on any reuse (caller must mint a fresh
// key) — acceptable because drive is off-by-default and unreleased (#313),
// so no pre-v32 callers exist in the wild. Safety net
// `ensure_coordinator_drive_prompt_claim_columns` lives alongside the other
// `ensure_*` helpers so a build that bumps `SCHEMA_VERSION` without yet
// containing the inline ALTERs picks the columns up on next launch.
//
/// v22 — Per-mesh pre-spawn pool size (issue #611): add the
/// `meshes.pre_spawn_pool_size` INTEGER column (0 = feature off,
/// 1..5 = target). The pool worker (issue #609 / v21) previously
/// hardcoded `POOL_TARGET_PER_MESH = 1`; the column lets each mesh
/// opt in/out and size up. No data migration needed — the column has a
/// `DEFAULT 0` so existing rows keep the previous behaviour. Mirrors
/// how `sandbox` (v18) is a single typed integer rather than a
/// separate enabled bool + size: one source of truth, one IPC boundary
/// to validate. See `commands::mesh_properties::update_mesh_pool_size`
/// for the typed write path and `services::warm_pool` for the reader.
//
/// v21 — Pre-spawn Worktree Pool (issue #609, PRD #608): add the
/// `warm_worktrees` table that tracks pre-warmed detached HEAD
/// worktrees. A row's `path` is the absolute on-disk directory the pool
/// pre-cut (under `{mesh.path}/.claude/worktrees/<slug>`);
/// `preassigned_name` is the slug; `status` is `filling` (worker is
/// still cutting the checkout), `available` (claimable), `claimed` (in
/// flight, dropped once the node row is in place); `base_sha` records
/// the commit the pool checked out at so a spawn can verify the entry
/// is still on the expected tip. No data migration needed — fresh
/// table, `CREATE TABLE IF NOT EXISTS`.
// v20 — Persistent device sessions (issue #502 / PRD #494): add the
// `device_sessions` table backing per-device mobile tokens + the
// "Authorized Devices" revocation panel. A brand-new table needs no data
// migration — `CREATE TABLE IF NOT EXISTS` in `init` materializes it for
// every DB; the version bump just records that the shape moved forward.
//
// v19 — Spawn Option composite ids (issue #575 / ADR-0016): rewrite
// legacy `agent_nodes.provider` ids (`minimax`/`kimi`/custom bare account
// id → `claude:<id>`) so archived nodes resolve under the new grouped
// Spawn Menu without a permanent resolver shim. The rewrite is
// unambiguous today because every Proxied Provider currently pairs with
// Claude Code only. See [`migrate_agent_node_provider_id_to_composite`].
//
// v29 — Node Pinning (wayfinder #982 / ticket #984): add the
// `agent_nodes.is_pinned INTEGER NOT NULL DEFAULT 0` column backing the
// Pinned Grid view mode. NOT NULL + default means no backfill is needed —
// every pre-v29 row reads back as `pinned = false` and the user can flip
// individual rows from the UI affordance (ticket #985). The safety net
// `ensure_agent_node_is_pinned` lives alongside the other `ensure_*`
// helpers so a build that bumps `SCHEMA_VERSION` without yet containing
// the inline `is_pinned` column still picks it up on the next launch.
//
// v31 — Looping Autopilot iteration marker (wayfinder #990 / ticket
// #992): add a single nullable column to `autopilot_runs` so the
// looping-mode poller can distinguish iteration rows from issue-driven
// rows in the same ledger table.
//   * `loop_iteration` (INTEGER, nullable) — the 1-based loop iteration
//     number for nodes spawned by the Looping-mode poller. `NULL` for
//     issue-driven rows (pre-v31 rows stay `NULL` and the Looping-mode
//     poller never reads them, so no backfill is required). The column
//     is the source of truth for: (a) iteration cap (`loop_max_iterations`
//     check vs `MAX(loop_iteration)`); (b) trailing-failure count for the
//     auto-pause threshold (`loop_consecutive_failures` check vs a
//     descending walk of `(loop_iteration, state)`); (c) interval-delay
//     pacing (`loop_interval_seconds` check vs `MAX(updated_at)` of
//     loop rows). All three checks happen in `services::autopilot`
//     (`evaluate_loop_continuation`), reading through the hydration
//     helper `list_loop_history`. See `ensure_autopilot_run_loop_iteration`
//     for the additive safety net.
//
// v30 — Looping Autopilot config (wayfinder #990 / ticket #991): add
// six `meshes` columns that the poller (#992) reads to drive a
// Looping-mode autopilot — sequential prompt-driven nodes, with optional
// suffix prompt and configurable iteration / interval / failure caps:
//   * `autopilot_mode` (TEXT NOT NULL DEFAULT 'issue_driven') — the
//     discriminated mode the poller reads (`IssueDriven` is today's
//     GitHub-label poller; `Looping` is the new sequential spawner).
//   * `loop_initial_prompt` (TEXT, nullable) — body of the prompt
//     injected into every loop-iteration node. `None` falls back to an
//     implementation-defined default at spawn time (ticket #992).
//   * `loop_suffix_prompt` (TEXT, nullable) — optional second-turn
//     prompt injected AFTER the issue-style wrap-up (#485) verifies
//     green, before the next loop iteration starts. `None` = no
//     suffix turn.
//   * `loop_max_iterations` (INTEGER, nullable) — optional hard cap on
//     loop iterations. `None` = continuous; `Some(n >= 1)` = stop after
//     n iterations.
//   * `loop_interval_seconds` (INTEGER NOT NULL DEFAULT 0) — pause
//     between consecutive loop spawns; the poller re-checks after this
//     many seconds. `0` = no pause.
//   * `loop_consecutive_failures` (INTEGER NOT NULL DEFAULT 0) —
//     auto-pause threshold; when `>= configured_cap` consecutive loop
//     iterations wrap-up-failed, the poller stops spawning until the
//     user clears or resets it. `0` default = feature off.
// Every column has a sensible default (or is nullable), so pre-v30
// rows read back as "loop not configured" without a backfill — the
// same pattern as `pre_spawn_pool_size` (v22). Safety net
// `ensure_mesh_loop_columns` lives alongside `ensure_mesh_autopilot_columns`
// so a build that bumps `SCHEMA_VERSION` without yet containing the
// inline ALTERs picks the columns up on next launch.
// `SCHEMA_VERSION` now lives in `db::migrations::SCHEMA_VERSION`
// (issue #249). The constant was duplicated here before #249; the
// single source of truth is the registry.
/// Apply the per-connection pragmas every Buildmesh connection needs.
///
/// - `journal_mode=WAL`: the default rollback journal creates/deletes a
///   journal file and double-fsyncs on *every* commit — with the whole DB
///   behind one `Mutex`, each attention flip or status write stalls every
///   other DB caller for the full fsync dance (worst on Windows, where
///   antivirus scanning inflates file-create latency). WAL appends to one
///   log instead. The mode is persistent in the DB file, but setting it is
///   idempotent so we apply it on every init.
/// - `synchronous=NORMAL`: the WAL-recommended pairing — one fsync per
///   checkpoint rather than per commit; WAL guarantees the DB stays
///   consistent after a crash (at most the last commits are lost, which for
///   status flips is fine — startup reconciles agent state anyway).
/// - `busy_timeout=5000`: if any second connection ever touches the file
///   (e.g. a dev-profile instance pointed at the same dir by mistake), fail
///   after 5s of retrying instead of an instant `SQLITE_BUSY`.
fn apply_connection_pragmas(conn: &Connection, reader: bool) -> SqlResult<()> {
    if !reader {
        // `journal_mode` returns the resulting mode as a row, so it needs
        // `query_row`, not `execute` (rusqlite errors on rows from execute).
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    if reader {
        conn.pragma_update(None, "query_only", "ON")?;
    }
    Ok(())
}

fn open_writer(db_path: &Path) -> SqlResult<Connection> {
    if db_path.to_string_lossy().starts_with("file:") {
        Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI,
        )
    } else {
        Connection::open(db_path)
    }
}

/// Initialize the database
pub fn init(db_path: &Path) -> SqlResult<()> {
    let _init_guard = INIT_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if DB.get().is_some() {
        return Ok(());
    }
    // SQLite's plain `:memory:` name creates one private database per
    // connection. Use a named shared-cache URI so the writer and readers see
    // the same in-memory schema, while also enabling URI paths generally.
    let db_path = if db_path.as_os_str() == ":memory:" {
        PathBuf::from("file:buildmesh-shared-memory?mode=memory&cache=shared")
    } else {
        db_path.to_path_buf()
    };
    let conn = open_writer(&db_path)?;
    apply_connection_pragmas(&conn, false)?;

    init_schema(&conn)?;

    // Silently treat "already initialized" as success: production calls
    // `init` exactly once at startup (the new `Connection` is dropped
    // here, so the existing one stays), and test files that share a
    // single `cargo test` process can each call `init` to set up
    // their own temp DB without coordinating order. The
    // `InvalidParameterName` error variant was originally intended to
    // surface a production double-init bug, but it was strictly
    // overzealous in tests where multiple files legitimately need to
    // share the global `DB` once it's set. See
    // `commands::agent::tests::ensure_pr_db` and `db::mesh_tests` for
    // the two consumer call sites that this unblocks.
    let readers = ReaderPool::open(&db_path)?;
    let _ = DB.set(Database {
        writer: Mutex::new(conn),
        readers,
    });
    Ok(())
}

/// Bring one SQLite connection to the current Buildmesh schema.
///
/// This is the schema module's test seam: it invokes the complete migration
/// pipeline without connection lifecycle or installation of the process-global
/// database singleton.
pub(crate) fn init_schema(conn: &Connection) -> SqlResult<()> {
    migrations::evolve_to(migrations::SCHEMA_VERSION, conn)
}

/// Create the baseline tables before the migration runner evolves their
/// columns. The runner calls this itself, so direct `evolve_to` callers and
/// application startup share this single DDL source.
pub(crate) fn ensure_baseline_tables(conn: &Connection) -> SqlResult<()> {
    // Ensure app_settings exists first (needed by evolve_to to probe
    // schema_version).
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "
    )?;

    // Create the baseline tables (IF NOT EXISTS so they're idempotent). For
    // fresh DBs this creates the tables; for existing DBs it's a no-op. MUST
    // run before the migration runner's
    // always-run column walk can find the tables and add missing
    // columns (e.g. `use_worktree`, `pre_spawn_pool_size`, etc., that
    // the v6-shape inline CREATE doesn't include).
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            layout TEXT NOT NULL DEFAULT 'grid',
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            worktree_directory TEXT
        );

        CREATE TABLE IF NOT EXISTS agent_nodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mesh_id INTEGER NOT NULL REFERENCES meshes(id),
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            branch TEXT NOT NULL DEFAULT 'main',
            env TEXT NOT NULL DEFAULT 'windows',
            provider TEXT NOT NULL DEFAULT 'anthropic',
            status TEXT NOT NULL DEFAULT 'idle',
            cli_session_id TEXT,
            session_started_at INTEGER,
            worktree_name TEXT,
            use_worktree INTEGER NOT NULL DEFAULT 1,
            is_pinned INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            status_changed_at TEXT NOT NULL DEFAULT (datetime('now')),
            source_pr INTEGER,
            head_repo_owner TEXT,
            head_repo_clone_url TEXT,
            source_pr_pinned_sha TEXT,
            signal_health TEXT,
            worktree_path TEXT
        );

        -- Node-level lifecycle ownership. Circuit cleanup intent and the
        -- spawn/cleanup leases live here rather than in historical run JSON.
        -- The spawn orchestrator uses the generic node lease seam; circuit
        -- cleanup is one consumer of it, not an implementation detail of the
        -- global spawn pipeline.
        CREATE TABLE IF NOT EXISTS agent_node_lifecycle_leases (
            node_id INTEGER PRIMARY KEY REFERENCES agent_nodes(id) ON DELETE CASCADE,
            cleanup_requested INTEGER NOT NULL DEFAULT 0,
            cleanup_generation TEXT,
            cleanup_expires_at INTEGER,
            spawn_generation TEXT,
            spawn_expires_at INTEGER,
            retired INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS pending_worktree_removals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            worktree_path TEXT NOT NULL UNIQUE,
            node_name TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS device_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            token_hash TEXT NOT NULL UNIQUE,
            label TEXT,
            last_ip TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_active_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Coordinator drive idempotency ledger (issue #320, ADR-0008 §6,
        -- hardened by issue #750). One row per (node, caller-supplied key)
        -- the Coordinator drove. v32 (issue #750) adds three columns:
        --   * `status` — pending | delivered | unverified. Drives the
        --     claim-before-send race fix (item 1): the atomic claim inserts
        --     `pending`, then finalize flips to the verdict after send.
        --   * `claimed_at` — when the `pending` row was inserted. The
        --     orphan-recovery pass reclaims rows older than
        --     `PENDING_CLAIM_TIMEOUT_SECS` (a crashed-mid-send row must not
        --     block the key forever — a retry can re-send).
        --   * `prompt_hash` — SHA-256 of the prompt body. Reusing a key
        --     with a *different* prompt is a 409 (Stripe-style, item 2) —
        --     prevents a silent 200-replay-of-different-prompt.
        -- `verdict` is now DEFAULT '' (was NOT NULL) so the claim
        -- `INSERT OR IGNORE` doesn't need to set it. Scoped by node so a
        -- key accidentally reused across two nodes still drives each node
        -- once. No data migration — a fresh table materialized for every
        -- DB; the additive ALTERs for v32 live in
        -- `ensure_coordinator_drive_prompt_claim_columns`.
        CREATE TABLE IF NOT EXISTS coordinator_drive_prompts (
            node_id INTEGER NOT NULL,
            idempotency_key TEXT NOT NULL,
            verdict TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'pending',
            claimed_at TEXT NOT NULL DEFAULT (datetime('now')),
            prompt_hash TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (node_id, idempotency_key)
        );

        -- Pre-spawn Worktree Pool (issue #609, PRD #608). One row per
        -- detached-HEAD worktree the background worker has pre-warmed under
        -- `{mesh.path}/.claude/worktrees/<slug>`. The pool is
        -- optional (the spawn path always falls back to a cold checkout
        -- when no `available` row matches); see `services::warm_pool`.
        CREATE TABLE IF NOT EXISTS warm_worktrees (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mesh_id INTEGER NOT NULL REFERENCES meshes(id) ON DELETE CASCADE,
            -- Absolute host-side path to the pre-warmed worktree directory.
            -- Always lives under `{mesh.path}/.claude/worktrees/...`.
            path TEXT NOT NULL UNIQUE,
            -- The slug baked into the directory name. Adopted as
            -- `agent_nodes.worktree_name` on claim, so the spawn pipeline
            -- never has to rename the directory (zero folder-rename overhead).
            preassigned_name TEXT NOT NULL,
            -- `filling` (worker is mid-checkout), `available` (claimable),
            -- `claimed` (spawned, dropped once the node row is in place).
            status TEXT NOT NULL DEFAULT 'filling',
            -- 40-char hex SHA the warm entry is checked out at. Spawn can
            -- compare against the mesh's resolved base SHA; mismatch → drop
            -- + cold spawn.
            base_sha TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        -- Autopilot runs (issue #482, PRD #480). One row per auto-spawned
        -- Agent Node, keyed by the node so close/delete cascades. Kept as a
        -- satellite table (not an agent_nodes column) so the positional
        -- AGENT_NODE_COLUMNS projection and its consumers stay untouched.
        -- `state` is the wrap-up pipeline machine: implementing (agent working
        -- on the issue) -> finishing (wrap-up prompt injected, attempt N) ->
        -- completed | failed. `attempts` counts wrap-up/self-correction
        -- injections (capped by autopilot::MAX_FINISH_ATTEMPTS).
        CREATE TABLE IF NOT EXISTS autopilot_runs (
            node_id INTEGER PRIMARY KEY REFERENCES agent_nodes(id) ON DELETE CASCADE,
            mesh_id INTEGER NOT NULL,
            issue_number INTEGER NOT NULL,
            state TEXT NOT NULL DEFAULT 'implementing',
            attempts INTEGER NOT NULL DEFAULT 0,
            pr_number INTEGER,
            pr_url TEXT,
            -- v31 — Looping Autopilot iteration marker (ticket #992). NULL
            -- for issue-driven runs (the pre-v31 default; preserves every
            -- existing row). The Looping-mode poller writes the 1-based
            -- iteration number on each spawn, so iteration count + cap
            -- checks reduce to MAX(loop_iteration) + 1 vs loop_max_iterations.
            loop_iteration INTEGER,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        -- Autopilot Circuits (spec #1205 / walking skeleton #1206, schema
        -- v34/v38). The three ledger tables plus a durable run-agent lease
        -- table. Canonical indexes are installed after schema evolution:
        --   * autopilot_circuits — the blueprint rows. `graph_json` holds
        --     the serialised Graph Blueprint AST (see
        --     autopilot::circuit::model); no per-node-kind migration — the
        --     AST evolves inside the JSON. `enabled` defaults to 0
        --     (draft-first, issue #1356) so a freshly created circuit
        --     cannot fire GitHub/interval pollers until the user opts in.
        --   * autopilot_circuit_runs — one execution instance per row.
        --     UNIQUE (circuit_id, trigger_identity) enforces the spec's
        --     dedupe: re-reporting an identity replays the existing run,
        --     while two circuits may react to the same source
        --     independently (the key is circuit-scoped).
        --   * autopilot_circuit_run_steps — per-circuit-node execution
        --     state. UNIQUE (run_id, node_id) backs the engine's upsert
        --     commit (`db::circuit::commit_circuit_advance`).
        --     `status = 'pending_slot'` marks a step parked on a
        --     concurrency/agent-slot limit; it promotes by queue_position
        --     when slots free up.
        --   * autopilot_circuit_run_agent_leases — one durable reservation
        --     per live run; its slots keep host-cap accounting independent
        --     of transient step/agent associations.
        CREATE TABLE IF NOT EXISTS autopilot_circuits (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mesh_id INTEGER NOT NULL REFERENCES meshes(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 0,
            concurrency_limit INTEGER NOT NULL DEFAULT 1,
            graph_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            is_preset INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS autopilot_circuit_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            circuit_id INTEGER NOT NULL REFERENCES autopilot_circuits(id) ON DELETE CASCADE,
            mesh_id INTEGER NOT NULL,
            trigger_identity TEXT NOT NULL DEFAULT '',
            state TEXT NOT NULL DEFAULT 'pending',
            context_json TEXT NOT NULL DEFAULT '{}',
            queue_position INTEGER NOT NULL DEFAULT 0,
            source_agent_node_id INTEGER REFERENCES agent_nodes(id) ON DELETE SET NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (circuit_id, trigger_identity)
        );
        CREATE TABLE IF NOT EXISTS autopilot_circuit_run_agent_leases (
            run_id INTEGER PRIMARY KEY REFERENCES autopilot_circuit_runs(id) ON DELETE CASCADE,
            slots INTEGER NOT NULL CHECK (slots > 0),
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS autopilot_circuit_run_steps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL REFERENCES autopilot_circuit_runs(id) ON DELETE CASCADE,
            node_id TEXT NOT NULL,
            agent_node_id INTEGER,
            parent_agent_node_id INTEGER REFERENCES agent_nodes(id) ON DELETE SET NULL,
            status TEXT NOT NULL DEFAULT 'pending_slot',
            attempt INTEGER NOT NULL DEFAULT 0,
            outcome TEXT,
            error_message TEXT,
            started_at TEXT,
            completed_at TEXT,
            UNIQUE (run_id, node_id)
        );
        "
    )?;

    // Single schema-evolution entry point (issue #249). Owns the
    // version-gated column adds + one-shot backfills for upgrades,
    // and the always-run idempotent safety nets (column adds +
    // data migrations). See `db::migrations` for the full design.
    // Runs AFTER the inline CREATE so the column walk's
    // `table_present` guard sees the freshly-created tables and adds
    // any missing columns (e.g. `use_worktree`, `pre_spawn_pool_size`
    // — the inline CREATE above is a v6-shape snapshot and the v8+
    // columns live in the registry's `all_column_specs`).
    Ok(())
}

/// Create every canonical index only after the migration runner has evolved
/// table columns. This keeps index ordering independent of when a column
/// entered the schema history.
pub(crate) fn create_canonical_indexes_after_evolution(conn: &Connection) -> SqlResult<()> {
    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_coordinator_drive_prompts_created_at
            ON coordinator_drive_prompts(created_at);
        CREATE INDEX IF NOT EXISTS idx_warm_worktrees_mesh ON warm_worktrees(mesh_id);
        CREATE INDEX IF NOT EXISTS idx_warm_worktrees_status ON warm_worktrees(status);
        CREATE INDEX IF NOT EXISTS idx_agent_nodes_mesh ON agent_nodes(mesh_id);
        CREATE INDEX IF NOT EXISTS idx_autopilot_runs_mesh ON autopilot_runs(mesh_id);
        CREATE INDEX IF NOT EXISTS idx_autopilot_circuits_mesh ON autopilot_circuits(mesh_id);
        CREATE INDEX IF NOT EXISTS idx_autopilot_circuits_preset_mesh ON autopilot_circuits(mesh_id, is_preset);
        CREATE UNIQUE INDEX IF NOT EXISTS uq_autopilot_circuits_preset_mesh
            ON autopilot_circuits(mesh_id) WHERE is_preset = 1;
        CREATE INDEX IF NOT EXISTS idx_autopilot_circuit_runs_circuit
            ON autopilot_circuit_runs(circuit_id);
        CREATE INDEX IF NOT EXISTS idx_autopilot_circuit_runs_state
            ON autopilot_circuit_runs(state);
        CREATE INDEX IF NOT EXISTS idx_circuit_runs_mesh_queue
            ON autopilot_circuit_runs(mesh_id, state, queue_position);
        CREATE INDEX IF NOT EXISTS idx_circuit_steps_run ON autopilot_circuit_run_steps(run_id);
        CREATE INDEX IF NOT EXISTS idx_circuit_runs_source_agent ON autopilot_circuit_runs(source_agent_node_id);
        ",
    )
}

// `migrate_if_needed` removed (issue #249). The single entry point is
// now `migrations::evolve_to(migrations::SCHEMA_VERSION, &conn)`, called
// from `init()` and from any future test that simulates a vN → current
// upgrade. See `db::migrations` for the full design.

// Pre-v6 dead code removed (issue #249): `migrate_mesh_columns` (v8) and the
// shared `ensure_column` helper (issue #456) both moved into
// `db::migrations`. The registry's `evolve_to` walker does the work via
// the always-run column-add pass (idempotent `pragma_table_info` skip).
// See the `// Pre-v6 dead code removed` block below the surviving
// safety-net wrappers for the full removal list.

// `migrate_agent_node_use_worktree` (v11) and the ensure_* safety-net
// wrappers below all moved to `db::migrations` (issue #249).

// `ensure_agent_node_use_worktree` moved to `db::migrations`.

// `ensure_agent_node_position` (v13) moved to `db::migrations`. The
// per-mesh position backfill now lives as a `OneShotBackfill` entry.

// `ensure_agent_node_status_changed_at` (v14) moved to `db::migrations`.

// `ensure_agent_node_source_pr` (v15, issue #420) moved to `db::migrations`.

// `ensure_agent_node_source_pr_fork_meta` (v16, issue #443) moved to `db::migrations`.

// `ensure_agent_node_source_pr_pinned_sha` (v16, issue #444) moved to `db::migrations`.

// `ensure_agent_node_is_pinned` (v29, wayfinder #982 / ticket #984) moved to `db::migrations`.

// `ensure_checkpoints_dropped` (v12) moved to `db::migrations::AlwaysStep::DropCheckpoints`.

// `ensure_autopilot_run_pr_columns` (merged-PR auto-close sweep) moved to `db::migrations`.

// `ensure_autopilot_run_loop_iteration` (v31, ticket #992) moved to `db::migrations`.

// `ensure_coordinator_drive_prompt_claim_columns` (v32, issue #750) moved to `db::migrations`.

// `ensure_coordinator_drive_prompt_created_at_index` (v32, issue #750
// item 3) moved into `evolve_to`'s post-evolution canonical index pass.

// `ensure_mesh_columns` (v8 user-tunable columns) moved to `db::migrations`.

// `ensure_mesh_scratchpad` (v17, issue #516) moved to `db::migrations`.

// `ensure_mesh_sandbox` (v18, #497/#498) moved to `db::migrations`.

// `ensure_mesh_autopilot_columns` (v26, issue #481) moved to `db::migrations`.

// `ensure_mesh_loop_columns` (v30, wayfinder #990 / ticket #991) moved to `db::migrations`.

// `ensure_mesh_root_command_columns` (v27, issue #802) moved to `db::migrations`.

// `ensure_mesh_color` (v25) moved to `db::migrations`.

// `ensure_mesh_pre_spawn_pool_size` (v22, issue #611) + the v24 one-shot
// `ensure_pool_default_backfill` moved to `db::migrations`. The column
// add runs from the always-pass column walk; the v24 backfill runs as a
// `OneShotBackfill` gated on the same `pool_default_backfill_v24` flag.

// `ensure_agent_node_provider_id_migrated` (v19, issue #575 first-class
// block) moved to `db::migrations::AlwaysStep::RewriteAgentNodeProviderId`.
// The always-run idempotent step runs every launch; the version-gated
// pass no longer has its own duplicate call.


// `migrate_projects_layout` removed (issue #249 — pre-v6 dead code).
// The legacy `projects` table has not existed since v6; this helper
// only ever ran via the version-gated `migrate_if_needed` ladder
// when the `projects` table was present (a state no production DB
// has been in for six years). The v6+ safety-net path always used
// `ensure_mesh_columns` (also now in `db::migrations`).

// `migrate_projects_position` removed (issue #249 — pre-v6 dead code).

// `migrate_sessions_worktree_name` removed (issue #249 — pre-v6 dead code).

// `migrate_mesh_rename` removed (issue #249 — pre-v6 dead code). The
// `projects`→`meshes` table rename is part of the v6 migration that
// production DBs completed six years ago; the helper was kept alive
// only by tests simulating v2 → current upgrades.

// `migrate_remote_access_token` removed (issue #249 — the v7 root
// token mint is now part of `get_or_create_root_token_inner`, which
// is idempotent and called on first read).

// `migrate_agent_node_source_issue` removed (issue #249). The v9 column
// add runs from the registry's always-pass column walk — the same
// pragma_table_info check skips present columns.

// `migrate_gemini_to_agy` removed (issue #249). The v10 rewrite
// (`gemini` → `agy`) lives nowhere in the schema-evolution registry
// because it predates the registry — it was a one-line `UPDATE`
// guarded by `WHERE provider = 'gemini'`. The pre-#697 harness list
// already covers every harness id; a v10-shape DB's `gemini` rows
// read back at runtime via the harness resolver (which has handled
// the missing id since #918). The rewrite is preserved here as a
// documentation note — re-introduce it as a `OneShotBackfill` only if
// a regression pin surfaces.

// `migrate_agent_node_provider_id_to_composite` (v19, issue #575
// first-class block) removed. The rewrite lives in
// `db::migrations::AlwaysStep::RewriteAgentNodeProviderId` — the
// `WHERE provider = 'minimax'` guard makes it idempotent and the
// registry's always-run pass handles DBs that bypassed the version
// gate (the v18→v19 bug class the doc-comment warned about).


fn get() -> &'static Database {
    DB.get().expect("database not initialized")
}

pub fn read_conn() -> ReadConnection<'static> {
    try_read_conn().expect("database reader pool checkout failed")
}

/// Check out a read-only connection with bounded waiting. This is the safe
/// entry point for async request paths, which must surface pool exhaustion
/// instead of blocking a runtime worker indefinitely.
pub fn try_read_conn() -> SqlResult<ReadConnection<'static>> {
    get().readers.checkout()
}

/// Lock the dedicated writer connection, recovering from a poisoned mutex instead of
/// propagating the poison as a panic (issue #1224).
///
/// `Mutex::lock()` returns `Err(PoisonError)` when any previous holder
/// panicked while holding the lock. `.unwrap()` on that error bricks
/// the DB for the rest of the process — every subsequent caller
/// re-panics with "poisoned lock" and persistence, preferences, and
/// the workers that read through here all deadlock from the user's
/// perspective.
///
/// For the writer `Connection` wrapped behind a `Mutex`, the
/// invariant the panic might have violated is the lock itself: the
/// panicked thread released the guard on unwind, so the
/// `Connection` inside is back to an unheld state and the next
/// caller can use it normally. `into_inner()` extracts that
/// inner value and trusts the next caller to perform whatever
/// recovery they need (sqlite rollback on the next statement, etc.).
///
/// All mutating callers should use this helper instead of locking the writer
/// directly. The recover-on-panic shape is the
/// project-wide convention — see `preferences::save` and
/// `services::warm_pool::with_inner_pool` for the same idiom on
/// smaller mutexes.
pub fn write_conn() -> std::sync::MutexGuard<'static, Connection> {
    let mutex = &get().writer;
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                "db writer mutex was poisoned by a prior panic — recovering (issue #1224)"
            );
            poisoned.into_inner()
        }
    }
}

/// Whether the global database has been initialised. Tests across the lib
/// binary share the same `DB` OnceCell, so the first one to call
/// `init` wins; later ones can use this to skip their own init and
/// share the existing connection. Production callers should still
/// `init` exactly once at startup and treat the error from a
/// double-init as a bug — this is purely a test-orchestration
/// affordance, not a permission to call `init` from production more
/// than once.
#[allow(dead_code)] // Test-only consumer (`commands::agent::tests`); clippy's
                    // lib-build dead-code check doesn't see across the test
                    // boundary, so we have to opt out. Same pattern as
                    // `set_lan_exposure_enabled` above.
pub fn is_initialized() -> bool {
    DB.get().is_some()
}
