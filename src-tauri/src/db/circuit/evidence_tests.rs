//! Tests for `db::circuit::evidence`.
//!
//! Split out of `src-tauri/src/db/circuit/evidence.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test db::circuit::evidence::tests

use super::*;

// The resolved-in-one-step form of [`super::evidence_view_unresolved`]: a
// test holds its connection for both halves, so it never needs the split
// that lets production release the connection before capability I/O. It
// lives here rather than in `evidence.rs` so the shipped module carries no
// test-gated item.
fn evidence_view_inner(db: &Connection, run_id: i64) -> Result<CircuitEvidenceView, String> {
    let (mut view, pending) = evidence_view_unresolved(db, run_id)?;
    resolve_pending_capabilities(&mut view, pending);
    Ok(view)
}

#[test]
fn legacy_projection_conflicts_restore_only_from_matching_history_and_attempt() {
    use crate::circuit::observation::*;
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');").unwrap();
    let identity = ObservationIdentity {
        run_id: 1,
        step_id: "gate".into(),
        attempt: 1,
        agent_node_id: 9,
        session_incarnation: Some("old".into()),
        session_id: None,
        turn_id: None,
        report_revision: None,
    };
    let current = ObservationIdentity {
        session_incarnation: Some("new".into()),
        ..identity.clone()
    };
    let observation = CircuitObservation {
        identity: current.clone(),
        source: "agent_status_projection".into(),
        source_id: Some("current".into()),
        observed_at_ms: 100,
        authoritative: false,
        fact: ObservedWorkFact::Working,
    };
    let mut evidence = WorkEvidence {
        identity: Some(identity),
        conflicted: true,
        ..Default::default()
    };
    evidence.observe(&current, &observation);
    evidence
        .conflicts
        .retain(|conflict| conflict.kind == EvidenceConflictKind::Identity);
    evidence.conflicts[0].status_projection = None;
    let detail =
        serde_json::json!({"observation":observation,"disposition":"conflicting"}).to_string();
    append_history(
        &db,
        HistoryEvent {
            run_id: 1,
            node_id: Some("gate"),
            attempt: Some(2),
            kind: "observation",
            detail: &detail,
            source: Some("agent_status_projection"),
            disposition: Some("conflicting"),
        },
    )
    .unwrap();
    assert!(!restore_projection_conflicts_inner(&db, 1, "gate", 1, &mut evidence).unwrap());
    append_history(
        &db,
        HistoryEvent {
            run_id: 1,
            node_id: Some("gate"),
            attempt: Some(1),
            kind: "observation",
            detail: &detail,
            source: Some("agent_status_projection"),
            disposition: Some("conflicting"),
        },
    )
    .unwrap();
    assert!(restore_projection_conflicts_inner(&db, 1, "gate", 1, &mut evidence).unwrap());
    assert_eq!(evidence.conflicts[0].status_projection, Some(true));
    assert!(
        !restore_projection_conflicts_inner(&db, 1, "gate", 1, &mut evidence).unwrap(),
        "idempotent"
    );
}

/// The same observation recorded twice is one fact (issue #2600).
///
/// The worker knows what *it* has seen, but that bookkeeping lives in the
/// run's context: it is rebuilt when a run is recovered, it is reset when a
/// session is replaced, and it is per-process. A hub that ran before the
/// row-level guard held the same observation tens of thousands of times —
/// one run alone carried 132k copies of a single rollout completion.
///
/// So the append path declines to write the same observation twice, and
/// this pins that: the second copy is not written.
#[test]
fn an_identical_observation_is_recorded_once() {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');")
        .unwrap();
    let detail =
        r#"{"observation":{"source":"codex_rollout_task_complete"},"disposition":"accepted"}"#;
    let event = || HistoryEvent {
        run_id: 1,
        node_id: Some("build"),
        attempt: Some(1),
        kind: "observation",
        detail,
        source: Some("codex_rollout_task_complete"),
        disposition: Some("accepted"),
    };

    assert!(
        append_history(&db, event()).unwrap(),
        "the first copy is recorded"
    );
    assert!(
        !append_history(&db, event()).unwrap(),
        "the second copy is the same fact and is not recorded again"
    );

    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE kind='observation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
}

/// The same words for a different node, attempt or source are different
/// observations.
///
/// The identity of an observation is its run, node, attempt and source, so
/// the guard must not collapse two observations that differ in any of those.
#[test]
fn observations_with_a_different_identity_are_both_recorded() {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');")
        .unwrap();
    let detail =
        r#"{"observation":{"source":"codex_rollout_task_complete"},"disposition":"accepted"}"#;
    let event = |node: &'static str, attempt: i32, source: &'static str| HistoryEvent {
        run_id: 1,
        node_id: Some(node),
        attempt: Some(attempt),
        kind: "observation",
        detail,
        source: Some(source),
        disposition: Some("accepted"),
    };

    for (node, attempt, source) in [
        ("build", 1, "codex_rollout_task_complete"),
        ("test", 1, "codex_rollout_task_complete"),
        ("build", 2, "codex_rollout_task_complete"),
        ("build", 1, "agent_status_projection"),
    ] {
        assert!(
            append_history(&db, event(node, attempt, source)).unwrap(),
            "{node}/{attempt}/{source} is its own observation"
        );
    }

    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE kind='observation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 4);
}

/// A transition is a timeline, not a fact.
///
/// Only observations are deduped. Two rows of any other kind with the same
/// words are two moments in the run's story, and the guard must leave them
/// both.
#[test]
fn a_repeated_transition_is_still_recorded_twice() {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');")
        .unwrap();
    let event = || HistoryEvent {
        run_id: 1,
        node_id: Some("build"),
        attempt: Some(1),
        kind: "step_transition",
        detail: "unverified",
        source: Some("circuit_worker"),
        disposition: Some("applied"),
    };

    assert!(append_history(&db, event()).unwrap());
    assert!(
        append_history(&db, event()).unwrap(),
        "the same transition twice is two moments in the timeline"
    );

    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE kind='step_transition'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2);
}

/// A repeated observation does not move the run's revision.
///
/// The revision is what refuses an operator action the person has not seen
/// yet. A repeated fact is not a new fact, so it must not make a poll lose
/// the race against the person's click.
#[test]
fn a_repeated_observation_does_not_move_the_run_revision() {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');")
        .unwrap();
    let detail =
        r#"{"observation":{"source":"codex_rollout_task_complete"},"disposition":"accepted"}"#;
    let event = || HistoryEvent {
        run_id: 1,
        node_id: Some("build"),
        attempt: Some(1),
        kind: "observation",
        detail,
        source: Some("codex_rollout_task_complete"),
        disposition: Some("accepted"),
    };

    append_history(&db, event()).unwrap();
    let revision = revision_inner(&db, 1).unwrap();
    append_history(&db, event()).unwrap();

    assert_eq!(
        revision_inner(&db, 1).unwrap(),
        revision,
        "the same fact recorded again does not advance the run"
    );
    assert!(
        !revision_moved(&db, 1, revision).unwrap(),
        "and it does not refuse the person's action"
    );
}

/// The persisted native receipt carrying this exact `source_id`.
///
/// Addressing receipts by identity rather than by row position matters
/// here: the ledger interleaves recorded submissions with receipts, and a
/// positional index silently re-reads an earlier row when one is inserted.
/// A prefix or "either event" match is worse still — it returns the turn
/// start when the assertion meant the `Stop`. Requiring exactly one match
/// turns both mistakes into a loud failure.
fn receipt_for(
    db: &Connection,
    run_id: i64,
    source_id: &str,
) -> crate::services::circuit_worker::native_hooks::NativeReceipt {
    let found: Vec<_> = history_inner(db, run_id)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == "native_hook_received")
        .filter(|entry| {
            entry
                .detail
                .contains(&format!("\"source_id\":\"{source_id}\""))
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one receipt with source_id {source_id}, found {}",
        found.len()
    );
    serde_json::from_str(&found[0].detail).unwrap()
}

/// The newest history entry for run 1 as `(id, detail, source)`, so an
/// attestation's wording can be asserted directly rather than inferred from
/// the absence of an error, and the next write can be fenced on it.
fn last_recorded_outcome(db: &Connection) -> (i64, String, Option<String>) {
    let row: (i64, String, Option<String>) = db
            .query_row(
                "SELECT id, detail, source FROM circuit_run_history WHERE run_id=1 ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
    row
}

/// How many native receipts the run has recorded.
fn receipt_count(db: &Connection, run_id: i64) -> usize {
    history_inner(db, run_id)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == "native_hook_received")
        .count()
}

#[test]
fn readiness_history_retains_reason_without_duplicate_poll_entries() {
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'source',1,'unverified');").unwrap();
    let waiting =
        serde_json::json!({"node.source.observation_blocker":"{\"kind\":\"input_uncertain\"}"})
            .to_string();
    for _ in 0..3 {
        super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&waiting), &[])
            .unwrap();
    }
    let history = history_inner(&db, 1).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].kind, "observation_readiness");
    assert_eq!(history[0].disposition.as_deref(), Some("waiting"));
    assert!(history[0]
        .detail
        .contains("Terminal input tracking is uncertain"));
    let resolved = serde_json::json!({"node.source.observation_blocker":""}).to_string();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&resolved), &[])
        .unwrap();
    let history = history_inner(&db, 1).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].disposition.as_deref(), Some("resolved"));
}

#[test]
fn input_freshness_rejections_are_distinct_from_transition_conflicts() {
    assert!(is_observation_freshness_rejection(
        &observation_freshness_rejection("a newer input was submitted",)
    ));
    assert!(!is_observation_freshness_rejection(
        &rusqlite::Error::InvalidQuery,
    ));
    assert!(!is_observation_freshness_rejection(
        &rusqlite::Error::InvalidParameterName("unrelated failure".into()),
    ));
}

#[test]
fn circuit_continuation_history_retains_dispatch_uncertainty_without_replay() {
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');").unwrap();
    let mut context = serde_json::json!({"node.verdict.continuation.attempt":"1","node.verdict.continuations.1":"1","node.verdict.continuation.delivery":"claimed"});
    for _ in 0..2 {
        super::super::ledger::commit_circuit_advance_locked(
            &mut db,
            1,
            None,
            Some(&context.to_string()),
            &[],
        )
        .unwrap();
    }
    context["node.verdict.continuation.delivery"] = "uncertain".into();
    super::super::ledger::commit_circuit_advance_locked(
        &mut db,
        1,
        None,
        Some(&context.to_string()),
        &[],
    )
    .unwrap();
    let history = history_inner(&db, 1).unwrap();
    let states: Vec<String> = history
        .iter()
        .map(|entry| {
            serde_json::from_str::<serde_json::Value>(&entry.detail).unwrap()["state"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(states, vec!["intent", "possible_dispatch", "uncertain"]);
    assert!(history
        .iter()
        .all(|entry| entry.node_id.as_deref() == Some("verdict") && entry.attempt == Some(1)));
}

#[test]
fn circuit_queue_wait_history_records_reason_changes_without_poll_duplicates() {
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'pending');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'spawn',1,'pending_slot');").unwrap();
    record_queue_wait_locked(&mut db, 1, QueueWaitReason::CircuitDisabled).unwrap();
    record_queue_wait_locked(&mut db, 1, QueueWaitReason::CircuitDisabled).unwrap();
    record_queue_wait_locked(&mut db, 1, QueueWaitReason::MeshCapacity { capacity: 3 }).unwrap();
    assert_eq!(history_inner(&db, 1).unwrap().len(), 2);
    assert!(history_inner(&db, 1).unwrap()[1]
        .detail
        .contains("mesh_capacity"));
    assert_eq!(
        queue_waits_inner(&db, 1)
            .unwrap()
            .get("1")
            .map(String::as_str),
        Some(r#"{"reason":"mesh_capacity","capacity":3}"#)
    );
    assert!(queue_waits_inner(&db, 2).unwrap().is_empty());
    db.execute(
        "UPDATE autopilot_circuit_runs SET state='cancelled' WHERE id=1",
        [],
    )
    .unwrap();
    record_queue_wait_locked(&mut db, 1, QueueWaitReason::ReservationUnavailable).unwrap();
    assert!(queue_waits_inner(&db, 1).unwrap().is_empty());
    assert_eq!(
        history_inner(&db, 1).unwrap().len(),
        2,
        "late capacity reports cannot reopen cancelled queue waits"
    );
    db.execute(
        "UPDATE autopilot_circuit_runs SET state='running' WHERE id=1",
        [],
    )
    .unwrap();
    let context =
        serde_json::json!({"node.spawn.capacity_wait":"{\"agent_limit\":true}"}).to_string();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&context), &[])
        .unwrap();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&context), &[])
        .unwrap();
    let history = history_inner(&db, 1).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[2].kind, "step_capacity_wait");
    assert_eq!(history[2].node_id.as_deref(), Some("spawn"));
    // Provenance and identity (issue #1909): the run-level admission wait
    // names its source and disposition but has no step identity, while the
    // step capacity wait names the parked step's attempt.
    for entry in &history[..2] {
        assert_eq!(entry.kind, "queue_wait");
        assert_eq!(entry.source.as_deref(), Some(SOURCE_ADMISSION));
        assert_eq!(entry.disposition.as_deref(), Some(DISPOSITION_WAITING));
        assert_eq!((entry.node_id.as_deref(), entry.attempt), (None, None));
    }
    assert_eq!(history[2].source.as_deref(), Some(SOURCE_CAPACITY));
    assert_eq!(history[2].disposition.as_deref(), Some(DISPOSITION_WAITING));
    assert_eq!(history[2].attempt, Some(1));
}

/// Issue #1909: a freed capacity or evidence window is a resolution, not an
/// active wait — the history must say so, or the operator surface renders a
/// cleared wait as still parked.
#[test]
fn circuit_wait_history_records_resolution_when_capacity_and_evidence_clear() {
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'spawn',1,'pending_slot');").unwrap();
    let parked =
        serde_json::json!({"node.spawn.capacity_wait":"{\"agent_limit\":true}"}).to_string();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&parked), &[])
        .unwrap();
    let freed =
        serde_json::json!({"node.spawn.capacity_wait":"{\"agent_limit\":false}"}).to_string();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&freed), &[])
        .unwrap();
    let history = history_inner(&db, 1).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].kind, "step_capacity_wait");
    assert_eq!(history[0].disposition.as_deref(), Some(DISPOSITION_WAITING));
    assert_eq!(history[1].kind, "step_capacity_wait");
    assert_eq!(
        history[1].disposition.as_deref(),
        Some(DISPOSITION_RESOLVED),
        "a freed capacity window is a resolution, not an active wait"
    );

    // The same rule applies to an evidence wait window: opening binds,
    // clearing resolves. The real clear writes an empty attempt string
    // (`CircuitContext::set(key, "")`), so identity must survive it.
    let waiting = serde_json::json!({
            "node.spawn.capacity_wait":"{\"agent_limit\":false}",
            "node.spawn.wait.attempt":"1","node.spawn.wait.timeout_ms":"60000","node.spawn.wait.since_ms":"1000"
        }).to_string();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&waiting), &[])
        .unwrap();
    let resolved = serde_json::json!({
        "node.spawn.capacity_wait":"{\"agent_limit\":false}",
        "node.spawn.wait.attempt":""
    })
    .to_string();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&resolved), &[])
        .unwrap();
    let windows: Vec<CircuitHistoryEntry> = history_inner(&db, 1)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == "evidence_window_changed")
        .collect();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].disposition.as_deref(), Some(DISPOSITION_WAITING));
    assert_eq!(windows[0].attempt, Some(1));
    assert_eq!(
        windows[1].disposition.as_deref(),
        Some(DISPOSITION_RESOLVED)
    );
    assert_eq!(
        windows[1].attempt,
        Some(1),
        "a resolved evidence wait keeps the attempt it was parked on"
    );
}

#[test]
fn circuit_configuration_and_wait_history_is_atomic_deduplicated_and_retained() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut db = Connection::open(file.path()).unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name,graph_json) VALUES(1,1,'test','{}');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');").unwrap();
    let configuration = crate::preferences::spawn_configurations::SpawnConfiguration {
        id: "launch/reviewer".into(),
        spawn_option_id: "codex".into(),
        model: Some("gpt-6-luna".into()),
        effort: Some("low".into()),
        extra_args: Some("--token private-secret".into()),
        ..Default::default()
    };
    let context = serde_json::json!({"review.launch.reviewer":serde_json::to_string(&configuration).unwrap()}).to_string();
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [&context],
    )
    .unwrap();
    {
        let tx = db.transaction().unwrap();
        pin_graph(&tx, 1).unwrap();
        pin_graph(&tx, 1).unwrap();
        tx.commit().unwrap();
    }
    let history = history_inner(&db, 1).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].kind, "configuration_pinned");
    assert!(history[0].detail.contains("gpt-6-luna"));
    assert!(!history[0].detail.contains("private-secret"));
    // Configuration revision provenance (issue #1909).
    assert_eq!(history[0].source.as_deref(), Some(SOURCE_RUN_CONFIGURATION));
    assert_eq!(history[0].disposition.as_deref(), Some(DISPOSITION_APPLIED));
    let mut next: serde_json::Value = serde_json::from_str(&context).unwrap();
    next["node.spawn.wait.attempt"] = "1".into();
    next["node.spawn.wait.since_ms"] = "1000".into();
    next["node.spawn.wait.timeout_ms"] = "60000".into();
    let next = next.to_string();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&next), &[])
        .unwrap();
    super::super::ledger::commit_circuit_advance_locked(&mut db, 1, None, Some(&next), &[])
        .unwrap();
    assert_eq!(history_inner(&db, 1).unwrap().len(), 2);
    db.execute_batch("CREATE TRIGGER fail_wait_history BEFORE INSERT ON circuit_run_history BEGIN SELECT RAISE(ABORT,'injected history failure'); END;").unwrap();
    let changed = next.replace("1000", "2000");
    assert!(super::super::ledger::commit_circuit_advance_locked(
        &mut db,
        1,
        None,
        Some(&changed),
        &[]
    )
    .is_err());
    assert_eq!(
        db.query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id=1",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        next
    );
    drop(db);
    let db = Connection::open(file.path()).unwrap();
    let history = history_inner(&db, 1).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].kind, "evidence_window_changed");
    assert_eq!(history[1].attempt, Some(1));
    assert!(history[1].detail.contains("60000"));
    assert_eq!(history[1].source.as_deref(), Some(SOURCE_RECONCILIATION));
    assert_eq!(history[1].disposition.as_deref(), Some(DISPOSITION_WAITING));
}

/// Issue #1909 acceptance: the wait / capacity / configuration history
/// survives an app restart and agrees with the materialized projection.
#[test]
fn circuit_wait_capacity_and_configuration_history_survives_reopen_and_agrees_with_projection() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut db = Connection::open(file.path()).unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name,graph_json) VALUES(1,1,'test','{}');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'pending');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'spawn',2,'pending_slot');").unwrap();
    // A pending run parked on mesh admission, then admitted.
    record_queue_wait_locked(&mut db, 1, QueueWaitReason::MeshCapacity { capacity: 2 }).unwrap();
    // The pinned configuration is written on run creation; pin it here and
    // assert the entry agrees with `circuit_run_snapshots` after reopen.
    {
        let tx = db.transaction().unwrap();
        pin_graph(&tx, 1).unwrap();
        tx.commit().unwrap();
    }
    let context = serde_json::json!({
        "node.spawn.capacity_wait": "{\"agent_limit\":true}",
        "node.spawn.wait.attempt": "2",
        "node.spawn.wait.timeout_ms": "60000",
        "node.spawn.wait.since_ms": "1000"
    })
    .to_string();
    // Queue exit: pending -> running commits the `run_transition` entry.
    super::super::ledger::commit_circuit_advance_locked(
        &mut db,
        1,
        Some("running"),
        Some(&context),
        &[],
    )
    .unwrap();
    drop(db);

    // Restart: reopen the same file and read the history back.
    let db = Connection::open(file.path()).unwrap();
    let history = history_inner(&db, 1).unwrap();
    let entry = |kind: &str| {
        history
            .iter()
            .find(|entry| entry.kind == kind)
            .unwrap_or_else(|| panic!("{kind} history retained across reopen"))
    };
    // Identity, time, source and disposition on every retained event.
    assert!(history.iter().all(|entry| entry.source.is_some()
        && entry.disposition.is_some()
        && !entry.observed_at.is_empty()));
    assert_eq!(
        (
            entry("queue_wait").source.as_deref(),
            entry("queue_wait").disposition.as_deref(),
            entry("queue_wait").attempt
        ),
        (Some(SOURCE_ADMISSION), Some(DISPOSITION_WAITING), None),
    );
    assert_eq!(
        (
            entry("step_capacity_wait").node_id.as_deref(),
            entry("step_capacity_wait").attempt
        ),
        (Some("spawn"), Some(2)),
    );
    assert_eq!(
        (
            entry("evidence_window_changed").node_id.as_deref(),
            entry("evidence_window_changed").attempt
        ),
        (Some("spawn"), Some(2)),
    );
    let configuration = entry("configuration_pinned");
    assert_eq!(
        (
            configuration.source.as_deref(),
            configuration.disposition.as_deref()
        ),
        (Some(SOURCE_RUN_CONFIGURATION), Some(DISPOSITION_APPLIED))
    );
    let run_transition = entry("run_transition");
    assert_eq!(run_transition.detail, "running");
    assert_eq!(
        (
            run_transition.source.as_deref(),
            run_transition.disposition.as_deref()
        ),
        (Some(SOURCE_CIRCUIT_WORKER), Some(DISPOSITION_APPLIED))
    );

    // Agreement with the materialized projection: the parked step, the
    // admitted run state and the pinned snapshot all match their history.
    let (state, step_attempt, step_status): (String, i32, String) = db
        .query_row(
            "SELECT r.state, s.attempt, s.status FROM autopilot_circuit_runs r
             JOIN autopilot_circuit_run_steps s ON s.run_id=r.id WHERE r.id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (state.as_str(), step_attempt, step_status.as_str()),
        ("running", 2, "pending_slot")
    );
    let snapshot_revision: i64 = db
        .query_row(
            "SELECT behavior_revision FROM circuit_run_snapshots WHERE run_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&configuration.detail).unwrap()
            ["behavior_revision"]
            .as_i64(),
        Some(snapshot_revision),
    );
}

#[test]
fn native_receipt_is_durable_deduplicated_and_scoped_to_active_attempts() {
    use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut db = Connection::open(file.path()).unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',2,'running',9);").unwrap();
    let mut graph = crate::circuit::model::CircuitGraph::walking_skeleton("");
    graph
        .nodes
        .iter_mut()
        .find(|n| n.id == "inject")
        .unwrap()
        .kind = crate::circuit::model::CircuitNodeKind::LlmTurnClassifier {
        target_node_id: None,
    };
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let mut receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("1:2".into()), session_incarnation: Some("1000".into()), source_id: "event-1".into(), received_at_ms: 1, turn_fenced: true, explicit_turn_mismatch: false, submission_correlated: false, submission_seq: None,
            hook: NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"prompt","hook_event_name":"Stop","background_tasks":[],"session_crons":[],"last_assistant_message":"Complete final report"}"#).unwrap() };
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    drop(db);
    let mut db = Connection::open(file.path()).unwrap();
    receipt.received_at_ms = 2;
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    let history = history_inner(&db, 1).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].node_id.as_deref(), Some("spawn"));
    assert_eq!(history[0].attempt, Some(2));
    let persisted: NativeReceipt = serde_json::from_str(&history[0].detail).unwrap();
    assert_eq!(persisted.received_at_ms, 1);
    assert_eq!(
        persisted.hook.final_report.as_deref(),
        Some("Complete final report")
    );
    db.execute_batch("UPDATE autopilot_circuit_run_steps SET status='completed';
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status) VALUES (1,'inject',1,'running');").unwrap();
    receipt.source_id = "event-2".into();
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    let history = history_inner(&db, 1).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[1].node_id.as_deref(),
        Some("inject"),
        "completed spawn retains the target identity for its running classifier"
    );
    db.execute("UPDATE autopilot_circuit_runs SET state='cancelled'", [])
        .unwrap();
    receipt.source_id = "event-3".into();
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    assert_eq!(history_inner(&db, 1).unwrap().len(), 2);
    for state in ["running", "paused", "completed", "cancelled"] {
        db.execute("UPDATE autopilot_circuit_runs SET state=?1", [state])
            .unwrap();
        let view = evidence_view_inner(&db, 1).unwrap();
        assert_eq!(
            view.coverage.len(),
            2,
            "capabilities retained while {state}"
        );
        assert!(view.coverage.iter().all(|item| item.deadline_ms.is_none()));
    }
}

#[test]
fn coverage_resolution_waits_until_the_connection_is_released() {
    // Resolving a harness reads preferences, which can cold-load a file.
    // The database pass must therefore only record which provider each row
    // needs resolved; `history` resolves after releasing its connection.
    let file = tempfile::NamedTempFile::new().unwrap();
    let db = Connection::open(file.path()).unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path,provider) VALUES (9,1,'owned','/repo','claude:minimax');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [
            crate::circuit::model::CircuitGraph::walking_skeleton("work")
                .to_json()
                .unwrap(),
        ],
    )
    .unwrap();
    let (unresolved, pending) = evidence_view_unresolved(&db, 1).unwrap();
    assert_eq!(
        pending,
        vec![Some(crate::circuit::strategy::HarnessSelector::Stored(
            "claude:minimax".into()
        ))]
    );
    assert_eq!(
        unresolved.coverage[0].capabilities.harness, "missing-agent",
        "the database pass must not resolve the harness"
    );
    let resolved = evidence_view_inner(&db, 1).unwrap();
    assert_eq!(resolved.coverage[0].capabilities.harness, "anthropic");
}

#[test]
fn native_delayed_stop_keeps_its_original_turn_input_after_new_submission() {
    use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut db = Connection::open(file.path()).unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [
            crate::circuit::model::CircuitGraph::walking_skeleton("work")
                .to_json()
                .unwrap(),
        ],
    )
    .unwrap();
    // Two real submissions: input A then input B (issue #1898). B is
    // submitted before A's turn start hook lands, so A's start describes
    // a superseded turn.
    record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "first prompt").unwrap();
    record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "second prompt").unwrap();
    let mut receipt = NativeReceipt { agent_node_id: 9, input_stamp: Some("input-a".into()), session_incarnation: Some("1000".into()),
            source_id: "start-a".into(), received_at_ms: 1000, turn_fenced: true, explicit_turn_mismatch: false, submission_correlated: true, submission_seq: None,
            hook: NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"a","hook_event_name":"UserPromptSubmit","prompt":"first prompt"}"#).unwrap() };
    // The delayed first start is refused: B is the newest submission, so
    // A's turn cannot acknowledge it, and a receipt can never vouch for
    // itself by asserting `submission_correlated`.
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    let refused = receipt_for(&db, 1, "start-a");
    assert!(!refused.submission_correlated);
    assert_eq!(refused.input_stamp, None);
    assert_eq!(refused.submission_seq, None);
    drop(db);
    let mut db = Connection::open(file.path()).unwrap();
    // Input B's turn has not been observed, so A's stop cannot inherit a
    // binding either. This asserts on the Stop receipt itself, not on the
    // start receipt recorded above it.
    receipt.input_stamp = Some("input-b".into());
    receipt.source_id = "stop-a".into();
    receipt.received_at_ms = 2000;
    receipt.hook = NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"a","hook_event_name":"Stop","last_assistant_message":"Old approval"}"#).unwrap();
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    let unbound_stop = receipt_for(&db, 1, "stop-a");
    assert_eq!(
        unbound_stop.input_stamp, None,
        "a Stop with no bound turn start keeps no input stamp"
    );
    assert!(!unbound_stop.submission_correlated);
    assert_eq!(unbound_stop.submission_seq, None);
    // B is observed in order, so it does bind, and its Stop inherits both
    // the stamp and the submission ordinal.
    receipt.hook = NativeHook::parse("claude", br#"{"session_id":"session","prompt_id":"b","hook_event_name":"UserPromptSubmit","prompt":"second prompt"}"#).unwrap();
    receipt.source_id = "start-b".into();
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    let current_start = receipt_for(&db, 1, "start-b");
    assert!(current_start.submission_correlated);
    assert_eq!(current_start.input_stamp.as_deref(), Some("input-b"));
    assert_eq!(
        current_start.submission_seq,
        Some(2),
        "B is the second recorded submission"
    );
    receipt.hook = NativeHook::parse(
        "claude",
        br#"{"session_id":"session","prompt_id":"b","hook_event_name":"Stop"}"#,
    )
    .unwrap();
    receipt.source_id = "stop-b".into();
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    let bound_stop = receipt_for(&db, 1, "stop-b");
    assert_eq!(bound_stop.input_stamp.as_deref(), Some("input-b"));
    assert!(bound_stop.submission_correlated);
    assert_eq!(
        bound_stop.submission_seq.as_ref(),
        current_start.submission_seq.as_ref(),
        "the Stop inherits the exact submission its turn start bound"
    );
    // An unobserved start cannot acquire authority from receipt-time input.
    receipt.hook.turn_id = Some("missing-start".into());
    receipt.source_id = "stop-c".into();
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    let missing = receipt_for(&db, 1, "stop-c");
    assert_eq!(missing.input_stamp, None);
    assert!(!missing.submission_correlated);
    assert_eq!(missing.submission_seq, None);
}

#[test]
fn claude_submission_correlation_binds_a_turn_only_to_a_provable_submission() {
    use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [
            crate::circuit::model::CircuitGraph::walking_skeleton("work")
                .to_json()
                .unwrap(),
        ],
    )
    .unwrap();

    // Buildmesh submits exactly this text.
    record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "run the tests").unwrap();
    let submit = |turn: &str, prompt: &str, stamp: &str| NativeReceipt {
        agent_node_id: 9,
        input_stamp: Some(stamp.into()),
        session_incarnation: Some("1000".into()),
        source_id: format!("start-{turn}"),
        received_at_ms: 1000,
        turn_fenced: true,
        explicit_turn_mismatch: false,
        // Never trusted: correlation is decided from durable evidence.
        submission_correlated: true,
        submission_seq: None,
        hook: NativeHook::parse(
            "claude",
            serde_json::to_vec(&serde_json::json!({
                "session_id": "session", "prompt_id": turn,
                "hook_event_name": "UserPromptSubmit", "prompt": prompt,
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap(),
    };
    let stop = |turn: &str| NativeReceipt {
        agent_node_id: 9,
        input_stamp: Some("1:9".into()),
        session_incarnation: Some("1000".into()),
        source_id: format!("stop-{turn}"),
        received_at_ms: 2000,
        turn_fenced: true,
        explicit_turn_mismatch: false,
        submission_correlated: true,
        submission_seq: None,
        hook: NativeHook::parse(
            "claude",
            serde_json::to_vec(&serde_json::json!({
                "session_id": "session", "prompt_id": turn, "hook_event_name": "Stop",
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap(),
    };

    // Happy path: the harness echoed back the exact text Buildmesh
    // wrote, so the turn token is bound to that submission.
    let start = submit("turn-1", "run the tests", "1:9");
    receive_native_hook_locked(&mut db, &start).unwrap();
    let start_receipt = receipt_for(&db, 1, "start-turn-1");
    assert!(
        start_receipt.submission_correlated,
        "content match earns the binding"
    );
    assert_eq!(start_receipt.input_stamp.as_deref(), Some("1:9"));
    assert_eq!(
        start_receipt.submission_seq,
        Some(1),
        "the first recorded submission"
    );
    receive_native_hook_locked(&mut db, &stop("turn-1")).unwrap();
    // Addressed by the Stop's own source_id. A lookup that also matched
    // the turn start would return that row instead and pass even if Stop
    // correlation had failed entirely.
    let stop_receipt = receipt_for(&db, 1, "stop-turn-1");
    assert!(
        stop_receipt.submission_correlated,
        "Stop inherits the turn's binding"
    );
    assert_eq!(stop_receipt.input_stamp.as_deref(), Some("1:9"));
    assert_eq!(
        stop_receipt.submission_seq, start_receipt.submission_seq,
        "the terminal receipt records which submission it completed"
    );
    // Redelivery of the same start is idempotent, not a second claim.
    receive_native_hook_locked(&mut db, &start).unwrap();
    assert_eq!(
        receipt_count(&db, 1),
        2,
        "redelivery is deduplicated, not appended"
    );

    // A different turn reporting the same text must not take over a
    // submission another turn already acknowledged.
    receive_native_hook_locked(&mut db, &submit("turn-2", "run the tests", "1:9")).unwrap();
    let second = receipt_for(&db, 1, "start-turn-2");
    assert!(
        !second.submission_correlated,
        "one submission, one acknowledged turn"
    );
    assert_eq!(second.input_stamp, None);
    assert_eq!(second.submission_seq, None);

    // Text Buildmesh never submitted cannot claim anything.
    receive_native_hook_locked(&mut db, &submit("turn-3", "an operator prompt", "1:9")).unwrap();
    let foreign = receipt_for(&db, 1, "start-turn-3");
    assert!(!foreign.submission_correlated);
    assert_eq!(foreign.input_stamp, None);
    // A hook with no turn token has nothing to bind in the first place.
    let untokened = NativeReceipt {
            source_id: "start-no-token".into(),
            hook: NativeHook::parse(
                "claude",
                br#"{"session_id":"session","hook_event_name":"UserPromptSubmit","prompt":"run the tests"}"#,
            )
            .unwrap(),
            ..submit("unused", "run the tests", "1:9")
        };
    receive_native_hook_locked(&mut db, &untokened).unwrap();
    let anonymous = receipt_for(&db, 1, "start-no-token");
    assert!(
        !anonymous.submission_correlated,
        "no prompt_id means no turn to bind"
    );
    assert_eq!(anonymous.input_stamp, None);

    // A start hook that arrives after Buildmesh submitted again describes
    // an older turn, so it must not claim the newer submission.
    record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "run the linter").unwrap();
    receive_native_hook_locked(&mut db, &submit("turn-late", "run the tests", "1:11")).unwrap();
    let late = receipt_for(&db, 1, "start-turn-late");
    assert!(
        !late.submission_correlated,
        "a delayed start cannot claim a superseded submission"
    );
    assert_eq!(late.input_stamp, None);
    // ...and its Stop is equally uncorrelated.
    receive_native_hook_locked(&mut db, &stop("turn-late")).unwrap();
    let late_stop = receipt_for(&db, 1, "stop-turn-late");
    assert!(!late_stop.submission_correlated);
    assert_eq!(late_stop.input_stamp, None);
    assert_eq!(late_stop.submission_seq, None);
    // The current submission still correlates, proving the refusal above
    // was about ordering and not a broken digest comparison — and that a
    // later submission is free to claim its own turn.
    receive_native_hook_locked(&mut db, &submit("turn-4", "run the linter", "1:11")).unwrap();
    let current = receipt_for(&db, 1, "start-turn-4");
    assert!(current.submission_correlated);
    assert_eq!(current.submission_seq, Some(2));

    // A submission recorded against another step is not this step's
    // evidence, even for the same agent and the same text.
    record_prompt_submission_locked(&db, 1, "other-step", 1, 9, "cross-step text").unwrap();
    receive_native_hook_locked(&mut db, &submit("turn-5", "cross-step text", "1:12")).unwrap();
    let crossed = receipt_for(&db, 1, "start-turn-5");
    assert!(
        !crossed.submission_correlated,
        "another step's submission cannot bind this step"
    );
    assert_eq!(crossed.input_stamp, None);
}

#[test]
fn claude_submission_correlation_never_accepts_a_receipt_time_claim() {
    use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [
            crate::circuit::model::CircuitGraph::walking_skeleton("work")
                .to_json()
                .unwrap(),
        ],
    )
    .unwrap();
    // No submission was ever recorded, so nothing can be correlated even
    // though the receipt asserts it and carries a plausible turn token.
    let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("1:2".into()),
            session_incarnation: Some("1000".into()),
            source_id: "claimed".into(),
            received_at_ms: 1,
            turn_fenced: true,
            explicit_turn_mismatch: false,
            submission_correlated: true, submission_seq: None,
            hook: NativeHook::parse(
                "claude",
                br#"{"session_id":"session","prompt_id":"turn-1","hook_event_name":"UserPromptSubmit","prompt":"anything"}"#,
            )
            .unwrap(),
        };
    receive_native_hook_locked(&mut db, &receipt).unwrap();
    let history = history_inner(&db, 1).unwrap();
    let stored: NativeReceipt = serde_json::from_str(&history[0].detail).unwrap();
    assert!(
        !stored.submission_correlated,
        "a receipt cannot vouch for itself"
    );
    assert_eq!(
        stored.input_stamp, None,
        "an uncorrelated receipt keeps no input authority"
    );
}

#[test]
fn recorded_prompt_submissions_never_persist_the_prompt_text() {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo'),(10,1,'other','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'spawn',1,'running',9);").unwrap();
    record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "deploy with token hunter2").unwrap();
    let detail: String = db
        .query_row(
            "SELECT detail FROM circuit_run_history WHERE kind='prompt_submitted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !detail.contains("hunter2"),
        "the ledger must keep only the digest"
    );
    let submission: PromptSubmission = serde_json::from_str(&detail).unwrap();
    assert_eq!(submission.agent_node_id, 9);
    assert_eq!(submission.submission_seq, 1);
    assert_eq!(
        submission.prompt_digest,
        crate::services::circuit_worker::native_hooks::submission_digest(
            "deploy with token hunter2"
        )
    );
    // A second submission for the same agent advances the ordinal; a
    // different agent has its own sequence.
    record_prompt_submission_locked(&db, 1, "spawn", 1, 9, "next").unwrap();
    record_prompt_submission_locked(&db, 1, "spawn", 1, 10, "other node").unwrap();
    let seqs: Vec<i64> = db
            .prepare("SELECT json_extract(detail,'$.submission_seq') FROM circuit_run_history WHERE kind='prompt_submitted' ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<SqlResult<Vec<_>>>()
            .unwrap();
    assert_eq!(
        seqs,
        vec![1, 2, 1],
        "ordinals are per agent node, not global"
    );
}

/// Issue #1932 — `newest_submission_seq` runs on the prompt-submission and
/// receipt-binding hot paths, while `circuit_run_history` grows with all
/// Circuit activity on the install. The plan is read from the same SQL
/// constant production runs, so a dropped index, or a query term that no
/// longer matches the indexed expression, fails here instead of quietly
/// restoring the full scan. The rows span two agent nodes and a
/// non-submission kind because the partial index is only useful if those
/// still resolve through one indexed seek.
#[test]
fn submission_ordinal_lookup_uses_its_partial_index() {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail) VALUES
                (1,'spawn',1,'prompt_submitted','{\"agent_node_id\":9,\"submission_seq\":2}'),
                (1,'spawn',1,'step_transition','{\"agent_node_id\":9,\"status\":\"working\"}'),
                (1,'spawn',1,'prompt_submitted','{\"agent_node_id\":10,\"submission_seq\":1}');")
            .unwrap();

    let mut stmt = db
        .prepare(&format!("EXPLAIN QUERY PLAN {NEWEST_SUBMISSION_SEQ_SQL}"))
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map(params![9_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<SqlResult<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.contains("idx_circuit_history_submission")),
        "the ordinal lookup must drive idx_circuit_history_submission, got: {plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|step| step.starts_with("SCAN circuit_run_history")),
        "the ordinal lookup must not full-scan circuit_run_history, got: {plan:?}"
    );

    // Index or no index, the ledger answers from the same rows: each
    // agent reads back its own sequence, and an unknown agent reads zero.
    assert_eq!(newest_submission_seq(&db, 9).unwrap(), 2);
    assert_eq!(newest_submission_seq(&db, 10).unwrap(), 1);
    assert_eq!(newest_submission_seq(&db, 11).unwrap(), 0);
}

#[test]
fn a_wake_up_is_recorded_as_a_prompt_submission_for_its_owning_step() {
    // Issue #2105: the nudge that wakes a stalled session is a real
    // submission, so it must land in the ledger bound to the step that
    // earned it — otherwise a later receipt could not be attributed.
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path) VALUES (9,1,'owned','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,attempt,status,agent_node_id) VALUES (1,'work',2,'running',9);")
            .unwrap();

    record_prompt_submission_locked(
            &db,
            1,
            "work",
            2,
            9,
            "Your background task bg_f1045ea6-225e-46cf-bc5f-10249ffa83ca has finished and its result has not been reported yet.",
        )
        .unwrap();

    let row: (String, Option<String>, Option<i64>) = db
            .query_row(
                "SELECT node_id, detail, attempt FROM circuit_run_history WHERE kind='prompt_submitted'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
    assert_eq!(row.0, "work");
    assert_eq!(row.2, Some(2), "the wake-up belongs to its own attempt");
    let detail = row.1.unwrap();
    assert!(
        !detail.contains("bg_f1045ea6"),
        "only the digest is persisted, never the prompt text"
    );
    let submission: PromptSubmission = serde_json::from_str(&detail).unwrap();
    assert_eq!(submission.agent_node_id, 9);
    assert_eq!(submission.submission_seq, 1);
}

/// A person rechecks a checkpoint; the Codex pull returns only the completion
/// it had already recorded. That repeated ownership gap cannot resolve the
/// checkpoint, so the recheck must end with the original checkpoint back in
/// place rather than leaving the step Running with its reason cleared. When
/// the checkpoint being rechecked was the ownership gap itself, which no longer
/// needs a person, the step stays Running.
#[test]
fn a_recheck_that_finds_only_repeated_ownership_evidence_restores_the_checkpoint() {
    use crate::circuit::{
        context::CircuitContext,
        model::{CircuitGraph, CircuitNodeKind},
        observation::{CircuitObservation, ObservationIdentity, ObservedWorkFact, WorkEvidence},
        stepper::{advance, CircuitEvent, RunState, RunView, StepStatus, StepView},
    };
    let gap = "Codex recorded a completed foreground turn, but its rollout cannot verify all owned child and background work.";
    let identity = ObservationIdentity {
        run_id: 1,
        step_id: "spawn".into(),
        attempt: 1,
        agent_node_id: 9,
        session_incarnation: Some("1000".into()),
        session_id: Some("session".into()),
        turn_id: Some("turn-1".into()),
        report_revision: None,
    };
    let completion = CircuitObservation {
        identity: identity.clone(),
        source: "codex_rollout".into(),
        source_id: Some("turn-1:2000:1".into()),
        observed_at_ms: 2000,
        authoritative: true,
        fact: ObservedWorkFact::OwnershipUnavailable { reason: gap.into() },
    };
    let rechecked_view = |checkpoint: &str| {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'agent','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id) VALUES(1,1,1,'running',9);
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id) VALUES(1,'spawn',1,'unverified',9);").unwrap();
        db.execute(
            "UPDATE autopilot_circuit_run_steps SET error_message=?1",
            [checkpoint],
        )
        .unwrap();
        let mut graph = CircuitGraph::walking_skeleton("");
        graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "spawn")
            .unwrap()
            .kind = CircuitNodeKind::LlmTurnClassifier {
            target_node_id: Some("$source".into()),
        };
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        let mut evidence = WorkEvidence::default();
        evidence.observe(&identity, &completion);
        let mut context = CircuitContext::default();
        context.set("source.agent_id", "9");
        context.set(
            "node.spawn.evidence.1",
            serde_json::to_string(&evidence).unwrap(),
        );
        db.execute(
            "UPDATE autopilot_circuit_runs SET context_json=?1",
            [context.to_json().unwrap()],
        )
        .unwrap();
        record_outcome_locked(
            &mut db,
            &CheckpointRequest {
                run_id: 1,
                node_id: "spawn".into(),
                attempt: 1,
                expected_revision: 0,
                action: CheckpointAction::Recheck,
                reason: String::new(),
            },
        )
        .unwrap();
        let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let step = super::super::ledger::list_circuit_run_steps_inner(&db, 1)
            .unwrap()
            .remove(0);
        assert_eq!(step.status, "running", "the recheck reopens the step");
        assert_eq!(step.error_message, None);
        RunView {
            run_id: 1,
            state: RunState::Running,
            graph,
            context: CircuitContext::from_json(&stored.context_json).unwrap(),
            steps: vec![StepView {
                node_id: "spawn".into(),
                attempt: 1,
                status: StepStatus::Running,
                agent_node_id: Some(9),
                outcome: None,
                error: None,
            }],
        }
    };
    let repeat = CircuitEvent::Observed {
        expected: identity.clone(),
        observation: Box::new(completion.clone()),
    };

    let delivery = "Prompt delivery is unverified: The harness did not confirm the pasted prompt";
    let mut view = rechecked_view(delivery);
    let transition = advance(&mut view, &repeat);
    assert_eq!(view.steps[0].status, StepStatus::Unverified);
    let error = view.steps[0].error.clone().unwrap();
    assert!(error.contains(delivery), "{error}");
    assert!(error.contains("recheck"), "{error}");
    assert_eq!(transition.step_writes.len(), 1);
    assert!(
        transition.observations.is_empty(),
        "the repeat is still not recorded as a new observation"
    );
    // A second unchanged pass writes nothing more.
    assert!(advance(&mut view, &repeat).step_writes.is_empty());

    let mut view = rechecked_view(gap);
    advance(&mut view, &repeat);
    assert_eq!(view.steps[0].status, StepStatus::Running);
    assert_eq!(view.steps[0].error, None);
}

#[test]
fn recheck_retains_ownership_uncertainty_and_cannot_authorize_classifier_completion() {
    use crate::circuit::{
        context::CircuitContext,
        model::{CircuitGraph, CircuitNodeKind},
        observation::WorkEvidence,
        stepper::{advance, CircuitEvent, RunState, RunView, StepStatus, StepView},
    };
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'agent','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id) VALUES(1,1,1,'running',9);
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id) VALUES(1,'spawn',1,'unverified',9);").unwrap();
    let mut graph = CircuitGraph::walking_skeleton("");
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "spawn")
        .unwrap()
        .kind = CircuitNodeKind::LlmTurnClassifier {
        target_node_id: Some("$source".into()),
    };
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let mut evidence = WorkEvidence {
        foreground_terminated: true,
        ..Default::default()
    };
    evidence.sources.insert("native-event".into(), 1000);
    let mut context = CircuitContext::default();
    context.set("source.agent_id", "9");
    context.set(
        "node.spawn.evidence.1",
        serde_json::to_string(&evidence).unwrap(),
    );
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();
    let request = CheckpointRequest {
        run_id: 1,
        node_id: "spawn".into(),
        attempt: 1,
        expected_revision: 0,
        action: CheckpointAction::Recheck,
        reason: "Inspect the same session again".into(),
    };
    record_outcome_locked(&mut db, &request).unwrap();
    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(context.get("node.spawn.recheck_only"), Some("1"));
    let retained: WorkEvidence =
        serde_json::from_str(context.get("node.spawn.evidence.1").unwrap()).unwrap();
    assert_eq!(retained.sources.get("native-event"), Some(&1000));
    assert!(!retained.ownership_covered);
    let mut view = RunView {
        run_id: 1,
        state: RunState::Running,
        graph,
        context,
        steps: vec![StepView {
            node_id: "spawn".into(),
            attempt: 1,
            status: StepStatus::Running,
            agent_node_id: Some(9),
            outcome: None,
            error: None,
        }],
    };
    let transition = advance(
        &mut view,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "spawn".into(),
            classification: Some(crate::circuit::evaluator::Classification::Completed),
            output: Some("Done".into()),
        },
    );
    assert!(transition.effects.is_empty());
    assert_eq!(view.steps[0].status, StepStatus::Unverified);
    let write = &transition.step_writes[0];
    commit_transition_locked(
        &mut db,
        1,
        None,
        &view.context.to_json().unwrap(),
        &[super::super::CircuitStepOp {
            node_id: write.node_id.clone(),
            status: write.status.as_db_str().into(),
            attempt: write.attempt,
            outcome: write
                .outcome
                .map(|value| value.map(|outcome| outcome.as_db_str().into())),
            error: write.error.clone(),
            agent_node_id: None,
            fresh_attempt: false,
        }],
        EvidenceWrite {
            expected: transition.expected.as_ref(),
            classifications: &transition.classifications,
            ..Default::default()
        },
    )
    .unwrap();
    let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
    assert_eq!(
        (steps[0].status.as_str(), steps[0].attempt),
        ("unverified", 1)
    );
    let history = history_inner(&db, 1).unwrap();
    let recorded: crate::circuit::observation::RecordedClassification = serde_json::from_str(
        &history
            .iter()
            .find(|entry| entry.kind == "classification")
            .expect("interpretation recorded atomically")
            .detail,
    )
    .unwrap();
    assert!(!recorded.lifecycle_verified);
    assert_eq!(recorded.report_revision, None);
    assert_eq!(
        recorded.interpretation,
        crate::circuit::observation::ReportInterpretation::Completed
    );
    assert!(record_outcome_locked(&mut db, &request).is_err());
    assert!(history_inner(&db, 1)
        .unwrap()
        .iter()
        .any(|entry| entry.kind == "evidence_recheck"));
}

#[test]
fn report_commit_rejects_lifecycle_blockers_present_at_commit() {
    use crate::agent::session_lifecycle::{
        HookSignalDetail, LifecycleChangedPayload, LifecycleKind,
    };
    use crate::circuit::observation::{
        RecordedClassification, ReportCompleteness, ReportInterpretation,
    };
    use crate::circuit::stepper::ObservationInputFence;
    use crate::models::SessionStatus;
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path,status,cli_session_id,session_started_at) VALUES (9,1,'agent','/repo','running','session',1000);
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');").unwrap();
    let guard = ObservationInputFence {
        transcript_guard: None,
        report_guard: None,
        agent_node_id: 9,
        input_stamp: "1:2".into(),
        observed_at_ms: 2000,
        session_id: "session".into(),
        session_incarnation: "1000".into(),
    };
    let classifications = [RecordedClassification {
        step_id: "review".into(),
        attempt: 1,
        interpretation: ReportInterpretation::Completed,
        report_revision: Some("report".into()),
        evidence_owner: None,
        lifecycle_verified: false,
        report_text: Some("Approved".into()),
        report_completeness: ReportCompleteness::Complete,
    }];
    for (kind, status) in [
        (
            LifecycleKind::QuestionRequested,
            SessionStatus::AwaitingInput,
        ),
        (
            LifecycleKind::PermissionRequested,
            SessionStatus::AwaitingInput,
        ),
        (LifecycleKind::BackgroundRunning, SessionStatus::Running),
    ] {
        // Classification has already produced a candidate. A harness
        // lifecycle callback can then arrive before the writer transaction;
        // it must veto the candidate even if a process status write makes
        // the positive lifecycle projection stale in the meantime.
        let mut payload =
            LifecycleChangedPayload::new(9, kind, status, &HookSignalDetail::default(), "");
        assert!(
            crate::db::agent_node::commit_agent_lifecycle_inner(&db, &mut payload, &[]).unwrap()
        );
        crate::db::agent_node::update_agent_node_status_inner(&db, 9, status).unwrap();
        let result = commit_transition_locked(
            &mut db,
            1,
            Some("completed"),
            "{}",
            &[],
            EvidenceWrite {
                input_guard: Some(&guard),
                classifications: &classifications,
                ..Default::default()
            },
        );
        assert!(
            result.is_err(),
            "{kind:?} must fence an already-classified report"
        );
        assert_eq!(
            super::super::ledger::get_circuit_run_inner(&db, 1)
                .unwrap()
                .unwrap()
                .state,
            "running"
        );
        assert!(history_inner(&db, 1).unwrap().is_empty());
        // Receiving native evidence must remain possible while blocked.
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &[],
            EvidenceWrite {
                input_guard: Some(&guard),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let mut payload = LifecycleChangedPayload::new(
        9,
        LifecycleKind::TurnCompleted,
        SessionStatus::Ready,
        &HookSignalDetail::default(),
        "",
    );
    assert!(crate::db::agent_node::commit_agent_lifecycle_inner(&db, &mut payload, &[]).unwrap());
    commit_transition_locked(
        &mut db,
        1,
        Some("completed"),
        "{}",
        &[],
        EvidenceWrite {
            input_guard: Some(&guard),
            classifications: &classifications,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap()
            .state,
        "completed"
    );
}

#[test]
fn receipt_commit_rejects_replaced_sessions_without_advancing_cursor_or_history() {
    use crate::circuit::stepper::ObservationInputFence;
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO agent_nodes (id,mesh_id,name,path,status,cli_session_id,session_started_at) VALUES (9,1,'agent','/repo','running','session',1000);
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');").unwrap();
    let guard = ObservationInputFence {
        transcript_guard: None,
        report_guard: None,
        agent_node_id: 9,
        input_stamp: "1:2".into(),
        observed_at_ms: 2000,
        session_id: "session".into(),
        session_incarnation: "1000".into(),
    };
    for change in [
        "UPDATE agent_nodes SET cli_session_id='replacement' WHERE id=9",
        "UPDATE agent_nodes SET cli_session_id='session',session_started_at=1001 WHERE id=9",
        "UPDATE agent_nodes SET session_started_at=1000,status='archived' WHERE id=9",
    ] {
        db.execute_batch(change).unwrap();
        let before = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap()
            .context_json;
        assert!(commit_transition_locked(
            &mut db,
            1,
            None,
            "{\"observer.receipt_cursor\":\"5\"}",
            &[],
            EvidenceWrite {
                input_guard: Some(&guard),
                intents: &[],
                agent_status_effects: &[],
                reconciled_effects: &[],
                observations: &[],
                classifications: &[],
                expected: None,
            }
        )
        .is_err());
        assert_eq!(
            super::super::ledger::get_circuit_run_inner(&db, 1)
                .unwrap()
                .unwrap()
                .context_json,
            before
        );
        assert!(history_inner(&db, 1).unwrap().is_empty());
    }
    db.execute_batch("UPDATE agent_nodes SET status='running' WHERE id=9")
        .unwrap();
    commit_transition_locked(
        &mut db,
        1,
        None,
        "{\"observer.receipt_cursor\":\"5\"}",
        &[],
        EvidenceWrite {
            input_guard: Some(&guard),
            intents: &[],
            agent_status_effects: &[],
            reconciled_effects: &[],
            observations: &[],
            classifications: &[],
            expected: None,
        },
    )
    .unwrap();
    assert_eq!(
        super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap()
            .context_json,
        "{\"observer.receipt_cursor\":\"5\"}"
    );
}

#[test]
fn observation_projection_and_effect_intent_commit_atomically() {
    use crate::circuit::observation::*;
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            CREATE TRIGGER reject_observation BEFORE INSERT ON circuit_run_history WHEN NEW.kind='observation'
            BEGIN SELECT RAISE(ABORT,'injected append failure'); END;").unwrap();
    let observation = RecordedObservation {
        disposition: ObservationDisposition::ReducedConfidence,
        observation: CircuitObservation {
            identity: ObservationIdentity {
                run_id: 1,
                step_id: "work".into(),
                attempt: 1,
                agent_node_id: 9,
                session_incarnation: None,
                session_id: None,
                turn_id: None,
                report_revision: None,
            },
            source: "projection".into(),
            source_id: Some("source-1".into()),
            observed_at_ms: 123,
            authoritative: false,
            fact: ObservedWorkFact::Yielded,
        },
    };
    let op = super::super::CircuitStepOp {
        node_id: "work".into(),
        status: "running".into(),
        attempt: 1,
        outcome: None,
        error: None,
        agent_node_id: None,
        fresh_attempt: false,
    };
    let intent = EffectIntent {
        node_id: "work".into(),
        attempt: 1,
        kind: EffectKind::Github,
    };
    let write = || EvidenceWrite {
        input_guard: None,
        intents: std::slice::from_ref(&intent),
        agent_status_effects: &[],
        reconciled_effects: &[],
        observations: std::slice::from_ref(&observation),
        classifications: &[],
        expected: None,
    };
    assert!(commit_transition_locked(
        &mut db,
        1,
        None,
        "{\"watermark\":\"source-1\"}",
        std::slice::from_ref(&op),
        write()
    )
    .is_err());
    assert!(super::super::ledger::list_circuit_run_steps_inner(&db, 1)
        .unwrap()
        .is_empty());
    assert!(history_inner(&db, 1).unwrap().is_empty());
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM circuit_effects", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER reject_observation").unwrap();
    commit_transition_locked(
        &mut db,
        1,
        None,
        "{\"watermark\":\"source-1\"}",
        &[op],
        write(),
    )
    .unwrap();
    assert_eq!(
        super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap()
            .context_json,
        "{\"watermark\":\"source-1\"}"
    );
    let history = history_inner(&db, 1).unwrap();
    let recorded: RecordedObservation = serde_json::from_str(
        &history
            .iter()
            .find(|entry| entry.kind == "observation")
            .unwrap()
            .detail,
    )
    .unwrap();
    assert_eq!(recorded, observation);
    assert_eq!(
        db.query_row("SELECT state FROM circuit_effects", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "intent"
    );
}

#[test]
fn set_node_status_and_step_commit_atomically_across_restart() {
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, SessionStatusKind,
    };
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut db = Connection::open(file.path()).unwrap();
    crate::db::init_schema(&db).unwrap();
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![
            CircuitNode {
                id: "agent".into(),
                kind: CircuitNodeKind::SpawnAgentNode {
                    prompt: "work".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: None,
                },
            },
            CircuitNode {
                id: "status".into(),
                kind: CircuitNodeKind::SetNodeStatus {
                    status: SessionStatusKind::Running,
                    target_node_id: Some("agent".into()),
                },
            },
        ],
        edges: vec![CircuitEdge {
            from: "agent".into(),
            to: "status".into(),
            condition: EdgeCondition::Always,
        }],
    };
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'agent','/repo','ready');").unwrap();
    crate::db::circuit::ledger::create_autopilot_circuit_inner(
        &db,
        1,
        "status recovery",
        "",
        &graph.to_json().unwrap(),
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'agent',1,'completed',9),(1,'status',1,'running',NULL);
            CREATE TRIGGER reject_status_ack BEFORE INSERT ON circuit_run_history
                WHEN NEW.kind='effect_result'
                BEGIN SELECT RAISE(ABORT,'injected status acknowledgement failure'); END;",
    )
    .unwrap();
    let steps = [super::super::CircuitStepOp {
        node_id: "status".into(),
        status: "completed".into(),
        outcome: None,
        error: None,
        agent_node_id: None,
        attempt: 1,
        fresh_attempt: false,
    }];
    let status_effects = [AgentStatusEffect {
        node_id: "status".into(),
        attempt: 1,
        agent_node_id: 9,
        status: crate::models::SessionStatus::Running,
    }];
    assert!(commit_transition_locked(
        &mut db,
        1,
        None,
        "{}",
        &steps,
        EvidenceWrite {
            agent_status_effects: &status_effects,
            ..Default::default()
        },
    )
    .is_err());
    let status: String = db
        .query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| {
            row.get(0)
        })
        .unwrap();
    let step: String = db
        .query_row(
            "SELECT status FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='status'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), step.as_str()),
        ("ready", "running"),
        "a failed append rolls back both the local effect and step completion"
    );

    db.execute_batch("DROP TRIGGER reject_status_ack").unwrap();
    commit_transition_locked(
        &mut db,
        1,
        None,
        "{}",
        &steps,
        EvidenceWrite {
            agent_status_effects: &status_effects,
            ..Default::default()
        },
    )
    .unwrap();
    drop(db);

    let reopened = Connection::open(file.path()).unwrap();
    let status: String = reopened
        .query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| {
            row.get(0)
        })
        .unwrap();
    let step: String = reopened
        .query_row(
            "SELECT status FROM autopilot_circuit_run_steps WHERE run_id=1 AND node_id='status'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let effect: String = reopened
        .query_row(
            "SELECT detail FROM circuit_run_history WHERE run_id=1 AND kind='effect_result'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((status.as_str(), step.as_str()), ("running", "completed"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&effect).unwrap()["effect"],
        "set_node_status"
    );
}

#[test]
fn set_node_status_does_not_apply_after_circuit_run_is_cancelled_or_deleted() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition,
            SessionStatusKind,
        },
        stepper::{RunState, StepStatus, StepView, TransitionFence},
    };
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![
            CircuitNode {
                id: "agent".into(),
                kind: CircuitNodeKind::SpawnAgentNode {
                    prompt: "work".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: None,
                },
            },
            CircuitNode {
                id: "status".into(),
                kind: CircuitNodeKind::SetNodeStatus {
                    status: SessionStatusKind::Running,
                    target_node_id: Some("agent".into()),
                },
            },
        ],
        edges: vec![CircuitEdge {
            from: "agent".into(),
            to: "status".into(),
            condition: EdgeCondition::Always,
        }],
    };
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'agent','/repo','ready');").unwrap();
    crate::db::circuit::ledger::create_autopilot_circuit_inner(
        &db,
        1,
        "cancelled status recovery",
        "",
        &graph.to_json().unwrap(),
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'cancelled');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status)
                VALUES(1,'status',1,'cancelled');",
    )
    .unwrap();
    let expected = TransitionFence {
        state: RunState::Cancelled,
        steps: vec![StepView {
            node_id: "status".into(),
            attempt: 1,
            status: StepStatus::Cancelled,
            outcome: None,
            error: None,
            agent_node_id: None,
        }],
        revision: None,
    };
    let steps = [super::super::CircuitStepOp {
        node_id: "status".into(),
        status: "completed".into(),
        outcome: None,
        error: None,
        agent_node_id: None,
        attempt: 1,
        fresh_attempt: false,
    }];
    let status_effects = [AgentStatusEffect {
        node_id: "status".into(),
        attempt: 1,
        agent_node_id: 9,
        status: crate::models::SessionStatus::Running,
    }];

    assert!(
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &steps,
            EvidenceWrite {
                agent_status_effects: &status_effects,
                expected: Some(&expected),
                ..Default::default()
            },
        )
        .is_err(),
        "a terminal run must reject its late local status effect"
    );
    let status: String = db
        .query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "ready");

    db.execute("DELETE FROM autopilot_circuit_runs WHERE id=1", [])
        .unwrap();
    assert!(
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &steps,
            EvidenceWrite {
                agent_status_effects: &status_effects,
                ..Default::default()
            },
        )
        .is_err(),
        "a deleted run must also reject a late local status effect"
    );
    let status: String = db
        .query_row("SELECT status FROM agent_nodes WHERE id=9", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "ready");
}

#[test]
fn unknown_open_pr_checkpoint_offers_read_only_recheck_for_same_attempt() {
    use crate::circuit::model::{CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind};
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'open_pr',1,'unverified');
            INSERT INTO circuit_effects VALUES(1,'open_pr',1,'github','uncertain');
            INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'open_pr',1,'effect_possible_dispatch','github');
            INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'open_pr',1,'effect_target','{}');").unwrap();
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: "open_pr".into(),
            kind: CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                open_pr_policy: None,
                label: None,
                comment: None,
            },
        }],
        edges: vec![],
    };
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let evidence = evidence_view_inner(&db, 1).unwrap();
    assert!(evidence.checkpoints[0]
        .actions
        .iter()
        .any(|action| matches!(action, CheckpointAction::Recheck)));
    let request = CheckpointRequest {
        run_id: 1,
        node_id: "open_pr".into(),
        attempt: 1,
        expected_revision: evidence.entries.last().unwrap().id,
        action: CheckpointAction::Recheck,
        reason: "Look for the pull request created by the interrupted request".into(),
    };
    record_outcome_locked(&mut db, &request).unwrap();

    let step = super::super::ledger::list_circuit_run_steps_inner(&db, 1)
        .unwrap()
        .remove(0);
    assert_eq!((step.status.as_str(), step.attempt), ("pending_slot", 1));
    let run = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = crate::circuit::context::CircuitContext::from_json(&run.context_json).unwrap();
    assert_eq!(context.get("node.open_pr.recheck_only"), Some("1"));
    assert_eq!(
        db.query_row(
            "SELECT state FROM circuit_effects WHERE run_id=1 AND node_id='open_pr' AND attempt=1",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "uncertain",
        "a read-only recheck does not claim or replay the create effect"
    );
    assert_eq!(
        history_inner(&db, 1).unwrap().last().unwrap().kind,
        "evidence_recheck"
    );
    use crate::circuit::stepper::{
        advance, Capacity, CircuitEvent, Effect, RunState, StepStatus, StepView,
    };
    let mut view = crate::circuit::stepper::RunView {
        run_id: 1,
        state: RunState::Running,
        graph,
        context,
        steps: vec![StepView {
            node_id: "open_pr".into(),
            attempt: 1,
            status: StepStatus::Queued,
            outcome: None,
            error: None,
            agent_node_id: None,
        }],
    };
    let transition = advance(
        &mut view,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 1,
        }),
    );
    assert!(matches!(
        transition.effects.as_slice(),
        [Effect::CallGithub {
            node_id,
            action: GithubActionKind::OpenPr,
            ..
        }] if node_id == "open_pr"
    ));
    assert_eq!(view.steps[0].status, StepStatus::Running);

    // Commit the scheduled recheck before the worker performs its remote
    // lookup. The existing uncertain create attempt must stay unchanged.
    let writes = transition
        .step_writes
        .iter()
        .map(|write| super::super::CircuitStepOp {
            node_id: write.node_id.clone(),
            status: write.status.as_db_str().into(),
            attempt: write.attempt,
            outcome: write
                .outcome
                .map(|outcome| outcome.map(|value| value.as_db_str().into())),
            error: write.error.clone(),
            agent_node_id: None,
            fresh_attempt: write.fresh_attempt,
        })
        .collect::<Vec<_>>();
    commit_transition_locked(
        &mut db,
        1,
        None,
        &view.context.to_json().unwrap(),
        &writes,
        EvidenceWrite {
            expected: transition.expected.as_ref(),
            ..Default::default()
        },
    )
    .unwrap();

    // A matching read-only lookup finishes after the operator cancels.
    // Its stepper result must not overwrite the terminal run or reconcile
    // the uncertain effect when the stale worker tries to commit it.
    let event = CircuitEvent::GithubActionResult {
        node_id: "open_pr".into(),
        success: true,
        pr_number: Some(314),
        pr_url: Some("https://github.com/example/buildmesh/pull/314".into()),
        pr_head_ref: Some("feature/circuit".into()),
        pr_title: Some("Implementation".into()),
        error: None,
    };
    view.context.set("node.open_pr.recheck_only", "0");
    view.context
        .set("node.open_pr.effect_reconciled_attempt", "1");
    let result = advance(&mut view, &event);
    assert_eq!(view.steps[0].status, StepStatus::Completed);
    assert_eq!(view.context.get("pr.number"), Some("314"));
    super::super::ledger::cancel_circuit_run_locked(&mut db, 1).unwrap();
    let completion = result
        .step_writes
        .iter()
        .map(|write| super::super::CircuitStepOp {
            node_id: write.node_id.clone(),
            status: write.status.as_db_str().into(),
            attempt: write.attempt,
            outcome: write
                .outcome
                .map(|outcome| outcome.map(|value| value.as_db_str().into())),
            error: write.error.clone(),
            agent_node_id: None,
            fresh_attempt: write.fresh_attempt,
        })
        .collect::<Vec<_>>();
    let reconciled = ReconciledEffect {
        intent: EffectIntent {
            node_id: "open_pr".into(),
            attempt: 1,
            kind: EffectKind::Github,
        },
        detail: "Read-only GitHub lookup found open pull request #314.".into(),
    };
    assert!(commit_transition_locked(
        &mut db,
        1,
        result.run_state_changed.then_some(view.state.as_db_str()),
        &view.context.to_json().unwrap(),
        &completion,
        EvidenceWrite {
            expected: result.expected.as_ref(),
            reconciled_effects: std::slice::from_ref(&reconciled),
            ..Default::default()
        },
    )
    .is_err());
    assert_eq!(
        super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap()
            .state,
        "cancelled"
    );
    let step = super::super::ledger::list_circuit_run_steps_inner(&db, 1)
        .unwrap()
        .remove(0);
    assert_eq!((step.status.as_str(), step.attempt), ("cancelled", 1));
    assert_eq!(
        db.query_row(
            "SELECT state FROM circuit_effects WHERE run_id=1 AND node_id='open_pr' AND attempt=1",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "uncertain"
    );
    assert!(!history_inner(&db, 1)
        .unwrap()
        .iter()
        .any(|entry| entry.kind == "effect_reconciled"));
    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = crate::circuit::context::CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(context.get("pr.number"), None);
}

#[test]
fn feedback_attestation_respects_current_requests_without_requiring_review_approval() {
    use crate::agent::session_lifecycle::{
        HookSignalDetail, LifecycleChangedPayload, LifecycleKind,
    };
    use crate::circuit::{context::CircuitContext, model::CircuitGraph};
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome)
                VALUES(1,'verdict',1,'completed','working'),(1,'feedback',1,'unverified',NULL);
            INSERT INTO circuit_effects VALUES(1,'feedback',1,'prompt','uncertain');",
    )
    .unwrap();
    let graph = CircuitGraph::agent_review(None, None, 3);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let mut context = CircuitContext::default();
    context.set("source.agent_id", "9");
    context.set("node.feedback.human_wait", "1");
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();
    let request = CheckpointRequest {
        run_id: 1,
        node_id: "feedback".into(),
        attempt: 1,
        expected_revision: 0,
        action: CheckpointAction::Completed,
        reason: "Submitted the staged feedback in the source terminal and confirmed acceptance"
            .into(),
    };
    assert!(record_outcome_locked(&mut db, &request).is_err());
    context.set("node.feedback.human_wait", "0");
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();

    let mut question = LifecycleChangedPayload::new(
        9,
        LifecycleKind::QuestionRequested,
        crate::models::SessionStatus::AwaitingInput,
        &HookSignalDetail::default(),
        "Approve this permission request",
    );
    assert!(crate::db::agent_node::commit_agent_lifecycle_inner(&db, &mut question, &[]).unwrap());
    let evidence_view = evidence_view_inner(&db, 1).unwrap();
    assert!(!evidence_view.checkpoints[0]
        .actions
        .iter()
        .any(|action| matches!(action, CheckpointAction::Completed)));
    assert!(
        record_outcome_locked(&mut db, &request).is_err(),
        "a lifecycle-only question must suppress the Completed attestation"
    );
    crate::db::agent_node::update_agent_node_status_inner(
        &db,
        9,
        crate::models::SessionStatus::AwaitingInput,
    )
    .unwrap();
    assert!(
        record_outcome_locked(&mut db, &request).is_err(),
        "an unrelated process status write cannot release the attestation veto"
    );

    let mut background = LifecycleChangedPayload::new(
        9,
        LifecycleKind::BackgroundRunning,
        crate::models::SessionStatus::Running,
        &HookSignalDetail::default(),
        "background task still active",
    );
    assert!(
        crate::db::agent_node::commit_agent_lifecycle_inner(&db, &mut background, &[]).unwrap()
    );
    let evidence_view = evidence_view_inner(&db, 1).unwrap();
    assert_eq!(
        evidence_view.coverage[0].observation_blocker,
        Some(crate::circuit::observation::CircuitObservationBlocker::KnownWorkOutstanding),
        "the running background veto must be visible in the operator evidence view"
    );
    assert!(record_outcome_locked(&mut db, &request).is_err());

    let mut resumed = LifecycleChangedPayload::new(
        9,
        LifecycleKind::WorkResumed,
        crate::models::SessionStatus::Running,
        &HookSignalDetail::default(),
        "The operator responded to the request",
    );
    assert!(crate::db::agent_node::commit_agent_lifecycle_inner(&db, &mut resumed, &[]).unwrap());
    record_outcome_locked(&mut db, &request).unwrap();
    let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
    assert_eq!(
        steps
            .iter()
            .find(|s| s.node_id == "feedback")
            .unwrap()
            .status,
        "completed"
    );
    assert_eq!(
        steps
            .iter()
            .find(|s| s.node_id == "verdict")
            .unwrap()
            .outcome
            .as_deref(),
        Some("working")
    );
    assert_eq!(
        history_inner(&db, 1)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "operator_attestation")
            .count(),
        1
    );
}

#[test]
fn unrelated_completion_does_not_persist_another_agents_lifecycle_veto() {
    use crate::agent::session_lifecycle::{
        HookSignalDetail, LifecycleChangedPayload, LifecycleKind,
    };
    use crate::circuit::{context::CircuitContext, model::CircuitGraph};

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES
                (9,1,'source','/repo'),(10,1,'reviewer','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id)
                VALUES(1,1,1,'running',9);
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',10),
                      (1,'verdict',1,'unverified',10),
                      (1,'await_source',1,'unverified',9);",
    )
    .unwrap();
    let graph = CircuitGraph::agent_review(None, None, 3);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let mut context = CircuitContext::default();
    context.set("source.agent_id", "9");
    context.set(
        "node.verdict.lifecycle_blocker",
        "{\"kind\":\"known_work_outstanding\"}",
    );
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();

    let mut question = LifecycleChangedPayload::new(
        10,
        LifecycleKind::QuestionRequested,
        crate::models::SessionStatus::AwaitingInput,
        &HookSignalDetail::default(),
        "Approve the reviewer permission request",
    );
    assert!(crate::db::agent_node::commit_agent_lifecycle_inner(&db, &mut question, &[]).unwrap());

    let request = CheckpointRequest {
        run_id: 1,
        node_id: "await_source".into(),
        attempt: 1,
        expected_revision: 0,
        action: CheckpointAction::Completed,
        reason: "The source agent finished its turn".into(),
    };
    record_outcome_locked(&mut db, &request).unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    assert!(
        !stored.context_json.contains("lifecycle_blocker"),
        "runtime lifecycle projections must not be serialized into run context"
    );
    let stored_context = CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(
        stored_context.get("node.verdict.lifecycle_blocker"),
        None,
        "a live veto is a read-time projection and must not hitchhike on an unrelated completion"
    );

    let mut resumed = LifecycleChangedPayload::new(
        10,
        LifecycleKind::WorkResumed,
        crate::models::SessionStatus::Running,
        &HookSignalDetail::default(),
        "The reviewer permission request was resolved",
    );
    assert!(crate::db::agent_node::commit_agent_lifecycle_inner(&db, &mut resumed, &[]).unwrap());
    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let view = recovery_view(&db, &stored).unwrap();
    assert_eq!(view.report_blocker("verdict"), None);
}

#[test]
fn deleted_agent_stays_visible_as_a_fail_closed_evidence_blocker() {
    use crate::circuit::observation::CircuitObservationBlocker;
    use crate::circuit::{context::CircuitContext, model::CircuitGraph};

    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id)
                VALUES(1,1,1,'running',9);
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',404),
                      (1,'verdict',1,'unverified',404);",
    )
    .unwrap();
    let graph = CircuitGraph::agent_review(None, None, 3);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let mut context = CircuitContext::default();
    context.set("source.agent_id", "9");
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();

    let evidence = evidence_view_inner(&db, 1).unwrap();
    let verdict = evidence
        .coverage
        .iter()
        .find(|item| item.node_id == "verdict")
        .expect("the missing reviewer still needs an operator-visible explanation");
    assert_eq!(verdict.platform, "Agent record unavailable");
    assert_eq!(
        verdict.observation_blocker,
        Some(CircuitObservationBlocker::LifecycleEvidenceUnavailable)
    );
}

#[test]
fn operator_completion_advances_evidence_checkpoint_without_forging_observations() {
    use crate::circuit::{
        context::CircuitContext,
        model::{CircuitGraph, CircuitNodeKind},
        observation::WorkEvidence,
        stepper::{advance, Capacity, CircuitEvent, RunState, RunView, StepStatus, StepView},
    };
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'agent','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
            VALUES(1,'spawn',1,'unverified',9);",
    )
    .unwrap();
    let mut graph = CircuitGraph::walking_skeleton("");
    graph.blueprint = None;
    graph.nodes.retain(|node| node.id != "inject");
    graph.edges.retain(|edge| edge.from != "inject");
    graph
        .edges
        .iter_mut()
        .find(|edge| edge.to == "inject")
        .unwrap()
        .to = "notify".into();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let request = CheckpointRequest {
        run_id: 1,
        node_id: "spawn".into(),
        attempt: 1,
        expected_revision: 0,
        action: CheckpointAction::Completed,
        reason: "Inspected the finished work and terminal; advance this handoff".into(),
    };
    for kind in [
        CircuitNodeKind::ReviewVerdict {
            target_node_id: None,
        },
        CircuitNodeKind::CollaboratorCheck {
            require_approval: true,
        },
    ] {
        let mut protected = graph.clone();
        protected
            .nodes
            .iter_mut()
            .find(|n| n.id == "spawn")
            .unwrap()
            .kind = kind;
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [protected.to_json().unwrap()],
        )
        .unwrap();
        assert!(record_outcome_locked(&mut db, &request).is_err());
    }
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    for evidence in [
        WorkEvidence {
            children: [("child".into(), false)].into_iter().collect(),
            ..Default::default()
        },
        WorkEvidence {
            conflicted: true,
            ..Default::default()
        },
    ] {
        let mut context = CircuitContext::default();
        context.set(
            "node.spawn.evidence.1",
            serde_json::to_string(&evidence).unwrap(),
        );
        db.execute(
            "UPDATE autopilot_circuit_runs SET context_json=?1",
            [context.to_json().unwrap()],
        )
        .unwrap();
        assert!(record_outcome_locked(&mut db, &request).is_err());
    }
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [r#"{"node.spawn.human_wait":"1"}"#],
    )
    .unwrap();
    assert!(record_outcome_locked(&mut db, &request).is_err());
    db.execute("UPDATE autopilot_circuit_runs SET context_json='{}'", [])
        .unwrap();
    assert!(evidence_view_inner(&db, 1).unwrap().checkpoints[0]
        .actions
        .iter()
        .any(|a| matches!(a, CheckpointAction::Completed)));
    record_outcome_locked(&mut db, &request).unwrap();
    assert!(
        record_outcome_locked(&mut db, &request).is_err(),
        "stale operator action must not be replayed"
    );
    let run = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
    assert_eq!(steps[0].status, "completed");
    assert_eq!(
        steps[0].error_message, None,
        "the attestation belongs in history, not a completed step's error"
    );
    assert_eq!(steps[0].attempt, 1);
    let entries = history_inner(&db, 1).unwrap();
    assert!(entries
        .iter()
        .any(|e| e.kind == "operator_attestation" && e.source.as_deref() == Some("operator")));
    assert!(!entries.iter().any(|e| matches!(
        e.kind.as_str(),
        "observation" | "classification" | "effect_intent"
    )));
    let mut view = RunView {
        run_id: 1,
        graph,
        state: RunState::Running,
        context: CircuitContext::from_json(&run.context_json).unwrap(),
        steps: vec![
            StepView {
                node_id: "trigger".into(),
                status: StepStatus::Completed,
                attempt: 1,
                outcome: Some(crate::circuit::model::StepOutcome::Completed),
                error: None,
                agent_node_id: None,
            },
            StepView {
                node_id: "spawn".into(),
                status: StepStatus::Completed,
                attempt: 1,
                outcome: Some(crate::circuit::model::StepOutcome::Completed),
                error: steps[0].error_message.clone(),
                agent_node_id: Some(9),
            },
        ],
    };
    advance(
        &mut view,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 4,
        }),
    );
    assert_eq!(view.state, RunState::Completed);
}

#[test]
fn operator_outcome_fences_retry_attempt_and_history() {
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt) VALUES (1,'comment','unverified',1);").unwrap();
    let graph = crate::circuit::model::CircuitGraph {
        version: 2,
        blueprint: None,
        nodes: vec![crate::circuit::model::CircuitNode {
            id: "comment".into(),
            kind: crate::circuit::model::CircuitNodeKind::GithubAction {
                action: crate::circuit::model::GithubActionKind::PostComment,
                label: None,
                comment: Some("test".into()),
                open_pr_policy: None,
            },
        }],
        edges: vec![],
    };
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let mut request = CheckpointRequest {
        run_id: 1,
        node_id: "comment".into(),
        attempt: 1,
        expected_revision: 0,
        action: CheckpointAction::Retry,
        reason: "Checked the remote issue".into(),
    };
    let mut open_pr = graph.clone();
    if let crate::circuit::model::CircuitNodeKind::GithubAction { action, .. } =
        &mut open_pr.nodes[0].kind
    {
        *action = crate::circuit::model::GithubActionKind::OpenPr;
    }
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [open_pr.to_json().unwrap()],
    )
    .unwrap();
    request.action = CheckpointAction::Completed;
    assert!(record_outcome_locked(&mut db, &request)
        .unwrap_err()
        .to_string()
        .contains("pull request identity"));
    assert!(!evidence_view_inner(&db, 1).unwrap().checkpoints[0]
        .actions
        .iter()
        .any(|a| matches!(a, CheckpointAction::Completed)));
    assert_eq!(
        super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap()[0].status,
        "unverified"
    );
    assert!(history_inner(&db, 1).unwrap().is_empty());
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    request.action = CheckpointAction::Retry;
    use crate::circuit::stepper::{RunState, StepStatus, StepView, TransitionFence};
    let stale = TransitionFence {
        state: RunState::Running,
        revision: Some(0),
        steps: vec![StepView {
            node_id: "comment".into(),
            attempt: 1,
            status: StepStatus::Unverified,
            outcome: None,
            error: None,
            agent_node_id: None,
        }],
    };
    assert!(record_outcome_locked(&mut db, &request)
        .unwrap_err()
        .to_string()
        .contains("not performed"));
    request.action = CheckpointAction::NotPerformed;
    // A blank note is not a gate: the attestation is still recorded, in its
    // own words rather than quoting an empty string.
    request.reason = "   ".into();
    record_outcome_locked(&mut db, &request).unwrap();
    let (revision, noted, source) = last_recorded_outcome(&db);
    assert_eq!(noted, "Operator-recorded outcome (NotPerformed).");
    assert_eq!(source.as_deref(), Some("operator"));
    request.reason = "Confirmed request was rejected before dispatch".into();
    request.expected_revision = revision;
    record_outcome_locked(&mut db, &request).unwrap();
    let (_, noted, source) = last_recorded_outcome(&db);
    assert!(
        noted.contains("Confirmed request was rejected before dispatch"),
        "{noted}"
    );
    assert_eq!(source.as_deref(), Some("operator"));
    assert!(
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &[],
            EvidenceWrite {
                expected: Some(&stale),
                ..Default::default()
            }
        )
        .is_err(),
        "an observer loaded before the operator action must refresh, even at the same attempt"
    );
    assert_eq!(
        super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap()[0].attempt,
        1
    );
    request.action = CheckpointAction::Retry;
    assert!(record_outcome_locked(&mut db, &request)
        .unwrap_err()
        .to_string()
        .contains("Refresh"));
    request.expected_revision = history_inner(&db, 1).unwrap().last().unwrap().id;
    record_outcome_locked(&mut db, &request).unwrap();
    let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
    assert_eq!(steps[0].attempt, 2);
    assert_eq!(steps[0].status, "pending_slot");
    assert!(
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &[],
            EvidenceWrite {
                expected: Some(&stale),
                ..Default::default()
            }
        )
        .is_err(),
        "stale worker must never roll the retry back to attempt one"
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM circuit_effects WHERE attempt=1",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "not_performed"
    );
    assert_eq!(
        history_inner(&db, 1)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "operator_attestation")
            .count(),
        3
    );
    request.expected_revision = history_inner(&db, 1).unwrap().last().unwrap().id;
    request.attempt = 2;
    db.execute("UPDATE autopilot_circuit_runs SET state='cancelled'", [])
        .unwrap();
    assert!(record_outcome_locked(&mut db, &request).is_err());
}

#[test]
fn spawn_attachment_acknowledgement_is_atomic_fenced_and_deduplicated() {
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'first','/repo'),(10,1,'other','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'spawn',1,'running');
            INSERT INTO circuit_effects VALUES(1,'spawn',1,'spawn','intent');").unwrap();
    assert_eq!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 1, 9, None, None).unwrap(),
        None,
        "unclaimed intent cannot acknowledge a spawn"
    );
    let intent = EffectIntent {
        node_id: "spawn".into(),
        attempt: 1,
        kind: EffectKind::Spawn,
    };
    assert!(claim_effect_locked(&mut db, 1, &intent).unwrap().is_some());
    db.execute_batch("CREATE TRIGGER reject_ack BEFORE INSERT ON circuit_run_history WHEN NEW.kind='effect_result'
            BEGIN SELECT RAISE(ABORT,'injected acknowledgement failure'); END;").unwrap();
    assert!(acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 1, 9, None, None).is_err());
    let (agent, state): (Option<i64>, String) = db.query_row("SELECT s.agent_node_id,e.state FROM autopilot_circuit_run_steps s JOIN circuit_effects e ON e.run_id=s.run_id", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(agent, None);
    assert_eq!(state, "possible_dispatch");
    db.execute_batch("DROP TRIGGER reject_ack").unwrap();
    let revision = acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 1, 9, None, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 1, 9, None, None).unwrap(),
        Some(revision)
    );
    assert_eq!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 1, 10, None, None).unwrap(),
        None
    );
    assert_eq!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 2, 9, None, None).unwrap(),
        None
    );
    db.execute("UPDATE autopilot_circuit_runs SET state='cancelled'", [])
        .unwrap();
    assert_eq!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 1, 9, None, None).unwrap(),
        None
    );
    assert!(claim_effect_locked(&mut db, 1, &intent).unwrap().is_none());
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE kind='effect_result'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    // A new claimed retry may replace exactly the prior retained agent.
    db.execute_batch(
        "UPDATE autopilot_circuit_runs SET state='running';
            UPDATE autopilot_circuit_run_steps SET attempt=2;
            INSERT INTO circuit_effects VALUES(1,'spawn',2,'spawn','possible_dispatch');",
    )
    .unwrap();
    assert_eq!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 2, 10, None, None).unwrap(),
        None
    );
    assert!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 2, 10, None, Some(9))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 2, 9, None, Some(10)).unwrap(),
        None
    );
    // Delivery to a retained live agent is acknowledged as a prompt, not allocation.
    db.execute_batch(
        "UPDATE autopilot_circuit_run_steps SET attempt=3;
            INSERT INTO circuit_effects VALUES(1,'spawn',3,'spawn','possible_dispatch');",
    )
    .unwrap();
    assert!(
        claim_effect_locked(
            &mut db,
            1,
            &EffectIntent {
                attempt: 3,
                ..intent.clone()
            }
        )
        .unwrap()
        .is_none(),
        "a crash after delivery must never replay it"
    );
    assert!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 3, 10, None, Some(10))
            .unwrap()
            .is_some()
    );
    assert!(history_inner(&db, 1)
        .unwrap()
        .last()
        .unwrap()
        .detail
        .contains("Prompt delivered to retained agent"));
    db.execute_batch("UPDATE autopilot_circuit_run_steps SET attempt=4")
        .unwrap();
    assert_eq!(
        acknowledge_spawn_attachment_locked(&mut db, 1, "spawn", 4, 10, None, Some(10)).unwrap(),
        None,
        "missing dispatch claim"
    );
}

#[test]
fn prompt_acknowledgement_survives_restart_and_fences_cancelled_or_retried_steps() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut db = Connection::open(file.path()).unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'feedback',1,'running');
            INSERT INTO circuit_effects VALUES(1,'feedback',1,'prompt','possible_dispatch');").unwrap();
    assert_eq!(
        acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 2).unwrap(),
        None
    );
    db.execute_batch(
        "CREATE TRIGGER reject_prompt_ack BEFORE INSERT ON circuit_run_history
            WHEN NEW.kind='effect_result' BEGIN SELECT RAISE(ABORT,'injected failure'); END;",
    )
    .unwrap();
    assert!(acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 1).is_err());
    assert_eq!(
        db.query_row("SELECT state FROM circuit_effects", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "possible_dispatch"
    );
    db.execute_batch("DROP TRIGGER reject_prompt_ack").unwrap();
    for state in ["cancelled", "failed", "completed"] {
        db.execute("UPDATE autopilot_circuit_runs SET state=?1", [state])
            .unwrap();
        assert_eq!(
            acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 1).unwrap(),
            None
        );
    }
    db.execute("UPDATE autopilot_circuit_runs SET state='paused'", [])
        .unwrap();
    let revision = acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        acknowledge_prompt_delivery_locked(&mut db, 1, "feedback", 1).unwrap(),
        None
    );
    append_history(
        &db,
        HistoryEvent {
            run_id: 1,
            node_id: Some("feedback"),
            attempt: Some(1),
            kind: "native_hook",
            detail: "{}",
            source: None,
            disposition: None,
        },
    )
    .unwrap();
    assert!(
        revision_inner(&db, 1).unwrap() > revision,
        "later receipts may invalidate the projection commit"
    );
    drop(db);
    let mut db = Connection::open(file.path()).unwrap();
    assert_eq!(
        db.query_row("SELECT state FROM circuit_effects", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "acknowledged"
    );
    assert_eq!(
        db.query_row("SELECT status FROM autopilot_circuit_run_steps", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "running",
        "delivery receipt survives independently of step projection"
    );
    db.execute("UPDATE autopilot_circuit_runs SET state='running'", [])
        .unwrap();
    assert!(
        claim_effect_locked(
            &mut db,
            1,
            &EffectIntent {
                node_id: "feedback".into(),
                attempt: 1,
                kind: EffectKind::Prompt,
            }
        )
        .unwrap()
        .is_none(),
        "recovery cannot dispatch the acknowledged prompt again"
    );
}

#[test]
fn effect_claim_survives_reopen_and_is_never_replayed() {
    for kind in [EffectKind::Github, EffectKind::Prompt, EffectKind::Spawn] {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id,name,path) VALUES (1,'test','/repo');
            INSERT INTO autopilot_circuits (id,mesh_id,name) VALUES (1,1,'test');
            INSERT INTO autopilot_circuit_runs (id,circuit_id,mesh_id,state) VALUES (1,1,1,'running');").unwrap();
        let intent = EffectIntent {
            node_id: "comment".into(),
            attempt: 1,
            kind,
        };
        let op = super::super::CircuitStepOp {
            node_id: "comment".into(),
            status: "running".into(),
            attempt: 1,
            outcome: None,
            error: None,
            agent_node_id: None,
            fresh_attempt: false,
        };
        commit_transition_locked(
            &mut db,
            1,
            None,
            "{}",
            &[op],
            EvidenceWrite {
                intents: std::slice::from_ref(&intent),
                ..Default::default()
            },
        )
        .unwrap();
        drop(db);
        let mut reopened = Connection::open(file.path()).unwrap();
        assert!(
            claim_effect_locked(&mut reopened, 1, &intent)
                .unwrap()
                .is_some(),
            "an intent that crashed before dispatch remains claimable after restart"
        );
        drop(reopened);
        let mut reopened = Connection::open(file.path()).unwrap();
        assert!(claim_effect_locked(&mut reopened, 1, &intent)
            .unwrap()
            .is_none());
        assert_eq!(
            reopened
                .query_row("SELECT state FROM circuit_effects", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "possible_dispatch"
        );
        assert_eq!(reopened.query_row("SELECT COUNT(*) FROM circuit_run_history WHERE kind='effect_possible_dispatch'", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
    }
}

#[test]
fn open_pr_target_is_saved_only_after_claim_and_survives_reopen() {
    use crate::circuit::model::{CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind};
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut db = Connection::open(file.path()).unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'open_pr',1,'running');
            INSERT INTO circuit_effects VALUES(1,'open_pr',1,'github','intent');").unwrap();
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: "open_pr".into(),
            kind: CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                open_pr_policy: None,
                label: None,
                comment: None,
            },
        }],
        edges: vec![],
    };
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let target = r#"{"owner":"example","repo":"buildmesh","head":"feature/circuit"}"#;
    assert!(record_effect_target_locked(&mut db, 1, "open_pr", 1, target).is_err());
    db.execute(
        "UPDATE circuit_effects SET state='possible_dispatch' WHERE run_id=1 AND node_id='open_pr'",
        [],
    )
    .unwrap();
    let revision = record_effect_target_locked(&mut db, 1, "open_pr", 1, target).unwrap();
    assert_eq!(revision, history_inner(&db, 1).unwrap().last().unwrap().id);
    assert_eq!(
        latest_effect_target_inner(&db, 1, "open_pr", 1)
            .unwrap()
            .as_deref(),
        Some(target)
    );
    drop(db);

    let reopened = Connection::open(file.path()).unwrap();
    assert_eq!(
        latest_effect_target_inner(&reopened, 1, "open_pr", 1)
            .unwrap()
            .as_deref(),
        Some(target)
    );
    assert_eq!(
        reopened
            .query_row(
                "SELECT COUNT(*) FROM circuit_run_history WHERE kind='effect_target'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn open_pr_recheck_completion_reconciles_the_unknown_effect() {
    use crate::circuit::model::{CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind};
    use crate::circuit::stepper::{advance, CircuitEvent, RunState, RunView, StepStatus, StepView};
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'open_pr',1,'running');
            INSERT INTO circuit_effects VALUES(1,'open_pr',1,'github','uncertain');").unwrap();
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: "open_pr".into(),
            kind: CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                open_pr_policy: None,
                label: None,
                comment: None,
            },
        }],
        edges: vec![],
    };
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let mut view = RunView {
        run_id: 1,
        state: RunState::Running,
        graph,
        context: crate::circuit::context::CircuitContext::default(),
        steps: vec![StepView {
            node_id: "open_pr".into(),
            attempt: 1,
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: None,
        }],
    };
    let result = advance(
        &mut view,
        &CircuitEvent::GithubActionResult {
            node_id: "open_pr".into(),
            success: true,
            pr_number: Some(314),
            pr_url: Some("https://github.com/example/buildmesh/pull/314".into()),
            pr_head_ref: Some("feature/circuit".into()),
            pr_title: Some("Implementation".into()),
            error: None,
        },
    );
    let writes = result
        .step_writes
        .iter()
        .map(|write| super::super::CircuitStepOp {
            node_id: write.node_id.clone(),
            status: write.status.as_db_str().into(),
            attempt: write.attempt,
            outcome: write
                .outcome
                .map(|outcome| outcome.map(|value| value.as_db_str().into())),
            error: write.error.clone(),
            agent_node_id: None,
            fresh_attempt: write.fresh_attempt,
        })
        .collect::<Vec<_>>();
    let reconciled = ReconciledEffect {
        intent: EffectIntent {
            node_id: "open_pr".into(),
            attempt: 1,
            kind: EffectKind::Github,
        },
        detail: "Read-only GitHub lookup found open pull request #314.".into(),
    };
    commit_transition_locked(
        &mut db,
        1,
        result.run_state_changed.then_some(view.state.as_db_str()),
        &view.context.to_json().unwrap(),
        &writes,
        EvidenceWrite {
            reconciled_effects: std::slice::from_ref(&reconciled),
            expected: result.expected.as_ref(),
            ..Default::default()
        },
    )
    .unwrap();
    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = crate::circuit::context::CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(stored.state, "completed");
    assert_eq!(context.get("pr.number"), Some("314"));
    assert_eq!(context.get("pr.head_ref"), Some("feature/circuit"));
    assert_eq!(
        super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap()[0].status,
        "completed"
    );
    assert_eq!(
        db.query_row("SELECT state FROM circuit_effects", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "acknowledged",
        "a matching read-only PR observation reconciles the prior uncertain dispatch"
    );
    assert_eq!(
        history_inner(&db, 1).unwrap().last().unwrap().kind,
        "effect_reconciled"
    );
    assert!(history_inner(&db, 1)
        .unwrap()
        .last()
        .unwrap()
        .detail
        .contains("#314"));
}

#[test]
fn open_pr_recheck_after_not_performed_reconciles_without_losing_attestation() {
    use crate::circuit::model::{CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind};
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status) VALUES(1,'open_pr',1,'unverified');
            INSERT INTO circuit_effects VALUES(1,'open_pr',1,'github','uncertain');
            INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'open_pr',1,'effect_possible_dispatch','github');
            INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'open_pr',1,'effect_target','{\"owner\":\"example\",\"repo\":\"buildmesh\",\"head\":\"feature/circuit\"}');").unwrap();
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: "open_pr".into(),
            kind: CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                open_pr_policy: None,
                label: None,
                comment: None,
            },
        }],
        edges: vec![],
    };
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let initial = evidence_view_inner(&db, 1).unwrap();
    let mut request = CheckpointRequest {
        run_id: 1,
        node_id: "open_pr".into(),
        attempt: 1,
        expected_revision: initial.entries.last().unwrap().id,
        action: CheckpointAction::NotPerformed,
        reason: "GitHub confirmed no matching pull request existed".into(),
    };
    record_outcome_locked(&mut db, &request).unwrap();

    let attested = evidence_view_inner(&db, 1).unwrap();
    assert!(attested.checkpoints[0]
        .actions
        .iter()
        .any(|action| matches!(action, CheckpointAction::Recheck)));
    request.expected_revision = attested.entries.last().unwrap().id;
    request.action = CheckpointAction::Recheck;
    request.reason = "Verify the saved repository branch without creating a pull request".into();
    record_outcome_locked(&mut db, &request).unwrap();

    let completion = super::super::CircuitStepOp {
        node_id: "open_pr".into(),
        status: "completed".into(),
        attempt: 1,
        outcome: Some(Some("completed".into())),
        error: None,
        agent_node_id: None,
        fresh_attempt: false,
    };
    let reconciled = ReconciledEffect {
        intent: EffectIntent {
            node_id: "open_pr".into(),
            attempt: 1,
            kind: EffectKind::Github,
        },
        detail: "Read-only GitHub lookup found open pull request #314.".into(),
    };
    commit_transition_locked(
        &mut db,
        1,
        None,
        "{}",
        &[completion],
        EvidenceWrite {
            reconciled_effects: std::slice::from_ref(&reconciled),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(
        db.query_row("SELECT state FROM circuit_effects", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "acknowledged"
    );
    let history = history_inner(&db, 1).unwrap();
    assert!(history.iter().any(|entry| {
        entry.kind == "operator_attestation"
            && entry.detail.contains("NotPerformed")
            && entry
                .detail
                .contains("GitHub confirmed no matching pull request existed")
    }));
    assert_eq!(history.last().unwrap().kind, "effect_reconciled");
}
#[test]
fn operator_review_records_verdict_and_rejects_stale_or_blocked_decisions() {
    use crate::circuit::{context::CircuitContext, model::CircuitGraph};
    for (action, verdict, outcome) in [
        (CheckpointAction::ReviewPass, "approved", "completed"),
        (CheckpointAction::ReviewFail, "changes_requested", "working"),
    ] {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch(
            "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
                INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(8,1,'source','/repo/source');
                INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'reviewer','/repo');
                INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
                INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                    VALUES(1,1,1,'running','{\"source.agent_id\":\"8\"}');
                INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                    VALUES(1,'reviewer',1,'completed',9),(1,'verdict',1,'unverified',9);",
        )
        .unwrap();
        let graph = CircuitGraph::agent_review(None, None, 3);
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        let mut request = CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action,
            reason: "Inspected the review and current diff".into(),
        };
        request.expected_revision = 99;
        assert!(record_outcome_locked(&mut db, &request).is_err());
        request.expected_revision = 0;
        request.attempt = 2;
        assert!(record_outcome_locked(&mut db, &request).is_err());
        request.attempt = 1;
        db.execute(
            "UPDATE autopilot_circuit_runs SET context_json=?1",
            [r#"{"node.verdict.human_wait":"1"}"#],
        )
        .unwrap();
        assert!(record_outcome_locked(&mut db, &request).is_err());
        db.execute("UPDATE autopilot_circuit_runs SET context_json='{}'", [])
            .unwrap();
        request.reason.clear();
        assert!(record_outcome_locked(&mut db, &request).is_err());
        request.reason = "Inspected the review and current diff".into();
        db.execute("UPDATE autopilot_circuit_runs SET context_json=?1", [r#"{"source.agent_id":"8","node.verdict.evaluated_output":"obsolete earlier report","node.verdict.review_verdict_attempt":"0"}"#]).unwrap();
        record_outcome_locked(&mut db, &request).unwrap();
        assert!(record_outcome_locked(&mut db, &request).is_err());
        let run = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let context = CircuitContext::from_json(&run.context_json).unwrap();
        assert_eq!(context.get("node.verdict.review_verdict"), Some(verdict));
        assert_eq!(context.get("node.verdict.status"), Some(outcome));
        assert_eq!(
            context.get("node.verdict.evaluated_output"),
            Some("Inspected the review and current diff")
        );
        assert_eq!(
            context.get("node.verdict.review_verdict_source"),
            Some("operator")
        );
        let steps = super::super::ledger::list_circuit_run_steps_inner(&db, 1).unwrap();
        let step = steps.iter().find(|step| step.node_id == "verdict").unwrap();
        assert_eq!(step.status, "completed");
        assert_eq!(step.outcome.as_deref(), Some(outcome));
        assert!(history_inner(&db, 1)
            .unwrap()
            .iter()
            .any(|entry| entry.kind == "operator_review"
                && entry.source.as_deref() == Some("operator")
                && entry.disposition.as_deref() == Some(verdict)));
    }
}

#[test]
fn operator_review_request_changes_without_a_feedback_route_explains_what_is_missing() {
    use crate::circuit::{context::CircuitContext, model::CircuitGraph};

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(8,1,'source','/repo/source');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"8\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',9),(1,'verdict',1,'unverified',9);",
    )
    .unwrap();
    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .edges
        .retain(|edge| !(edge.from == "verdict" && edge.to == "feedback"));
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let error = record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "Fix the unchecked null.".into(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.to_lowercase().contains("feedback injection"),
        "{error}"
    );

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        CircuitContext::from_json(&stored.context_json)
            .unwrap()
            .get("node.verdict.review_verdict"),
        None
    );
}

#[test]
fn operator_review_is_refused_when_an_unrun_outcome_can_bypass_feedback() {
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
    };

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\",\"source.name\":\"issue #42\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',10),(1,'verdict',1,'unverified',10);",
    )
    .unwrap();

    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .edges
        .retain(|edge| !(edge.from == "verdict" && edge.to == "feedback"));
    graph.nodes.push(CircuitNode {
        id: "await_feedback_gate".into(),
        kind: CircuitNodeKind::AwaitAgentTurn {
            target_node_id: Some("$source".into()),
        },
    });
    graph.nodes.push(CircuitNode {
        id: "feedback_blocked_notice".into(),
        kind: CircuitNodeKind::Notify {
            message: "The source turn was blocked.".into(),
        },
    });
    // All other AwaitAgentTurn outcomes lead to feedback; Blocked is the bypass.
    graph.edges.extend([
        CircuitEdge {
            from: "verdict".into(),
            to: "await_feedback_gate".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Working),
        },
        CircuitEdge {
            from: "await_feedback_gate".into(),
            to: "feedback".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Completed),
        },
        CircuitEdge {
            from: "await_feedback_gate".into(),
            to: "feedback_blocked_notice".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Blocked),
        },
        CircuitEdge {
            from: "await_feedback_gate".into(),
            to: "feedback".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Working),
        },
        CircuitEdge {
            from: "await_feedback_gate".into(),
            to: "feedback".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Failed),
        },
    ]);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let view = recovery_view(&db, &stored).unwrap();
    assert!(view.step("await_feedback_gate").is_none());
    assert!(
        !view.can_record_review("verdict", StepOutcome::Working),
        "Request changes must be refused if an unrun route can bypass feedback"
    );
    let error = record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "The source turn might never reach feedback.".into(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("every possible outcome on the Request changes route"),
        "the refusal should explain how to repair the route: {error}"
    );

    db.execute(
            "INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome,agent_node_id)
             VALUES(1,'await_feedback_gate',1,'completed','completed',9)",
            [],
        )
        .unwrap();
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET attempt=2 WHERE run_id=1 AND node_id='verdict'",
        [],
    )
    .unwrap();
    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let later_round = recovery_view(&db, &stored).unwrap();
    assert!(
        !later_round.can_record_review("verdict", StepOutcome::Working),
        "a completed outcome from an earlier review round must not hide a bypass"
    );
}

#[test]
fn operator_review_is_refused_when_exclusive_branches_cannot_satisfy_feedback_join() {
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
    };

    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\",\"source.name\":\"issue #42\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',10),(1,'verdict',1,'unverified',10);",
    )
    .unwrap();

    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .edges
        .retain(|edge| !(edge.from == "verdict" && edge.to == "feedback"));
    graph.nodes.extend([
        CircuitNode {
            id: "await_feedback_gate".into(),
            kind: CircuitNodeKind::AwaitAgentTurn {
                target_node_id: Some("$source".into()),
            },
        },
        CircuitNode {
            id: "completed_notice".into(),
            kind: CircuitNodeKind::Notify {
                message: "The source turn completed.".into(),
            },
        },
        CircuitNode {
            id: "blocked_notice".into(),
            kind: CircuitNodeKind::Notify {
                message: "The source turn was blocked.".into(),
            },
        },
    ]);
    graph.edges.extend([
        CircuitEdge {
            from: "verdict".into(),
            to: "await_feedback_gate".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Working),
        },
        CircuitEdge {
            from: "await_feedback_gate".into(),
            to: "completed_notice".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Completed),
        },
        CircuitEdge {
            from: "await_feedback_gate".into(),
            to: "blocked_notice".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Blocked),
        },
        CircuitEdge {
            from: "await_feedback_gate".into(),
            to: "completed_notice".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Working),
        },
        CircuitEdge {
            from: "await_feedback_gate".into(),
            to: "completed_notice".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Failed),
        },
        CircuitEdge {
            from: "completed_notice".into(),
            to: "feedback".into(),
            condition: EdgeCondition::Always,
        },
        CircuitEdge {
            from: "blocked_notice".into(),
            to: "feedback".into(),
            condition: EdgeCondition::Always,
        },
    ]);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let view = recovery_view(&db, &stored).unwrap();
    assert!(view.step("await_feedback_gate").is_none());
    assert!(
        !view.can_record_review("verdict", StepOutcome::Working),
        "exclusive outcome branches cannot jointly satisfy feedback's all-incoming-edge join"
    );
}

#[test]
fn operator_review_is_refused_when_fail_fast_outcome_cannot_reach_feedback() {
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
    };

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\",\"source.name\":\"issue #42\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',10),(1,'verdict',1,'unverified',10);",
    )
    .unwrap();

    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .edges
        .retain(|edge| !(edge.from == "verdict" && edge.to == "feedback"));
    graph.nodes.push(CircuitNode {
        id: "verify_changes".into(),
        kind: CircuitNodeKind::DeterministicVerification {
            command: "cargo test".into(),
        },
    });
    graph.edges.extend([
        CircuitEdge {
            from: "verdict".into(),
            to: "verify_changes".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Working),
        },
        CircuitEdge {
            from: "verify_changes".into(),
            to: "feedback".into(),
            // Even this unconditional route cannot run after fail_step
            // marks the run failed when no RetryLimit owns the failure.
            condition: EdgeCondition::Always,
        },
    ]);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let view = recovery_view(&db, &stored).unwrap();
    assert!(
        !view.can_record_review("verdict", StepOutcome::Working),
        "Request changes must be refused when verification can fail the run before feedback"
    );

    let error = record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "Fix the unchecked null.".into(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("every possible outcome on the Request changes route"),
        "the refusal should explain why this route cannot deliver the note: {error}"
    );
}

#[test]
fn operator_review_is_refused_when_parallel_failure_can_cancel_feedback() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition,
            GithubActionKind, StepOutcome,
        },
        stepper::{advance, Capacity, CircuitEvent, Effect, RunState, StepStatus},
    };

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\",\"source.name\":\"issue #42\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',10),(1,'verdict',1,'unverified',10);",
    )
    .unwrap();

    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph.nodes.push(CircuitNode {
        id: "parallel_label".into(),
        kind: CircuitNodeKind::GithubAction {
            action: GithubActionKind::AddLabel,
            open_pr_policy: None,
            label: Some("reviewed".into()),
            comment: None,
        },
    });
    graph.edges.push(CircuitEdge {
        from: "verdict".into(),
        to: "parallel_label".into(),
        condition: EdgeCondition::OnOutcome(StepOutcome::Working),
    });
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let mut simulated = recovery_view(&db, &stored).unwrap();
    let verdict = simulated.step_mut("verdict").unwrap();
    verdict.status = StepStatus::Completed;
    verdict.outcome = Some(StepOutcome::Working);
    advance(
        &mut simulated,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 1,
        }),
    );
    assert_eq!(
        simulated.step("feedback").unwrap().status,
        StepStatus::Running
    );
    assert_eq!(
        simulated.step("parallel_label").unwrap().status,
        StepStatus::Running,
        "the scheduler starts the sibling action while feedback waits for AgentReady"
    );
    advance(
        &mut simulated,
        &CircuitEvent::GithubActionResult {
            node_id: "parallel_label".into(),
            success: false,
            pr_number: None,
            pr_url: None,
            pr_head_ref: None,
            pr_title: None,
            error: Some("GitHub rejected the label change".into()),
        },
    );
    assert_eq!(simulated.state, RunState::Failed);
    let agent_ready = advance(
        &mut simulated,
        &CircuitEvent::AgentReady {
            node_id: "feedback".into(),
        },
    );
    assert!(
        agent_ready
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::InjectPty { .. })),
        "a fail-fast sibling can cancel feedback before prompt delivery"
    );

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let view = recovery_view(&db, &stored).unwrap();
    assert!(
        !view.can_record_review("verdict", StepOutcome::Working),
        "Request changes must be refused when a parallel fail-fast step can cancel feedback"
    );
    let error = record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "Fix the unchecked null.".into(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("every possible outcome on the Request changes route"),
        "the refusal should explain why the parallel route is unsafe: {error}"
    );
}

#[test]
fn operator_review_accepts_a_routed_confirm_pr_merged_failure() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition,
            GithubActionKind, StepOutcome,
        },
        stepper::{advance, Capacity, CircuitEvent, Effect, StepStatus},
    };

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\",\"source.name\":\"issue #42\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',10),(1,'verdict',1,'unverified',10);",
    )
    .unwrap();

    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .edges
        .retain(|edge| !(edge.from == "verdict" && edge.to == "feedback"));
    graph.nodes.push(CircuitNode {
        id: "confirm_merge".into(),
        kind: CircuitNodeKind::GithubAction {
            action: GithubActionKind::ConfirmPrMerged,
            open_pr_policy: None,
            label: None,
            comment: None,
        },
    });
    graph.edges.extend([
        CircuitEdge {
            from: "verdict".into(),
            to: "confirm_merge".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Working),
        },
        CircuitEdge {
            from: "confirm_merge".into(),
            to: "feedback".into(),
            condition: EdgeCondition::Always,
        },
    ]);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let view = recovery_view(&db, &stored).unwrap();
    assert!(
        view.can_record_review("verdict", StepOutcome::Working),
        "ConfirmPrMerged reports an unmerged PR as a completed, routed Failed outcome"
    );
    record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "Fix the unchecked null.".into(),
        },
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let mut active = recovery_view(&db, &stored).unwrap();
    advance(
        &mut active,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 1,
        }),
    );
    assert_eq!(
        active.step("confirm_merge").unwrap().status,
        StepStatus::Running
    );
    advance(
        &mut active,
        &CircuitEvent::GithubActionResult {
            node_id: "confirm_merge".into(),
            success: false,
            pr_number: None,
            pr_url: None,
            pr_head_ref: None,
            pr_title: None,
            error: Some("The pull request is not merged.".into()),
        },
    );
    assert_eq!(
        active.step("confirm_merge").unwrap().status,
        StepStatus::Completed
    );
    assert_eq!(
        active.step("confirm_merge").unwrap().outcome,
        Some(StepOutcome::Failed)
    );
    assert_eq!(active.state, crate::circuit::stepper::RunState::Running);
    let agent_ready = advance(
        &mut active,
        &CircuitEvent::AgentReady {
            node_id: "feedback".into(),
        },
    );
    assert!(agent_ready.effects.iter().any(|effect| matches!(
            effect,
            Effect::InjectPty { node_id, prompt, .. }
                if node_id == "feedback" && prompt.contains("Human review findings (operator):\nFix the unchecked null.")
        )));
}

#[test]
fn operator_review_accepts_a_routed_retry_limit_feedback_exhaustion() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
        },
        stepper::{advance, Capacity, CircuitEvent, Effect, StepStatus},
    };

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\",\"source.name\":\"issue #42\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                VALUES(1,'reviewer',1,'completed',10),(1,'verdict',1,'unverified',10);",
    )
    .unwrap();

    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph.edges.retain(|edge| {
        edge.from != "verdict"
            && edge.from != "retry"
            && edge.to != "retry"
            && edge.to != "feedback"
    });
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "retry")
        .unwrap()
        .kind = CircuitNodeKind::RetryLimit { max_retries: 1 };
    graph.nodes.extend([
        CircuitNode {
            id: "start_notice".into(),
            kind: CircuitNodeKind::Notify {
                message: "Begin feedback cycle".into(),
            },
        },
        CircuitNode {
            id: "before_retry".into(),
            kind: CircuitNodeKind::AnyCompleted,
        },
        CircuitNode {
            id: "retry_cycle".into(),
            kind: CircuitNodeKind::Notify {
                message: "Bounded feedback cycle".into(),
            },
        },
        CircuitNode {
            id: "completed_notice".into(),
            kind: CircuitNodeKind::Notify {
                message: "Retry remains available".into(),
            },
        },
        CircuitNode {
            id: "failed_notice".into(),
            kind: CircuitNodeKind::Notify {
                message: "Retry limit reached".into(),
            },
        },
        CircuitNode {
            id: "retry_join".into(),
            kind: CircuitNodeKind::AnyCompleted,
        },
    ]);
    graph.edges.extend([
        CircuitEdge {
            from: "verdict".into(),
            to: "start_notice".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Working),
        },
        CircuitEdge {
            from: "start_notice".into(),
            to: "before_retry".into(),
            condition: EdgeCondition::Always,
        },
        CircuitEdge {
            from: "before_retry".into(),
            to: "retry".into(),
            condition: EdgeCondition::Always,
        },
        // The first child is the bounded loop target, matching the
        // scheduler's feedback-cycle detection. Both routed outcomes
        // converge before the operator's feedback prompt.
        CircuitEdge {
            from: "retry".into(),
            to: "retry_cycle".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Completed),
        },
        CircuitEdge {
            from: "retry".into(),
            to: "completed_notice".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Completed),
        },
        CircuitEdge {
            from: "retry".into(),
            to: "failed_notice".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Failed),
        },
        CircuitEdge {
            from: "retry_cycle".into(),
            to: "before_retry".into(),
            condition: EdgeCondition::Always,
        },
        CircuitEdge {
            from: "completed_notice".into(),
            to: "retry_join".into(),
            condition: EdgeCondition::Always,
        },
        CircuitEdge {
            from: "failed_notice".into(),
            to: "retry_join".into(),
            condition: EdgeCondition::Always,
        },
        CircuitEdge {
            from: "retry_join".into(),
            to: "feedback".into(),
            condition: EdgeCondition::Always,
        },
    ]);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let view = recovery_view(&db, &stored).unwrap();
    assert!(
        view.can_record_review("verdict", StepOutcome::Working),
        "a RetryLimit feedback-cycle exhaustion follows its configured Failed route"
    );
    record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "Fix the unchecked null.".into(),
        },
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let mut active = recovery_view(&db, &stored).unwrap();
    advance(
        &mut active,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 1,
        }),
    );
    assert_eq!(active.step("retry").unwrap().status, StepStatus::Completed);
    assert_eq!(
        active.step("retry").unwrap().outcome,
        Some(StepOutcome::Failed)
    );
    let agent_ready = advance(
        &mut active,
        &CircuitEvent::AgentReady {
            node_id: "feedback".into(),
        },
    );
    assert!(agent_ready.effects.iter().any(|effect| matches!(
            effect,
            Effect::InjectPty { node_id, prompt, .. }
                if node_id == "feedback" && prompt.contains("Human review findings (operator):\nFix the unchecked null.")
        )));
}

#[test]
fn operator_review_refuses_a_running_feedback_with_a_fail_fast_sibling() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition,
            GithubActionKind, StepOutcome,
        },
        stepper::{advance, CircuitEvent, RunState, StepStatus},
    };
    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
            "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome,agent_node_id)
                VALUES(1,'reviewer',1,'completed','completed',10),
                    (1,'verdict',1,'unverified',NULL,10),
                    (1,'alternate_join',1,'completed','completed',NULL),
                    (1,'feedback',1,'running',NULL,9),
                    (1,'label_sibling',1,'running',NULL,NULL);",
        ).unwrap();
    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .edges
        .retain(|edge| !(edge.from == "verdict" && edge.to == "feedback"));
    graph.nodes.extend([
        CircuitNode {
            id: "alternate_join".into(),
            kind: CircuitNodeKind::AnyCompleted,
        },
        CircuitNode {
            id: "label_sibling".into(),
            kind: CircuitNodeKind::GithubAction {
                action: GithubActionKind::AddLabel,
                open_pr_policy: None,
                label: Some("reviewed".into()),
                comment: None,
            },
        },
    ]);
    graph.edges.extend([
        CircuitEdge {
            from: "reviewer".into(),
            to: "alternate_join".into(),
            condition: EdgeCondition::Always,
        },
        CircuitEdge {
            from: "verdict".into(),
            to: "alternate_join".into(),
            condition: EdgeCondition::OnOutcome(StepOutcome::Working),
        },
        CircuitEdge {
            from: "alternate_join".into(),
            to: "feedback".into(),
            condition: EdgeCondition::Always,
        },
        CircuitEdge {
            from: "reviewer".into(),
            to: "label_sibling".into(),
            condition: EdgeCondition::Always,
        },
    ]);
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let mut view = recovery_view(&db, &stored).unwrap();
    assert!(
        !view.can_record_review("verdict", StepOutcome::Working),
        "a fail-fast sibling can cancel the unbuilt feedback prompt"
    );
    let error = record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "Findings must reach feedback.".into(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("Request changes route"),
        "error was: {error}"
    );
    advance(
        &mut view,
        &CircuitEvent::GithubActionResult {
            node_id: "label_sibling".into(),
            success: false,
            pr_number: None,
            pr_url: None,
            pr_head_ref: None,
            pr_title: None,
            error: Some("GitHub action failed".into()),
        },
    );
    assert_eq!(view.state, RunState::Failed);
    assert_eq!(view.step("feedback").unwrap().status, StepStatus::Cancelled);
}

#[test]
fn operator_review_refuses_a_feedback_prompt_already_built_by_an_alternate_path() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
        },
        stepper::{advance, Capacity, CircuitEvent, Effect, StepStatus},
    };

    fn persist_stepper_transition(
        db: &mut Connection,
        view: &mut crate::circuit::stepper::RunView,
        transition: &crate::circuit::stepper::Transition,
    ) {
        let steps = transition
            .step_writes
            .iter()
            .map(|write| super::super::CircuitStepOp {
                node_id: write.node_id.clone(),
                status: write.status.as_db_str().into(),
                attempt: write.attempt,
                outcome: write
                    .outcome
                    .map(|outcome| outcome.map(|value| value.as_db_str().into())),
                error: write.error.clone(),
                agent_node_id: None,
                fresh_attempt: write.fresh_attempt,
            })
            .collect::<Vec<_>>();
        let revision = commit_transition_locked(
            db,
            1,
            transition
                .run_state_changed
                .then_some(view.state.as_db_str()),
            &view.context.to_json().unwrap(),
            &steps,
            EvidenceWrite {
                expected: transition.expected.as_ref(),
                ..Default::default()
            },
        )
        .unwrap();
        view.context.set("evidence.revision", revision.to_string());
    }

    for delivered in [false, true] {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch(
                "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
                INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
                INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
                INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');",
            )
            .unwrap();
        let context = serde_json::json!({
            "source.agent_id": "9",
            "source.name": "issue #42",
        });
        db.execute(
            "INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                    VALUES(1,1,1,'running',?1)",
            [context.to_string()],
        )
        .unwrap();

        let mut graph = CircuitGraph::agent_review(None, None, 3);
        graph
            .edges
            .retain(|edge| !(edge.from == "verdict" && edge.to == "feedback"));
        graph.nodes.extend([CircuitNode {
            id: "alternate_join".into(),
            kind: CircuitNodeKind::AnyCompleted,
        }]);
        graph.edges.extend([
            CircuitEdge {
                from: "reviewer".into(),
                to: "alternate_join".into(),
                condition: EdgeCondition::Always,
            },
            CircuitEdge {
                from: "verdict".into(),
                to: "alternate_join".into(),
                condition: EdgeCondition::OnOutcome(StepOutcome::Working),
            },
            CircuitEdge {
                from: "alternate_join".into(),
                to: "feedback".into(),
                condition: EdgeCondition::Always,
            },
        ]);
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO autopilot_circuit_run_steps
                    (run_id,node_id,attempt,status,outcome,agent_node_id)
                VALUES
                    (1,'reviewer',1,'completed','completed',10),
                    (1,'verdict',1,'unverified',NULL,10)",
            [],
        )
        .unwrap();

        let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let mut view = recovery_view(&db, &stored).unwrap();
        let scheduled = advance(
            &mut view,
            &CircuitEvent::Tick(Capacity {
                agent_free_slots: 1,
            }),
        );
        assert_eq!(
            view.step("alternate_join").unwrap().status,
            StepStatus::Completed
        );
        assert_eq!(view.step("feedback").unwrap().status, StepStatus::Running);
        assert_eq!(view.step("verdict").unwrap().status, StepStatus::Unverified);
        assert!(
            view.can_record_review("verdict", StepOutcome::Working),
            "an unbuilt prompt can still receive a newly recorded note"
        );
        persist_stepper_transition(&mut db, &mut view, &scheduled);

        let attempt = view.step("feedback").unwrap().attempt;
        let ready = advance(
            &mut view,
            &CircuitEvent::AgentReady {
                node_id: "feedback".into(),
            },
        );
        assert!(ready.effects.iter().any(|effect| matches!(
            effect,
            Effect::InjectPty { node_id, .. } if node_id == "feedback"
        )));
        assert_eq!(
            view.context
                .get(&format!("node.feedback.prompt_delivery.{attempt}")),
            Some("intent")
        );
        assert!(
            !view.can_record_review("verdict", StepOutcome::Working),
            "a built prompt cannot acquire a note from a later operator decision"
        );
        persist_stepper_transition(&mut db, &mut view, &ready);

        if delivered {
            let acknowledged = advance(
                &mut view,
                &CircuitEvent::PromptDelivered {
                    node_id: "feedback".into(),
                    attempt,
                },
            );
            assert_eq!(view.step("feedback").unwrap().status, StepStatus::Completed);
            assert_eq!(
                view.context
                    .get(&format!("node.feedback.prompt_delivery.{attempt}")),
                Some("acknowledged")
            );
            persist_stepper_transition(&mut db, &mut view, &acknowledged);
        }

        let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let view = recovery_view(&db, &stored).unwrap();
        assert!(
                !view.can_record_review("verdict", StepOutcome::Working),
                "Request changes must be refused after the alternate path builds the feedback prompt (delivered={delivered})"
            );
        let expected_revision = revision_inner(&db, 1).unwrap();
        let error = record_outcome_locked(
            &mut db,
            &CheckpointRequest {
                run_id: 1,
                node_id: "verdict".into(),
                attempt: 1,
                expected_revision,
                action: CheckpointAction::ReviewFail,
                reason: "Fix the unchecked null.".into(),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("every possible outcome on the Request changes route"),
            "the refusal should explain why feedback can no longer receive the note: {error}"
        );
    }
}

#[test]
fn operator_review_refuses_an_older_running_feedback_prompt_already_built() {
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
    };

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
            "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome,agent_node_id)
                VALUES(1,'reviewer',2,'completed','completed',10),
                    (1,'verdict',2,'unverified',NULL,10),
                    (1,'prior_notice',1,'completed','completed',NULL),
                    (1,'feedback',1,'running',NULL,9);",
        ).unwrap();
    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .nodes
        .retain(|node| matches!(node.id.as_str(), "reviewer" | "verdict" | "feedback"));
    graph
        .edges
        .retain(|edge| edge.from == "verdict" && edge.to == "feedback");
    graph.edges.push(CircuitEdge {
        from: "reviewer".into(),
        to: "verdict".into(),
        condition: EdgeCondition::Always,
    });
    graph.nodes.push(CircuitNode {
        id: "prior_notice".into(),
        kind: CircuitNodeKind::Notify {
            message: "Round one notice".into(),
        },
    });
    graph.edges.push(CircuitEdge {
        from: "prior_notice".into(),
        to: "feedback".into(),
        condition: EdgeCondition::Always,
    });
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let mut view = recovery_view(&db, &stored).unwrap();
    view.context
        .set("node.feedback.prompt_delivery.1", "intent");
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1 WHERE id=1",
        [view.context.to_json().unwrap()],
    )
    .unwrap();
    assert!(
        !view.can_record_review("verdict", StepOutcome::Working),
        "an older Running injection cannot be rearmed to deliver a new note"
    );
    let error = record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 2,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "These findings arrived after prompt intent.".into(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("Request changes route"),
        "error was: {error}"
    );
}

#[test]
fn operator_review_round_two_joins_a_prior_notify_and_delivers_the_note() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
        },
        stepper::{advance, Capacity, CircuitEvent, Effect, StepStatus},
    };

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
            "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome,agent_node_id)
                VALUES(1,'reviewer',2,'completed','completed',10),
                    (1,'verdict',2,'unverified',NULL,10),
                    (1,'prior_notice',1,'completed','completed',NULL),
                    (1,'feedback',1,'completed','completed',9);",
        ).unwrap();
    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .nodes
        .retain(|node| matches!(node.id.as_str(), "reviewer" | "verdict" | "feedback"));
    graph
        .edges
        .retain(|edge| edge.from == "verdict" && edge.to == "feedback");
    graph.edges.push(CircuitEdge {
        from: "reviewer".into(),
        to: "verdict".into(),
        condition: EdgeCondition::Always,
    });
    graph.nodes.push(CircuitNode {
        id: "prior_notice".into(),
        kind: CircuitNodeKind::Notify {
            message: "Round one notice".into(),
        },
    });
    graph.edges.push(CircuitEdge {
        from: "prior_notice".into(),
        to: "feedback".into(),
        condition: EdgeCondition::Always,
    });
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let view = recovery_view(&db, &stored).unwrap();
    assert!(
        view.can_record_review("verdict", StepOutcome::Working),
        "a completed prior-attempt Notify still satisfies the feedback join"
    );
    record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 2,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "Fix the round-two null check.".into(),
        },
    )
    .unwrap();
    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let mut active = recovery_view(&db, &stored).unwrap();
    advance(
        &mut active,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 1,
        }),
    );
    let feedback = active.step("feedback").expect("feedback reruns");
    assert_eq!(feedback.status, StepStatus::Running);
    assert_eq!(feedback.attempt, 2);
    assert_eq!(active.step("prior_notice").unwrap().attempt, 1);
    let ready = advance(
        &mut active,
        &CircuitEvent::AgentReady {
            node_id: "feedback".into(),
        },
    );
    assert!(ready.effects.iter().any(|effect| matches!(effect,
        Effect::InjectPty { node_id, prompt, .. } if node_id == "feedback"
            && prompt.contains("Human review findings (operator):\nFix the round-two null check.")
    )));
}

#[test]
fn operator_review_refuses_a_join_blocked_by_a_prior_failed_retry_limit() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
        },
        stepper::{advance, Capacity, CircuitEvent, StepStatus},
    };

    let mut db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch(
            "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
            INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                VALUES(1,1,1,'running','{\"source.agent_id\":\"9\"}');
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,outcome,agent_node_id)
                VALUES(1,'reviewer',2,'completed','completed',10),
                    (1,'verdict',2,'unverified',NULL,10),
                    (1,'prior_retry',1,'completed','failed',NULL),
                    (1,'feedback',1,'completed','completed',9);",
        ).unwrap();
    let mut graph = CircuitGraph::agent_review(None, None, 3);
    graph
        .nodes
        .retain(|node| matches!(node.id.as_str(), "reviewer" | "verdict" | "feedback"));
    graph
        .edges
        .retain(|edge| edge.from == "verdict" && edge.to == "feedback");
    graph.edges.push(CircuitEdge {
        from: "reviewer".into(),
        to: "verdict".into(),
        condition: EdgeCondition::Always,
    });
    graph.nodes.push(CircuitNode {
        id: "prior_retry".into(),
        kind: CircuitNodeKind::RetryLimit { max_retries: 1 },
    });
    graph.edges.push(CircuitEdge {
        from: "prior_retry".into(),
        to: "feedback".into(),
        condition: EdgeCondition::OnOutcome(StepOutcome::Completed),
    });
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();

    let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let mut view = recovery_view(&db, &stored).unwrap();
    assert!(
        !view.can_record_review("verdict", StepOutcome::Working),
        "the prior RetryLimit's failed outcome still blocks its Completed edge"
    );
    let error = record_outcome_locked(
        &mut db,
        &CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 2,
            expected_revision: 0,
            action: CheckpointAction::ReviewFail,
            reason: "Findings must reach the implementer.".into(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("Request changes route"),
        "error was: {error}"
    );
    assert_eq!(view.step("verdict").unwrap().status, StepStatus::Unverified);

    // Reproduce what the real scheduler would do if the refused verdict
    // were nevertheless accepted: its retained RetryLimit edge blocks feedback.
    let verdict = view
        .steps
        .iter_mut()
        .find(|step| step.node_id == "verdict")
        .unwrap();
    verdict.status = StepStatus::Completed;
    verdict.outcome = Some(StepOutcome::Working);
    advance(
        &mut view,
        &CircuitEvent::Tick(Capacity {
            agent_free_slots: 1,
        }),
    );
    assert_eq!(view.step("feedback").unwrap().attempt, 1);
    assert_eq!(view.step("feedback").unwrap().status, StepStatus::Completed);
}

#[test]
fn recorded_operator_review_drives_the_next_tick_and_injected_prompt() {
    use crate::circuit::{
        model::{
            CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, StepOutcome,
        },
        stepper::{advance, Capacity, CircuitEvent, Effect, StepStatus},
    };

    for (action, next_node, expected_prompt, note) in [
        (
            CheckpointAction::ReviewPass,
            "merge",
            "The human reviewer approved",
            "I checked the current PR and its required checks.",
        ),
        (
            CheckpointAction::ReviewFail,
            "feedback",
            "Human review findings (operator):\nFix the unchecked null before merging.",
            "Fix the unchecked null before merging.",
        ),
    ] {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
                INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'source','/repo');
                INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(10,1,'reviewer','/repo/reviewer');
                INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
                INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,context_json)
                    VALUES(1,1,1,'running','{\"source.agent_id\":\"9\",\"source.name\":\"issue #42\"}');
                INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id)
                    VALUES(1,'reviewer',1,'completed',10),(1,'verdict',1,'unverified',10);").unwrap();
        let mut graph = CircuitGraph::agent_review(None, None, 3);
        if let Some(node) = graph.nodes.iter_mut().find(|node| node.id == "feedback") {
            let crate::circuit::model::CircuitNodeKind::InjectPty { prompt, .. } = &mut node.kind
            else {
                panic!("feedback must inject a prompt");
            };
            *prompt = "Address the requested changes and summarize your work.".into();
        }
        if next_node == "feedback" {
            graph
                .edges
                .retain(|edge| !(edge.from == "verdict" && edge.to == "feedback"));
            graph.nodes.push(CircuitNode {
                id: "feedback_notice".into(),
                kind: CircuitNodeKind::Notify {
                    message: "An operator requested changes.".into(),
                },
            });
            graph.edges.push(CircuitEdge {
                from: "verdict".into(),
                to: "feedback_notice".into(),
                condition: EdgeCondition::OnOutcome(StepOutcome::Working),
            });
            graph.edges.push(CircuitEdge {
                from: "feedback_notice".into(),
                to: "feedback".into(),
                condition: EdgeCondition::OnOutcome(StepOutcome::Completed),
            });
        }
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();

        if next_node == "feedback" {
            let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
                .unwrap()
                .unwrap();
            let view = recovery_view(&db, &stored).unwrap();
            assert!(view.step("feedback_notice").is_none());
            assert!(
                view.can_record_review("verdict", StepOutcome::Working),
                "a not-yet-run completed-outcome intermediate can reach feedback"
            );
        }

        let request = CheckpointRequest {
            run_id: 1,
            node_id: "verdict".into(),
            attempt: 1,
            expected_revision: 0,
            action,
            reason: note.into(),
        };
        record_outcome_locked(&mut db, &request).unwrap();
        let stored = super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let mut view = recovery_view(&db, &stored).unwrap();
        let tick = advance(
            &mut view,
            &CircuitEvent::Tick(Capacity {
                agent_free_slots: 1,
            }),
        );
        assert_eq!(view.step(next_node).unwrap().status, StepStatus::Running);
        if next_node == "feedback" {
            assert_eq!(
                view.step("verdict").unwrap().outcome,
                Some(StepOutcome::Working)
            );
            assert!(tick.effects.iter().any(|effect| matches!(effect, Effect::Notify { message } if message == "An operator requested changes.")));
        } else {
            assert!(tick.effects.iter().any(|effect| matches!(effect, Effect::CloseAgentNode { node_id, .. } if node_id == "close_approved")));
        }

        let ready = advance(
            &mut view,
            &CircuitEvent::AgentReady {
                node_id: next_node.into(),
            },
        );
        let prompt = ready
            .effects
            .iter()
            .find_map(|effect| match effect {
                Effect::InjectPty {
                    node_id, prompt, ..
                } if node_id == next_node => Some(prompt),
                _ => None,
            })
            .expect("the next scheduled prompt is injected when its target is ready");
        assert!(prompt.contains(expected_prompt), "prompt was: {prompt}");
        assert!(prompt.contains("Address the requested changes") || next_node == "merge");
    }
}
