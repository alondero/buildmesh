//! Queue and step wait history, including binding and resolution of evidence windows.

use super::*;
#[derive(Debug, serde::Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub(crate) enum QueueWaitReason {
    CircuitDisabled,
    MeshCapacity {
        capacity: i64,
    },
    AgentCapacity {
        required: i64,
        available: Option<i64>,
    },
    ReservationUnavailable,
}

pub(crate) fn queue_waits(mesh_id: i64) -> SqlResult<std::collections::HashMap<String, String>> {
    let db = crate::db::read_conn()?;
    queue_waits_inner(&db, mesh_id)
}

pub(super) fn queue_waits_inner(
    db: &Connection,
    mesh_id: i64,
) -> SqlResult<std::collections::HashMap<String, String>> {
    let mut statement = db.prepare(
        "SELECT CAST(r.id AS TEXT), h.detail
        FROM autopilot_circuit_runs r JOIN circuit_run_history h ON h.id =
        (SELECT MAX(id) FROM circuit_run_history WHERE run_id=r.id AND kind='queue_wait')
        WHERE r.mesh_id=?1 AND r.state='pending'",
    )?;
    let rows = statement.query_map([mesh_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

pub(crate) fn record_queue_wait(run_id: i64, reason: QueueWaitReason) -> SqlResult<()> {
    record_queue_wait_locked(&mut crate::db::write_conn(), run_id, reason)
}

pub(super) fn record_queue_wait_locked(
    db: &mut Connection,
    run_id: i64,
    reason: QueueWaitReason,
) -> SqlResult<()> {
    let tx = db.transaction()?;
    let pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM autopilot_circuit_runs WHERE id=?1 AND state='pending')",
        [run_id],
        |row| row.get(0),
    )?;
    if !pending {
        return Ok(());
    }
    let detail = serde_json::to_string(&reason).expect("queue wait fields");
    let previous: Option<String> = tx.query_row("SELECT detail FROM circuit_run_history WHERE run_id=?1 AND kind='queue_wait' ORDER BY id DESC LIMIT 1", [run_id], |row|row.get(0)).optional()?;
    if previous.as_deref() != Some(&detail) {
        append_history(
            &tx,
            HistoryEvent {
                run_id,
                node_id: None,
                attempt: None,
                kind: "queue_wait",
                detail: &detail,
                source: Some(SOURCE_ADMISSION),
                disposition: Some(DISPOSITION_WAITING),
            },
        )?;
    }
    tx.commit()
}

/// Record changes to the persisted evidence window alongside its projection.
pub(in crate::db::circuit) fn record_wait_changes(
    db: &Connection,
    run_id: i64,
    next_context: &str,
) -> SqlResult<()> {
    let previous: String = db.query_row(
        "SELECT context_json FROM autopilot_circuit_runs WHERE id=?1",
        [run_id],
        |row| row.get(0),
    )?;
    let previous: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&previous).unwrap_or_default();
    let next: std::collections::BTreeMap<String, String> =
        serde_json::from_str(next_context).unwrap_or_default();
    let continuation_keys: std::collections::BTreeSet<&String> = previous
        .keys()
        .chain(next.keys())
        .filter(|key| key.starts_with("node.") && key.ends_with(".continuation.delivery"))
        .collect();
    for key in continuation_keys {
        let node = key
            .strip_prefix("node.")
            .and_then(|key| key.strip_suffix(".continuation.delivery"))
            .expect("filtered continuation key");
        let attempt = next
            .get(&format!("node.{node}.continuation.attempt"))
            .and_then(|value| value.parse::<i32>().ok());
        let count_key = format!("node.{node}.continuations.{}", attempt.unwrap_or_default());
        if previous.get(key) != next.get(key) || previous.get(&count_key) != next.get(&count_key) {
            let state = match next.get(key).map(String::as_str) {
                Some("pending") => "intent",
                Some("claimed") => "possible_dispatch",
                Some("delivered") => "acknowledged",
                Some("obsolete") => "not_performed",
                Some("uncertain") => "uncertain",
                _ => continue,
            };
            if state == "possible_dispatch"
                && previous.get(key).map(String::as_str) != Some("pending")
            {
                append_history(
                    db,
                    HistoryEvent {
                        run_id,
                        node_id: Some(node),
                        attempt,
                        kind: "continuation_effect",
                        detail: &serde_json::json!({
                            "effect":"continuation_prompt","state":"intent","ordinal":next.get(&count_key)
                        }).to_string(),
                        source: Some(SOURCE_CIRCUIT_WORKER),
                        disposition: Some(DISPOSITION_INTENT),
                    },
                )?;
            }
            append_history(
                db,
                HistoryEvent {
                    run_id,
                    node_id: Some(node),
                    attempt,
                    kind: "continuation_effect",
                    detail: &serde_json::json!({
                        "effect":"continuation_prompt","state":state,"ordinal":next.get(&count_key)
                    })
                    .to_string(),
                    source: Some(SOURCE_CIRCUIT_WORKER),
                    disposition: Some(state),
                },
            )?;
        }
    }
    let blocker_keys: std::collections::BTreeSet<&String> = previous
        .keys()
        .chain(next.keys())
        .filter(|key| key.starts_with("node.") && key.ends_with(".observation_blocker"))
        .collect();
    for key in blocker_keys {
        if previous.get(key) == next.get(key) {
            continue;
        }
        let node = key
            .strip_prefix("node.")
            .and_then(|key| key.strip_suffix(".observation_blocker"));
        let Some(node) = node else {
            continue;
        };
        let attempt = db
            .query_row(
                "SELECT attempt FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id=?2",
                params![run_id, node],
                |row| row.get::<_, i32>(0),
            )
            .optional()?;
        let blocker = next.get(key).and_then(|value| {
            serde_json::from_str::<crate::circuit::observation::CircuitObservationBlocker>(value)
                .ok()
        });
        let detail = serde_json::json!({"blocker": blocker, "message": blocker.as_ref().map(|blocker| blocker.message())});
        append_history(
            db,
            HistoryEvent {
                run_id,
                node_id: Some(node),
                attempt,
                kind: "observation_readiness",
                detail: &detail.to_string(),
                source: Some(SOURCE_RECONCILIATION),
                disposition: Some(wait_disposition(blocker.is_some())),
            },
        )?;
    }
    let capacity_keys: std::collections::BTreeSet<&String> = previous
        .keys()
        .chain(next.keys())
        .filter(|key| key.starts_with("node.") && key.ends_with(".capacity_wait"))
        .collect();
    for key in capacity_keys {
        if previous.get(key) != next.get(key) {
            let node = key
                .strip_prefix("node.")
                .and_then(|key| key.strip_suffix(".capacity_wait"));
            // Identity: the parked step's current attempt, so the operator
            // surface can name which execution is waiting (issue #1909).
            let attempt = match node {
                Some(node) => db.query_row(
                    "SELECT attempt FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id=?2",
                    params![run_id, node], |r| r.get::<_, i32>(0),
                ).optional()?,
                None => None,
            };
            // A freed window (`agent_limit` false, or the key gone) is a
            // resolution, not an active wait (issue #1909). Rows written
            // before ADR 0042 also carried a `circuit_limit` flag; it is
            // never read here because nothing sets it any more.
            let window = next
                .get(key)
                .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
            let binding = window
                .as_ref()
                .is_some_and(|value| value["agent_limit"].as_bool().unwrap_or(false));
            append_history(
                db,
                HistoryEvent {
                    run_id,
                    node_id: node,
                    attempt,
                    kind: "step_capacity_wait",
                    detail: &serde_json::json!({"before":previous.get(key),"after":next.get(key)})
                        .to_string(),
                    source: Some(SOURCE_CAPACITY),
                    disposition: Some(wait_disposition(binding)),
                },
            )?;
        }
    }
    let nodes: std::collections::BTreeSet<&str> = previous
        .keys()
        .chain(next.keys())
        .filter_map(|key| key.strip_prefix("node.")?.strip_suffix(".wait.attempt"))
        .collect();
    for node in nodes {
        let prefix = format!("node.{node}.wait.");
        let fields = [
            "attempt",
            "timeout_ms",
            "since_ms",
            "observed",
            "explicit_budget",
        ];
        let project = |context: &std::collections::BTreeMap<String,String>| -> std::collections::BTreeMap<&str,Option<String>> {
            fields.iter().map(|field|(*field,context.get(&format!("{prefix}{field}")).cloned())).collect()
        };
        let before = project(&previous);
        let after = project(&next);
        if before != after {
            // Identity survives a resolution: a cleared window writes an empty
            // attempt, so fall back to the prior window's attempt rather than
            // dropping the identity the wait was parked on (issue #1909 review).
            let attempt = next
                .get(&format!("{prefix}attempt"))
                .and_then(|value| value.parse::<i32>().ok())
                .or_else(|| {
                    previous
                        .get(&format!("{prefix}attempt"))
                        .and_then(|value| value.parse::<i32>().ok())
                });
            // A window whose (possibly cleared) attempt is empty is a
            // resolution, not an active wait.
            let binding = after
                .get("attempt")
                .and_then(|value| value.as_deref())
                .is_some_and(|value| !value.is_empty());
            append_history(
                db,
                HistoryEvent {
                    run_id,
                    node_id: Some(node),
                    attempt,
                    kind: "evidence_window_changed",
                    detail: &serde_json::json!({"before":before,"after":after}).to_string(),
                    source: Some(SOURCE_RECONCILIATION),
                    disposition: Some(wait_disposition(binding)),
                },
            )?;
        }
    }
    Ok(())
}
