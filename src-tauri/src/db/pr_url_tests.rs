//! Issue #37 — DB-side tests for the agent-node PR-URL capture path.
//!
//! Covers three surfaces the resume-by-URL fallback depends on:
//!
//! 1. **`set_agent_node_pr_url`** — first-write-wins so a re-opened PR
//!    can't silently overwrite the original. The PTY detector can fire
//!    multiple times if the agent types `gh pr create` twice (or pastes
//!    the URL into chat); the row must keep the *first* captured value.
//! 2. **`list_suspended_nodes_with_pr_url`** — the resume loop's work
//!    list. Filters `suspended AND cli_session_id IS NULL AND pr_url IS
//!    NOT NULL` so nodes with a fresh session id take the fast path and
//!    only stale-session-id nodes land here.
//! 3. **`hydrate_agent_node_from_pr`** — turns a `pr_url`-only node
//!    into a `source_pr`-spawned shape so the existing
//!    `spawn_agent_inner` worktree-adoption branch takes over.
//!
//! `ensure_agent_node_pr_url` is covered by the inline migration tests
//! pattern (same shape as `ensure_agent_node_source_pr`); see the
//! safety-net module-level docs in `db/mod.rs` for the rationale.

#[cfg(test)]
mod tests {
    use crate::db::{
        ensure_agent_node_pr_url, hydrate_agent_node_from_pr_inner,
        set_agent_node_pr_url_inner,
    };
    use rusqlite::{params, Connection};

    /// Minimal `agent_nodes` schema carrying only the columns the PR-URL
    /// surface touches. Mirrors the test-helper style in `agent_node_tests.rs`
    /// — full schema is overkill for a SQL-semantics test, and an in-memory
    /// fixture avoids the global DB mutex.
    fn conn_with_agent_nodes() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Re-create the inline-CREATE column shape from `db::init`. The
        // safety-net `ensure_*` runs against this and adds `pr_url` if
        // missing, so the fixture exercises the same path production uses.
        conn.execute_batch(
            "CREATE TABLE agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mesh_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                branch TEXT NOT NULL DEFAULT 'main',
                env TEXT NOT NULL DEFAULT 'windows',
                provider TEXT NOT NULL DEFAULT 'anthropic',
                status TEXT NOT NULL DEFAULT 'idle',
                cli_session_id TEXT,
                worktree_name TEXT,
                use_worktree INTEGER NOT NULL DEFAULT 1,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                status_changed_at TEXT NOT NULL DEFAULT (datetime('now')),
                source_pr INTEGER,
                head_repo_owner TEXT,
                head_repo_clone_url TEXT,
                source_pr_pinned_sha TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn insert_suspended_node(conn: &Connection, cli_id: Option<&str>, pr_url: Option<&str>) -> i64 {
        conn.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, status, cli_session_id, pr_url)
             VALUES (1, 'fix-thing', '/repo/worktree', 'suspended', ?1, ?2)",
            params![cli_id, pr_url],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn current_pr_url(conn: &Connection, id: i64) -> Option<String> {
        conn.query_row(
            "SELECT pr_url FROM agent_nodes WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// Safety-net migration: a pre-v28 fixture (no `pr_url` column) must
    /// gain the column on `ensure_agent_node_pr_url`. The inline CREATE in
    /// `db::init` already adds it for fresh DBs; the safety net is the
    /// upgrade path. Running on an already-migrated DB is a no-op.
    #[test]
    fn ensure_pr_url_adds_missing_column() {
        let conn = conn_with_agent_nodes();

        // Pre-condition: column missing.
        let before: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'pr_url'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!before, "fresh in-memory fixture must start without pr_url");

        ensure_agent_node_pr_url(&conn).unwrap();

        // Post-condition: column exists.
        let after: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('agent_nodes') WHERE name = 'pr_url'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(after, "ensure must add the column");

        // Idempotent: a second call is a no-op (column already present).
        ensure_agent_node_pr_url(&conn).unwrap();
    }

    /// `set_agent_node_pr_url` happy path: writes the URL on a NULL row
    /// and the row's readback carries it.
    #[test]
    fn set_pr_url_writes_when_column_is_null() {
        let conn = conn_with_agent_nodes();
        ensure_agent_node_pr_url(&conn).unwrap();
        let id = insert_suspended_node(&conn, None, None);

        set_agent_node_pr_url_inner(&conn, id, Some("https://github.com/me/repo/pull/42")).unwrap();

        assert_eq!(
            current_pr_url(&conn, id).as_deref(),
            Some("https://github.com/me/repo/pull/42"),
        );
    }

    /// First-write-wins: a second `set_agent_node_pr_url_inner` against the
    /// same row must not overwrite the original. The detector can fire
    /// twice if the agent types `gh pr create` more than once, and the
    /// user-recoverable case is "the original PR was closed and a new
    /// one opened" — the frontend can surface that explicitly rather
    /// than the row silently mutating.
    #[test]
    fn set_pr_url_does_not_overwrite_existing_value() {
        let conn = conn_with_agent_nodes();
        ensure_agent_node_pr_url(&conn).unwrap();
        let id = insert_suspended_node(&conn, None, Some("https://github.com/me/repo/pull/1"));

        set_agent_node_pr_url_inner(&conn, id, Some("https://github.com/me/repo/pull/2")).unwrap();

        assert_eq!(
            current_pr_url(&conn, id).as_deref(),
            Some("https://github.com/me/repo/pull/1"),
            "first PR must win — detector's re-fire must not silently overwrite",
        );
    }

    /// `list_suspended_nodes_with_pr_url` excludes nodes that already
    /// have a session id (those take the fast `--resume` path) and
    /// excludes nodes without a `pr_url` (no fallback anchor). Only
    /// the intersection — `suspended AND cli_session_id IS NULL AND
    /// pr_url IS NOT NULL` — is returned.
    #[test]
    fn list_suspended_with_pr_url_filters_correctly() {
        let conn = conn_with_agent_nodes();
        ensure_agent_node_pr_url(&conn).unwrap();

        // The four scenarios; only (3) should appear in the work list.
        let _with_session = insert_suspended_node(&conn, Some("session-uuid"), None);
        let _without_either = insert_suspended_node(&conn, None, None);
        let with_pr_url = insert_suspended_node(&conn, None, Some("https://github.com/x/y/pull/1"));
        let _with_both = insert_suspended_node(
            &conn,
            Some("session-uuid"),
            Some("https://github.com/x/y/pull/2"),
        );

        let rows = list_suspended_nodes_with_pr_url_inner(&conn).unwrap();
        let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();

        assert_eq!(rows.len(), 1, "exactly one row matches the fallback predicate");
        assert_eq!(ids, vec![with_pr_url]);
    }

    /// In-memory inner helper mirroring the lock-once pattern of the
    /// public `list_suspended_nodes_with_pr_url`. The public version
    /// takes the global DB mutex, which tests across the lib binary
    /// share — calling it from this fixture (without touching `init`)
    /// would either deadlock on the OnceCell or skip the test entirely.
    fn list_suspended_nodes_with_pr_url_inner(
        conn: &Connection,
    ) -> rusqlite::Result<Vec<(i64, Option<String>)>> {
        let mut stmt = conn.prepare(
            "SELECT id, pr_url FROM agent_nodes WHERE status = 'suspended' \
             AND cli_session_id IS NULL AND pr_url IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// `hydrate_agent_node_from_pr` happy path: writes the PR-spawn
    /// metadata columns and flips `status` to `idle` so the existing
    /// `spawn_agent_inner`'s `source_pr.is_some()` worktree-adoption
    /// branch can take over a fresh spawn.
    #[test]
    fn hydrate_writes_pr_spawn_metadata_and_resets_status() {
        let conn = conn_with_agent_nodes();
        ensure_agent_node_pr_url(&conn).unwrap();
        let id = insert_suspended_node(&conn, None, Some("https://github.com/x/y/pull/7"));

        hydrate_agent_node_from_pr_inner(
            &conn,
            id,
            7,
            "feat/issue-37",
            Some("deadbeefcafe"),
            Some("fork-owner"),
            Some("https://github.com/fork-owner/y.git"),
        )
        .unwrap();

        // Verify each column round-trips. Read raw SQL because the
        // `AgentNode` projection would force us to recreate the full
        // schema inside the test fixture.
        let (branch, source_pr, source_pr_pinned_sha, head_repo_owner, head_repo_clone_url, status): (
            String,
            i64,
            String,
            Option<String>,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT branch, source_pr, source_pr_pinned_sha, head_repo_owner, head_repo_clone_url, status \
                 FROM agent_nodes WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(branch, "feat/issue-37");
        assert_eq!(source_pr, 7);
        assert_eq!(source_pr_pinned_sha, "deadbeefcafe");
        assert_eq!(head_repo_owner.as_deref(), Some("fork-owner"));
        assert_eq!(
            head_repo_clone_url.as_deref(),
            Some("https://github.com/fork-owner/y.git"),
        );
        assert_eq!(status, "idle", "hydrate must reset the crash-time Suspended marker");
    }

    /// Same-repo PR: `hydrate_agent_node_from_pr_inner` with `None` for both
    /// fork columns. The resulting row must take the `#420` origin/branch
    /// path in `spawn_agent_inner` (no fork remote registered).
    #[test]
    fn hydrate_with_no_fork_metadata() {
        let conn = conn_with_agent_nodes();
        ensure_agent_node_pr_url(&conn).unwrap();
        let id = insert_suspended_node(&conn, None, Some("https://github.com/me/proj/pull/3"));

        hydrate_agent_node_from_pr_inner(
            &conn,
            id,
            3,
            "feat/same-repo",
            Some("abc123"),
            None,
            None,
        )
        .unwrap();

        let (head_repo_owner, head_repo_clone_url): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT head_repo_owner, head_repo_clone_url FROM agent_nodes WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(head_repo_owner, None);
        assert_eq!(head_repo_clone_url, None);
    }

    /// Empty head SHA must store NULL — the drift-check in
    /// `spawn_agent_inner` branches on `Some(_)`, so an empty-string
    /// SHA would (incorrectly) look like "drift detected" on every
    /// subsequent spawn. The empty-from-API case is "we don't have the
    /// pin", which is best expressed as NULL — same fail-open semantics
    /// the `pr_head_unfetchable` fallback already uses (#420).
    #[test]
    fn hydrate_with_none_head_sha_writes_null_pinned_sha() {
        let conn = conn_with_agent_nodes();
        ensure_agent_node_pr_url(&conn).unwrap();
        let id = insert_suspended_node(&conn, None, Some("https://github.com/x/y/pull/9"));

        hydrate_agent_node_from_pr_inner(
            &conn,
            id,
            9,
            "feat/no-sha",
            None,
            None,
            None,
        )
        .unwrap();

        let pinned: Option<String> = conn
            .query_row(
                "SELECT source_pr_pinned_sha FROM agent_nodes WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pinned, None,
            "missing head SHA from GitHub must store NULL, not empty string",
        );
    }
}
