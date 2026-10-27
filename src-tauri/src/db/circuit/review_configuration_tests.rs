use super::*;

fn seed(db: &rusqlite::Connection) {
    let graph = crate::circuit::model::CircuitGraph::agent_review(None, None, 3);
    db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
        INSERT INTO agent_nodes(id,mesh_id,name,path,branch,env,provider,status) VALUES(1,1,'one','/repo','main','windows','claude','running'),(2,1,'two','/repo','main','windows','claude','running');
        INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
        INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id,trigger_identity) VALUES(1,1,1,'running',1,'one'),(2,1,1,'running',2,'two');").unwrap();
    db.execute(
        "UPDATE autopilot_circuits SET graph_json=?1",
        [graph.to_json().unwrap()],
    )
    .unwrap();
}

#[test]
fn updates_unstarted_reviewers_and_rolls_back_entire_batch_on_started_reviewer() {
    let _isolation = crate::db::test_support::isolated();
    let mut db = crate::db::write_conn();
    seed(&db);
    reconfigure_locked(&mut db, &[1, 2], "claude", "{\"id\":\"first\"}").unwrap();
    let before: String = db
        .query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let context = crate::circuit::context::CircuitContext::from_json(&before).unwrap();
    assert_eq!(
        context.get("review.launch.reviewer"),
        Some("{\"id\":\"first\"}")
    );
    db.execute_batch("INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,attempt) VALUES(2,'reviewer','pending_slot',1);").unwrap();
    assert!(reconfigure_locked(&mut db, &[1, 2], "codex", "second").is_err());
    let after: String = db
        .query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    let history: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE run_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        history, 1,
        "rolled-back edit must not leave a history fence"
    );
}
