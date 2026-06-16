//! Coordinator control API (ADR-0008) — the agent-agnostic surface through
//! which an external [Coordinator](../../CONTEXT.md) reads what every Agent
//! Node is doing. This module owns the read-model building block ([`node_digest`])
//! and the auth scaffold that every later slice (read enrichment, the log
//! endpoint, and the whole drive side) reuses.
//!
//! Security stance for this slice (issue #315):
//! - **Off by default.** The master switch lives in the database and defaults
//!   to disabled; a fresh install is never an open endpoint.
//! - **Loopback/LAN only.** The surface rides the existing embedded HTTP server
//!   (`crate::http`), which binds `0.0.0.0` on the 1992–1994 LAN range and opens
//!   no internet-facing port — reaching it remotely is the user's own tunnel.
//! - **Separate, capability-scoped token.** A read-scoped coordinator token,
//!   distinct from the mobile root token, validated independently of it.

pub mod drive;
pub mod enrichment;
pub mod node_digest;

/// Pull a bearer token out of an `Authorization: Bearer <token>` header. This
/// is the natural shape for an external agent / `curl -H` and for the later
/// MCP wrap; `?token=` stays a supported fallback (see [`authenticate_read`]).
fn bearer_token(headers: &str) -> Option<String> {
    let value = crate::http::request::extract_header_value(headers, "Authorization")?;
    value
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Authenticate a request for READ access to the coordinator surface. Accepts
/// the token from either an `Authorization: Bearer` header or a `?token=` URL
/// param (curl convenience). Returns `false` — never touching the database —
/// when no token is presented, so an unauthenticated probe can't even reach a
/// DB lookup. Validation rejects unless the API is enabled AND the token
/// matches the minted read token.
pub fn authenticate_read(headers: &str, url_token: Option<String>) -> bool {
    let Some(token) = bearer_token(headers).or_else(|| url_token.filter(|t| !t.is_empty())) else {
        return false;
    };
    crate::db::validate_coordinator_read_token(&token).unwrap_or(false)
}

/// Authenticate a request for DRIVE (write) access to the coordinator surface
/// (issue #319). Same token-presentation shape as [`authenticate_read`], but
/// validated against the *drive-scoped* token: a read-only token is rejected
/// here, and drive must be independently enabled. No token presented → `false`
/// without touching the DB.
pub fn authenticate_drive(headers: &str, url_token: Option<String>) -> bool {
    let Some(token) = bearer_token(headers).or_else(|| url_token.filter(|t| !t.is_empty())) else {
        return false;
    };
    crate::db::validate_coordinator_drive_token(&token).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_token_extracts_and_trims() {
        let headers = "Host: localhost\r\nAuthorization: Bearer deadbeef\r\n";
        assert_eq!(bearer_token(headers), Some("deadbeef".to_string()));
    }

    #[test]
    fn bearer_token_none_without_header_or_scheme() {
        assert_eq!(bearer_token("Host: localhost\r\n"), None);
        // A non-Bearer scheme is not a coordinator token.
        assert_eq!(bearer_token("Authorization: Basic abc\r\n"), None);
        // Empty after the scheme is rejected.
        assert_eq!(bearer_token("Authorization: Bearer \r\n"), None);
    }

    // --- Integration test: the read pipe end-to-end over an in-memory DB ---
    //
    // Exercises the same `_inner(&Connection)` functions the live route locks
    // and calls, so it covers the AC ("disabled/no-token → rejected; valid read
    // token → node list returned") without contending on the process-global DB
    // or standing up a socket.

    use crate::db;
    use rusqlite::Connection;

    /// A minimal current-schema in-memory DB: the three tables the read pipe
    /// touches, including the v14 `status_changed_at` column.
    fn seeded_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE agent_nodes (
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
                source_issue INTEGER,
                source_pr INTEGER,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                status_changed_at TEXT NOT NULL DEFAULT (datetime('now')),
                head_repo_owner TEXT,
                head_repo_clone_url TEXT
            );
            INSERT INTO meshes (id, name, path) VALUES (1, 'core', '/tmp/core');
            INSERT INTO agent_nodes (mesh_id, name, path, provider, status, position)
                VALUES (1, 'running-node', '/tmp/core/a', 'anthropic', 'running', 0);
            INSERT INTO agent_nodes (mesh_id, name, path, provider, status, position)
                VALUES (1, 'blocked-node', '/tmp/core/b', 'minimax', 'awaiting_input', 1);
            INSERT INTO agent_nodes (mesh_id, name, path, provider, status, position)
                VALUES (1, 'gone-node', '/tmp/core/c', 'anthropic', 'archived', 2);
            ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn disabled_api_rejects_even_a_valid_token() {
        let conn = seeded_db();
        let token = db::generate_coordinator_read_token_inner(&conn).unwrap();

        // Off by default: a freshly minted, correct token is still rejected
        // until the master switch is flipped on.
        assert!(!db::coordinator_api_enabled_inner(&conn).unwrap());
        assert!(!db::validate_coordinator_read_token_inner(&conn, &token).unwrap());
    }

    #[test]
    fn enabled_api_accepts_only_the_minted_token() {
        let conn = seeded_db();
        let token = db::generate_coordinator_read_token_inner(&conn).unwrap();
        db::set_coordinator_api_enabled_inner(&conn, true).unwrap();

        assert!(db::validate_coordinator_read_token_inner(&conn, &token).unwrap());
        assert!(!db::validate_coordinator_read_token_inner(&conn, "wrong").unwrap());
        assert!(!db::validate_coordinator_read_token_inner(&conn, "").unwrap());
    }

    // --- Drive scope (issue #319) ---

    /// The drive scope is OFF by default: even with the master switch on, a drive
    /// token, and a correct presentation, driving is rejected until the drive
    /// kill-switch is explicitly flipped on.
    #[test]
    fn drive_is_off_by_default() {
        let conn = seeded_db();
        db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
        let drive = db::generate_coordinator_drive_token_inner(&conn).unwrap();

        assert!(!db::coordinator_drive_enabled_inner(&conn).unwrap());
        assert!(!db::validate_coordinator_drive_token_inner(&conn, &drive).unwrap());
    }

    /// A read-scoped token can never drive — drive validates against its own
    /// stored token, so the read token simply doesn't match.
    #[test]
    fn read_token_is_rejected_for_drive() {
        let conn = seeded_db();
        db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
        db::set_coordinator_drive_enabled_inner(&conn, true).unwrap();
        let read = db::generate_coordinator_read_token_inner(&conn).unwrap();
        db::generate_coordinator_drive_token_inner(&conn).unwrap();

        assert!(
            !db::validate_coordinator_drive_token_inner(&conn, &read).unwrap(),
            "a read-scoped token must not unlock drive"
        );
    }

    /// The master kill-switch covers drive: turning the whole surface off rejects
    /// a valid drive token even while the drive kill-switch is on.
    #[test]
    fn master_switch_disables_drive() {
        let conn = seeded_db();
        db::set_coordinator_drive_enabled_inner(&conn, true).unwrap();
        let drive = db::generate_coordinator_drive_token_inner(&conn).unwrap();

        // master off (default) → rejected
        assert!(!db::validate_coordinator_drive_token_inner(&conn, &drive).unwrap());

        db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
        assert!(db::validate_coordinator_drive_token_inner(&conn, &drive).unwrap());
    }

    /// Fully enabled (master + drive on, token minted): only the exact minted
    /// drive token is accepted; wrong/empty tokens are not.
    #[test]
    fn enabled_drive_accepts_only_the_minted_token() {
        let conn = seeded_db();
        db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
        db::set_coordinator_drive_enabled_inner(&conn, true).unwrap();
        let drive = db::generate_coordinator_drive_token_inner(&conn).unwrap();

        assert!(db::validate_coordinator_drive_token_inner(&conn, &drive).unwrap());
        assert!(!db::validate_coordinator_drive_token_inner(&conn, "wrong").unwrap());
        assert!(!db::validate_coordinator_drive_token_inner(&conn, "").unwrap());
    }

    #[test]
    fn valid_token_path_returns_spine_digests_across_the_mesh() {
        let conn = seeded_db();

        let rows = db::list_coordinator_node_rows_inner(&conn).unwrap();
        // Archived node is excluded; the two live nodes come back in grid order.
        assert_eq!(rows.len(), 2);

        let digests: Vec<_> = rows
            .iter()
            .map(|(node, mesh, changed)| node_digest::spine(node, mesh, *changed))
            .collect();

        assert_eq!(digests[0].name, "running-node");
        assert_eq!(digests[0].mesh, "core");
        assert_eq!(digests[0].provider, "anthropic");
        assert!(!digests[0].needs_feedback);
        assert!(digests[0].waiting_since.is_none());

        assert_eq!(digests[1].name, "blocked-node");
        assert_eq!(digests[1].provider, "minimax");
        assert_eq!(digests[1].status, "awaiting_input");
        assert!(digests[1].needs_feedback, "the blocked node needs feedback");
        assert!(
            digests[1].waiting_since.is_some(),
            "an awaiting node carries waiting_since"
        );

        // The whole list serializes to a plain JSON array (curl-inspectable).
        let json = serde_json::to_string(&digests).unwrap();
        assert!(json.starts_with('['));
        assert!(json.contains("\"needs_feedback\":true"));
    }

    #[test]
    fn migrated_db_with_null_status_changed_at_does_not_break_the_query() {
        // Reproduces the pre-v14-migration shape: the column was added nullable
        // (no DEFAULT), and a node inserted without it stores NULL. A non-Option
        // read would error the whole query on that row, blanking the endpoint.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            -- agent_nodes WITHOUT status_changed_at, then the real migration ALTER.
            CREATE TABLE agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mesh_id INTEGER NOT NULL REFERENCES meshes(id),
                name TEXT NOT NULL, path TEXT NOT NULL,
                branch TEXT NOT NULL DEFAULT 'main',
                env TEXT NOT NULL DEFAULT 'windows',
                provider TEXT NOT NULL DEFAULT 'anthropic',
                status TEXT NOT NULL DEFAULT 'idle',
                cli_session_id TEXT, worktree_name TEXT,
                use_worktree INTEGER NOT NULL DEFAULT 1,
                source_issue INTEGER,
                source_pr INTEGER,
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                head_repo_owner TEXT,
                head_repo_clone_url TEXT
            );
            ALTER TABLE agent_nodes ADD COLUMN status_changed_at TEXT;
            INSERT INTO meshes (id, name, path) VALUES (1, 'core', '/tmp/core');
            -- Inserted omitting status_changed_at → stored NULL.
            INSERT INTO agent_nodes (mesh_id, name, path, status, position)
                VALUES (1, 'post-migration-node', '/tmp/core/a', 'running', 0);
            ",
        )
        .unwrap();

        let rows = db::list_coordinator_node_rows_inner(&conn).unwrap();
        assert_eq!(rows.len(), 1, "the NULL row must not error the whole query");
        let (node, mesh, changed) = &rows[0];
        // Falls back to creation time rather than erroring or panicking.
        let digest = node_digest::spine(node, mesh, *changed);
        assert_eq!(digest.name, "post-migration-node");
        assert!(!digest.needs_feedback);
    }

    #[test]
    fn status_change_stamps_a_fresh_waiting_since() {
        // A node flipped to awaiting_input reports the new transition time, not
        // its creation time — the spine tracks lifecycle, not row age.
        let conn = seeded_db();
        let rows = db::list_coordinator_node_rows_inner(&conn).unwrap();
        let blocked = rows.iter().find(|(n, _, _)| n.name == "blocked-node").unwrap();
        let digest = node_digest::spine(&blocked.0, &blocked.1, blocked.2);
        assert_eq!(digest.waiting_since, Some(blocked.2));
    }
}
