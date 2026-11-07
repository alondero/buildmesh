//! The desktop command adapter's own suite: the flat wire payload, the
//! canonical graph save, and the guard that keeps the built-in review
//! blueprint out of the trigger path.
//!
//! The workflow behaviour these commands used to own now lives in
//! `services::circuit_control`, and its suite moved with it (issue #2673).

/// Issue #2449: `create_circuit` reads its eight fields through
/// `WirePayload` so the Rust signature stops being a positional bag,
/// and the wire is asserted here so that refactor can never quietly
/// become an IPC change. This payload is exactly what
/// `createCircuit` in `src/lib/tauri.ts` sends, `?? null` optionals
/// included.
#[test]
fn create_circuit_still_reads_the_flat_frontend_payload() {
    use super::CreateCircuitRequest;
    use crate::circuit::model::{CircuitBlueprintKind, CircuitTriggerKind};
    let wire = serde_json::json!({
        "meshId": 4,
        "name": "  nightly review  ",
        "description": "review open PRs",
        "initialPrompt": "start",
        "triggerKind": "github_issue_label",
        "triggerLabel": "ready",
        "intervalSeconds": 3600,
        "blueprint": "walking_skeleton",
    });
    let request: CreateCircuitRequest =
        serde_json::from_value(wire).expect("flat invoke payload still parses");
    assert_eq!(request.mesh_id, 4);
    assert_eq!(request.name.trim(), "nightly review");
    assert_eq!(request.description, "review open PRs");
    assert_eq!(request.initial_prompt, "start");
    assert_eq!(
        request.trigger_kind,
        Some(CircuitTriggerKind::GithubIssueLabel)
    );
    assert_eq!(request.trigger_label.as_deref(), Some("ready"));
    assert_eq!(request.interval_seconds, Some(3600));
    assert_eq!(
        request.blueprint,
        Some(CircuitBlueprintKind::WalkingSkeleton)
    );
}

/// The frontend wrapper substitutes `null` for absent optionals and
/// `undefined` keys never reach the wire, so both shapes must land on
/// `None` rather than failing the whole command.
#[test]
fn create_circuit_accepts_null_and_omitted_optional_fields() {
    use super::CreateCircuitRequest;
    let nulled: CreateCircuitRequest = serde_json::from_value(serde_json::json!({
        "meshId": 1,
        "name": "n",
        "description": "",
        "initialPrompt": "",
        "triggerKind": "manual",
        "triggerLabel": null,
        "intervalSeconds": null,
        "blueprint": "walking_skeleton",
    }))
    .expect("null optionals parse");
    assert_eq!(nulled.trigger_label, None);
    assert_eq!(nulled.interval_seconds, None);

    let omitted: CreateCircuitRequest = serde_json::from_value(serde_json::json!({
        "meshId": 1,
        "name": "n",
        "description": "",
        "initialPrompt": "",
    }))
    .expect("omitted optionals default to None");
    assert_eq!(omitted.trigger_kind, None);
    assert_eq!(omitted.blueprint, None);
}

#[test]
fn saving_a_legacy_graph_applies_the_full_upgrade_sequence_and_preserves_authored_text() {
    use crate::circuit::model::{CircuitGraph, CircuitNodeKind};
    let _db = crate::db::test_support::isolated();
    let mesh = crate::db::create_mesh("legacy save", "C:/legacy-save").unwrap();
    let mut graph = crate::circuit::test_support::stuck_issue_review();
    if let CircuitNodeKind::InjectPty { prompt, .. } = &mut graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "follow_feedback")
        .unwrap()
        .kind
    {
        *prompt = "Apply our team's review checklist.".into();
    }
    let circuit_id = {
        let conn = crate::db::write_conn();
        conn.execute(
            "INSERT INTO autopilot_circuits (mesh_id, name, graph_json) VALUES (?1, 'legacy', ?2)",
            rusqlite::params![mesh.id, graph.to_json().unwrap()],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    super::update_circuit_graph(circuit_id, graph.to_json().unwrap()).unwrap();
    let saved = crate::db::get_autopilot_circuit(circuit_id)
        .unwrap()
        .unwrap()
        .graph_json;
    let upgraded = CircuitGraph::from_json(&saved).unwrap();
    upgraded.validate().unwrap();
    assert!(
        upgraded.has_review_topology_of(&CircuitGraph::issue_driven_autopilot_review(
            "buildmesh:run"
        ))
    );
    assert!(upgraded.node("merge_verify").is_some());
    assert!(matches!(
        &upgraded.node("follow_feedback").unwrap().kind,
        CircuitNodeKind::InjectPty { prompt, .. }
            if prompt == "Apply our team's review checklist."
    ));
    super::update_circuit_graph(circuit_id, saved.clone()).unwrap();
    assert_eq!(
        crate::db::get_autopilot_circuit(circuit_id)
            .unwrap()
            .unwrap()
            .graph_json,
        saved
    );
}

/// #1910: the built-in Review Blueprint is present and inspectable but is
/// never a trigger source of its own — a run only exists once a user
/// starts it from an implementation agent's title bar. Trigger Now is the
/// one IPC path that would otherwise mint a run on an unattended row.
#[test]
fn the_built_in_review_blueprint_cannot_be_triggered_on_its_own() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let mesh = crate::db::create_mesh_inner(&conn, "trigger guard", "/tmp/trigger-guard").unwrap();
    crate::db::circuit::ledger::ensure_review_blueprint_inner(&conn, mesh.id).unwrap();
    let mut conn = conn;
    let blueprint = crate::db::circuit::list_circuits_with_recent_runs_inner(&conn, mesh.id, 10)
        .unwrap()[0]
        .0
        .id;

    assert_eq!(
        super::trigger_circuit_now_locked(&mut conn, blueprint).unwrap_err(),
        "Start this Circuit from the source agent's title bar."
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM autopilot_circuit_runs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0,
        "the refused trigger left no run behind"
    );
}
