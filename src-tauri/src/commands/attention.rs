//! Attention marking for Agent Nodes.
//!
//! When an agent yields control back to the user — a Node Turn (see CONTEXT.md
//! and `crate::node_turn`) — the node is marked as awaiting input and the
//! frontend is notified. This module owns *only* that reaction; the Node Turn
//! itself is published by `crate::node_turn::publish`, which also drives session
//! naming independently. Keeping the two reactions separate means attention
//! marking can be exercised without the LLM rename machinery (see the tests).

use crate::db;
use crate::models::{AgentNode, SessionStatus};
use tauri::{command, AppHandle, Emitter};

/// The side effects of marking a node for attention, behind a seam so the logic
/// is testable without a real [`AppHandle`] or the process-global DB. Mirrors
/// `session_naming::SessionNamingRepository`.
pub(crate) trait AttentionSink {
    fn set_awaiting_input(&self, node_id: i64) -> Result<(), String>;
    fn emit_attention_needed(&self, node_id: i64);
}

struct AppAttentionSink<'a> {
    app: &'a AppHandle,
}

impl AttentionSink for AppAttentionSink<'_> {
    fn set_awaiting_input(&self, node_id: i64) -> Result<(), String> {
        db::update_agent_node_status(node_id, SessionStatus::AwaitingInput).map_err(|e| e.to_string())
    }
    fn emit_attention_needed(&self, node_id: i64) {
        let _ = self
            .app
            .emit("attention-needed", serde_json::json!({ "session_id": node_id }));
    }
}

/// Mark a node as awaiting user input and notify the frontend. One of the two
/// independent reactions to a Node Turn; see [`crate::node_turn::publish`].
/// Idempotent — re-marking an already-awaiting node is a no-op write + re-emit.
pub fn mark_attention(node_id: i64, app: &AppHandle) {
    mark_attention_with(&AppAttentionSink { app }, node_id);
    // Arm the resume-detection safety net (issue #878): if the agent starts
    // producing output again without user input, the mark was stale and gets
    // auto-cleared.
    crate::attention_autoclear::on_marked(node_id);
}

pub(crate) fn mark_attention_with(sink: &dyn AttentionSink, node_id: i64) {
    let _ = sink.set_awaiting_input(node_id);
    sink.emit_attention_needed(node_id);
    tracing::info!("Node {} awaiting user input (Node Turn)", node_id);
}

/// Register that a node is awaiting user input. Publishes a Node Turn so both
/// attention-marking and session naming react.
#[command]
pub async fn register_attention_node(app: AppHandle, node_id: i64) -> Result<(), String> {
    crate::node_turn::publish(node_id, &app);
    Ok(())
}

/// Clear the attention state for a node — called when the user resumes it.
#[command]
pub async fn clear_attention_node(app: AppHandle, node_id: i64) -> Result<(), String> {
    crate::attention_autoclear::disarm(node_id);
    db::update_agent_node_status(node_id, SessionStatus::Running).map_err(|e| e.to_string())?;
    app.emit(
        "attention-cleared",
        serde_json::json!({ "node_id": node_id }),
    )
    .map_err(|e| e.to_string())?;
    tracing::info!("Node {} attention cleared", node_id);
    Ok(())
}

/// Whether a node is currently awaiting user input. Derived from the lifecycle
/// `status` column — the single source of truth — rather than a mirrored
/// in-memory set. The old set desynced two ways: it was empty after an app
/// restart (so a persisted `awaiting_input` row read as "not pending"), and a
/// silently-dropped status write left the set and the column disagreeing.
#[command]
pub async fn is_attention_pending(session_id: i64) -> bool {
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
    use rusqlite::Connection;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeSink {
        set_awaiting: RefCell<Vec<i64>>,
        emitted: RefCell<Vec<i64>>,
    }
    impl AttentionSink for FakeSink {
        fn set_awaiting_input(&self, node_id: i64) -> Result<(), String> {
            self.set_awaiting.borrow_mut().push(node_id);
            Ok(())
        }
        fn emit_attention_needed(&self, node_id: i64) {
            self.emitted.borrow_mut().push(node_id);
        }
    }

    /// The decoupling payoff: attention marking is now exercisable on its own,
    /// with no `AppHandle` and no session-naming/LLM machinery in the way.
    #[test]
    fn mark_attention_sets_status_and_emits() {
        let sink = FakeSink::default();
        mark_attention_with(&sink, 42);
        assert_eq!(*sink.set_awaiting.borrow(), vec![42]);
        assert_eq!(*sink.emitted.borrow(), vec![42]);
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
