//! Claim external effects and acknowledge their durable dispatch targets and receipts.

use super::*;
/// Must commit before any bytes are sent. A repeated claim never dispatches.
pub(crate) fn acknowledge_spawn_attachment(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent_node_id: i64,
    parent_agent_node_id: Option<i64>,
    expected_agent_node_id: Option<i64>,
) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    acknowledge_spawn_attachment_locked(
        &mut db,
        run_id,
        node_id,
        attempt,
        agent_node_id,
        parent_agent_node_id,
        expected_agent_node_id,
    )
}

pub(super) fn acknowledge_spawn_attachment_locked(
    db: &mut Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent_node_id: i64,
    parent_agent_node_id: Option<i64>,
    expected_agent_node_id: Option<i64>,
) -> SqlResult<Option<i64>> {
    let tx = db.transaction()?;
    let updated = tx.execute("UPDATE autopilot_circuit_run_steps SET agent_node_id=?4,parent_agent_node_id=?5
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND status='running'
        AND EXISTS(SELECT 1 FROM circuit_effects e WHERE e.run_id=?1 AND e.node_id=?2 AND e.attempt=?3 AND e.kind='spawn'
            AND ((e.state='possible_dispatch' AND agent_node_id IS ?6 AND (agent_node_id IS NULL OR agent_node_id=?4 OR ?3>1))
                OR (e.state='acknowledged' AND agent_node_id=?4)))
        AND EXISTS(SELECT 1 FROM autopilot_circuit_runs WHERE id=?1 AND state='running')",
        params![run_id,node_id,attempt,agent_node_id,parent_agent_node_id,expected_agent_node_id])?;
    if updated == 0 {
        return Ok(None);
    }
    let acknowledged = tx.execute("INSERT INTO circuit_effects(run_id,node_id,attempt,kind,state) VALUES (?1,?2,?3,'spawn','acknowledged')
        ON CONFLICT(run_id,node_id,attempt,kind) DO UPDATE SET state='acknowledged' WHERE state='possible_dispatch'",
        params![run_id,node_id,attempt])?;
    if acknowledged > 0 {
        append_history(
            &tx,
            HistoryEvent {
                run_id,
                node_id: Some(node_id),
                attempt: Some(attempt),
                kind: "effect_result",
                detail: &serde_json::json!({"effect":"spawn","state":"acknowledged","agent_node_id":agent_node_id,
                    "meaning":if expected_agent_node_id == Some(agent_node_id) { "Prompt delivered to retained agent; completion still requires evidence" }
                        else { "Agent allocation attached; foreground and owned work completion still require evidence" }}).to_string(),
                source: Some(SOURCE_CIRCUIT_WORKER),
                disposition: Some(DISPOSITION_ACKNOWLEDGED),
            },
        )?;
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(Some(revision))
}

pub fn claim_effect(run_id: i64, intent: &EffectIntent) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    claim_effect_locked(&mut db, run_id, intent)
}

pub(crate) fn acknowledge_prompt_delivery(
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<Option<i64>> {
    let mut db = crate::db::write_conn();
    acknowledge_prompt_delivery_locked(&mut db, run_id, node_id, attempt)
}

pub(super) fn acknowledge_prompt_delivery_locked(
    db: &mut Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<Option<i64>> {
    let tx = db.transaction()?;
    let updated = tx.execute("UPDATE circuit_effects SET state='acknowledged'
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt' AND state='possible_dispatch'
        AND EXISTS(SELECT 1 FROM autopilot_circuit_runs r JOIN autopilot_circuit_run_steps s ON s.run_id=r.id
            WHERE r.id=?1 AND r.state IN ('running','paused') AND s.node_id=?2 AND s.attempt=?3 AND s.status='running')",
        params![run_id, node_id, attempt])?;
    if updated == 0 {
        return Ok(None);
    }
    append_history(
        &tx,
        HistoryEvent {
            run_id,
            node_id: Some(node_id),
            attempt: Some(attempt),
            kind: "effect_result",
            detail: "acknowledged",
            source: Some(SOURCE_CIRCUIT_WORKER),
            disposition: Some(DISPOSITION_ACKNOWLEDGED),
        },
    )?;
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(Some(revision))
}

pub(crate) fn prompt_delivery_acknowledged(
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<bool> {
    let db = crate::db::read_conn()?;
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM circuit_effects
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt' AND state='acknowledged')",
        params![run_id, node_id, attempt],
        |row| row.get(0),
    )
}

pub(crate) fn record_effect_target(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    detail: &str,
) -> Result<i64, DbError> {
    let mut db = crate::db::write_conn();
    record_effect_target_locked(&mut db, run_id, node_id, attempt, detail)
}

pub(super) fn record_effect_target_locked(
    db: &mut Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    detail: &str,
) -> Result<i64, DbError> {
    let tx = db.transaction()?;
    let graph = run_graph(&tx, run_id)?;
    if !matches!(
        graph.node(node_id).map(|node| &node.kind),
        Some(crate::circuit::model::CircuitNodeKind::GithubAction {
            action: crate::circuit::model::GithubActionKind::OpenPr,
            ..
        })
    ) {
        return Err(DbError::invalid(
            "Effect target is only supported for an OpenPr step.",
        ));
    }
    let dispatch_claimed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM circuit_effects e
             JOIN autopilot_circuit_runs r ON r.id=e.run_id
             JOIN autopilot_circuit_run_steps s ON s.run_id=e.run_id AND s.node_id=e.node_id
             WHERE e.run_id=?1 AND e.node_id=?2 AND e.attempt=?3 AND e.kind='github'
               AND e.state='possible_dispatch' AND r.state='running'
               AND s.attempt=?3 AND s.status='running')",
        params![run_id, node_id, attempt],
        |row| row.get(0),
    )?;
    if !dispatch_claimed {
        return Err(DbError::conflict(
            "OpenPr dispatch claim changed before its target was recorded.",
        ));
    }
    append_history(
        &tx,
        HistoryEvent {
            run_id,
            node_id: Some(node_id),
            attempt: Some(attempt),
            kind: "effect_target",
            detail,
            source: Some(SOURCE_GITHUB),
            disposition: Some(DISPOSITION_RECORDED),
        },
    )?;
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(revision)
}

pub(crate) fn latest_effect_target(
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<Option<String>> {
    let db = crate::db::read_conn()?;
    latest_effect_target_inner(&db, run_id, node_id, attempt)
}

pub(super) fn latest_effect_target_inner(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<Option<String>> {
    db.query_row(
        "SELECT detail FROM circuit_run_history
         WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='effect_target'
         ORDER BY id DESC LIMIT 1",
        params![run_id, node_id, attempt],
        |row| row.get(0),
    )
    .optional()
}

pub(super) fn has_effect_target(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
) -> SqlResult<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM circuit_run_history WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='effect_target')",
        params![run_id, node_id, attempt],
        |row| row.get(0),
    )
}

pub(super) fn claim_effect_locked(
    db: &mut Connection,
    run_id: i64,
    intent: &EffectIntent,
) -> SqlResult<Option<i64>> {
    let tx = db.transaction()?;
    let claimed = tx.execute("UPDATE circuit_effects SET state='possible_dispatch'
        WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind=?4 AND state='intent'
        AND EXISTS (SELECT 1 FROM autopilot_circuit_runs r JOIN autopilot_circuit_run_steps s ON s.run_id=r.id
          WHERE r.id=?1 AND r.state='running' AND s.node_id=?2 AND s.attempt=?3 AND s.status='running')",
        params![run_id,intent.node_id,intent.attempt,intent.kind.as_db_str()])? == 1;
    if claimed {
        append_history(
            &tx,
            HistoryEvent {
                run_id,
                node_id: Some(&intent.node_id),
                attempt: Some(intent.attempt),
                kind: "effect_possible_dispatch",
                detail: intent.kind.as_db_str(),
                source: Some(SOURCE_CIRCUIT_WORKER),
                disposition: Some(DISPOSITION_POSSIBLE_DISPATCH),
            },
        )?;
    }
    let revision = revision_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(claimed.then_some(revision))
}
