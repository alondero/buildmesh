//! Retain exactly what delivery prepared, and expose it only for its current
//! unverified checkpoint. Filesystem work stays outside database connections.
use crate::circuit::{delivery::PreparedPrompt, handoff};
use crate::db::{
    circuit::evidence::{
        prompt_delivery::{self, CircuitPromptRequest, PayloadIdentity},
        CheckpointRequest,
    },
    DbError,
};
pub(crate) use prompt_delivery::DeliveryKind;
pub(crate) use prompt_delivery::{initial_fence, initial_pending, InitialDeliveryFence};
use std::path::{Path, PathBuf};

fn payload_path(root: &Path, run_id: i64, node_id: &str, attempt: i32) -> PathBuf {
    // Hex encoding is injective and cannot introduce path traversal or collide
    // for ids that the human-readable handoff filename sanitises identically.
    handoff::run_dir_in(root, run_id).join(format!(
        "payload-{}-{attempt}.txt",
        hex::encode(node_id.as_bytes())
    ))
}

pub(crate) fn retain(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    prompt: &PreparedPrompt,
    kind: DeliveryKind,
    turn: Option<&handoff::HandoffTurn>,
) -> Result<i64, DbError> {
    let saved = crate::preferences::app_data_dir().is_some_and(|root| {
        handoff::write_prompt(&payload_path(&root, run_id, node_id, attempt), prompt.text())
            .map_err(|error| tracing::warn!(run_id, node_id, %error, "Could not retain manual-delivery payload")).is_ok()
    });
    if !saved {
        return crate::db::circuit::evidence::record_prompt_submission(
            run_id, node_id, attempt, prompt,
        );
    }
    let mut identity = PayloadIdentity {
        agent_node_id: prompt.node_id(),
        digest: crate::services::circuit_worker::native_hooks::submission_digest(prompt.text()),
        kind,
        turn: turn.map(|t| (t.node_id.clone(), t.attempt)),
        submission_seq: 0,
        prepared_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    prompt_delivery::retain(run_id, node_id, attempt, &mut identity, prompt.text())
}

pub(crate) fn payload(request: &CircuitPromptRequest) -> Result<String, DbError> {
    let identity = prompt_delivery::payload_identity(request)?;
    let root = crate::preferences::app_data_dir()
        .ok_or_else(|| DbError::invalid("App data directory is unavailable."))?;
    let text = read_payload(&root, request, &identity)?;
    // A worker/operator can change the run while the file is read.
    if prompt_delivery::payload_identity(request)? != identity {
        return Err(DbError::conflict(
            "The prepared prompt changed. Refresh before acting.",
        ));
    }
    Ok(text)
}

fn read_payload(
    root: &Path,
    request: &CircuitPromptRequest,
    identity: &PayloadIdentity,
) -> Result<String, DbError> {
    let text = std::fs::read_to_string(payload_path(
        root,
        request.run_id,
        &request.node_id,
        request.attempt,
    ))
    .map_err(|e| DbError::invalid(format!("Could not read retained prompt payload: {e}")))?;
    if crate::services::circuit_worker::native_hooks::submission_digest(&text) != identity.digest {
        return Err(DbError::conflict(
            "The retained prompt payload changed. Inspect the agent before continuing.",
        ));
    }
    Ok(text)
}

pub(crate) fn attest(request: &CheckpointRequest) -> Result<(), DbError> {
    let query = CircuitPromptRequest {
        run_id: request.run_id,
        node_id: request.node_id.clone(),
        attempt: request.attempt,
        expected_revision: request.expected_revision,
    };
    let identity = prompt_delivery::payload_identity(&query)?;
    // Dispatch armed the handoff before retaining this submission. Keep it and
    // its sequence intact: the user may already have submitted the prompt and
    // the native start hook may already have earned its input/turn binding.
    let lifecycle = prompt_delivery::attest(request, &identity)?;
    if let Some(app) = crate::http::app_handle() {
        use crate::agent::session_lifecycle::{AppSessionLifecycleSink, SessionLifecycleSink};
        AppSessionLifecycleSink { app }.emit_lifecycle_changed(lifecycle);
    }
    Ok(())
}

pub(crate) fn initial_delivery_uncertain(
    app: &tauri::AppHandle,
    fence: Option<&InitialDeliveryFence>,
    reason: &str,
) -> bool {
    let Some(fence) = fence else { return false };
    let agent = fence.identity.agent_node_id;
    match prompt_delivery::park_initial(fence, reason) {
        Ok(true) => {
            crate::commands::attention::mark_attention(agent, app);
            crate::commands::circuit::emit_run_updated(Some(app), fence.run_id, "running");
            crate::services::circuit_worker::wake_circuit_worker();
            true
        }
        Ok(false) => true, // A superseded delivery must not change the new agent turn.
        Err(error) => {
            tracing::warn!(agent, %error, "Could not record unverified initial prompt");
            false
        }
    }
}

#[cfg(test)]
#[path = "circuit_prompt_tests.rs"]
mod tests;
