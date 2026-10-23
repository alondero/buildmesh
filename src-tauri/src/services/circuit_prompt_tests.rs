use super::*;

#[test]
fn retained_payload_is_exact_and_detects_missing_or_modified_files() {
    let dir = tempfile::tempdir().unwrap();
    let text = "Read and follow C:\\task brief.md\r\nUnicode: café\nComplete payload\n";
    let request = CircuitPromptRequest {
        run_id: 1,
        node_id: "send/../prompt".into(),
        attempt: 2,
        expected_revision: 0,
    };
    let identity = PayloadIdentity {
        agent_node_id: 9,
        digest: crate::services::circuit_worker::native_hooks::submission_digest(text),
        kind: DeliveryKind::Inject,
        turn: None,
        submission_seq: 1,
        prepared_at_ms: 0,
    };
    assert!(read_payload(dir.path(), &request, &identity).is_err());
    let path = payload_path(dir.path(), 1, &request.node_id, 2);
    handoff::write_prompt(&path, text).unwrap();
    assert_eq!(read_payload(dir.path(), &request, &identity).unwrap(), text);
    handoff::write_prompt(&path, "different payload").unwrap();
    assert!(read_payload(dir.path(), &request, &identity).is_err());
    assert!(path.starts_with(handoff::run_dir_in(dir.path(), 1)));
    assert_ne!(
        payload_path(dir.path(), 1, "send.a", 2),
        payload_path(dir.path(), 1, "send_a", 2)
    );
}

#[test]
fn payload_command_rejects_missing_run_and_malformed_request() {
    let _db = crate::db::test_support::isolated();
    let request = CircuitPromptRequest {
        run_id: 999,
        node_id: "missing".into(),
        attempt: 1,
        expected_revision: 0,
    };
    assert!(crate::commands::circuit::circuit_prompt_payload_blocking(request).is_err());
    assert!(
        serde_json::from_value::<CircuitPromptRequest>(serde_json::json!({"run_id":"bad"}))
            .is_err()
    );
}

#[test]
fn payload_ipc_and_manual_outcome_use_the_retained_text_without_rebuilding_the_handoff() {
    let _db = crate::db::test_support::isolated();
    let dir = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(dir.path().to_path_buf());
    {
        let db = crate::db::write_conn();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
            INSERT INTO agent_nodes(id,mesh_id,name,path) VALUES(9,1,'agent','/repo');
            INSERT INTO autopilot_circuits(id,mesh_id,name,graph_json) VALUES(1,1,'test','{\"version\":3,\"nodes\":[{\"id\":\"send\",\"type\":{\"type\":\"inject_pty\",\"prompt\":\"changed template\"}}],\"edges\":[]}');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id) VALUES(1,1,1,'running',9);
            INSERT INTO autopilot_circuit_run_steps(run_id,node_id,attempt,status,agent_node_id) VALUES(1,'send',1,'unverified',9);").unwrap();
    }
    let text = "Read and follow the complete task brief in this file: C:\\brief.md\n";
    let turn = handoff::HandoffTurn {
        node_id: "send".into(),
        attempt: 1,
    };
    handoff::set_agent_turn(1, 9, Some(&turn));
    let result = handoff::paths_in(dir.path(), 1, "send", 1).result;
    handoff::write_prompt(&result, "Already finished\nRESULT: COMPLETED").unwrap();
    let revision = retain(
        1,
        "send",
        1,
        &PreparedPrompt::from_raw(9, text),
        DeliveryKind::Inject,
        Some(&turn),
    )
    .unwrap();
    let request = CircuitPromptRequest {
        run_id: 1,
        node_id: "send".into(),
        attempt: 1,
        expected_revision: revision,
    };
    assert_eq!(
        crate::commands::circuit::circuit_prompt_payload_blocking(request.clone()).unwrap(),
        text
    );
    let stale = CircuitPromptRequest {
        expected_revision: 0,
        ..request.clone()
    };
    assert!(crate::commands::circuit::circuit_prompt_payload_blocking(stale).is_err());
    crate::db::circuit::evidence::record_outcome(&CheckpointRequest {
        run_id: 1,
        node_id: "send".into(),
        attempt: 1,
        expected_revision: revision,
        action: crate::db::circuit::evidence::CheckpointAction::ManualDelivered,
        reason: String::new(),
    })
    .unwrap();
    assert_eq!(handoff::agent_turn(1, 9), Some(turn));
    assert_eq!(
        std::fs::read_to_string(result).unwrap(),
        "Already finished\nRESULT: COMPLETED"
    );
    assert!(crate::commands::circuit::circuit_prompt_payload_blocking(request).is_err());
    crate::preferences::reset_for_tests();
}
