//! Unit tests for the coordinator drive idempotency ledger (issues #320 + #750).
//!
//! Exercises the lock-free `_inner` helpers against an in-memory connection,
//! following the `device_session_tests` pattern. The ledger is what makes a
//! Coordinator's retry a no-op: a `(node_id, key)` records the honest verdict
//! once and replays it thereafter (ADR-0008 §6). v32 (issue #750) reshaped
//! the table from `lookup + record` to the atomic
//! `claim + finalize + release_claim` protocol, plus a `prune` pass for the
//! bounded-age GC.

use crate::db;
use rusqlite::Connection;

/// In-memory connection with the `coordinator_drive_prompts` table the
/// helpers need. Kept in sync with the `CREATE TABLE` in `db::init` plus
/// the additive v32 columns.
fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE coordinator_drive_prompts (
            node_id INTEGER NOT NULL,
            idempotency_key TEXT NOT NULL,
            verdict TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'pending',
            claimed_at TEXT NOT NULL DEFAULT (datetime('now')),
            prompt_hash TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (node_id, idempotency_key)
        );",
    )
    .unwrap();
    conn
}

#[test]
fn unknown_key_claims_as_fresh() {
    // The atomic claim step distinguishes "key never seen" from "couldn't
    // read" — a fresh key claims successfully (no peer row to read).
    let conn = db();
    assert_eq!(
        db::claim_drive_prompt_inner(&conn, 7, "never-seen", "hash").unwrap(),
        db::ClaimOutcome::Claimed
    );
}

#[test]
fn claim_then_finalize_round_trips_to_replay() {
    // After finalize, the same hash claims as Replay — the headline AC.
    let conn = db();
    let hash = "v1-hash";
    db::claim_drive_prompt_inner(&conn, 7, "k", hash).unwrap();
    db::finalize_drive_prompt_inner(&conn, 7, "k", db::VerdictStr::Delivered).unwrap();
    let outcome = db::claim_drive_prompt_inner(&conn, 7, "k", hash).unwrap();
    match outcome {
        db::ClaimOutcome::Replay { verdict } => assert_eq!(verdict, "delivered"),
        other => panic!("expected Replay, got {other:?}"),
    }
}

#[test]
fn same_key_different_prompt_claims_as_mismatch() {
    // Issue #750 item 2: same key + different prompt is a Mismatch, not a
    // silent Replay.
    let conn = db();
    let hash_v1 = "v1";
    let hash_v2 = "v2";
    db::claim_drive_prompt_inner(&conn, 7, "k", hash_v1).unwrap();
    db::finalize_drive_prompt_inner(&conn, 7, "k", db::VerdictStr::Delivered).unwrap();
    assert_eq!(
        db::claim_drive_prompt_inner(&conn, 7, "k", hash_v2).unwrap(),
        db::ClaimOutcome::Mismatch
    );
}

#[test]
fn lookup_is_scoped_by_node() {
    // The same key on a different node is a distinct row — an accidentally
    // reused key must not leak one node's verdict to another.
    let conn = db();
    let hash = "h";
    db::claim_drive_prompt_inner(&conn, 7, "shared", hash).unwrap();
    db::finalize_drive_prompt_inner(&conn, 7, "shared", db::VerdictStr::Delivered).unwrap();
    assert_eq!(
        db::claim_drive_prompt_inner(&conn, 9, "shared", hash).unwrap(),
        db::ClaimOutcome::Claimed,
        "node 9 has never seen this key — it's a fresh claim"
    );
}

#[test]
fn a_read_error_propagates_rather_than_looking_like_none() {
    // Fail-safe (issue #320 review): a genuine read failure must be `Err`,
    // NOT `Ok(Claimed)` — otherwise the drive path mistakes "couldn't
    // check" for "key never seen" and re-delivers. Here the ledger table
    // is absent, so the SELECT inside the claim transaction errors; the
    // helper must surface that error, not swallow it to Claimed.
    let conn = Connection::open_in_memory().unwrap();
    assert!(
        db::claim_drive_prompt_inner(&conn, 7, "k", "h").is_err(),
        "an unreadable ledger must propagate an error, not report the key as new"
    );
}

#[test]
fn finalize_is_idempotent_and_keeps_first_verdict() {
    // `UPDATE ... WHERE status = 'pending'` is naturally idempotent and
    // refuses to overwrite a finalized row — the first verdict wins, exactly
    // like the pre-#750 `INSERT OR IGNORE` rule.
    let conn = db();
    db::claim_drive_prompt_inner(&conn, 7, "k", "h").unwrap();
    db::finalize_drive_prompt_inner(&conn, 7, "k", db::VerdictStr::Delivered).unwrap();
    // A second finalize must not change the row's status.
    db::finalize_drive_prompt_inner(&conn, 7, "k", db::VerdictStr::Unverified).unwrap();
    let outcome = db::claim_drive_prompt_inner(&conn, 7, "k", "h").unwrap();
    match outcome {
        db::ClaimOutcome::Replay { verdict } => assert_eq!(verdict, "delivered"),
        other => panic!("expected Replay of the first verdict, got {other:?}"),
    }
}

#[test]
fn release_claim_deletes_only_pending_rows() {
    // A NotLive drive must leave no `pending` row behind (so a retry can
    // re-attempt rather than wait on the orphan-recovery window).
    let conn = db();
    db::claim_drive_prompt_inner(&conn, 7, "k", "h").unwrap();
    let released = db::release_drive_prompt_claim_inner(&conn, 7, "k").unwrap();
    assert_eq!(released, 1, "release of a pending row deletes one row");
    // The slot is open: a retry claims as fresh.
    assert_eq!(
        db::claim_drive_prompt_inner(&conn, 7, "k", "h").unwrap(),
        db::ClaimOutcome::Claimed
    );
}

#[test]
fn release_claim_leaves_finalized_rows_intact() {
    // A finalized row must stay put — a retry should see the recorded
    // verdict (Replay) rather than silently re-drive.
    let conn = db();
    db::claim_drive_prompt_inner(&conn, 7, "k", "h").unwrap();
    db::finalize_drive_prompt_inner(&conn, 7, "k", db::VerdictStr::Delivered).unwrap();
    let released = db::release_drive_prompt_claim_inner(&conn, 7, "k").unwrap();
    assert_eq!(released, 0, "release of a finalized row is a no-op");
    // Still Replay.
    let outcome = db::claim_drive_prompt_inner(&conn, 7, "k", "h").unwrap();
    assert!(matches!(outcome, db::ClaimOutcome::Replay { .. }));
}

#[test]
fn pending_row_older_than_timeout_is_reclaimed() {
    // The orphan-recovery pass inside the claim transaction reclaims a
    // `pending` row whose `claimed_at` is older than
    // `PENDING_CLAIM_TIMEOUT_SECS` (a crashed-mid-send row must not block
    // the key forever). Hand-craft an old row, then claim and assert the
    // claim wins.
    let conn = db();
    conn.execute(
        "INSERT INTO coordinator_drive_prompts
             (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict)
             VALUES (7, 'k', 'pending', datetime('now', '-1 hour'), 'old-hash', '')",
        [],
    )
    .unwrap();
    let outcome = db::claim_drive_prompt_inner(&conn, 7, "k", "new-hash").unwrap();
    // The orphan-recovery DELETE frees the slot; the new claim wins and
    // is recorded as Claimed (with the new prompt_hash).
    assert_eq!(outcome, db::ClaimOutcome::Claimed);
}

#[test]
fn pending_row_younger_than_timeout_returns_in_progress() {
    // The mirror of the previous test: a `pending` row whose `claimed_at`
    // is *younger* than the timeout is a real peer's in-flight drive —
    // the claim returns InProgress rather than reclaiming.
    let conn = db();
    conn.execute(
        "INSERT INTO coordinator_drive_prompts
             (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict)
             VALUES (7, 'k', 'pending', datetime('now'), 'peer-hash', '')",
        [],
    )
    .unwrap();
    assert_eq!(
        db::claim_drive_prompt_inner(&conn, 7, "k", "new-hash").unwrap(),
        db::ClaimOutcome::InProgress
    );
}

#[test]
fn pre_v32_row_with_verdict_replays_even_when_status_is_pending() {
    // A pre-v32 row has `verdict` non-empty (v23 required NOT NULL) but
    // `status='pending'` from the v32 column DEFAULT — the row's actual
    // shape is "finalized by the old schema, mis-labelled by the v32
    // ALTER". The claim must recognise the recorded verdict and Replay
    // rather than stall as `InProgress` for `PENDING_CLAIM_TIMEOUT_SECS`
    // (the v23 row is *not* a live peer — it's a finished drive that the
    // v32 migration didn't re-stamp). The `prompt_hash=''` default makes
    // any reuse surface as Mismatch (caller must mint a fresh key).
    let conn = db();
    conn.execute(
        "INSERT INTO coordinator_drive_prompts
             (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict, created_at)
             VALUES (7, 'k', 'pending', datetime('now'), '', 'delivered', datetime('now'))",
        [],
    )
    .unwrap();
    // Same prompt-hash-from-empty: an incoming hash of "" replays the
    // recorded verdict. (A real caller's prompt_hash is never "" — the
    // SHA-256 of any non-empty prompt has 64 chars.)
    let outcome = db::claim_drive_prompt_inner(&conn, 7, "k", "").unwrap();
    match outcome {
        db::ClaimOutcome::Replay { verdict } => assert_eq!(verdict, "delivered"),
        other => panic!("expected Replay of the pre-v32 verdict, got {other:?}"),
    }
    // A non-empty incoming hash mismatches the empty stored hash →
    // Mismatch (the caller must mint a fresh key).
    assert_eq!(
        db::claim_drive_prompt_inner(&conn, 7, "k", "real-hash").unwrap(),
        db::ClaimOutcome::Mismatch
    );
}

#[test]
fn prune_older_than_deletes_only_expired_rows() {
    // Issue #750 item 3: bounded-age GC. Hand-craft three rows with
    // different `created_at` ages; the prune deletes only the expired
    // ones.
    let conn = db();
    conn.execute(
        "INSERT INTO coordinator_drive_prompts
             (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict, created_at)
             VALUES (1, 'fresh', 'delivered', datetime('now'), '', 'delivered', datetime('now'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO coordinator_drive_prompts
             (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict, created_at)
             VALUES (2, 'old1', 'delivered', datetime('now'), '', 'delivered', datetime('now', '-10 days'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO coordinator_drive_prompts
             (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict, created_at)
             VALUES (3, 'old2', 'delivered', datetime('now'), '', 'delivered', datetime('now', '-30 days'))",
        [],
    )
    .unwrap();
    let pruned = db::prune_drive_prompts_older_than_inner(&conn, 7).unwrap();
    assert_eq!(pruned, 2, "two rows are older than 7 days");
    // Only the fresh row remains.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM coordinator_drive_prompts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
