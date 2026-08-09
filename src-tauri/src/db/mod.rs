//! Database module using rusqlite for local SQLite storage
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
//! - the post-migration verification (always-run idempotent safety nets).
//!
//! A new column becomes "add a [`migrations::ColumnSpec`] entry to
//! `migrations::SPECS` and you're done" — one place, not three. See
//! the module-level doc on `db::migrations` for the full design and
//! the bug class it closes (the v8→v9 `source_issue` regression is
//! the canonical pin).

pub(crate) mod migrations;

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

use rusqlite::{Connection, params};
pub use rusqlite::Result as SqlResult;
use once_cell::sync::OnceCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::*;

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

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
fn apply_connection_pragmas(conn: &Connection) -> SqlResult<()> {
    // `journal_mode` returns the resulting mode as a row, so it needs
    // `query_row`, not `execute` (rusqlite errors on rows from execute).
    let _mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(())
}

/// Initialize the database
pub fn init(db_path: &PathBuf) -> SqlResult<()> {
    let conn = Connection::open(db_path)?;
    apply_connection_pragmas(&conn)?;

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

    // Create schema (all tables + indexes, IF NOT EXISTS so they're
    // idempotent). For fresh DBs this creates the tables; for existing
    // DBs it's a no-op. MUST run before `evolve_to` so the runner's
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
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
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
            worktree_name TEXT,
            use_worktree INTEGER NOT NULL DEFAULT 1,
            is_pinned INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            status_changed_at TEXT NOT NULL DEFAULT (datetime('now')),
            source_pr INTEGER,
            head_repo_owner TEXT,
            head_repo_clone_url TEXT,
            source_pr_pinned_sha TEXT
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
        CREATE INDEX IF NOT EXISTS idx_coordinator_drive_prompts_created_at
            ON coordinator_drive_prompts(created_at);

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
        CREATE INDEX IF NOT EXISTS idx_warm_worktrees_mesh ON warm_worktrees(mesh_id);
        CREATE INDEX IF NOT EXISTS idx_warm_worktrees_status ON warm_worktrees(status);

        CREATE INDEX IF NOT EXISTS idx_agent_nodes_mesh ON agent_nodes(mesh_id);

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
        CREATE INDEX IF NOT EXISTS idx_autopilot_runs_mesh ON autopilot_runs(mesh_id);
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
    migrations::evolve_to(migrations::SCHEMA_VERSION, &conn)?;

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
    let _ = DB.set(Mutex::new(conn));
    Ok(())
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
// item 3) — the inline CREATE INDEX in init() handles this for fresh
// DBs; the helper was an idempotent safety-net re-run that is no
// longer needed once the inline `CREATE INDEX IF NOT EXISTS` is the
// canonical schema. The inline CREATE is still in init().

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

/// Safety net (v19): re-apply the **custom-account** Spawn Option
/// composite-id migration. Called from `lib.rs::setup` after
/// `preferences::init` (because it needs the live
/// `Vec<ProviderAccount>`). The companion to
/// [`ensure_agent_node_provider_id_migrated`] — together they
/// guarantee a v19+ DB never has a bare proxied-provider id in
/// `agent_nodes.provider` that should have been rewritten.
///
/// **Idempotent**: the underlying migration's `WHERE provider NOT
/// LIKE '%:%'` guard skips rows already in composite form, and the
/// `provider NOT IN (...)` whitelist protects bare
/// `HarnessProfile` ids from being rewritten.
pub(crate) fn ensure_agent_node_provider_id_custom_accounts_migrated(
    conn: &Connection,
    accounts: &[crate::preferences::ProviderAccount],
) -> SqlResult<()> {
    migrate_agent_node_provider_id_custom_accounts(conn, accounts)
}

/// Safety net: re-apply the **mesh-default** Spawn Option composite-id
/// rewrite. The v19 first-class block in
/// [`migrate_agent_node_provider_id_to_composite`] only rewrites
/// `agent_nodes.provider` — `meshes.default_provider` was missed, and a
/// pre-#575 user still has bare `"minimax"` / `"kimi"` values in the
/// per-mesh column after upgrade. Without this safety net, the bare
/// form routes through `resolve_provider_env` to the keyed **account**
/// instead of the post-#575 proxied pairing — the same trap the
/// `preferences::ensure_default_provider_normalized` helper closes for
/// the app-wide default.
///
/// Called from `lib.rs::setup` immediately after `preferences::init`.
/// **Idempotent**: the `WHERE default_provider IN (...)` whitelist
/// skips already-composite rows, so re-running on a healthy v19+ DB is
/// a no-op.
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

/// v19 Spawn Option composite-id migration, **custom-account block**
/// (issue #575 / ADR-0016 §6). Rewrites any bare id that names a
/// `claude_compatible` `ProviderAccount` (a user-configured custom
/// endpoint) to `claude:<id>`. Split from
/// [`migrate_agent_node_provider_id_to_composite`] so it can be
/// called from `lib.rs::setup` *after* `preferences::init` — the
/// first-class block has no preferences dependency and is therefore
/// safe to run from `db::init`, but the custom-account block needs
/// the live `Vec<ProviderAccount>` (the user's stored
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

/// Generate a random 32-character hex token (16 bytes of random data).
/// `pub(crate)` so the WS ticket store (`http::ws_ticket`, issue #500) can reuse
/// the same 128-bit entropy source for its short-lived handshake tickets.
pub(crate) fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    hex::encode(bytes)
}

/// Hash a token for at-rest storage (issue #495). Returns the lowercase
/// SHA-256 hex (64 chars). Tokens are high-entropy (128-bit random hex from
/// `generate_token`), so a plain SHA-256 is the right primitive here — no salt
/// or slow KDF, which exist to slow brute force on *low-entropy* passwords.
/// Because a raw token is 32 chars and this output is 64, the length alone
/// distinguishes a pre-hashing cleartext value from an already-hashed one
/// (used by `ensure_coordinator_tokens_hashed` to migrate idempotently).
pub(crate) fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// Get or create the root remote access token (stored in app_settings).
pub fn get_or_create_root_token() -> SqlResult<String> {
    let db = get().lock().unwrap();
    get_or_create_root_token_inner(&db)
}

/// Lock-free core, so the HTTP auth layer's tests (`http::auth`, issue #500) can
/// seed a root token on an in-memory connection.
pub fn get_or_create_root_token_inner(conn: &Connection) -> SqlResult<String> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'remote_access_token'",
            [],
            |row| row.get(0),
        )
        .ok();

    if let Some(token) = existing {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = generate_token();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('remote_access_token', ?1)",
        params![&token],
    )?;
    Ok(token)
}

/// Validate the root remote access token (the Admin-role credential, issue #500).
/// Lock-free core so the HTTP auth layer (`http::auth`, issue #500) and the
/// `/api/session` login endpoint can be unit-tested against an in-memory
/// connection — mirroring the coordinator validators' `_inner` pattern. Only the
/// `_inner` form exists: every caller (`resolve_role`, `login_device_session`)
/// already holds the DB lock. The root token is still stored cleartext (hashing
/// deferred to the Keychain slice, #495); an empty presented token never matches
/// an absent stored value.
pub fn validate_root_token_inner(conn: &Connection, token: &str) -> SqlResult<bool> {
    if token.is_empty() {
        return Ok(false);
    }
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'remote_access_token'",
            [],
            |row| row.get(0),
        )
        .ok();
    Ok(stored.as_deref() == Some(token))
}

// --- Coordinator read API auth (ADR-0008) ---
//
// The coordinator surface is a SEPARATE, capability-scoped credential from the
// mobile root token, gated behind a master enable switch that defaults OFF.
// A read-scoped token can never be used to drive nodes (drive is a future
// slice). All three keys live in app_settings alongside `remote_access_token`.
const COORDINATOR_ENABLED_KEY: &str = "coordinator_api_enabled";
const COORDINATOR_READ_TOKEN_KEY: &str = "coordinator_read_token";
// Drive (write) side (issue #319). The drive scope is a SEPARATE token from the
// read token — a read-scoped credential can never drive a node — behind its own
// enable switch (also defaulting OFF) so drive can be killed independently while
// reads stay up. Both still sit under the coordinator master switch above, so
// disabling the whole surface disables drive too.
const COORDINATOR_DRIVE_ENABLED_KEY: &str = "coordinator_drive_enabled";
const COORDINATOR_DRIVE_TOKEN_KEY: &str = "coordinator_drive_token";

/// Is the coordinator read API enabled? Defaults to `false` (off) for a fresh
/// install, so a naive setup is never an open endpoint.
pub fn coordinator_api_enabled() -> SqlResult<bool> {
    let db = get().lock().unwrap();
    coordinator_api_enabled_inner(&db)
}

pub fn coordinator_api_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Flip the master enable switch for the coordinator read API.
pub fn set_coordinator_api_enabled(enabled: bool) -> SqlResult<()> {
    let db = get().lock().unwrap();
    set_coordinator_api_enabled_inner(&db, enabled)
}

pub fn set_coordinator_api_enabled_inner(conn: &Connection, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Whether the embedded HTTP/WS server may bind beyond loopback (issue #496).
/// Off by default: a fresh install binds only `127.0.0.1`/`::1`, so external
/// devices on the LAN cannot reach the hub without an explicit opt-in. Enabling
/// it is what lets a phone connect over LAN/VPN (TLS for that path is a later
/// slice). The secure default is enforced by `http::start_http_server` reading
/// this before choosing its bind addresses.
const LAN_EXPOSURE_ENABLED_KEY: &str = "lan_exposure_enabled";

/// Is LAN/VPN exposure enabled? Defaults to `false` (loopback-only) so a naive
/// setup is never reachable from another machine.
pub fn lan_exposure_enabled() -> SqlResult<bool> {
    let db = get().lock().unwrap();
    lan_exposure_enabled_inner(&db)
}

pub fn lan_exposure_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![LAN_EXPOSURE_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Flip the LAN/VPN exposure switch. Takes effect on the next server start.
pub fn set_lan_exposure_enabled(enabled: bool) -> SqlResult<()> {
    let db = get().lock().unwrap();
    set_lan_exposure_enabled_inner(&db, enabled)
}

pub fn set_lan_exposure_enabled_inner(conn: &Connection, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![LAN_EXPOSURE_ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Mint (or replace) the read-scoped coordinator token, returning it. Minting a
/// fresh token invalidates any previously issued one.
pub fn generate_coordinator_read_token() -> SqlResult<String> {
    let db = get().lock().unwrap();
    generate_coordinator_read_token_inner(&db)
}

pub fn generate_coordinator_read_token_inner(conn: &Connection) -> SqlResult<String> {
    // Return the raw token to the caller once; persist only its hash (#495) so a
    // DB dump or rogue agent reading app_settings can't recover the secret.
    let token = generate_token();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_READ_TOKEN_KEY, hash_token(&token)],
    )?;
    Ok(token)
}

/// The stored read token *hash*, if one has been minted (and is non-empty).
/// Used by the status command to report `has_token` — presence only; the value
/// is a SHA-256 hash (#495), never the raw token.
pub fn coordinator_read_token() -> SqlResult<Option<String>> {
    let db = get().lock().unwrap();
    coordinator_read_token_inner(&db)
}

pub fn coordinator_read_token_inner(conn: &Connection) -> SqlResult<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_READ_TOKEN_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.filter(|v| !v.is_empty()))
}

/// Validate a presented token for READ access. Rejects unless the API is
/// enabled AND the token matches the minted read token — so disabling the
/// master switch instantly cuts off all read access even with a valid token.
/// Only the `_inner` form exists: the HTTP auth layer (`http::auth::resolve_role`,
/// issue #500) locks the DB once and calls it, so a separate self-locking
/// wrapper would only invite a nested-lock deadlock.
pub fn validate_coordinator_read_token_inner(conn: &Connection, token: &str) -> SqlResult<bool> {
    if token.is_empty() || !coordinator_api_enabled_inner(conn)? {
        return Ok(false);
    }
    // The DB holds only the hash (#495), so hash the presented token and compare
    // hashes. The raw token never has to be reconstructed to authenticate.
    match coordinator_read_token_inner(conn)? {
        Some(stored) => Ok(stored == hash_token(token)),
        None => Ok(false),
    }
}

// --- Coordinator drive (write) auth (ADR-0008 §5, issue #319) ---

/// Is the drive side enabled? Defaults to `false` (off), independent of the
/// read side, so a deployment can offer read-only coordination without ever
/// exposing the ability to write to a node's PTY. (No process-global wrapper
/// yet — only `validate_coordinator_drive_token_inner` reads this; a drive
/// Settings slice can add the public getter when it surfaces the state.)
pub fn coordinator_drive_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_DRIVE_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Flip the drive kill-switch. Off by default; killing it stops all driving
/// while leaving the read surface untouched.
pub fn set_coordinator_drive_enabled(enabled: bool) -> SqlResult<()> {
    let db = get().lock().unwrap();
    set_coordinator_drive_enabled_inner(&db, enabled)
}

pub fn set_coordinator_drive_enabled_inner(conn: &Connection, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_DRIVE_ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Mint (or replace) the drive-scoped coordinator token. Distinct from the read
/// token, so granting drive is an explicit, separate act from granting read.
pub fn generate_coordinator_drive_token() -> SqlResult<String> {
    let db = get().lock().unwrap();
    generate_coordinator_drive_token_inner(&db)
}

pub fn generate_coordinator_drive_token_inner(conn: &Connection) -> SqlResult<String> {
    // Raw token returned once; only its hash is persisted (#495).
    let token = generate_token();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_DRIVE_TOKEN_KEY, hash_token(&token)],
    )?;
    Ok(token)
}

/// The stored drive token *hash*, if one has been minted (and is non-empty).
/// Only the validator reads it today (no process-global getter until a Settings
/// slice reports drive state); kept `_inner` so a future caller can lock once.
/// The value is a SHA-256 hash (#495), never the raw token.
pub fn coordinator_drive_token_inner(conn: &Connection) -> SqlResult<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_DRIVE_TOKEN_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.filter(|v| !v.is_empty()))
}

/// Validate a presented token for DRIVE access. Rejects unless the coordinator
/// master switch is on AND the drive kill-switch is on AND the token matches the
/// minted drive token. Because the drive token is stored under its own key, a
/// read-scoped token never validates here — drive is its own capability. Only the
/// `_inner` form exists (locked once by `http::auth::resolve_role`, issue #500).
pub fn validate_coordinator_drive_token_inner(conn: &Connection, token: &str) -> SqlResult<bool> {
    if token.is_empty()
        || !coordinator_api_enabled_inner(conn)?
        || !coordinator_drive_enabled_inner(conn)?
    {
        return Ok(false);
    }
    // Stored value is the hash (#495); compare against the hashed presentation.
    match coordinator_drive_token_inner(conn)? {
        Some(stored) => Ok(stored == hash_token(token)),
        None => Ok(false),
    }
}

// --- Coordinator drive idempotency ledger (issue #320, ADR-0008 §6) ---
//
// A Coordinator on a flaky network retries a timed-out drive; the caller-supplied
// idempotency key lets Buildmesh recognise the retry and replay the original
// verdict instead of sending the prompt twice. The store deals in the verdict's
// wire string (`"delivered"`/`"unverified"`) so this layer never depends on the
// `coordinator::drive` module — the drive side owns the string↔enum mapping.
// Lock-once + `_inner(&Connection)` so the logic is unit-testable in memory.

/// The outcome of an atomic claim attempt (issue #750, item 1). Drives the
/// orchestrator: `Claimed` means the caller owns the row and must drive +
/// finalize; `Replay` means a finalized peer row exists with the same prompt
/// payload, return its verdict; `Mismatch` means a finalized peer row exists
/// but the *prompt* differs — Stripe-style reject (#750 item 2); `InProgress`
/// means a peer holds the row in `pending` (the caller's brief wait inside
/// `drive_node_idempotent` polls for finalize before surfacing this).
///
/// A genuine read error is propagated as `Err` (not collapsed into any
/// variant) so the drive path can fail safe — same fail-safe contract the
/// pre-#750 lookup established (issue #320 review).
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This caller inserted the pending row; proceed to drive.
    Claimed,
    /// The row was already finalized with the same prompt; replay its verdict.
    Replay { verdict: String },
    /// The row was already finalized but with a different prompt; reject.
    Mismatch,
    /// The row is `pending` — another caller is currently driving this key.
    InProgress,
}

/// Maximum age (in seconds) a `pending` row may sit in the ledger before the
/// next claim attempt treats it as orphaned (crash-mid-send) and reclaims it.
/// A successful drive is sub-millisecond; 30s is generous enough that a
/// live, currently-driving peer is never reclaimed out from under itself, and
/// short enough that a crashed Buildmesh doesn't lock out a key for long.
pub const PENDING_CLAIM_TIMEOUT_SECS: i64 = 30;

/// Atomic claim-before-send (issue #750, item 1). In a single transaction:
///   1. Reclaim any `pending` row older than `PENDING_CLAIM_TIMEOUT_SECS`
///      for this `(node_id, key)` (a crash-mid-send row must not block the
///      key forever — a retry can re-send the prompt).
///   2. Try to `INSERT OR IGNORE` a fresh `pending` row with the prompt
///      hash. If this caller wins the race (no prior row exists), the
///      orchestrator proceeds to drive; if a prior row exists, read its
///      state and map to `Replay`/`Mismatch`/`InProgress`.
///
/// Lock-once + `_inner(&Connection)` so the drive logic is unit-testable
/// in-memory (`db::drive_idempotency_tests`). Returns `Err(_)` only on a
/// real DB failure (lock, IO, corruption) — the same fail-safe contract
/// the pre-#750 `lookup` established (issue #320 review): the orchestrator
/// must never mistake "couldn't read" for "key never seen".
pub fn claim_drive_prompt(
    node_id: i64,
    key: &str,
    prompt_hash: &str,
) -> SqlResult<ClaimOutcome> {
    let db = get().lock().unwrap();
    claim_drive_prompt_inner(&db, node_id, key, prompt_hash)
}

pub fn claim_drive_prompt_inner(
    conn: &Connection,
    node_id: i64,
    key: &str,
    prompt_hash: &str,
) -> SqlResult<ClaimOutcome> {
    let tx = conn.unchecked_transaction()?;

    // Step 1: orphan recovery. Any `pending` row older than the timeout is
    // from a crashed prior attempt — the prompt never landed, so a retry
    // is safe to re-send. DELETE (not UPDATE-to-expired) so the slot is
    // open for the new INSERT OR IGNORE below.
    tx.execute(
        "DELETE FROM coordinator_drive_prompts
            WHERE node_id = ?1 AND idempotency_key = ?2
              AND status = 'pending'
              AND claimed_at < datetime('now', '-' || ?3 || ' seconds')",
        params![node_id, key, PENDING_CLAIM_TIMEOUT_SECS],
    )?;

    // Step 2: try to insert a fresh pending row. `INSERT OR IGNORE` is the
    // race resolver: only one concurrent claim survives, the rest fall into
    // the SELECT below.
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO coordinator_drive_prompts
             (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict)
             VALUES (?1, ?2, 'pending', datetime('now'), ?3, '')",
        params![node_id, key, prompt_hash],
    )?;

    if inserted == 1 {
        // We won the race — return early without the SELECT (no peer row to
        // read). The finalize step will UPDATE this row after the send.
        tx.commit()?;
        return Ok(ClaimOutcome::Claimed);
    }

    // Step 3: we lost (or hit a finalized row). Read its current state to
    // decide which `ClaimOutcome` variant to surface. The SELECT can't
    // realistically miss — we just inserted or already had a row — but if
    // it does, propagate the `Err` rather than collapse it to a default
    // outcome (fail-safe contract from issue #320 review).
    let (status, verdict, stored_hash) = tx.query_row(
        "SELECT status, verdict, prompt_hash FROM coordinator_drive_prompts
             WHERE node_id = ?1 AND idempotency_key = ?2",
        params![node_id, key],
        |row| {
            let status: String = row.get(0)?;
            let verdict: String = row.get(1)?;
            let stored_hash: String = row.get(2)?;
            Ok((status, verdict, stored_hash))
        },
    )?;

    tx.commit()?;

    Ok(match (status.as_str(), verdict.is_empty()) {
        // Pre-v32 rows have a non-empty `verdict` (v23 required it NOT NULL)
        // but `status='pending'` from the v32 column DEFAULT — treat them as
        // finalized, not in-progress, so a Coordinator hitting a pre-v32
        // ledger row replays the verdict rather than getting a phantom
        // `InProgress` and stalling for `PENDING_CLAIM_TIMEOUT_SECS`.
        // The `prompt_hash == ''` default still makes a key-reuse-with-
        // different-payload surface as Mismatch (same-key-same-verdict but
        // empty stored hash) — acceptable because drive is off-by-default
        // and unreleased (#313), so there are no real pre-v32 callers.
        (_, false) if stored_hash == prompt_hash => ClaimOutcome::Replay { verdict },
        (_, false) => ClaimOutcome::Mismatch,
        // Live `pending` row with an empty verdict — a real peer's
        // in-flight drive. The orchestrator briefly polls for finalize.
        ("pending", true) => ClaimOutcome::InProgress,
        // A finalized row with no recorded verdict (shouldn't happen on
        // v32+, where finalize always writes `verdict` together with
        // `status`). Treated as Mismatch rather than a silent InProgress
        // so the caller can mint a fresh key rather than wait for a row
        // that will never finalize.
        _ => ClaimOutcome::Mismatch,
    })
}

/// Finalize a claim: UPDATE the `pending` row to its terminal status +
/// verdict. Idempotent (UPDATE is naturally so) and does not insert if no
/// `pending` row exists — a finalized row is left alone (the first verdict
/// wins, mirroring the pre-#750 `INSERT OR IGNORE` rule). Returns the number
/// of rows actually changed so the caller can log a warning when a drive
/// completed but the finalize found no row to update.
pub fn finalize_drive_prompt(
    node_id: i64,
    key: &str,
    verdict: VerdictStr<'_>,
) -> SqlResult<usize> {
    let db = get().lock().unwrap();
    finalize_drive_prompt_inner(&db, node_id, key, verdict)
}

pub fn finalize_drive_prompt_inner(
    conn: &Connection,
    node_id: i64,
    key: &str,
    verdict: VerdictStr<'_>,
) -> SqlResult<usize> {
    // Only update `status` + `verdict`; the `prompt_hash` column stays as it
    // was set during the claim — a future claim must be able to read it back
    // and Replay (status == terminal AND stored_hash == incoming_hash) rather
    // than Mismatch because we cleared the hash on finalize.
    conn.execute(
        "UPDATE coordinator_drive_prompts
             SET status = ?3, verdict = ?4
             WHERE node_id = ?1 AND idempotency_key = ?2 AND status = 'pending'",
        params![node_id, key, verdict.as_status_str(), verdict.as_str()],
    )
}

/// Verdict wrapper so callers don't have to thread both the DB status string
/// ("delivered" / "unverified") and the wire verdict string. They're the same
/// word today; this keeps the call site clean if they ever diverge.
pub enum VerdictStr<'a> {
    Delivered,
    Unverified,
    // PhantomData-friendly lifetime so the enum stays `'a`-parameterised
    // (matches the wire form the tests use); the `PhantomData` is invisible
    // to consumers and disappears at compile time.
    #[doc(hidden)]
    _Phantom(std::marker::PhantomData<&'a ()>),
}

impl<'a> VerdictStr<'a> {
    /// The DB status string (`'delivered'` / `'unverified'`).
    pub fn as_status_str(&self) -> &'static str {
        match self {
            VerdictStr::Delivered => "delivered",
            VerdictStr::Unverified => "unverified",
            VerdictStr::_Phantom(_) => unreachable!("PhantomData is uninhabited"),
        }
    }

    /// The verdict wire string (same value as `as_status_str` today, but kept
    /// distinct so a future schema split can move the two columns apart).
    pub fn as_str(&self) -> &'static str {
        self.as_status_str()
    }
}

/// Release a claim when the drive itself failed (so a retry can re-attempt
/// rather than wait on a `pending` row the orchestrator never finalized).
/// Only deletes `pending` rows — a finalized row stays put, and a second
/// `NotLive` retry still sees the verdict if the row happens to already be
/// terminal from a peer.
pub fn release_drive_prompt_claim(node_id: i64, key: &str) -> SqlResult<usize> {
    let db = get().lock().unwrap();
    release_drive_prompt_claim_inner(&db, node_id, key)
}

pub fn release_drive_prompt_claim_inner(
    conn: &Connection,
    node_id: i64,
    key: &str,
) -> SqlResult<usize> {
    conn.execute(
        "DELETE FROM coordinator_drive_prompts
             WHERE node_id = ?1 AND idempotency_key = ?2 AND status = 'pending'",
        params![node_id, key],
    )
}

/// Bounded-age prune (issue #750, item 3). Deletes every row whose
/// `created_at` is older than `days` days — same shape as the
/// `pending_worktree_removals` drain prior art. The background worker
/// (`services::coordinator_ledger_maintenance`) calls this on a 30-minute
/// cadence; a startup sweep runs from the same module. Returns the number of
/// rows deleted (informational; the worker logs non-zero results).
pub fn prune_drive_prompts_older_than(days: i64) -> SqlResult<usize> {
    let db = get().lock().unwrap();
    prune_drive_prompts_older_than_inner(&db, days)
}

pub fn prune_drive_prompts_older_than_inner(
    conn: &Connection,
    days: i64,
) -> SqlResult<usize> {
    conn.execute(
        "DELETE FROM coordinator_drive_prompts
             WHERE created_at < datetime('now', '-' || ?1 || ' days')",
        params![days],
    )
}

// --- Persistent device sessions (issue #502, PRD #494) ---
//
// A paired phone is identified by its own token, minted at pairing and stored
// here as a SHA-256 hash (never the raw value, mirroring the coordinator
// tokens). Because each device holds a *distinct* token, the IP is no longer an
// auth factor — that's what lets a phone roam across networks — and revoking one
// device (deleting its row) leaves every other device untouched. All functions
// follow the lock-once + `_inner(&Connection)` pattern so the HTTP auth layer
// can validate against an in-memory connection in tests (issue #500).

/// Pair a new device: mint a token, persist only its hash + metadata, and return
/// the row id with the *raw* token (handed to the client exactly once, then only
/// ever re-presented by the client). `label` is a human-friendly name derived
/// from the client's `User-Agent`; `ip` is the peer address at pairing. Only the
/// `_inner` form exists — pairing always happens inside `login_device_session`,
/// which already holds the lock.
pub fn pair_device_session_inner(
    conn: &Connection,
    label: Option<&str>,
    ip: Option<&str>,
) -> SqlResult<(i64, String)> {
    let token = generate_token();
    conn.execute(
        "INSERT INTO device_sessions (token_hash, label, last_ip) VALUES (?1, ?2, ?3)",
        params![hash_token(&token), label, ip],
    )?;
    Ok((conn.last_insert_rowid(), token))
}

/// Resolve a presented token to its device id, or `None` if no live device holds
/// it. A revoked device's row is deleted, so a revoked token resolves to `None`
/// here — which is exactly what makes the next request fail auth. An empty token
/// never matches. Only the `_inner` form exists: the auth layer
/// (`http::auth::resolve_role`) already locks the DB once and passes the
/// connection through, and `login_device_session` calls it under its own lock.
pub fn validate_device_token_inner(conn: &Connection, token: &str) -> SqlResult<Option<i64>> {
    if token.is_empty() {
        return Ok(None);
    }
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM device_sessions WHERE token_hash = ?1",
            params![hash_token(token)],
            |row| row.get(0),
        )
        .ok();
    Ok(id)
}

/// Record activity for a device: bump `last_active_at` to now and refresh the
/// last-seen IP. Called on login refresh and WS-ticket mint, not per request, so
/// a polling client doesn't write the DB on every poll. A no-op for an unknown
/// id (the row may have just been revoked).
pub fn touch_device_session(id: i64, ip: Option<&str>) -> SqlResult<()> {
    let db = get().lock().unwrap();
    touch_device_session_inner(&db, id, ip)
}

pub fn touch_device_session_inner(conn: &Connection, id: i64, ip: Option<&str>) -> SqlResult<()> {
    conn.execute(
        "UPDATE device_sessions SET last_active_at = datetime('now'), last_ip = ?2 WHERE id = ?1",
        params![id, ip],
    )?;
    Ok(())
}

/// List all paired devices, newest first, for the "Authorized Devices" panel.
/// Returns the wire view (`DeviceSession`) — never the `token_hash`.
pub fn list_device_sessions() -> SqlResult<Vec<DeviceSession>> {
    let db = get().lock().unwrap();
    list_device_sessions_inner(&db)
}

pub fn list_device_sessions_inner(conn: &Connection) -> SqlResult<Vec<DeviceSession>> {
    let mut stmt = conn.prepare(
        "SELECT id, label, last_ip, created_at, last_active_at \
         FROM device_sessions ORDER BY last_active_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(DeviceSession {
            id: row.get(0)?,
            label: row.get(1)?,
            last_ip: row.get(2)?,
            created_at: row.get(3)?,
            last_active_at: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// Revoke a device by deleting its row. Returns `true` if a row was removed
/// (`false` if the id was already gone). The caller is responsible for kicking
/// any live WebSocket the device holds (`http::revocation::revoke`); deleting the
/// row alone only blocks the *next* request, not an already-open socket.
pub fn revoke_device_session(id: i64) -> SqlResult<bool> {
    let db = get().lock().unwrap();
    revoke_device_session_inner(&db, id)
}

pub fn revoke_device_session_inner(conn: &Connection, id: i64) -> SqlResult<bool> {
    let affected = conn.execute("DELETE FROM device_sessions WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

/// The `POST /api/session` decision (issue #502), resolving what cookie to set:
///
/// - presented token is an **existing device token** → *refresh*: bump the
///   device's activity (new IP for roaming) and hand the same token back, so a
///   re-launching phone keeps its identity instead of accumulating a new device
///   row on every load;
/// - presented token is the **root token** (the pairing secret from the desktop
///   QR) → *pair*: mint a brand-new device session and return its token, which
///   the client then persists in place of the root token;
/// - anything else → `None` (the caller answers 401).
///
/// Returns the effective `(device_id, raw_token)` to set as the `bm_session`
/// cookie. Checking the device token first means a paired client re-presenting
/// its device token never spuriously mints a second device.
pub fn login_device_session(
    presented: &str,
    label: Option<&str>,
    ip: Option<&str>,
) -> SqlResult<Option<(i64, String)>> {
    let db = get().lock().unwrap();
    login_device_session_inner(&db, presented, label, ip)
}

pub fn login_device_session_inner(
    conn: &Connection,
    presented: &str,
    label: Option<&str>,
    ip: Option<&str>,
) -> SqlResult<Option<(i64, String)>> {
    if let Some(id) = validate_device_token_inner(conn, presented)? {
        touch_device_session_inner(conn, id, ip)?;
        return Ok(Some((id, presented.to_string())));
    }
    if validate_root_token_inner(conn, presented)? {
        return Ok(Some(pair_device_session_inner(conn, label, ip)?));
    }
    Ok(None)
}

// `ensure_coordinator_tokens_hashed` (issue #495) moved to
// `db::migrations::AlwaysStep::HashCoordinatorTokens`. The 32-char raw
// token → SHA-256 hash rehash runs as part of the registry's
// always-pass step on every launch.
//
// `test_migrate_if_needed` removed — its purpose was to exercise the
// pre-v6 `migrate_projects_*` / `migrate_mesh_rename` ladder against
// in-memory fixtures simulating v2 → current upgrades. With those
// helpers gone, the function has no callers; tests that need to
// simulate a v6 → current upgrade now call
// `migrations::evolve_to(migrations::SCHEMA_VERSION, &conn)` directly.

pub fn get() -> &'static Mutex<Connection> {
    DB.get().expect("database not initialized")
}

/// Whether the global DB has been initialised. Tests across the lib
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
    migrations::mesh_columns_projection()
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
    })
}

fn get_mesh_by_id_inner(conn: &Connection, id: i64) -> SqlResult<Mesh> {
    let mut stmt = conn.prepare(
        &format!("SELECT {} FROM meshes WHERE id = ?1", mesh_columns())
    )?;
    stmt.query_row(params![id], map_mesh_row)
}

fn parse_str(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

const AGENT_NODE_COLUMNS: &str =
    "id, mesh_id, name, path, branch, env, provider, status, cli_session_id, worktree_name, created_at, source_issue, use_worktree, is_pinned, position, source_pr, head_repo_owner, head_repo_clone_url, source_pr_pinned_sha";

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
    let db = get().lock().unwrap();
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
        // map_agent_node_row reads positional indices 0..18, which match the
        // AGENT_NODE_COLUMNS order we selected first; mesh name and
        // status_changed_at follow at 19 and 20 (v16 added head_repo_owner +
        // head_repo_clone_url at 16/17, source_pr_pinned_sha at 18, v29
        // added is_pinned at 13 — see AGENT_NODE_COLUMNS).
        let node = map_agent_node_row(row)?;
        let mesh_name: String = row.get(19)?;
        // Read as Option: a DB migrated from a pre-v14 schema added the column
        // nullable, so any row inserted before `create_agent_node` started
        // stamping it (or via some other path) can be NULL. A non-Option read
        // would make rusqlite error the whole query on a single NULL row,
        // blanking the endpoint. Fall back to the node's creation time.
        let status_changed_at: Option<String> = row.get(20)?;
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

// --- Mesh operations ---

pub fn create_mesh(name: &str, path: &str) -> SqlResult<Mesh> {
    let db = get().lock().unwrap();

    // Check if mesh with this path already exists (idempotent upsert)
    let existing: Option<i64> = db.query_row(
        "SELECT id FROM meshes WHERE path = ?1",
        params![path],
        |row| row.get(0),
    ).ok();

    if let Some(id) = existing {
        return get_mesh_by_id_inner(&db, id);
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
    get_mesh_by_id_inner(&db, id)
}

pub fn get_mesh_by_id(id: i64) -> SqlResult<Mesh> {
    let db = get().lock().unwrap();
    get_mesh_by_id_inner(&db, id)
}

/// Set (or clear) a mesh's accent colour. `Some(hex)` stores the `#rrggbb`
/// string; `None` clears it back to the deterministic-palette fallback.
/// Returns the number of rows updated so callers can surface a "mesh not
/// found" error rather than a silent no-op (matches the zero-rows contract
/// used by `set_mesh_sandbox` / `update_mesh_pool_size`).
pub fn set_mesh_color(id: i64, color: Option<&str>) -> SqlResult<usize> {
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
    db.execute(
        "UPDATE meshes SET autopilot_enabled = ?1 WHERE id = ?2",
        params![if enabled { 1 } else { 0 }, id],
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
    let db = get().lock().unwrap();
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

/// Every mesh with Autopilot enabled — the poller's work list.
pub fn list_autopilot_enabled_meshes() -> SqlResult<Vec<Mesh>> {
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
/// (`services::agent_node::delete`) — the table declares `ON DELETE
/// CASCADE`, but this codebase never turns on SQLite's `foreign_keys`
/// pragma (see `apply_connection_pragmas`), so the cascade is decorative
/// and the delete must be explicit. Deleting the row also un-dedupes the
/// issue (`list_known_autopilot_issue_numbers`), which is the intended
/// behaviour: closing a bad autopilot node while the issue stays labelled
/// lets the poller retry it.
pub fn delete_autopilot_run(node_id: i64) -> SqlResult<()> {
    let db = get().lock().unwrap();
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
const COUNT_ACTIVE_AUTOPILOT_SQL: &str = "SELECT COUNT(*) FROM autopilot_runs r \
     JOIN agent_nodes a ON a.id = r.node_id \
     WHERE r.state IN ('implementing', 'finishing', 'suffix_pending') \
     AND a.status != 'archived'";

/// Number of *active* Autopilot nodes for a mesh. This is the count the
/// poller compares against `autopilot_concurrency_limit`; completed/failed
/// runs free their slot.
pub fn count_active_autopilot_nodes(mesh_id: i64) -> SqlResult<i64> {
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
    db.query_row(COUNT_ACTIVE_AUTOPILOT_SQL, [], |row| row.get(0))
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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

pub fn update_mesh_positions_batch(updates: &[(i64, i64)]) -> SqlResult<()> {
    if updates.is_empty() { return Ok(()); }
    let db = get().lock().unwrap();
    for (id, pos) in updates {
        db.execute(
            "UPDATE meshes SET position = ?1 WHERE id = ?2",
            params![pos, id],
        )?;
    }
    Ok(())
}

pub fn list_meshes() -> SqlResult<Vec<Mesh>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM meshes ORDER BY position ASC, name ASC", mesh_columns())
    )?;
    let rows = stmt.query_map([], map_mesh_row)?;
    rows.collect()
}

/// Look up a mesh by its path.
pub fn get_mesh_by_path(path: &str) -> SqlResult<Mesh> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM meshes WHERE path = ?1", mesh_columns())
    )?;
    stmt.query_row(params![path], map_mesh_row)
}

pub fn delete_mesh(id: i64) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("DELETE FROM agent_nodes WHERE mesh_id = ?1", params![id])?;
    // The `warm_worktrees.mesh_id` FK declares ON DELETE CASCADE, but SQLite
    // leaves foreign-key enforcement off by default (no `PRAGMA foreign_keys`
    // is set), so the cascade never fires — drop the mesh's pool rows
    // explicitly or they outlive the mesh as orphans (issue #609).
    delete_warm_worktrees_for_mesh_inner(&db, id)?;
    db.execute("DELETE FROM meshes WHERE id = ?1", params![id])?;
    Ok(())
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
) -> SqlResult<AgentNode> {
    let db = get().lock().unwrap();
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
    db.execute(
        "INSERT INTO agent_nodes (mesh_id, name, path, branch, env, provider, status, worktree_name, source_issue, source_pr, source_pr_pinned_sha, use_worktree, position, status_changed_at, head_repo_owner, head_repo_clone_url)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
        ],
    )?;
    let id = db.last_insert_rowid();
    get_agent_node_by_id_inner(&db, id)
}

/// Persist new grid positions for a batch of agent nodes (drag-to-reorder).
/// Callers send the full new ordering for the affected mesh so the DB stays in
/// sync with the frontend's optimistic update. Mirrors `update_mesh_positions_batch`.
pub fn update_agent_node_positions_batch(updates: &[(i64, i64)]) -> SqlResult<()> {
    if updates.is_empty() { return Ok(()); }
    let db = get().lock().unwrap();
    for (id, pos) in updates {
        db.execute(
            "UPDATE agent_nodes SET position = ?1 WHERE id = ?2",
            params![pos, id],
        )?;
    }
    Ok(())
}

/// Rename IPC only — writes `name` alone and is **not** the spawn-path
/// slug adoption. The spawn path uses `adopt_manual_pool_slug`, which
/// writes `name` and `worktree_name` together so the close path's
/// removal directory cannot drift from the displayed name (#1080).
/// Mixing the two would reintroduce the bug.
pub fn update_agent_node_name(id: i64, name: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute(
        "UPDATE agent_nodes SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    Ok(())
}

/// Adopt the pool's pre-assigned slug onto a Manual node. Writes `name`
/// AND `worktree_name` in one UPDATE — they are two halves of one
/// adoption, and #1080 was the two halves drifting apart. Issue/PR
/// spawns keep their own `gh{N}-`/`pr{N}-` identity; their pool dir is
/// moved to match instead (see `git::worktree::provision`).
pub fn adopt_manual_pool_slug(id: i64, slug: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    adopt_manual_pool_slug_inner(&db, id, slug)
}

fn adopt_manual_pool_slug_inner(conn: &Connection, id: i64, slug: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_nodes SET name = ?1, worktree_name = ?1 WHERE id = ?2",
        params![slug, id],
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
    let db = get().lock().unwrap();
    db.execute(
        "UPDATE agent_nodes SET provider = ?1 WHERE id = ?2",
        params![provider, id],
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
    get_agent_node_by_id_inner(&db, id)
}

pub fn list_agent_nodes() -> SqlResult<Vec<AgentNode>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE status != 'archived' ORDER BY mesh_id ASC, position ASC, created_at ASC", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map([], map_agent_node_row)?;
    rows.collect()
}

pub fn list_agent_nodes_by_mesh(mesh_id: i64) -> SqlResult<Vec<AgentNode>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE mesh_id = ?1 ORDER BY position ASC, created_at ASC", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map(params![mesh_id], map_agent_node_row)?;
    rows.collect()
}

pub fn update_agent_node_status(id: i64, status: SessionStatus) -> SqlResult<()> {
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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

/// Update the persisted CLI session id for an agent node. For the fill-only
/// variant used by attention-hook fallback, see `set_cli_session_id_if_missing`.
pub fn update_cli_session_id(id: i64, cli_id: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    db.execute("UPDATE agent_nodes SET cli_session_id = ?1 WHERE id = ?2", params![cli_id, id])?;
    Ok(())
}

/// Persist a provider-assigned session id without overwriting an id captured
/// by an earlier, more immediate source. Codex hook callbacks use this as a
/// structured fallback when PTY output did not expose the UUID (issue #1089).
pub fn set_cli_session_id_if_missing(id: i64, cli_id: &str) -> SqlResult<bool> {
    let db = get().lock().unwrap();
    set_cli_session_id_if_missing_inner(&db, id, cli_id)
}

fn set_cli_session_id_if_missing_inner(
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
    let db = get().lock().unwrap();
    // Deliberately does NOT touch `status_changed_at`: a restart-time suspend is
    // bookkeeping, not agent activity, so the coordinator digest's
    // `last_activity` should keep reporting when the node *actually* last did
    // work (pre-crash), not the moment the app reopened. (See ADR-0008 spine.)
    // `spawning` (issue #654) is included so a crash between process launch
    // and the 3s Running promotion leaves a recoverable `suspended` row.
    let count = db.execute(
        "UPDATE agent_nodes SET status = 'suspended' \
         WHERE status IN ('running', 'awaiting_input', 'pending', 'spawning')",
        [],
    )?;
    Ok(count)
}

pub fn list_suspended_nodes() -> SqlResult<Vec<AgentNode>> {
    let db = get().lock().unwrap();
    let mut stmt = db.prepare(
        &format!("SELECT {} FROM agent_nodes WHERE status = 'suspended' AND cli_session_id IS NOT NULL", AGENT_NODE_COLUMNS)
    )?;
    let rows = stmt.query_map([], map_agent_node_row)?;
    rows.collect()
}

// --- Pending worktree removal queue ---
//
// Closing a node deletes its row immediately so the UI can drop it at once, but
// the worktree directory removal is slow and retry-prone. We record the intent
// here (atomically with the row delete) so a background task — or the next app
// launch — can finish the removal. `worktree_path` is UNIQUE so re-enqueuing the
// same path is a no-op rather than a duplicate.

fn enqueue_worktree_removal_inner(conn: &Connection, path: &str, node_name: &str) -> SqlResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pending_worktree_removals (worktree_path, node_name) VALUES (?1, ?2)",
        params![path, node_name],
    )?;
    Ok(())
}

fn list_pending_worktree_removals_inner(conn: &Connection) -> SqlResult<Vec<PendingWorktreeRemoval>> {
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

fn delete_agent_node_enqueueing_removal_inner(
    conn: &Connection,
    id: i64,
    removal: Option<(&str, &str)>,
) -> SqlResult<()> {
    conn.execute("DELETE FROM agent_nodes WHERE id = ?1", params![id])?;
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
    let mut db = get().lock().unwrap();
    let tx = db.transaction()?;
    delete_agent_node_enqueueing_removal_inner(&tx, id, removal)?;
    tx.commit()
}

pub fn list_pending_worktree_removals() -> SqlResult<Vec<PendingWorktreeRemoval>> {
    let db = get().lock().unwrap();
    list_pending_worktree_removals_inner(&db)
}

pub fn delete_pending_worktree_removal(path: &str) -> SqlResult<()> {
    let db = get().lock().unwrap();
    delete_pending_worktree_removal_inner(&db, path)
}

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

// `ensure_warm_worktables_table` (v21) moved to
// `db::migrations::AlwaysStep::EnsureWarmWorktreesTable`. Fresh DBs
// still create the table via the inline CREATE in `init()`; the
// always-run pass is the v6+ safety net for DBs whose
// `schema_version` was bumped past 21 without the inline CREATE.

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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
    delete_warm_worktree_inner(&db, id)
}

/// Lock-free `_inner` so the use-site guard in
/// `services::warm_pool::recheck_after_claim` can drop a row against an
/// in-memory test connection without taking the global DB mutex. The
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
    delete_orphaned_claimed_warm_worktrees_inner(&db, &live_session_ids)
}

pub(crate) fn delete_orphaned_claimed_warm_worktrees_inner(
    conn: &Connection,
    live_session_ids: &[i64],
) -> SqlResult<usize> {
    // Read the full set of claimed rows up front, then drop the prepared
    // statement so the loop below doesn't keep a statement handle alive
    // across the `remove_one_worktree` shell-out (which can take seconds
    // on a slow disk and would otherwise hold an `&mut Connection`
    // borrow that blocks the agent_node lookup we issue against the same
    // connection).
    let mut stmt = conn.prepare(
        "SELECT id, mesh_id, path FROM warm_worktrees WHERE status = 'claimed'",
    )?;
    let claimed: Vec<(i64, i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    if claimed.is_empty() {
        return Ok(0);
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
                "delete_orphaned_claimed_warm_worktrees_inner: could not resolve \
                 live_mesh_ids ({} live session ids in snapshot); falling back to \
                 row-only behaviour (no filesystem teardown) to be safe",
                live_session_ids.len()
            );
            None
        }
    };

    let mut deleted = 0;
    for (id, mesh_id, path) in claimed {
        let treat_as_orphan = match &live_mesh_ids {
            // Couldn't query live sessions → be conservative. Even though
            // there almost certainly is no live agent (startup reconcile
            // runs before user-facing spawn), a transient DB error is not
            // an excuse to destroy a working tree. Drop the row but leave
            // the dir intact — the next reconcile (with a recovered DB)
            // can retry the teardown.
            None => false,
            Some(live) => !live.contains(&mesh_id),
        };
        if treat_as_orphan {
            // Branch 2: crashed-spawn orphan. Best-effort: a partial
            // git-worktree state is acceptable to leak if the call fails
            // (the next reconcile pass will retry — see #610 / #642.3
            // for the same "best-effort" pattern). We never block the
            // row drop on a filesystem failure: a stuck orphan row is
            // strictly less harmful than not dropping it.
            //
            // Two-step teardown: `remove_one_worktree` is the polite
            // path that clears `git worktree remove --force` metadata;
            // if it returns Err (test fixture was a plain tempdir, OR
            // production hit a locked handle / non-worktree state), the
            // `remove_dir_all` fallback guarantees the on-disk leak
            // actually closes. Order matters — `remove_one_worktree`
            // first so a real git worktree loses its bookkeeping before
            // the dir goes (which is what unblocks a future
            // `git worktree add` for the same slug).
            if crate::git::worktree::remove_one_worktree(&path).is_err()
                && std::path::Path::new(&path).exists()
            {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
        // Branch 1 (live session on this mesh) and Branch 3 (no dir on
        // disk) both fall through to a row-only delete. Same for the
        // conservative-fallback `treat_as_orphan = false` case.
        conn.execute(
            "DELETE FROM warm_worktrees WHERE id = ?1",
            params![id],
        )?;
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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

/// True iff `path` corresponds to a row in `warm_worktrees`. The prune
/// pipeline queries this per worktree so the Worktree Manager tab can
/// badge pool entries and `delete_worktrees` can reject them. Cheap
/// (indexed on `path UNIQUE`) and side-effect free — safe to call from
/// `collect_prune_info` for every worktree.
pub fn is_warm_pool_path(path: &str) -> SqlResult<bool> {
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
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
    let db = get().lock().unwrap();
    list_worktree_enabled_meshes_for_warm_inner(&db)
}

pub(crate) fn list_worktree_enabled_meshes_for_warm_inner(
    conn: &Connection,
) -> SqlResult<Vec<WarmPoolMeshRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, base_ref, pre_spawn_pool_size FROM meshes WHERE use_worktree = 1",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(WarmPoolMeshRow {
            id: row.get(0)?,
            path: row.get(1)?,
            base_ref: row.get(2)?,
            pre_spawn_pool_size: row.get(3)?,
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

