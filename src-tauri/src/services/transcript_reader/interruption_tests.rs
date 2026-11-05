use super::*;

fn failed(code: &str) -> String {
    serde_json::json!({"type":"event_msg","timestamp":"2026-11-05T08:30:19.103Z",
        "payload":{"type":"task_complete","turn_id":"t","last_agent_message":null,
            "error":{"codex_error_info":code}}})
    .to_string()
        + "\n"
}

#[test]
fn native_capacity_retries_but_daybreak_and_unknown_errors_do_not() {
    let failure = codex(&failed("server_overloaded")).unwrap();
    assert_eq!(failure.kind, FailureKind::Transient);
    assert_eq!(
        (0..4).map(|n| failure.delay_ms(n)).collect::<Vec<_>>(),
        vec![Some(30_000), Some(120_000), Some(300_000), None]
    );
    for code in ["cyber_policy", "unauthorized", "new_unknown_error"] {
        assert_eq!(codex(&failed(code)).unwrap().delay_ms(0), None);
    }
}

#[test]
fn newer_work_user_interrupt_damage_and_success_invalidate_failure() {
    for next in [
        r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"new"}}"#,
        r#"{"type":"event_msg","payload":{"type":"turn_aborted"}}"#,
        r#"{"type":"response_item","payload":{"type":"function_call"}}"#,
        r#"{"type":"response_item","payload":{"type":"message","role":"user"}}"#,
        r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"new","error":null}}"#,
        "{broken",
    ] {
        assert!(
            codex(&(failed("server_overloaded") + next)).is_none(),
            "{next}"
        );
    }
    assert!(codex(r#"{"type":"response_item","payload":{"type":"message","role":"assistant","text":"server_overloaded"}}"#).is_none());
}

fn store() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE local_runtime_sessions(session_id TEXT PRIMARY KEY,status TEXT,error_code INTEGER,error_message TEXT,updated_at_ms INTEGER);
        CREATE TABLE local_runtime_session_agent_state(session_id TEXT PRIMARY KEY,turn_id TEXT,terminal_outcome TEXT,updated_at_ms INTEGER);
        CREATE TABLE local_runtime_turn_ingress(turn_id TEXT PRIMARY KEY,session_id TEXT,status TEXT,accepted_sequence INTEGER);
        CREATE INDEX idx_local_runtime_turn_ingress_session_accepted_sequence ON local_runtime_turn_ingress(session_id,accepted_sequence DESC) WHERE status='accepted';
        CREATE UNIQUE INDEX idx_local_runtime_turn_ingress_sequence ON local_runtime_turn_ingress(accepted_sequence) WHERE accepted_sequence IS NOT NULL;
        INSERT INTO local_runtime_turn_ingress VALUES('t','s','failed',1);
        INSERT INTO local_runtime_sessions VALUES('s','error',50113,'Connection error.',100);
        INSERT INTO local_runtime_session_agent_state VALUES('s','t','failed',105);").unwrap();
    c
}

#[test]
fn mcode_requires_matching_failed_session_and_no_newer_or_active_turn() {
    let c = store();
    assert_eq!(
        mcode(&c, "s").unwrap().unwrap().kind,
        FailureKind::Transient
    );
    assert!(mcode(&c, "other").unwrap().is_none());
    c.execute(
        "INSERT INTO local_runtime_turn_ingress VALUES('new','s','accepted',2)",
        [],
    )
    .unwrap();
    assert!(mcode(&c, "s").unwrap().is_none());
    c.execute(
        "UPDATE local_runtime_turn_ingress SET status='completed' WHERE turn_id='new'",
        [],
    )
    .unwrap();
    assert!(mcode(&c, "s").unwrap().is_none());
    c.execute(
        "DELETE FROM local_runtime_turn_ingress WHERE turn_id='new'",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE local_runtime_session_agent_state SET terminal_outcome='completed'",
        [],
    )
    .unwrap();
    assert!(mcode(&c, "s").unwrap().is_none());
}

#[test]
fn quota_has_slow_bounded_retries_and_unrecognized_errors_need_inspection() {
    let c = store();
    c.execute("UPDATE local_runtime_sessions SET error_code=42212,error_message='The Token Plan usage limit has been reached. (2067)'", []).unwrap();
    let failure = mcode(&c, "s").unwrap().unwrap();
    assert_eq!(failure.kind, FailureKind::Quota);
    assert_eq!(failure.delay_ms(0), Some(900_000));
    assert_eq!(failure.delay_ms(3), None);
    c.execute(
        "UPDATE local_runtime_sessions SET error_message='Different account error'",
        [],
    )
    .unwrap();
    assert_eq!(mcode(&c, "s").unwrap().unwrap().kind, FailureKind::Other);
}

#[test]
fn mcode_probe_uses_native_indexes_and_bounds_newer_ingress() {
    let c = store();
    for sql in [MCODE_FAILURE_SQL, MCODE_ACTIVE_SQL, MCODE_NEWER_SQL] {
        let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan = stmt
            .query_map(["s"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(!plan.contains("SCAN"), "unbounded native probe: {plan}");
        assert!(plan.contains("SEARCH"), "missing indexed lookup: {plan}");
    }
    for n in 2..=1026 {
        c.execute(
            "INSERT INTO local_runtime_turn_ingress VALUES(?1,'another-session','completed',?2)",
            rusqlite::params![format!("other-{n}"), n],
        )
        .unwrap();
    }
    assert!(
        mcode(&c, "s").is_none(),
        "overflow is unavailable, not proof of a safe retry"
    );
    c.execute(
        "DELETE FROM local_runtime_turn_ingress WHERE accepted_sequence=1026",
        [],
    )
    .unwrap();
    assert!(mcode(&c, "s").unwrap().is_some());
    c.execute("DROP INDEX idx_local_runtime_turn_ingress_sequence", [])
        .unwrap();
    assert!(
        mcode(&c, "s").is_none(),
        "schema drift must not fall back to table scans"
    );
}
