//! Guard native failure observations against current lifecycle and terminal input.
use super::*;

/// Amend only our own still-current failure notice, preserving status and input.
pub(crate) fn native_interruption_notice(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    revision: &str,
    message: &str,
) -> Result<(), String> {
    let published = {
        let mut conn = db::write_conn();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let observed = db::agent_node::agent_status_observation_inner(&tx, node_id)
            .map_err(|e| e.to_string())?;
        if observed.source_revision != revision {
            return Ok(());
        }
        let node =
            db::agent_node::get_agent_node_by_id_inner(&tx, node_id).map_err(|e| e.to_string())?;
        let Some(mut payload) = node
            .lifecycle
            .filter(|l| l.provider_event.as_deref() == Some("buildmesh_native_interruption"))
        else {
            return Ok(());
        };
        payload.message = Some(message.into());
        payload.timestamp = chrono::Utc::now().to_rfc3339();
        if !db::agent_node::commit_agent_lifecycle_inner(
            &tx,
            &mut payload,
            FORBIDDEN_HOOK_TRANSITION,
        )
        .map_err(|e| e.to_string())?
        {
            return Ok(());
        }
        tx.commit().map_err(|e| e.to_string())?;
        payload
    };
    sink.emit_lifecycle_changed(published);
    Ok(())
}

pub(crate) fn interruption_eligible(node: &crate::models::AgentNode) -> bool {
    if !matches!(
        node.status,
        SessionStatus::Running | SessionStatus::Ready | SessionStatus::AwaitingInput
    ) {
        return false;
    }
    if let Some(life) = &node.lifecycle {
        if matches!(
            life.kind,
            LifecycleKind::PermissionRequested
                | LifecycleKind::QuestionRequested
                | LifecycleKind::BackgroundRunning
        ) || life.request.is_some()
            || life.message.as_deref() == Some("turn interrupted by the user")
            || (node.status == SessionStatus::AwaitingInput
                && !matches!(
                    life.kind,
                    LifecycleKind::Error | LifecycleKind::SignalUnavailable
                ))
        {
            return false;
        }
    } else if node.status == SessionStatus::AwaitingInput {
        return false;
    }
    true
}

/// Publish a native failure only while database and terminal ownership match.
pub(crate) fn on_native_interruption(
    sink: &dyn SessionLifecycleSink,
    node_id: i64,
    observed: &db::agent_node::AgentStatusObservation,
    input: &str,
    failed_at_ms: i64,
    detail: &HookSignalDetail,
) -> Result<Option<db::agent_node::AgentStatusObservation>, String> {
    let mut payload = LifecycleChangedPayload::new(
        node_id,
        LifecycleKind::Error,
        SessionStatus::AwaitingInput,
        detail,
        "The harness stopped with an error",
    );
    let mut published_observation = None;
    let committed = {
        let mut conn = db::write_conn();
        crate::agent::process::PROCESS_REGISTRY.commit_recovered_turn(
            node_id,
            input,
            failed_at_ms,
            || {
                let tx = conn.transaction().map_err(|e| e.to_string())?;
                let current = db::agent_node::agent_status_observation_inner(&tx, node_id)
                    .map_err(|e| e.to_string())?;
                if current.source_revision != observed.source_revision {
                    return Ok(false);
                }
                let node = db::agent_node::get_agent_node_by_id_inner(&tx, node_id)
                    .map_err(|e| e.to_string())?;
                if !interruption_eligible(&node) {
                    return Ok(false);
                }
                let changed = db::agent_node::commit_agent_lifecycle_inner(
                    &tx,
                    &mut payload,
                    FORBIDDEN_HOOK_TRANSITION,
                )
                .map_err(|e| e.to_string())?;
                let next = db::agent_node::agent_status_observation_inner(&tx, node_id)
                    .map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                if changed {
                    published_observation = Some(next);
                }
                Ok(changed)
            },
        )?
    };
    if committed {
        sink.disarm_attention_autoclear(node_id);
        sink.emit_attention_needed(node_id);
        sink.emit_lifecycle_changed(payload);
    }
    Ok(published_observation)
}
