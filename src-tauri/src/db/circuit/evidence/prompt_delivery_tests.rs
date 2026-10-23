use super::*;
use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};

fn native(event: &str, turn: &str, source: &str) -> NativeReceipt {
    NativeReceipt {
        agent_node_id:9,input_stamp:Some("manual-input".into()),session_incarnation:Some("1000".into()),
        source_id:source.into(),received_at_ms:1000,turn_fenced:true,explicit_turn_mismatch:false,
        submission_correlated:false,submission_seq:None,
        hook:NativeHook::parse("claude",serde_json::to_vec(&serde_json::json!({
            "session_id":"session","prompt_id":turn,"hook_event_name":event,"prompt":"run the tests"
        })).unwrap().as_slice()).unwrap(),
    }
}

fn fixture(kind: DeliveryKind) -> (Connection, PayloadIdentity, CheckpointRequest) {
    let db = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&db).unwrap();
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
        INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(9,1,'agent','/repo','awaiting_input');
        INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
        INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id) VALUES(1,1,1,'running',9);
        INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id) VALUES(1,'send',1,'unverified',9);").unwrap();
    let node_kind = match kind {
        DeliveryKind::Inject => {
            serde_json::json!({"type":"inject_pty","prompt":"work","target_node_id":"$source"})
        }
        DeliveryKind::Initial => serde_json::json!({"type":"spawn_agent_node","prompt":"work"}),
        DeliveryKind::Continuation | DeliveryKind::Nudge => {
            serde_json::json!({"type":"await_agent_turn","target_node_id":"$source"})
        }
    };
    let graph =
        serde_json::json!({"version":3,"nodes":[{"id":"send","type":node_kind}],"edges":[]});
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_string()],
    )
    .unwrap();
    let mut context = crate::circuit::context::CircuitContext::default();
    context.set("source.agent_id", "9");
    context.set("node.send.human_wait", "1");
    if kind == DeliveryKind::Continuation {
        context.set("node.send.continuation.delivery", "uncertain");
    }
    if kind == DeliveryKind::Nudge {
        context.set("node.send.nudge.delivery", "uncertain");
    }
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();
    let text = "run the tests";
    record_prompt_submission_locked(&db, 1, "send", 1, 9, text).unwrap();
    let identity = PayloadIdentity {
        agent_node_id: 9,
        digest: crate::services::circuit_worker::native_hooks::submission_digest(text),
        kind,
        turn: None,
        submission_seq: 1,
        prepared_at_ms: 500,
    };
    append_history(
        &db,
        HistoryEvent {
            run_id: 1,
            node_id: Some("send"),
            attempt: Some(1),
            kind: "prompt_payload",
            detail: &serde_json::to_string(&identity).unwrap(),
            source: Some(SOURCE_PROMPT_SUBMISSION),
            disposition: Some(DISPOSITION_RECORDED),
        },
    )
    .unwrap();
    if kind == DeliveryKind::Initial {
        db.execute(
            "INSERT INTO circuit_effects VALUES(1,'send',1,'spawn','acknowledged')",
            [],
        )
        .unwrap();
        append_history(
            &db,
            HistoryEvent {
                run_id: 1,
                node_id: Some("send"),
                attempt: Some(1),
                kind: "prompt_delivery_uncertain",
                detail: "Initial prompt input was owned",
                source: Some(SOURCE_CIRCUIT_WORKER),
                disposition: Some("uncertain"),
            },
        )
        .unwrap();
    }
    let request = CheckpointRequest {
        run_id: 1,
        node_id: "send".into(),
        attempt: 1,
        expected_revision: revision_inner(&db, 1).unwrap(),
        action: CheckpointAction::ManualDelivered,
        reason: String::new(),
    };
    (db, identity, request)
}

#[test]
fn manual_delivery_ignores_stale_waits_and_preserves_submission_for_every_prompt_kind() {
    for kind in [
        DeliveryKind::Inject,
        DeliveryKind::Initial,
        DeliveryKind::Continuation,
        DeliveryKind::Nudge,
    ] {
        let (mut db, identity, request) = fixture(kind);
        assert!(available(&db, 1, "send", 1).unwrap());
        attest_locked(&mut db, &request, &identity).unwrap();
        let status: String = db
            .query_row("SELECT status FROM autopilot_circuit_run_steps", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "running");
        assert_eq!(
            newest_submission_seq(&db, 9).unwrap(),
            1,
            "must preserve hooks that already correlated"
        );
        let state: String = db
            .query_row(
                "SELECT state FROM circuit_effects WHERE kind='prompt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "acknowledged");
        let status: String = db
            .query_row("SELECT status FROM agent_nodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "running");
        let run = super::super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let context =
            crate::circuit::context::CircuitContext::from_json(&run.context_json).unwrap();
        assert_ne!(context.get("node.send.human_wait"), Some("1"));
        if kind == DeliveryKind::Inject {
            let mut view = recovery_view(&db, &run).unwrap();
            crate::circuit::stepper::advance(
                &mut view,
                &crate::circuit::stepper::CircuitEvent::PromptDelivered {
                    node_id: "send".into(),
                    attempt: 1,
                },
            );
            assert_eq!(
                view.step("send").unwrap().status,
                crate::circuit::stepper::StepStatus::Completed
            );
        }
        assert!(
            attest_locked(&mut db, &request, &identity).is_err(),
            "stale duplicate cannot act"
        );
    }
}

#[test]
fn initial_prompt_recovery_requires_current_delivery_uncertainty_not_an_inconclusive_report() {
    {
        let (mut db, identity, request) = fixture(DeliveryKind::Initial);
        attest_locked(&mut db, &request, &identity).unwrap();
        db.execute(
            "UPDATE autopilot_circuit_run_steps SET status='unverified'",
            [],
        )
        .unwrap();
        assert!(
            !available(&db, 1, "send", 1).unwrap(),
            "delivery already acknowledged before an inconclusive report"
        );
    }
    let (mut db, mut identity, mut request) = fixture(DeliveryKind::Initial);
    assert!(
        available(&db, 1, "send", 1).unwrap(),
        "explicit initial delivery failure"
    );
    db.execute(
        "DELETE FROM circuit_run_history WHERE kind='prompt_delivery_uncertain'",
        [],
    )
    .unwrap();
    assert!(
        !available(&db, 1, "send", 1).unwrap(),
        "acknowledged spawn with only a report checkpoint"
    );
    request.expected_revision = revision_inner(&db, 1).unwrap();
    assert!(attest_locked(&mut db, &request, &identity).is_err());
    db.execute(
        "UPDATE circuit_effects SET state='uncertain' WHERE kind='spawn'",
        [],
    )
    .unwrap();
    assert!(
        available(&db, 1, "send", 1).unwrap(),
        "live-agent retry failed before attachment acknowledgement"
    );
    db.execute(
        "UPDATE circuit_effects SET state='acknowledged' WHERE kind='spawn'",
        [],
    )
    .unwrap();
    append_history(
        &db,
        HistoryEvent {
            run_id: 1,
            node_id: Some("send"),
            attempt: Some(1),
            kind: "prompt_delivery_uncertain",
            detail: "old delivery",
            source: Some(SOURCE_CIRCUIT_WORKER),
            disposition: Some("uncertain"),
        },
    )
    .unwrap();
    record_prompt_submission_locked(&db, 1, "send", 1, 9, "new prepared prompt").unwrap();
    identity.submission_seq = 2;
    identity.digest =
        crate::services::circuit_worker::native_hooks::submission_digest("new prepared prompt");
    append_history(
        &db,
        HistoryEvent {
            run_id: 1,
            node_id: Some("send"),
            attempt: Some(1),
            kind: "prompt_payload",
            detail: &serde_json::to_string(&identity).unwrap(),
            source: Some(SOURCE_PROMPT_SUBMISSION),
            disposition: Some(DISPOSITION_RECORDED),
        },
    )
    .unwrap();
    assert!(
        !available(&db, 1, "send", 1).unwrap(),
        "an older uncertainty does not authorize a newer payload"
    );
}

#[test]
fn stale_revision_attempt_target_and_replaced_submission_cannot_attest() {
    for sql in [
        "UPDATE autopilot_circuit_run_steps SET attempt=2",
        "UPDATE autopilot_circuit_run_steps SET agent_node_id=NULL; UPDATE autopilot_circuit_runs SET source_agent_node_id=NULL",
        "UPDATE autopilot_circuit_runs SET state='cancelled'",
        "UPDATE autopilot_circuit_run_steps SET status='completed'",
    ] {
        let (mut db, identity, request) = fixture(DeliveryKind::Initial);
        db.execute_batch(sql).unwrap();
        assert!(attest_locked(&mut db,&request,&identity).is_err(), "{sql}");
    }
    let (mut db, identity, mut request) = fixture(DeliveryKind::Inject);
    request.expected_revision = 0;
    assert!(attest_locked(&mut db, &request, &identity).is_err());
    record_prompt_submission_locked(&db, 1, "other", 1, 9, "new input").unwrap();
    request.expected_revision = revision_inner(&db, 1).unwrap();
    assert!(!available(&db, 1, "send", 1).unwrap());
    assert!(attest_locked(&mut db, &request, &identity).is_err());
}

#[test]
fn a_manually_delivered_prompt_binds_the_subsequent_native_turn_and_stop() {
    use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};
    let (mut db, identity, request) = fixture(DeliveryKind::Initial);
    attest_locked(&mut db, &request, &identity).unwrap();
    for (event, source) in [("UserPromptSubmit", "start"), ("Stop", "stop")] {
        let hook=NativeHook::parse("claude",serde_json::to_vec(&serde_json::json!({
            "session_id":"session","prompt_id":"manual-turn","hook_event_name":event,"prompt":"run the tests"
        })).unwrap().as_slice()).unwrap();
        let receipt = NativeReceipt {
            agent_node_id: 9,
            input_stamp: Some("manual-input".into()),
            session_incarnation: Some("1000".into()),
            source_id: source.into(),
            received_at_ms: 1000,
            turn_fenced: true,
            explicit_turn_mismatch: false,
            submission_correlated: false,
            submission_seq: None,
            hook,
        };
        receive_native_hook_locked(&mut db, &receipt).unwrap();
    }
    let rows = history_inner(&db, 1).unwrap();
    let receipts: Vec<NativeReceipt> = rows
        .iter()
        .filter(|r| r.kind == "native_hook_received")
        .map(|r| serde_json::from_str(&r.detail).unwrap())
        .collect();
    assert_eq!(receipts.len(), 2);
    for receipt in receipts {
        assert!(receipt.submission_correlated);
        assert_eq!(receipt.submission_seq, Some(1));
        assert_eq!(receipt.input_stamp.as_deref(), Some("manual-input"));
    }
}

#[test]
fn manual_injection_keeps_the_turn_bound_across_its_successor_wait() {
    use crate::circuit::stepper::{advance, CircuitEvent, StepStatus};
    for start_before_confirmation in [true, false] {
        let (mut db, identity, mut request) = fixture(DeliveryKind::Inject);
        let mut graph = run_graph(&db, 1).unwrap();
        graph.nodes.push(crate::circuit::model::CircuitNode {
            id: "wait".into(),
            kind: CircuitNodeKind::AwaitAgentTurn {
                target_node_id: Some("$source".into()),
            },
        });
        graph.edges.push(crate::circuit::model::CircuitEdge {
            from: "send".into(),
            to: "wait".into(),
            condition: Default::default(),
        });
        db.execute(
            "UPDATE autopilot_circuits SET graph_json=?1",
            [graph.to_json().unwrap()],
        )
        .unwrap();
        if start_before_confirmation {
            receive_native_hook_locked(&mut db, &native("UserPromptSubmit", "turn-1", "start"))
                .unwrap();
            request.expected_revision = revision_inner(&db, 1).unwrap();
        }
        attest_locked(&mut db, &request, &identity).unwrap();
        let run = super::super::super::ledger::get_circuit_run_inner(&db, 1)
            .unwrap()
            .unwrap();
        let mut view = recovery_view(&db, &run).unwrap();
        let transition = advance(
            &mut view,
            &CircuitEvent::PromptDelivered {
                node_id: "send".into(),
                attempt: 1,
            },
        );
        assert_eq!(view.step("send").unwrap().status, StepStatus::Completed);
        assert_eq!(view.step("wait").unwrap().status, StepStatus::Running);
        let ops: Vec<_> = transition
            .step_writes
            .iter()
            .map(|s| super::super::super::CircuitStepOp {
                node_id: s.node_id.clone(),
                status: s.status.as_db_str().into(),
                attempt: s.attempt,
                outcome: s.outcome.map(|o| o.map(|o| o.as_db_str().into())),
                error: s.error.clone(),
                agent_node_id: s.agent_node_id,
                fresh_attempt: s.fresh_attempt,
            })
            .collect();
        super::super::super::ledger::commit_circuit_advance_inner(
            &db,
            1,
            None,
            Some(&view.context.to_json().unwrap()),
            &ops,
        )
        .unwrap();
        if !start_before_confirmation {
            receive_native_hook_locked(&mut db, &native("UserPromptSubmit", "turn-1", "start"))
                .unwrap();
        }
        receive_native_hook_locked(&mut db, &native("Stop", "turn-1", "stop")).unwrap();
        let rows = history_inner(&db, 1).unwrap();
        let stop: NativeReceipt = rows
            .iter()
            .filter(|r| r.node_id.as_deref() == Some("wait") && r.kind == "native_hook_received")
            .map(|r| serde_json::from_str::<NativeReceipt>(&r.detail).unwrap())
            .find(|r| r.source_id == "stop")
            .unwrap();
        assert!(stop.submission_correlated);
        assert_eq!(stop.submission_seq, Some(1));
        assert_eq!(stop.input_stamp.as_deref(), Some("manual-input"));
        // A second turn cannot reuse a submission just because its first
        // start was recorded against the downstream observer.
        receive_native_hook_locked(
            &mut db,
            &native("UserPromptSubmit", "other-turn", "other-start"),
        )
        .unwrap();
        let other: NativeReceipt = history_inner(&db, 1)
            .unwrap()
            .iter()
            .filter(|r| r.node_id.as_deref() == Some("wait") && r.kind == "native_hook_received")
            .map(|r| serde_json::from_str::<NativeReceipt>(&r.detail).unwrap())
            .find(|r| r.source_id == "other-start")
            .unwrap();
        assert!(!other.submission_correlated);
    }
}

#[test]
fn manual_delivery_preserves_requests_and_background_work_from_the_new_turn() {
    use crate::circuit::observation::{
        HumanWait, HumanWaitKind, ObservationIdentity, WorkEvidence,
    };
    let (mut db, identity, mut request) = fixture(DeliveryKind::Initial);
    receive_native_hook_locked(&mut db, &native("UserPromptSubmit", "turn-1", "start")).unwrap();
    let mut evidence = WorkEvidence::default();
    evidence.children.insert("still-working".into(), false);
    let owner = ObservationIdentity {
        run_id: 1,
        step_id: "send".into(),
        attempt: 1,
        agent_node_id: 9,
        session_incarnation: Some("1000".into()),
        session_id: Some("session".into()),
        turn_id: Some("turn-1".into()),
        report_revision: None,
    };
    for observed_at_ms in [200, 1200] {
        evidence.human_waits.push(HumanWait {
            wait_kind: HumanWaitKind::Permission,
            request_id: Some(format!("permission-{observed_at_ms}")),
            identity: owner.clone(),
            source: "native".into(),
            source_id: None,
            observed_at_ms,
            authoritative: true,
            resolved_at_ms: None,
        });
    }
    let mut context = crate::circuit::context::CircuitContext::default();
    context.set(
        "node.send.evidence.1",
        serde_json::to_string(&evidence).unwrap(),
    );
    db.execute(
        "UPDATE autopilot_circuit_runs SET context_json=?1",
        [context.to_json().unwrap()],
    )
    .unwrap();
    request.expected_revision = revision_inner(&db, 1).unwrap();
    attest_locked(&mut db, &request, &identity).unwrap();
    let run = super::super::super::ledger::get_circuit_run_inner(&db, 1)
        .unwrap()
        .unwrap();
    let context = crate::circuit::context::CircuitContext::from_json(&run.context_json).unwrap();
    let retained: WorkEvidence =
        serde_json::from_str(context.get("node.send.evidence.1").unwrap()).unwrap();
    assert!(retained.human_waits[0].resolved_at_ms.is_some());
    assert!(retained.human_waits[1].resolved_at_ms.is_none());
    assert_eq!(retained.children.get("still-working"), Some(&false));
}

#[test]
fn old_initial_watchers_cannot_undo_manual_recovery_or_touch_a_later_attempt() {
    let (mut db, identity, request) = fixture(DeliveryKind::Initial);
    let fence = InitialDeliveryFence {
        run_id: 1,
        node_id: "send".into(),
        attempt: 1,
        identity: identity.clone(),
    };
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET status='running'",
        [],
    )
    .unwrap();
    assert!(initial_pending_inner(&db, &fence).unwrap());
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET status='unverified'",
        [],
    )
    .unwrap();
    attest_locked(&mut db, &request, &identity).unwrap();
    assert!(!initial_pending_inner(&db, &fence).unwrap());
    db.execute_batch(
        "DELETE FROM circuit_effects; UPDATE autopilot_circuit_run_steps SET attempt=2",
    )
    .unwrap();
    assert!(!initial_pending_inner(&db, &fence).unwrap());
}
