//! Schema-evolution registry — the single source of truth for "what does
//! the current schema look like, and how do we get there?" (issue #249).
//!
//! ## The bug class this exists to fix
//!
//! Before this module, schema evolution lived in **three overlapping
//! mechanisms** that every column change had to touch in sync:
//!
//! 1. A version-gated migration in `migrate_if_needed` (line numbers
//!    that drifted as the file grew).
//! 2. A `pub(crate) fn ensure_<table>_<column>` safety net called from
//!    `init()` after `migrate_if_needed` — these were the safety nets
//!    for DBs whose `schema_version` was bumped past N without the
//!    column being added.
//! 3. Inline `COALESCE(col, ...)` defaults in `MESH_COLUMNS` so the read
//!    projection didn't error on a briefly-missing column.
//!
//! The comment at `db/mod.rs:101-106` (pre-#249) explicitly warned future
//! developers not to "fix" the `projects` table guard, because the inner
//! `migrate_projects_*` helpers still referenced the renamed-away
//! `projects` table and would crash on a v6+ schema. The safety nets
//! themselves were historically the cause of the migration bugs they
//! were meant to fix — a column added in the version-gated migration
//! but missing from the safety net would silently skip a vN-1 → vN
//! upgrade's column add (the `test_v8_to_v9_adds_source_issue_via_safety_net`
//! regression is the canonical pin).
//!
//! ## What this module owns
//!
//! [`evolve_to`] is the **single public entry point** for schema
//! evolution. It owns:
//!
//! - the version-by-version migration steps (column adds, one-shot
//!   backfills),
//! - the read-side `COALESCE` defaults (via [`ColumnSpec::read_default`]
//!   and [`mesh_columns_projection`]),
//! - the `schema_version` probe and the post-migration bump,
//! - the post-migration verification (always-run idempotent safety nets).
//!
//! A new column becomes "add a [`ColumnSpec`] entry to
//! [`all_column_specs`] and you're done" — one place, not three.
//!
//! ## The shape of `evolve_to`
//!
//! The runner does two passes:
//!
//! 1. **Version-gated pass** — runs only if `current_version <
//!    target_version`. Walks [`all_column_specs`] in order and
//!    `ALTER`s every column whose `version > current_version`. Runs any
//!    [`OneShotBackfill`] entries whose `version > current_version`.
//!    Bumps `schema_version` to `target_version` on success.
//! 2. **Always-run pass** — re-runs the entire column list (the
//!    `pragma_table_info` check makes the loop a no-op on present
//!    columns; the bug-class regression lives here), then runs every
//!    [`AlwaysStep`] (idempotent data migrations + DROP IF EXISTS).
//!
//! The two passes share the column registry, so the bug class
//! "migration adds the column, safety net doesn't, vN-1 → vN upgrade
//! silently skips" is structurally impossible: the migration step IS
//! the safety-net step.
//!
//! ## Pre-v6 dead code
//!
//! The pre-v6 `migrate_projects_*` helpers and the `projects` table
//! guard in the old `migrate_if_needed` referenced tables that have
//! not existed since v6 (six years and 26 versions ago). They were
//! kept alive solely by tests simulating v2 → current upgrades —
//! tests that pinned a hypothetical path no production DB can ever
//! exercise. They are now deleted. The bug class they guarded against
//! (v6+ DBs bypassing the migration gate) is closed structurally by
//! the always-run pass above; the regression pin is
//! `test_v8_to_v9_adds_source_issue_via_safety_net` and the new
//! `evolve_to_handles_v6_to_current` test.

use rusqlite::{Connection, Result as SqlResult, params};

/// The schema version this build expects. Bumped per PR whenever a new
/// migration entry is added to [`all_column_specs`].
///
/// v33 — Per-Mesh harness overrides (issue #1151 / slice 2 of #1148):
/// adds the `meshes.harness_overrides` JSON column (sparse map keyed by
/// stable harness id → [`crate::preferences::HarnessConfigValue`]) and
/// the one-shot backfill that migrates non-empty legacy `meshes.model`
/// / `meshes.effort` values into a `claude` (Claude Code) override entry.
/// The legacy columns remain physically present for positional / upgrade
/// compatibility but are no longer read as active configuration after
/// the spawn resolver wires the new `mesh_override` layer (issue #1148
/// cascade order: explicit > mesh override > application > native).
///
/// v34 — Autopilot Circuits ledger (spec #1205 / walking skeleton #1206):
/// three new tables (`autopilot_circuits`, `autopilot_circuit_runs`,
/// `autopilot_circuit_run_steps`). Whole tables need no column registry
/// entries — they follow the warm_worktrees precedent: inline
/// `CREATE TABLE IF NOT EXISTS` in `init()` for fresh DBs plus an
/// [`AlwaysStep`] safety net for DBs whose version was bumped past 34 by
/// a build that didn't yet contain the inline CREATE.
pub(crate) const SCHEMA_VERSION: u32 = 34;

// ---------------------------------------------------------------------------
// ColumnSpec — one column the runner knows how to add and read back.
// ---------------------------------------------------------------------------

/// A column the runner adds (write side) and reads back (read side).
///
/// The `type_with_default` is the **ALTER-time** default — what SQLite
/// applied when the column was first introduced. If a later version
/// changed the default (e.g. `pre_spawn_pool_size`: v22 ALTER said
/// `DEFAULT 0`, v24 inline CREATE said `DEFAULT 1`), the ALTER-time
/// default is preserved here for the safety-net ALTER, and the v24
/// change is captured by the [`ONE_SHOT_BACKFILLS`] v24 entry + the
/// fresh-DB inline CREATE in `init()`. This split keeps the migration
/// history inspectable: each column's introduction point shows the
/// default that was live at the time.
///
/// The `read_default` is the `COALESCE(col, default)` the read
/// projection uses when the column is briefly missing — a pre-version
/// DB whose safety net hasn't yet run on the read path, or a unit
/// test fixture that constructs an older schema in-memory. The
/// COALESCE shields the read path from a missing-column error; the
/// safety-net ALTER makes the COALESCE a no-op on the next read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ColumnSpec {
    /// Schema version that introduced this column. Used to gate the
    /// ALTER in the version-gated pass and to pin the regression test.
    pub version: u32,
    /// Target table (`meshes`, `agent_nodes`, `autopilot_runs`, etc.).
    pub table: &'static str,
    /// Column name.
    pub column: &'static str,
    /// Full `ADD COLUMN` type clause, e.g. `INTEGER NOT NULL DEFAULT 0`.
    /// See the struct-level doc above for the v24 default-flip pattern.
    pub type_with_default: &'static str,
    /// Read-side default (the COALESCE in `MESH_COLUMNS`). `Nullable`
    /// means the column is genuinely nullable and NULL is the right
    /// read shape — no COALESCE needed.
    pub read_default: ReadDefault,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ReadDefault {
    /// No COALESCE; column is nullable, NULL is the right read shape
    /// (used for the few genuinely-nullable TEXT columns like
    /// `color`, `model`, `effort` whose read-side `None` is meaningful
    /// rather than a missing-column fallback).
    Nullable,
    /// `COALESCE(col, N)`. Used for INTEGER columns whose read path
    /// treats NULL and the sentinel default the same way.
    CoalesceInt(i64),
    /// `COALESCE(col, 'literal')`. Used for TEXT columns the read path
    /// normalizes via `parse_str` (`''` → `None`), so an empty default
    /// is indistinguishable from the "column missing" case at the
    /// consumer boundary.
    CoalesceText(&'static str),
}

// ---------------------------------------------------------------------------
// One-shot backfills + always-run data migrations.
// ---------------------------------------------------------------------------

/// A backfill that runs at most once per DB, gated on an
/// `app_settings` flag written **after** the SQL commits. The runner
/// writes the flag only after `execute()` returns `Ok`, so a crash
/// between the SQL and the flag write retries the SQL on next launch —
/// crash-safe per the `pool_default_backfill_v24` precedent.
///
/// `params` is bound if non-empty. Currently no entry uses it (every
/// one-shot is a single-statement UPDATE), but the binding path is in
/// place for future entries.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OneShotBackfill {
    pub version: u32,
    /// `app_settings` key written on success. Use a version-suffixed
    /// name so a future migration that wants to re-run with different
    /// logic doesn't collide with the original flag.
    pub flag: &'static str,
    /// Bound parameters for the SQL statement (currently always empty;
    /// see struct doc).
    pub params: &'static [&'static str],
    /// The SQL statement. Single statement — `execute` (not
    /// `execute_batch`) so a future migration that wants multiple
    /// statements can switch to `execute_batch` without a code change
    /// here.
    pub sql: &'static str,
}

/// Idempotent step the runner re-runs on every init. The migration
/// path runs these on first upgrade past their owning version; the
/// safety-net pass runs them every launch. Each variant maps to a
/// pure function — no state, no race window.
#[derive(Debug, Clone, Copy)]
pub(crate) enum AlwaysStep {
    /// Re-apply the v19 Spawn Option composite-id rewrite (issue #575,
    /// first-class block). Idempotent (`WHERE provider NOT LIKE '%:%'`).
    RewriteAgentNodeProviderId,
    /// DROP TABLE IF EXISTS checkpoints (v12). The checkpoint feature
    /// was removed; the table is dead and must not linger.
    DropCheckpoints,
    /// Rehash pre-hashing cleartext coordinator tokens (issue #495).
    /// Idempotent: a SHA-256 hex is 64 chars, a raw token is 32, so
    /// the length distinguishes the two.
    HashCoordinatorTokens,
    /// CREATE TABLE IF NOT EXISTS warm_worktrees (v21). The table
    /// backs the pre-spawn Worktree Pool (issue #609). Fresh DBs
    /// create it via the inline CREATE in `init()`; this safety net
    /// covers v6+ DBs whose `schema_version` was bumped past 21 by a
    /// build that didn't yet include the inline CREATE. Idempotent
    /// via `IF NOT EXISTS`.
    EnsureWarmWorktreesTable,
    /// CREATE TABLE IF NOT EXISTS for the three Autopilot Circuits
    /// ledger tables (v34, spec #1205). Same rationale as
    /// [`AlwaysStep::EnsureWarmWorktreesTable`]: fresh DBs get them from
    /// the inline CREATE in `init()`; this safety net covers any DB that
    /// reached v34+ without them. Idempotent via `IF NOT EXISTS`.
    EnsureAutopilotCircuitsTables,
}

// ---------------------------------------------------------------------------
// The registry.
// ---------------------------------------------------------------------------

/// Every column the schema ever grew, in projection-stable order.
///
/// **Iteration order pins the projection order for `meshes`** — see
/// [`mesh_columns_projection`] and `map_mesh_row` in `db/mod.rs`.
/// Reordering this slice reshuffles the positional `row.get(N)` reads
/// in `map_mesh_row`. Append-only on the right is the safe edit shape
/// (the in-test `column_specs_*` regressions pin this).
///
/// Each entry is grouped by table (meshes, agent_nodes,
/// autopilot_runs, coordinator_drive_prompts) and then by version
/// ascending within the table — so a single `WHERE version > ?` scan
/// on a version-gated upgrade hits the entries in the order SQLite
/// wants them (and never ALTERs a column before the table it depends
/// on is created).
pub(crate) fn all_column_specs() -> &'static [ColumnSpec] {
    SPECS
}

const SPECS: &[ColumnSpec] = &[
    // ============================================================
    // meshes
    // ============================================================
    // Initial CREATE columns (version 1 = always present on a fresh DB).
    // Kept here so the registry's mesh subset is the source of truth
    // for MESH_COLUMNS' order. The runner skips these on the
    // version-gated pass (any vN+1+ already has them) but they still
    // participate in the always-run pass, where the pragma check
    // makes them no-ops.
    ColumnSpec { version: 1, table: "meshes", column: "id", type_with_default: "INTEGER PRIMARY KEY AUTOINCREMENT", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "meshes", column: "name", type_with_default: "TEXT NOT NULL", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "meshes", column: "path", type_with_default: "TEXT NOT NULL UNIQUE", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "meshes", column: "layout", type_with_default: "TEXT NOT NULL DEFAULT 'grid'", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "meshes", column: "position", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "meshes", column: "created_at", type_with_default: "TEXT NOT NULL DEFAULT (datetime('now'))", read_default: ReadDefault::Nullable },
    // v8 — user-tunable columns (issue #456).
    ColumnSpec { version: 8, table: "meshes", column: "build_command", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 8, table: "meshes", column: "run_command", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 8, table: "meshes", column: "model", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 8, table: "meshes", column: "effort", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 8, table: "meshes", column: "use_worktree", type_with_default: "INTEGER NOT NULL DEFAULT 1", read_default: ReadDefault::CoalesceInt(1) },
    ColumnSpec { version: 8, table: "meshes", column: "worktree_mode", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 8, table: "meshes", column: "default_provider", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 8, table: "meshes", column: "base_ref", type_with_default: "TEXT NOT NULL DEFAULT 'origin/main'", read_default: ReadDefault::CoalesceText("origin/main") },
    // v17 — Scratch Pad (issue #516). NOT NULL + DEFAULT '' so the
    // no-notes case is indistinguishable from the already-has-notes
    // case at the read boundary.
    ColumnSpec { version: 17, table: "meshes", column: "scratchpad", type_with_default: "TEXT NOT NULL DEFAULT ''", read_default: ReadDefault::Nullable },
    // v18 — OS-level sandbox toggle (#497/#498). Off by default (0).
    ColumnSpec { version: 18, table: "meshes", column: "sandbox", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::CoalesceInt(0) },
    // v22 — pre-spawn pool target (issue #611). ALTER-time default
    // is 0 (feature off); v24 changed the inline CREATE default to 1
    // (ADR 0020, pool on by default). The flip is captured by the
    // ONE_SHOT_BACKFILLS v24 entry on the upgrade path; pre-v22 reads
    // via COALESCE(col, 0) — the ALTER-time default — which is the
    // correct "feature off" semantics for those DBs.
    ColumnSpec { version: 22, table: "meshes", column: "pre_spawn_pool_size", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::CoalesceInt(0) },
    // v25 — per-mesh accent colour (user-picked hex). Nullable.
    ColumnSpec { version: 25, table: "meshes", column: "color", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    // v26 — Autopilot Policy (issue #481).
    ColumnSpec { version: 26, table: "meshes", column: "autopilot_enabled", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::CoalesceInt(0) },
    ColumnSpec { version: 26, table: "meshes", column: "autopilot_trigger_label", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 26, table: "meshes", column: "autopilot_concurrency_limit", type_with_default: "INTEGER NOT NULL DEFAULT 2", read_default: ReadDefault::CoalesceInt(2) },
    ColumnSpec { version: 26, table: "meshes", column: "autopilot_provider", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 26, table: "meshes", column: "autopilot_action_on_success", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    // v27 — per-context build/run commands (issue #802).
    ColumnSpec { version: 27, table: "meshes", column: "root_build_command", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 27, table: "meshes", column: "root_run_command", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    // v30 — Looping Autopilot config (wayfinder #990 / ticket #991).
    // autopilot_mode default 'issue_driven' matches the v29 behaviour
    // byte-for-byte so the upgrade is invisible to the existing
    // autopilot flow.
    ColumnSpec { version: 30, table: "meshes", column: "autopilot_mode", type_with_default: "TEXT NOT NULL DEFAULT 'issue_driven'", read_default: ReadDefault::CoalesceText("issue_driven") },
    ColumnSpec { version: 30, table: "meshes", column: "loop_initial_prompt", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 30, table: "meshes", column: "loop_suffix_prompt", type_with_default: "TEXT", read_default: ReadDefault::CoalesceText("") },
    ColumnSpec { version: 30, table: "meshes", column: "loop_max_iterations", type_with_default: "INTEGER", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 30, table: "meshes", column: "loop_interval_seconds", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::CoalesceInt(0) },
    ColumnSpec { version: 30, table: "meshes", column: "loop_consecutive_failures", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::CoalesceInt(0) },
    // v33 — Per-Mesh harness overrides (issue #1151 / slice 2 of #1148).
    // NON-NULL with an empty-object default so the read path never has to
    // handle a NULL: every pre-v33 row reads back as `{}` (the migration
    // below also runs `UPDATE ... SET harness_overrides = '{}'` defensively,
    // but the COALESCE(.,'{}') shields the read path during the brief
    // window between ALTER-add and backfill). The JSON shape is a sparse
    // map keyed by stable harness id (e.g. `"claude"`, `"codex"`,
    // `"agy"`) → `{"model": "opus-4-1", "effort": "high"}`. Mirrors the
    // application-level `AppPreferences.harness_defaults` map (issue #1150)
    // and reuses the same `HarnessConfigValue` wire type. A missing key
    // means "inherit" (no override); an empty `{model: null, effort: null}`
    // value is unreachable because the CRUD command removes the entry
    // when the post-validation value is empty. The legacy `model` /
    // `effort` columns remain physically present for positional row
    // integrity but are no longer read by the spawn resolver.
    ColumnSpec { version: 33, table: "meshes", column: "harness_overrides", type_with_default: "TEXT NOT NULL DEFAULT '{}'", read_default: ReadDefault::CoalesceText("{}") },

    // ============================================================
    // agent_nodes
    // ============================================================
    // Initial CREATE columns. No COALESCE in the projection (these
    // are read via `Option<i64>`/`Option<String>` directly — the
    // column's nullable storage IS the read-default).
    ColumnSpec { version: 1, table: "agent_nodes", column: "id", type_with_default: "INTEGER PRIMARY KEY AUTOINCREMENT", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "mesh_id", type_with_default: "INTEGER NOT NULL REFERENCES meshes(id)", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "name", type_with_default: "TEXT NOT NULL", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "path", type_with_default: "TEXT NOT NULL", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "branch", type_with_default: "TEXT NOT NULL DEFAULT 'main'", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "env", type_with_default: "TEXT NOT NULL DEFAULT 'windows'", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "provider", type_with_default: "TEXT NOT NULL DEFAULT 'anthropic'", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "status", type_with_default: "TEXT NOT NULL DEFAULT 'idle'", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "cli_session_id", type_with_default: "TEXT", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "worktree_name", type_with_default: "TEXT", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "agent_nodes", column: "created_at", type_with_default: "TEXT NOT NULL DEFAULT (datetime('now'))", read_default: ReadDefault::Nullable },
    // v9 — issue spawn linkage.
    ColumnSpec { version: 9, table: "agent_nodes", column: "source_issue", type_with_default: "INTEGER", read_default: ReadDefault::Nullable },
    // v11 — worktree toggle.
    ColumnSpec { version: 11, table: "agent_nodes", column: "use_worktree", type_with_default: "INTEGER NOT NULL DEFAULT 1", read_default: ReadDefault::Nullable },
    // v13 — drag-to-reorder grid position.
    ColumnSpec { version: 13, table: "agent_nodes", column: "position", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::Nullable },
    // v14 — status-changed-at (ADR-0008 spine). Nullable so the
    // backfill (`UPDATE ... SET status_changed_at = created_at`) can
    // run on pre-v14 rows without a non-constant default (SQLite
    // can't ALTER-add a non-constant default).
    ColumnSpec { version: 14, table: "agent_nodes", column: "status_changed_at", type_with_default: "TEXT", read_default: ReadDefault::Nullable },
    // v15 — PR-spawn linkage (issue #420).
    ColumnSpec { version: 15, table: "agent_nodes", column: "source_pr", type_with_default: "INTEGER", read_default: ReadDefault::Nullable },
    // v16 — fork-PR metadata (issue #443) + pinned SHA (issue #444).
    ColumnSpec { version: 16, table: "agent_nodes", column: "head_repo_owner", type_with_default: "TEXT", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 16, table: "agent_nodes", column: "head_repo_clone_url", type_with_default: "TEXT", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 16, table: "agent_nodes", column: "source_pr_pinned_sha", type_with_default: "TEXT", read_default: ReadDefault::Nullable },
    // v29 — Pinned Grid view mode (wayfinder #982 / ticket #984).
    ColumnSpec { version: 29, table: "agent_nodes", column: "is_pinned", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::Nullable },

    // ============================================================
    // autopilot_runs
    // ============================================================
    ColumnSpec { version: 1, table: "autopilot_runs", column: "node_id", type_with_default: "INTEGER PRIMARY KEY REFERENCES agent_nodes(id) ON DELETE CASCADE", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "autopilot_runs", column: "mesh_id", type_with_default: "INTEGER NOT NULL", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "autopilot_runs", column: "issue_number", type_with_default: "INTEGER NOT NULL", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "autopilot_runs", column: "state", type_with_default: "TEXT NOT NULL DEFAULT 'implementing'", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "autopilot_runs", column: "attempts", type_with_default: "INTEGER NOT NULL DEFAULT 0", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "autopilot_runs", column: "pr_number", type_with_default: "INTEGER", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "autopilot_runs", column: "pr_url", type_with_default: "TEXT", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "autopilot_runs", column: "created_at", type_with_default: "TEXT NOT NULL DEFAULT (datetime('now'))", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "autopilot_runs", column: "updated_at", type_with_default: "TEXT NOT NULL DEFAULT (datetime('now'))", read_default: ReadDefault::Nullable },
    // v31 — Looping Autopilot iteration marker (ticket #992). NULL
    // for issue-driven runs (the pre-v31 default; preserves every
    // existing row).
    ColumnSpec { version: 31, table: "autopilot_runs", column: "loop_iteration", type_with_default: "INTEGER", read_default: ReadDefault::Nullable },

    // ============================================================
    // coordinator_drive_prompts
    // ============================================================
    ColumnSpec { version: 1, table: "coordinator_drive_prompts", column: "node_id", type_with_default: "INTEGER NOT NULL", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "coordinator_drive_prompts", column: "idempotency_key", type_with_default: "TEXT NOT NULL", read_default: ReadDefault::Nullable },
    // `verdict` was NOT NULL → DEFAULT '' in v32; that loosening does
    // not need a separate spec (no existing row violates a DEFAULT).
    ColumnSpec { version: 1, table: "coordinator_drive_prompts", column: "verdict", type_with_default: "TEXT NOT NULL DEFAULT ''", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 1, table: "coordinator_drive_prompts", column: "created_at", type_with_default: "TEXT NOT NULL DEFAULT (datetime('now'))", read_default: ReadDefault::Nullable },
    // v32 — Coordinator drive idempotency hardening (issue #750).
    ColumnSpec { version: 32, table: "coordinator_drive_prompts", column: "status", type_with_default: "TEXT NOT NULL DEFAULT 'pending'", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 32, table: "coordinator_drive_prompts", column: "claimed_at", type_with_default: "TEXT NOT NULL DEFAULT (datetime('now'))", read_default: ReadDefault::Nullable },
    ColumnSpec { version: 32, table: "coordinator_drive_prompts", column: "prompt_hash", type_with_default: "TEXT NOT NULL DEFAULT ''", read_default: ReadDefault::Nullable },
];

/// One-shot backfills the runner runs in the version-gated pass.
/// Crash-safe: the `flag` is written to `app_settings` only AFTER
/// the SQL commits, so a crash between the SQL and the flag write
/// retries next launch.
const ONE_SHOT_BACKFILLS: &[OneShotBackfill] = &[
    // v13 — per-mesh position backfill. The v13 column add sets
    // every pre-v13 row's `position` to 0 (the column DEFAULT), which
    // would clobber the order users already had via `created_at ASC`
    // (lists previously sorted purely by creation time). This
    // backfill reassigns each row's `position` to its 0-based rank
    // by `created_at` WITHIN its own mesh, so a pre-v13 mesh keeps
    // the order it already rendered in. Ties broken by `id` for
    // determinism. Gated on its own `app_settings` flag — runs once
    // per DB; re-running would re-rank nodes a user may have
    // intentionally re-ordered via drag-to-reorder (issue #65).
    OneShotBackfill {
        version: 13,
        flag: "agent_node_position_backfill_v13",
        params: &[],
        sql: "UPDATE agent_nodes SET position = (SELECT COUNT(*) FROM agent_nodes AS earlier WHERE earlier.mesh_id = agent_nodes.mesh_id AND (earlier.created_at < agent_nodes.created_at OR (earlier.created_at = agent_nodes.created_at AND earlier.id < agent_nodes.id)))",
    },
    // v14 — `status_changed_at` backfill. The v14 column add leaves
    // pre-v14 rows with NULL `status_changed_at`; SQLite forbids a
    // non-constant default on `ALTER TABLE ADD COLUMN`, hence the
    // add-then-backfill two-step. The coordinator digest reads
    // `status_changed_at` for `last_activity`; a NULL there renders
    // as "never active", which is wrong for a pre-v14 row that's
    // been doing real work. Backfill from `created_at` so a pre-v14
    // node reports a sane (if coarse) activity time.
    OneShotBackfill {
        version: 14,
        flag: "agent_node_status_changed_at_backfill_v14",
        params: &[],
        sql: "UPDATE agent_nodes SET status_changed_at = created_at \
              WHERE status_changed_at IS NULL",
    },
    // v24 — flip worktree-enabled meshes from the v22 ALTER-time
    // default (`pre_spawn_pool_size = 0`) to the new ADR 0020 default
    // (1). Gated on its own `app_settings` flag (NOT the
    // `schema_version` bump) so the flip survives a crash mid-init and
    // is never re-applied once the user has explicitly set the value
    // back to 0. Mirrors the pre-#249 `ensure_pool_default_backfill`
    // precedent.
    OneShotBackfill {
        version: 24,
        flag: "pool_default_backfill_v24",
        params: &[],
        sql: "UPDATE meshes \
              SET pre_spawn_pool_size = 1 \
              WHERE COALESCE(pre_spawn_pool_size, 0) = 0 \
                AND COALESCE(use_worktree, 1) = 1",
    },
    // v33 — Per-Mesh harness overrides legacy migration (issue #1151 /
    // slice 2 of #1148 / acceptance criteria 22-23). One-shot per DB:
    // copies non-empty legacy `meshes.model` / `meshes.effort` values into
    // a `claude` (Claude Code) entry in the new `harness_overrides` JSON
    // map. The cascade after this migration lands is:
    //   explicit > mesh override > application default > native
    // so a non-empty legacy Mesh setting translates to a sparse Claude
    // Code override today — equivalent behaviour for the only harness
    // that supported model/effort at the v32 cut-off (Claude Code, Codex
    // got `-c model_reasoning_effort` later).
    //
    // Skip rules (each pinned by acceptance criteria 22-23):
    //   * Both legacy values empty / whitespace-only → no override created.
    //     The user's "no preferences" mesh stays a "no exclusions" mesh.
    //   * Existing non-empty `claude` override entry → do NOT overwrite.
    //     A power user may have hand-edited the JSON column before the
    //     migration ran. The `_idempotent_guard` predicate
    //     `json_extract(harness_overrides, '$.claude') IS NULL` covers
    //     both a missing key and an explicit `null` value.
    //
    // Idempotent: gated on a `mesh_harness_overrides_migrated_v33`
    // `app_settings` flag (NOT the `schema_version` bump) so the SQL
    // re-runs iff a prior attempt crashed before the flag was written
    // (crash-safe per the `pool_default_backfill_v24` precedent). The
    // flag is written in `run_one_shot` AFTER the SQL commits, so the
    // JSON parsing failure surfaces as a real error rather than a
    // silent skip — the JSON column is set above by the column-add
    // pass to a non-NULL `{}`, so `json_extract` always succeeds.
    //
    // The legacy Mesh.settings.json mirror is intentionally NOT touched —
    // the schema never mirrored model/effort there, so there is no
    // filesystem side-effect to roll back.
    OneShotBackfill {
        version: 33,
        flag: "mesh_harness_overrides_migrated_v33",
        params: &[],
        // The body is exported as `V33_BACKFILL_SQL` so the test
        // fixtures in `db::harness_overrides_tests` can call the same
        // SQL verbatim through a hand-rolled v32 schema — keeping the
        // test pins tight against accidental column-reorder / predicate
        // edits without copy-pasting the body across five tests.
        sql: V33_BACKFILL_SQL,
    },
];

/// The v33 one-shot backfill SQL. Mirrored as the `OneShotBackfill`
/// entry above so the test fixtures can drive the same statement verbatim
/// against hand-rolled v32 schemas (issue #1151 acceptance criteria 23-25).
pub(crate) const V33_BACKFILL_SQL: &str = "UPDATE meshes \
         SET harness_overrides = json_patch( \
             COALESCE(harness_overrides, '{}'), \
             json_object( \
                 'claude', json_object( \
                     'model', CASE WHEN TRIM(COALESCE(model, '')) = '' \
                                      THEN NULL ELSE TRIM(model) END, \
                     'effort', CASE WHEN TRIM(COALESCE(effort, '')) = '' \
                                       THEN NULL ELSE TRIM(effort) END \
                 ) \
             ) \
         ) \
         WHERE (TRIM(COALESCE(model, '')) != '' \
                OR TRIM(COALESCE(effort, '')) != '') \
           AND json_extract(harness_overrides, '$.claude') IS NULL";

/// Always-run idempotent steps. See [`AlwaysStep`].
const ALWAYS_STEPS: &[AlwaysStep] = &[
    AlwaysStep::DropCheckpoints,
    AlwaysStep::EnsureWarmWorktreesTable,
    AlwaysStep::EnsureAutopilotCircuitsTables,
    AlwaysStep::RewriteAgentNodeProviderId,
    AlwaysStep::HashCoordinatorTokens,
];

// ---------------------------------------------------------------------------
// The runner: evolve_to.
// ---------------------------------------------------------------------------

/// Probe `schema_version` from `app_settings`. Returns `0` for a fresh
/// DB (no row) or a malformed value (`parse` failure → `0` so the
/// runner falls into the "upgrade everything" path, which is safe —
/// every ALTER has the `pragma_table_info` skip).
///
/// Self-sufficient: ensures `app_settings` exists before reading
/// (test fixtures and unit tests pass in an in-memory connection
/// without the production `init()` preamble). Production callers
/// still create `app_settings` first in `init()` for the documented
/// order, but a missing table here is no longer an error — we treat
/// it as `schema_version = 0` and let the runner do its work.
pub(crate) fn current_version(conn: &Connection) -> i32 {
    // Ensure the table exists; no-op if it already does.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
         );",
    )
    .ok();
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0).map(|v| v.parse().unwrap_or(0)),
    )
    .unwrap_or(0)
}

fn bump_version(conn: &Connection, target: u32) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
        params![target.to_string()],
    )?;
    Ok(())
}

/// The single public entry point for schema evolution (issue #249).
///
/// Owns:
/// - the version-by-version migration steps (column adds, one-shot
///   backfills),
/// - the read-side `COALESCE` defaults (via [`mesh_columns_projection`]),
/// - the `schema_version` probe + bump,
/// - the post-migration verification (always-run idempotent safety nets).
///
/// A new column becomes "add a [`ColumnSpec`] entry to
/// [`all_column_specs`]" — one place, not three.
pub(crate) fn evolve_to(target_version: u32, conn: &Connection) -> SqlResult<()> {
    let v = current_version(conn);
    if v < target_version as i32 {
        tracing::info!(
            "evolve_to: migrating database from version {} to {}",
            v,
            target_version
        );

        // Version-gated pass — columns FIRST, then backfills.
        //
        // We add ALL columns whose version is > v (idempotent — the
        // pragma_table_info skip makes the `version > v` gate
        // unnecessary, but it's preserved for ordering and to make
        // the trace log explicit). The critical detail is that the
        // full column walk runs BEFORE the backfills: a backfill that
        // references a column from an earlier version (e.g. the v24
        // `pool_default_backfill_v24` references the v22
        // `pre_spawn_pool_size` column) needs the column in place.
        // A naive `if (col.version as i32) > v` gate would skip the
        // v22 column when upgrading from v23 — the backfill then
        // trips `no such column`.
        //
        // The always-run pass below re-walks the registry. With the
        // version-gated pass having added everything it knows about,
        // the always-run pass is a no-op for healthy DBs and a
        // catch-up for DBs that bypassed the version gate (the
        // v8→v9 `source_issue` bug class — see
        // `db::tests::test_v8_to_v9_adds_source_issue_via_safety_net`).
        for col in all_column_specs() {
            add_column_if_missing(conn, col)?;
        }
        // Version-gated pass — one-shot backfills.
        for backfill in ONE_SHOT_BACKFILLS {
            if (backfill.version as i32) > v {
                run_one_shot(conn, backfill)?;
            }
        }
        bump_version(conn, target_version)?;
    }

    // Always-run pass — column adds (idempotent safety net). Re-runs
    // the entire registry: the `pragma_table_info` check makes the
    // loop a no-op on already-present columns, but it WILL pick up
    // any column a build skipped when it bumped `schema_version` past
    // N without yet containing the inline ALTER.
    for col in all_column_specs() {
        add_column_if_missing(conn, col)?;
    }
    // Always-run pass — idempotent data migrations.
    for step in ALWAYS_STEPS {
        run_always(conn, *step)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Step implementations.
// ---------------------------------------------------------------------------

fn add_column_if_missing(conn: &Connection, col: &ColumnSpec) -> SqlResult<()> {
    if !table_present(conn, col.table)? {
        return Ok(());
    }
    if column_present(conn, col.table, col.column)? {
        return Ok(());
    }
    let sql = format!(
        "ALTER TABLE {} ADD COLUMN {} {}",
        col.table, col.column, col.type_with_default
    );
    conn.execute(&sql, [])?;
    tracing::warn!(
        "evolve_to: added missing column {}.{} (version {})",
        col.table,
        col.column,
        col.version
    );
    Ok(())
}

fn run_one_shot(conn: &Connection, backfill: &OneShotBackfill) -> SqlResult<()> {
    let done: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM app_settings WHERE key = ?1",
            params![backfill.flag],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);
    if done {
        return Ok(());
    }
    // Table-exists guard mirrors the column registry's `add_column_if_missing`:
    // a backfill that targets a not-yet-created table is a no-op
    // (the column-walk pass will have added any missing columns by
    // the time the backfill runs, but a v0-DB that lacks the table
    // entirely is outside this backfill's purview — it belongs to
    // a separate `CREATE TABLE IF NOT EXISTS` step in the inline
    // `init()` or the AlwaysStep registry).
    //
    // Heuristic: inspect the SQL for the first `meshes` / `agent_nodes`
    // / `autopilot_runs` / `coordinator_drive_prompts` / `app_settings`
    // reference and bail if the table doesn't exist. Simpler and
    // more robust than parsing: extract the first `FROM <name>` or
    // `INTO <name>` or `UPDATE <name>` token. Every existing backfill
    // targets exactly one table, so the heuristic is sufficient.
    if let Some(table) = backfill_target_table(backfill.sql) {
        if !table_present(conn, table)? {
            tracing::info!(
                "evolve_to: skipping backfill (v{}) — table '{}' not present",
                backfill.version,
                table
            );
            // Mark the flag as done so we don't retry the heuristic
            // on every launch.
            conn.execute(
                "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, '1')",
                params![backfill.flag],
            )?;
            return Ok(());
        }
    }
    let updated = if backfill.params.is_empty() {
        conn.execute(backfill.sql, [])?
    } else {
        // Currently unused — every one-shot takes no params. The
        // path is here so a future migration that needs a bound
        // value can opt in by populating `OneShotBackfill::params`.
        // A future implementation will need to handle the
        // blanket-impl coercion `&str` → `&dyn ToSql` correctly —
        // left for the migration that needs it.
        return Err(rusqlite::Error::InvalidQuery);
    };
    if updated > 0 {
        tracing::info!(
            "evolve_to: one-shot backfill (v{}) updated {} row(s)",
            backfill.version,
            updated
        );
    }
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, '1')",
        params![backfill.flag],
    )?;
    Ok(())
}

fn run_always(conn: &Connection, step: AlwaysStep) -> SqlResult<()> {
    match step {
        AlwaysStep::DropCheckpoints => {
            // v12 — checkpoint feature was removed; the table is dead.
            // `DROP TABLE IF EXISTS` is fully idempotent. The runner
            // calls this every launch so a v6+ DB whose schema_version
            // was bumped past 12 without the inline DROP eventually
            // loses the table too.
            conn.execute("DROP TABLE IF EXISTS checkpoints", [])?;
        }
        AlwaysStep::RewriteAgentNodeProviderId => {
            // v19 — Spawn Option composite-id rewrite (issue #575).
            // First-class block: hardcoded SQL for the one provider
            // still needing the rewrite (`minimax`). The custom-
            // account block needs the live `Vec<ProviderAccount>` and
            // is called separately from `lib.rs::setup` (see
            // `db::ensure_agent_node_provider_id_custom_accounts_migrated`).
            // Idempotent — the `WHERE provider NOT LIKE '%:%'` guard
            // skips already-migrated rows.
            let table_exists = table_present(conn, "agent_nodes")?;
            if table_exists {
                let rows = conn.execute(
                    "UPDATE agent_nodes \
                        SET provider = 'claude:' || provider \
                      WHERE provider = 'minimax'",
                    [],
                )?;
                if rows > 0 {
                    tracing::info!(
                        "evolve_to: rewrote {} agent_nodes from minimax bare id (v19)",
                        rows
                    );
                }
            }
        }
        AlwaysStep::HashCoordinatorTokens => {
            // Issue #495 — rehash pre-hashing cleartext coordinator
            // tokens. The raw token the user already holds keeps
            // validating because the validator hashes incoming tokens
            // and compares against the stored hash. Idempotent: a
            // SHA-256 hex is 64 chars, a raw token is 32.
            for key in [
                crate::db::COORDINATOR_READ_TOKEN_KEY,
                crate::db::COORDINATOR_DRIVE_TOKEN_KEY,
            ] {
                let value: Option<String> = conn
                    .query_row(
                        "SELECT value FROM app_settings WHERE key = ?1",
                        params![key],
                        |row| row.get(0),
                    )
                    .ok();
                if let Some(raw) = value {
                    if raw.len() == 32 {
                        conn.execute(
                            "UPDATE app_settings SET value = ?2 WHERE key = ?1",
                            params![key, crate::db::hash_token(&raw)],
                        )?;
                        tracing::warn!("evolve_to: rehashed cleartext {}", key);
                    }
                }
            }
        }
        AlwaysStep::EnsureWarmWorktreesTable => {
            // v21 — Pre-spawn Worktree Pool (issue #609). Fresh DBs
            // create the table via the inline CREATE in `init()`;
            // this safety net covers v6+ DBs whose `schema_version`
            // was bumped past 21 by a build that didn't yet include
            // the inline CREATE. `CREATE TABLE IF NOT EXISTS` is a
            // no-op on healthy v21+ DBs. The table-exists guard in
            // `add_column_if_missing` skips the pool's columns on a
            // pre-v21 DB until this step runs.
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS warm_worktrees (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    mesh_id INTEGER NOT NULL REFERENCES meshes(id) ON DELETE CASCADE,
                    path TEXT NOT NULL UNIQUE,
                    preassigned_name TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'filling',
                    base_sha TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE INDEX IF NOT EXISTS idx_warm_worktrees_mesh ON warm_worktrees(mesh_id);
                CREATE INDEX IF NOT EXISTS idx_warm_worktrees_status ON warm_worktrees(status);
                ",
            )?;
        }
        AlwaysStep::EnsureAutopilotCircuitsTables => {
            // v34 — Autopilot Circuits ledger (spec #1205). Mirrors the
            // inline CREATE in `db::init` verbatim; see that comment for
            // the column semantics.
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS autopilot_circuits (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    mesh_id INTEGER NOT NULL REFERENCES meshes(id) ON DELETE CASCADE,
                    name TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '',
                    enabled INTEGER NOT NULL DEFAULT 1,
                    concurrency_limit INTEGER NOT NULL DEFAULT 1,
                    graph_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE INDEX IF NOT EXISTS idx_autopilot_circuits_mesh ON autopilot_circuits(mesh_id);

                CREATE TABLE IF NOT EXISTS autopilot_circuit_runs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    circuit_id INTEGER NOT NULL REFERENCES autopilot_circuits(id) ON DELETE CASCADE,
                    mesh_id INTEGER NOT NULL,
                    trigger_identity TEXT NOT NULL DEFAULT '',
                    state TEXT NOT NULL DEFAULT 'pending',
                    context_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE INDEX IF NOT EXISTS idx_autopilot_circuit_runs_circuit ON autopilot_circuit_runs(circuit_id);
                CREATE INDEX IF NOT EXISTS idx_autopilot_circuit_runs_state ON autopilot_circuit_runs(state);

                CREATE TABLE IF NOT EXISTS autopilot_circuit_run_steps (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id INTEGER NOT NULL REFERENCES autopilot_circuit_runs(id) ON DELETE CASCADE,
                    node_id TEXT NOT NULL,
                    agent_node_id INTEGER,
                    status TEXT NOT NULL DEFAULT 'pending_slot',
                    attempt INTEGER NOT NULL DEFAULT 0,
                    outcome TEXT,
                    error_message TEXT,
                    started_at TEXT,
                    completed_at TEXT,
                    UNIQUE (run_id, node_id)
                );
                CREATE INDEX IF NOT EXISTS idx_circuit_steps_run ON autopilot_circuit_run_steps(run_id);
                ",
            )?;
        }
    }
    Ok(())
}

fn column_present(conn: &Connection, table: &str, column: &str) -> SqlResult<bool> {
    conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info(?1) WHERE name=?2",
        rusqlite::params![table, column],
        |row| row.get(0),
    )
}

/// Extract the first target table from a backfill SQL statement.
/// Heuristic: walk the SQL looking for the first `FROM <name>` /
/// `INTO <name>` / `UPDATE <name>` / `TABLE <name>` token (case-
/// insensitive) and return the identifier. Returns `None` for SQL
/// that doesn't target a single table — the caller falls through
/// and runs the backfill unconditionally.
///
/// Every entry in [`ONE_SHOT_BACKFILLS`] targets exactly one table
/// (the v24 backfill is `UPDATE meshes ...`), so this heuristic is
/// sufficient for the current registry. A future multi-table
/// backfill can opt out by structuring its SQL so the heuristic
/// returns `None`.
fn backfill_target_table(sql: &str) -> Option<&str> {
    // Cheap tokeniser: scan whitespace-separated words, ignoring
    // string literals (none in our SQL, but cheap defence) and
    // comments (none either). The candidate keyword is the token
    // BEFORE the identifier.
    let keywords = ["FROM", "INTO", "UPDATE", "TABLE"];
    let mut tokens = sql.split_whitespace().peekable();
    let mut prev: Option<&str> = None;
    while let Some(tok) = tokens.next() {
        // Strip leading punctuation like `(` or `,` from the token
        // (the SQL can have `INSERT INTO foo (...)` etc.).
        let bare = tok.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '_');
        let upper = bare.to_ascii_uppercase();
        if keywords.contains(&upper.as_str()) {
            // Next token is the table identifier (possibly
            // backtick-quoted, which we strip).
            if let Some(next) = tokens.next() {
                let cleaned = next
                    .trim_start_matches('`')
                    .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
                if !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        }
        prev = Some(tok);
    }
    let _ = prev;
    None
}

fn table_present(conn: &Connection, table: &str) -> SqlResult<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get::<_, i64>(0).map(|c| c > 0),
    )
}

// ---------------------------------------------------------------------------
// The read projection: MESH_COLUMNS built from the registry.
// ---------------------------------------------------------------------------

/// The mesh-only subset of [`all_column_specs`], preserving iteration
/// order (which pins the `map_mesh_row` positional reads — see
/// `db/mod.rs`'s `map_mesh_row` doc).
///
/// Filters at first call, then caches in a `OnceLock` so the cost is
/// paid once per process. The cache is process-lifetime, not
/// connection-lifetime — the registry is a `&'static` slice and
/// never changes at runtime.
pub(crate) fn mesh_column_specs() -> &'static [ColumnSpec] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<ColumnSpec>> = OnceLock::new();
    let cached = CACHE.get_or_init(|| {
        all_column_specs()
            .iter()
            .copied()
            .filter(|c| c.table == "meshes")
            .collect()
    });
    // Leaks the filtered Vec into a `&'static [ColumnSpec]`. The leak
    // is bounded (one Vec, process-lifetime, ~30 entries) and matches
    // the `lazy_static!` pattern. The Vec lives for the lifetime of
    // the process (OnceLock never drops its contents), so the &[T]
    // slice is valid forever.
    Box::leak(cached.clone().into_boxed_slice())
}

/// The `MESH_COLUMNS` read projection, built from the registry's
/// `read_default`. Replaces the pre-#249 hand-written
/// `const MESH_COLUMNS: &str = "id, name, ..., COALESCE(pre_spawn_pool_size, 0), ..."`.
///
/// The string is cached in a `OnceLock` so the projection is built
/// once per process — the cost is a `format!` loop over ~30 entries,
/// which is microseconds and well below the per-query overhead
/// `db::prepare` already pays.
pub(crate) fn mesh_columns_projection() -> &'static str {
    use std::sync::OnceLock;
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        let cols: Vec<String> = mesh_column_specs()
            .iter()
            .map(|c| match c.read_default {
                ReadDefault::Nullable => c.column.to_string(),
                ReadDefault::CoalesceInt(n) => format!("COALESCE({}, {})", c.column, n),
                ReadDefault::CoalesceText(s) => format!("COALESCE({}, '{}')", c.column, s),
            })
            .collect();
        cols.join(", ")
    })
}

#[cfg(test)]
mod tests {
    //! Regression pins for the registry itself. The interesting
    //! invariants are:
    //!
    //! - the registry's iteration order matches the projection order
    //!   (`map_mesh_row` reads positional columns in that exact order,
    //!   and a reorder would silently desync every read),
    //! - every column appears exactly once (a duplicate would error
    //!   on the second `ALTER TABLE ... ADD COLUMN` at runtime,
    //!   but only on a real upgrade — the unit-test path that uses
    //!   fresh in-memory connections would never hit it),
    //! - the schema version never decreases (the registry is
    //!   append-only; a regression pin for accidental version renumber).

    use super::*;
    use std::collections::{HashMap, HashSet};

    /// Schema versions monotonically non-decreasing across the slice.
    /// Per the registry doc, append-only is the safe edit shape — a
    /// reorder that pushed a v25 entry before a v22 entry would ALTER
    /// the columns in the wrong order on an upgrade.
    #[test]
    fn schema_versions_are_non_decreasing_within_each_table() {
        let mut last: HashMap<&'static str, u32> = HashMap::new();
        for col in all_column_specs() {
            let prev = last.get(col.table).copied().unwrap_or(0);
            assert!(
                col.version >= prev,
                "column {}.{} has version {} but a later-table column had version {} \
                 (registry iteration order must be non-decreasing within each table)",
                col.table,
                col.column,
                col.version,
                prev
            );
            last.insert(col.table, col.version);
        }
    }

    /// Every column appears exactly once. A duplicate would let the
    /// second `ALTER TABLE ... ADD COLUMN` error out at runtime
    /// (SqliteError: duplicate column), but only on a real upgrade —
    /// the unit-test path that uses fresh in-memory connections would
    /// never hit it.
    #[test]
    fn column_specs_are_unique_per_table() {
        let mut seen: HashSet<(&'static str, &'static str)> = HashSet::new();
        for col in all_column_specs() {
            assert!(
                seen.insert((col.table, col.column)),
                "duplicate column entry: {}.{}",
                col.table,
                col.column
            );
        }
    }

    /// The mesh subset must be non-empty (the projection would render
    /// as an empty `SELECT , , FROM meshes` if the registry ever
    /// regressed to a state with no mesh entries — `db::init` would
    /// then construct a broken Mesh for every row).
    #[test]
    fn mesh_column_specs_subset_is_non_empty() {
        assert!(
            !mesh_column_specs().is_empty(),
            "mesh_column_specs must contain at least the base-table columns"
        );
    }

    /// The projection is non-empty (the SELECT would error with
    /// `no columns` if it were ever empty).
    #[test]
    fn mesh_columns_projection_is_non_empty() {
        assert!(
            !mesh_columns_projection().is_empty(),
            "mesh_columns_projection must render at least one column"
        );
    }

    /// Every `meshes` column has a `read_default` that is one of the
    /// three variants — guard against a future contributor adding a
    /// 4th variant and forgetting to update the projection mapper.
    /// Uses `std::mem::discriminant` so the test compiles even if a
    /// new variant is added (the assertion catches the miss at
    /// runtime).
    #[test]
    fn mesh_read_default_variants_are_exhaustively_handled() {
        use std::mem::discriminant;
        let null = discriminant(&ReadDefault::Nullable);
        let int = discriminant(&ReadDefault::CoalesceInt(0));
        let text = discriminant(&ReadDefault::CoalesceText(""));
        for col in mesh_column_specs() {
            let d = discriminant(&col.read_default);
            assert!(
                d == null || d == int || d == text,
                "{}.{} has a ReadDefault variant the projection mapper doesn't handle",
                col.table,
                col.column
            );
        }
    }
}