//! Attention marking for Agent Nodes.
//!
//! When an agent yields control back to the user — a Node Turn (see CONTEXT.md
//! and `crate::node_turn`) — the node is marked as awaiting input and the
//! frontend is notified. This module owns *only* that reaction; the Node Turn
//! itself is published by `crate::node_turn::publish`, which also drives session
//! naming independently. Keeping the two reactions separate means attention
//! marking can be exercised without the LLM rename machinery (see the tests).
//!
//! Status writes + Tauri events are delegated to `crate::agent::session_lifecycle`
//! (issue #132) — this module is now a thin routing layer that builds an
//! `AppSessionLifecycleSink` and adds the autoclear arm on top.
//!
//! ## Threading (issue #1389, review feedback)
//!
//! All commands are sync `#[command] fn`. In Tauri 2, sync commands run on
//! Tauri's IPC thread pool, NOT on Tokio's async worker pool — so the SQLite
//! write + Tauri emit inside [`mark_attention`] / [`clear_attention`] never
//! park a Tokio worker. The first PR-#1429 draft converted these to `async
//! fn` + `run_blocking`, adding a Tokio scheduler hop and a `spawn_blocking`
//! hop to work that was already off the Tokio pool; the only justification
//! was to expose the hidden DB write to the #1380 pattern guard. Review
//! feedback correctly flagged this as degrading the architecture to appease
//! a linter. The corrected shape keeps the commands sync and extends the
//! guard (see `tests/unit/async-command-blocking.test.ts`) to flag direct
//! `session_lifecycle::on_attention[_cleared]` calls in **both** sync and
//! async `#[command]` bodies — the helpers in this module are the public
//! surface; the sink writes are an internal detail.

use crate::agent::session_lifecycle::AppSessionLifecycleSink;
use crate::agent::session_lifecycle::SemanticTurnPayload;
use crate::db;
use crate::models::{AgentNode, SessionStatus};
use tauri::{command, AppHandle};

// Wire-type structs (AttentionNeededPayload, AttentionClearedPayload,
// ResumeFailedPayload) live in `crate::agent::session_lifecycle` — the
// lifecycle module owns the emit, so the payload definitions stay there
// too (issue #161 + #132).

/// Mark a node as awaiting user input and notify the frontend. One of the two
/// independent reactions to a Node Turn; see [`crate::node_turn::publish`].
/// Idempotent — re-marking an already-awaiting node is a no-op write + re-emit.
///
/// **Public surface** — every caller (Tauri commands, mobile HTTP routes,
/// autoclear safety net) routes through here rather than calling
/// `session_lifecycle::on_attention` directly. The pattern guard enforces
/// this: direct sink calls inside a `#[command]` body fail CI. Callers
/// running on the Tokio worker pool must wrap this in
/// `crate::commands::run_blocking` (see `http/routes/attention.rs`).
pub fn mark_attention(node_id: i64, app: &AppHandle) {
    mark_attention_with_detail(node_id, app, None);
}

pub fn mark_attention_with_detail(
    node_id: i64,
    app: &AppHandle,
    semantic_turn: Option<crate::agent::session_lifecycle::SemanticTurnPayload>,
) {
    let sink = AppSessionLifecycleSink { app };
    let _ = crate::agent::session_lifecycle::on_attention_with_detail(&sink, node_id, semantic_turn);
    // Arm the resume-detection safety net (issue #878): if the agent starts
    // producing output again without user input, the mark was stale and gets
    // auto-cleared.
    crate::attention_autoclear::on_marked(node_id);
}

/// Clear the attention state for a node — called when the user resumes it.
/// Symmetric counterpart to [`mark_attention`]: same threading contract,
/// same guard rule (direct `session_lifecycle::on_attention_cleared` calls
/// in `#[command]` bodies fail CI).
pub fn clear_attention(node_id: i64, app: &AppHandle) {
    crate::attention_autoclear::disarm(node_id);
    let _ = db::persist_semantic_turn(node_id, None);
    let sink = AppSessionLifecycleSink { app };
    let _ = crate::agent::session_lifecycle::on_attention_cleared(&sink, node_id);
}

/// Register that a node is awaiting user input. Publishes a Node Turn so both
/// attention-marking and session naming react.
///
/// Sync `#[command] fn` — runs on Tauri's IPC thread pool, NOT Tokio.
/// `node_turn::publish` does a SQLite write plus the session-naming and
/// autopilot-pipeline fan-outs — all blocking, but none of it parks a
/// Tokio worker. See module docstring for the threading rationale.
#[command]
pub fn register_attention_node(app: AppHandle, node_id: i64) -> Result<(), String> {
    crate::node_turn::publish(node_id, &app);
    Ok(())
}

/// Clear the attention state for a node — called when the user resumes it.
/// Sync for the same reason as [`register_attention_node`].
#[command]
pub fn clear_attention_node(app: AppHandle, node_id: i64) -> Result<(), String> {
    clear_attention(node_id, &app);
    Ok(())
}

#[command]
pub fn list_semantic_turns() -> Result<Vec<SemanticTurnPayload>, String> {
    let rows = db::list_semantic_turns().map_err(|e| e.to_string())?;
    Ok(rows.into_iter().filter_map(|(_, value)| serde_json::from_str(&value).ok()).collect())
}

/// Whether a node is currently awaiting user input. Derived from the lifecycle
/// `status` column — the single source of truth — rather than a mirrored
/// in-memory set. The old set desynced two ways: it was empty after an app
/// restart (so a persisted `awaiting_input` row read as "not pending"), and a
/// silently-dropped status write left the set and the column disagreeing.
///
/// Pure sync — single SQLite read. Sync commands are allowed to call
/// `db::*` directly (they're already off the Tokio pool), so this stays
/// `#[command] fn`.
#[command]
pub fn is_attention_pending(session_id: i64) -> bool {
    db::get_agent_node_by_id(session_id)
        .map(|n| status_is_awaiting(&n))
        .unwrap_or(false)
}

fn status_is_awaiting(node: &AgentNode) -> bool {
    node.status == SessionStatus::AwaitingInput
}

#[cfg(test)]
mod tests {
    use super::*;
    // Named import so the `impl SessionLifecycleSink for FakeLifecycleSink`
    // below resolves the trait by name (the `as _` form only brings the
    // trait's methods into scope, not the trait identifier itself).
    use crate::agent::session_lifecycle::SessionLifecycleSink;
    use rusqlite::Connection;
    use std::cell::RefCell;

    /// Records every status write and emit so the transition test
    /// can assert exactly one DB write + exactly one event emit.
    #[derive(Default)]
    struct FakeLifecycleSink {
        writes: RefCell<Vec<(i64, SessionStatus)>>,
        attention_needed: RefCell<Vec<i64>>,
    }

    impl SessionLifecycleSink for FakeLifecycleSink {
        fn write_status(&self, node_id: i64, new: SessionStatus) -> Result<(), String> {
            self.writes.borrow_mut().push((node_id, new));
            Ok(())
        }
        fn write_status_if(
            &self,
            _node_id: i64,
            _new: SessionStatus,
            _expected: SessionStatus,
        ) -> Result<bool, String> {
            Ok(true)
        }
        fn write_status_unless_in(
            &self,
            _node_id: i64,
            _new: SessionStatus,
            _forbidden: &[SessionStatus],
        ) -> Result<bool, String> {
            Ok(true)
        }
        fn emit_attention_needed(&self, node_id: i64) {
            self.attention_needed.borrow_mut().push(node_id);
        }
        fn emit_attention_cleared(&self, _node_id: i64) {}
        fn emit_resume_failed(&self, _node_id: i64, _reason: &str) {}
    }

    /// The decoupling payoff: attention marking is now exercisable on its own,
    /// with no `AppHandle` and no session-naming/LLM machinery in the way.
    /// Routes through `session_lifecycle::on_attention` (issue #132) so the
    /// transition is owned by the lifecycle module and exercised via its
    /// `SessionLifecycleSink` seam.
    #[test]
    fn mark_attention_sets_status_and_emits() {
        let sink = FakeLifecycleSink::default();
        crate::agent::session_lifecycle::on_attention(&sink, 42).unwrap();
        assert_eq!(
            *sink.writes.borrow(),
            vec![(42, SessionStatus::AwaitingInput)],
            "on_attention must write AwaitingInput exactly once"
        );
        assert_eq!(
            *sink.attention_needed.borrow(),
            vec![42],
            "on_attention must emit attention-needed exactly once"
        );
    }

    /// Minimal current-schema in-memory DB with one node, mirroring the pattern
    /// the coordinator read tests use to avoid the process-global DB.
    fn seeded_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE meshes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
                layout TEXT NOT NULL DEFAULT 'grid',
                position INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE agent_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mesh_id INTEGER NOT NULL REFERENCES meshes(id),
                name TEXT NOT NULL, path TEXT NOT NULL,
                branch TEXT NOT NULL DEFAULT 'main',
                env TEXT NOT NULL DEFAULT 'windows',
                provider TEXT NOT NULL DEFAULT 'anthropic',
                status TEXT NOT NULL DEFAULT 'running',
                cli_session_id TEXT, worktree_name TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                source_issue INTEGER,
                source_pr INTEGER,
                use_worktree INTEGER NOT NULL DEFAULT 1,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0,
                status_changed_at TEXT NOT NULL DEFAULT (datetime('now')),
                head_repo_owner TEXT,
                head_repo_clone_url TEXT,
                source_pr_pinned_sha TEXT
            );
            INSERT INTO meshes (id, name, path) VALUES (1, 'core', '/tmp/core');
            INSERT INTO agent_nodes (id, mesh_id, name, path, status)
                VALUES (7, 1, 'node', '/tmp/core/a', 'running');
            ",
        )
        .unwrap();
        conn
    }

    fn is_pending_inner(conn: &Connection, node_id: i64) -> bool {
        db::get_agent_node_by_id_inner(conn, node_id)
            .map(|n| status_is_awaiting(&n))
            .unwrap_or(false)
    }

    /// "Pending" is derived from the status column, so it tracks the source of
    /// truth across a flip — and an unknown node never panics. The deleted
    /// in-memory set could not have passed the restart half of this (empty set
    /// vs a persisted `awaiting_input` row).
    #[test]
    fn is_attention_pending_derives_from_status() {
        let conn = seeded_conn();
        assert!(!is_pending_inner(&conn, 7), "a running node is not pending");

        conn.execute(
            "UPDATE agent_nodes SET status = 'awaiting_input' WHERE id = 7",
            [],
        )
        .unwrap();
        assert!(is_pending_inner(&conn, 7), "an awaiting node is pending");

        assert!(
            !is_pending_inner(&conn, 999),
            "an unknown node is not pending and does not panic"
        );
    }
}
