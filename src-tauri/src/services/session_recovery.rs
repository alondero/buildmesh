//! Recover missed harness identities before startup resume (#1555).
//!
//! This reads historic metadata, not fresh-spawn pollers: those require a live
//! process and use today's clock. Never substitute a new conversation or guess
//! between two IDs. A missing identity is retried on the next startup.

use crate::models::AgentNode;

pub(crate) const CLOCK_SKEW_MS: i64 = 2_000;
pub(crate) const INITIAL_SPAWN_WINDOW_MS: i64 = 300_000;

/// Legacy rows have a creation time but no durable process-start time. Limit
/// recovery to the initial launch window so a later conversation in a reused
/// directory cannot be mistaken for this node. Regenerated/ambiguous rows need
/// explicit recovery if their original identity was never captured.
pub(crate) fn select_recovery_identity(
    candidates: impl IntoIterator<Item = (String, i64)>,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let candidates = candidates.into_iter().collect::<Vec<_>>();
    let cutoff = anchor_ms.saturating_sub(CLOCK_SKEW_MS);
    if !recorded_start {
        // A legacy node has no durable generation anchor. If more than one
        // conversation exists after its creation, refusing to guess is safer
        // than reviving a replacement conversation.
        let ids = candidates
            .iter()
            .filter(|(_, time)| *time >= cutoff)
            .map(|(id, _)| id)
            .collect::<std::collections::HashSet<_>>();
        if ids.len() != 1 {
            return None;
        }
    }
    let mut ids = candidates
        .into_iter()
        .filter(|(_, time)| {
            *time >= cutoff
                && *time <= anchor_ms.saturating_add(INITIAL_SPAWN_WINDOW_MS)
        })
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    (ids.len() == 1).then(|| ids.remove(0))
}

fn find_identity(node: &AgentNode, anchor_ms: i64, recorded_start: bool) -> Option<String> {
    let directory = crate::env::node_working_path(node).spawn_path;
    let adapter = crate::preferences::resolve_harness_provider(&node.provider).adapter();
    adapter.recover_suspended_session_id(&directory, node.env, anchor_ms, recorded_start)
}

/// Called on the worker's throttled report probe and by fresh capture. This
/// only repairs identity; it never starts or resumes an agent process.
pub(crate) fn recover_live_node(node_id: i64) -> Result<Option<String>, String> {
    let generation = crate::db::session_started_at_ms(node_id).map_err(|e| e.to_string())?;
    let node = crate::db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    if let Some(id) = node.cli_session_id.as_ref().filter(|id| !id.is_empty()) {
        return Ok(Some(id.clone()));
    }
    // Old rows without a launch generation cannot safely bind a live process.
    let Some(generation) = generation else { return Ok(None); };
    let Some(id) = find_identity(&node, generation, true) else { return Ok(None); };
    if crate::db::recover_live_cli_session_id(&node, &id, generation).map_err(|e| e.to_string())? {
        tracing::info!("session recovery: captured delayed identity for node {node_id}");
        Ok(Some(id))
    } else {
        Ok(None)
    }
}

pub async fn recover_suspended_node(node: AgentNode) -> Result<bool, String> {
    crate::blocking::run_blocking("recover_suspended_session", move || {
        if node.cli_session_id.as_deref().is_some_and(|id| !id.is_empty())
            || !crate::preferences::resolve_harness_provider(&node.provider).adapter().auto_resume_on_startup()
            // A suspended Autopilot node without an identity may be awaiting
            // sandbox approval and must never be started by transcript matching.
            || crate::db::get_autopilot_run(node.id).map_err(|e| e.to_string())?.is_some()
        {
            return Ok(false);
        }
        let generation = crate::db::session_started_at_ms(node.id).map_err(|e| e.to_string())?;
        let started = generation.unwrap_or_else(|| node.created_at.timestamp_millis());
        let Some(id) = find_identity(&node, started, generation.is_some()) else { return Ok(false); };
        let recovered = crate::db::recover_suspended_cli_session_id(&node, &id, generation)
            .map_err(|e| e.to_string())?;
        if recovered {
            tracing::info!("session recovery: restored identity for node {} ({})", node.id, node.provider);
        }
        Ok(recovered)
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_generation_uses_a_bounded_window_and_deduplicates_rollouts() {
        assert_eq!(
            select_recovery_identity(
                vec![
                    ("original".into(), 10_005),
                    ("original".into(), 10_006),
                    ("later".into(), 400_000),
                ],
                10_000,
                true,
            ),
            Some("original".into())
        );
    }

    #[test]
    fn legacy_generation_refuses_ambiguous_conversations() {
        assert_eq!(
            select_recovery_identity(
                vec![("a".into(), 10_005), ("b".into(), 10_010)],
                10_000,
                false,
            ),
            None
        );
        assert_eq!(select_recovery_identity(Vec::new(), 10_000, false), None);
        assert_eq!(
            select_recovery_identity(vec![("later".into(), 400_000)], 10_000, false),
            None
        );
    }
}
