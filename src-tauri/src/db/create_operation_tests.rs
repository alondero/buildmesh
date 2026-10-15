//! Unit tests for the remote create-and-start operation ledger (issue #2331).
//!
//! These exercise the lock-free `_inner` helpers against an in-memory
//! connection built from the real schema (`db::init_schema`), so the
//! `agent_node_create_operations` table, its `ON DELETE CASCADE`, and its
//! unique `operation_key` are the production ones — not a hand-rolled fixture
//! that could drift from the migration.
//!
//! Run with: cargo test --lib db::create_operation_tests

use rusqlite::Connection;

use crate::db::create_operation::{
    bind_create_operation_tx, claim_create_operation_inner, find_create_operation_inner,
    record_create_start_result_inner, CreateBindOutcome, CreateClaim, CreateStartState,
};

/// Real schema plus one mesh, so `agent_nodes` rows can be inserted.
fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    crate::db::init_schema(&conn).unwrap();
    conn.execute(
        "INSERT INTO meshes (id,name,path) VALUES (1,'API','C:/api')",
        [],
    )
    .unwrap();
    conn
}

fn insert_node(conn: &Connection, name: &str) -> i64 {
    conn.execute(
        "INSERT INTO agent_nodes (mesh_id, name, path, provider) VALUES (1, ?1, 'C:/api', 'anthropic')",
        [name],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn bind(conn: &Connection, key: &str, fingerprint: &str, node_id: i64) -> CreateBindOutcome {
    bind_create_operation_tx(conn, key, fingerprint, node_id).unwrap()
}

#[test]
fn unknown_key_is_not_a_claim() {
    // `None` means "never seen — go ahead and create". Collapsing this into a
    // claim would make a first submission look like a replay.
    let conn = db();
    assert_eq!(
        claim_create_operation_inner(&conn, "op-1", "fp-1").unwrap(),
        None
    );
}

#[test]
fn bind_then_claim_replays_the_same_node() {
    let conn = db();
    let node_id = insert_node(&conn, "idea-a");
    assert_eq!(
        bind(&conn, "op-1", "fp-1", node_id),
        CreateBindOutcome::Bound { node_id }
    );

    match claim_create_operation_inner(&conn, "op-1", "fp-1").unwrap() {
        Some(CreateClaim::Replay {
            node_id: replayed,
            start_state,
            ..
        }) => {
            assert_eq!(replayed, node_id);
            // A fresh row is "committed, start not attempted" — the state a
            // restart between insertion and startup leaves behind.
            assert_eq!(start_state, CreateStartState::Created);
        }
        other => panic!("expected a replay, got {other:?}"),
    }
}

#[test]
fn same_key_different_content_is_a_conflict() {
    // Stripe-style reject (the same rule the drive ledger uses for prompt
    // reuse): a key reused for a different idea must never silently bind.
    let conn = db();
    let node_id = insert_node(&conn, "idea-a");
    bind(&conn, "op-1", "fp-1", node_id);

    let other_node = insert_node(&conn, "idea-b");
    assert_eq!(
        bind(&conn, "op-1", "fp-2", other_node),
        CreateBindOutcome::Conflict
    );
    assert!(matches!(
        claim_create_operation_inner(&conn, "op-1", "fp-2").unwrap(),
        Some(CreateClaim::Conflict)
    ));

    // The rejected request must leave no ledger row for the second node.
    assert_eq!(
        find_create_operation_inner(&conn, "op-1")
            .unwrap()
            .unwrap()
            .node_id,
        node_id
    );
}

#[test]
fn start_state_survives_the_recorded_attempt() {
    let conn = db();
    let node_id = insert_node(&conn, "idea-a");
    bind(&conn, "op-1", "fp-1", node_id);

    let changed = record_create_start_result_inner(
        &conn,
        node_id,
        CreateStartState::StartFailed,
        Some("claude CLI not found on PATH"),
    )
    .unwrap();
    assert_eq!(changed, 1);

    let stored = find_create_operation_inner(&conn, "op-1").unwrap().unwrap();
    assert_eq!(stored.start_state, CreateStartState::StartFailed);
    assert_eq!(
        stored.start_error.as_deref(),
        Some("claude CLI not found on PATH")
    );

    // A retry sees the recorded failure rather than a bare "created".
    match claim_create_operation_inner(&conn, "op-1", "fp-1").unwrap() {
        Some(CreateClaim::Replay {
            start_state,
            start_error,
            ..
        }) => {
            assert_eq!(start_state, CreateStartState::StartFailed);
            assert!(start_error.is_some());
        }
        other => panic!("expected a replay, got {other:?}"),
    }
}

#[test]
fn started_state_is_recorded_for_a_lost_response_retry() {
    // "A lost response after successful startup can be retried safely": the
    // retry must find `started`, so the client is told the agent is active
    // instead of being invited to create again.
    let conn = db();
    let node_id = insert_node(&conn, "idea-a");
    bind(&conn, "op-1", "fp-1", node_id);
    record_create_start_result_inner(&conn, node_id, CreateStartState::Started, None).unwrap();

    match claim_create_operation_inner(&conn, "op-1", "fp-1").unwrap() {
        Some(CreateClaim::Replay { start_state, .. }) => {
            assert_eq!(start_state, CreateStartState::Started);
        }
        other => panic!("expected a replay, got {other:?}"),
    }
}

#[test]
fn deleting_a_node_retires_its_operation_key() {
    // Otherwise a deliberately removed node would leave a key bound forever,
    // and resubmitting the same idea could never create again.
    let conn = db();
    let node_id = insert_node(&conn, "idea-a");
    bind(&conn, "op-1", "fp-1", node_id);

    conn.execute("DELETE FROM agent_nodes WHERE id = ?1", [node_id])
        .unwrap();

    assert!(find_create_operation_inner(&conn, "op-1")
        .unwrap()
        .is_none());
    assert_eq!(
        claim_create_operation_inner(&conn, "op-1", "fp-1").unwrap(),
        None
    );
}

#[test]
fn unknown_start_state_value_is_an_error_not_a_silent_created() {
    // Fail loud: collapsing a drifted value to `Created` would report a
    // started node as not-started.
    let conn = db();
    let node_id = insert_node(&conn, "idea-a");
    bind(&conn, "op-1", "fp-1", node_id);
    conn.execute(
        "UPDATE agent_node_create_operations SET start_state = 'wat' WHERE operation_key = 'op-1'",
        [],
    )
    .unwrap();

    assert!(find_create_operation_inner(&conn, "op-1").is_err());
}

#[test]
fn binding_rolls_back_with_its_transaction() {
    // The binding must not be able to outlive a rolled-back node INSERT: a
    // key pointing at a node that never existed is the orphan #2331 exists to
    // prevent.
    let conn = db();
    let node_id = insert_node(&conn, "idea-a");
    {
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, provider)
             VALUES (1, 'rolled-back', 'C:/api', 'anthropic')",
            [],
        )
        .unwrap();
        let rolled_back_id = tx.last_insert_rowid();
        bind_create_operation_tx(&tx, "op-1", "fp-1", rolled_back_id).unwrap();
        // Dropped without commit.
    }

    assert!(find_create_operation_inner(&conn, "op-1")
        .unwrap()
        .is_none());
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM agent_nodes WHERE name = 'rolled-back'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(conn
        .query_row(
            "SELECT 1 FROM agent_nodes WHERE id = ?1",
            [node_id],
            |_| Ok(())
        )
        .is_ok());
}

#[test]
fn fingerprint_separates_field_boundaries() {
    // Length-prefixed hashing must not let two different field splits collide:
    // ("ab", "c") and ("a", "bc") are different requests.
    let a = super::super::services::create_operation::request_fingerprint(1, "ab", Some("c"), None);
    let b = super::super::services::create_operation::request_fingerprint(1, "a", Some("bc"), None);
    assert_ne!(a, b);
}

#[test]
fn fingerprint_ignores_cosmetic_whitespace_but_not_content() {
    use crate::services::create_operation::request_fingerprint;

    assert_eq!(
        request_fingerprint(1, " anthropic ", Some(" cfg "), Some(" do it ")),
        request_fingerprint(1, "anthropic", Some("cfg"), Some("do it")),
    );
    assert_ne!(
        request_fingerprint(1, "anthropic", None, Some("do it")),
        request_fingerprint(1, "anthropic", None, Some("do something else")),
    );
    assert_ne!(
        request_fingerprint(1, "anthropic", None, Some("do it")),
        request_fingerprint(2, "anthropic", None, Some("do it")),
    );
}
