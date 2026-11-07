//! Starting and continuing a review.
//!
//! A review is a Circuit Run with a borrowed source agent: the person starts
//! it from an agent's title bar, the run's reviewer provider is frozen at that
//! moment, and the evaluator registers the source (and any retained
//! reviewer) so their turns are observed. Continuing a failed review resumes
//! the retained agents under their lifecycle leases and extends the run's
//! allowance in place.
//!
//! Both operations live here so the desktop command and the mobile route
//! obey the same eligibility (issue #2673).

use tauri::AppHandle;

use crate::preferences::PreferencesStore;

/// Start a review or an authored Circuit from an agent node's title bar.
///
/// `reviewer_provider` is the optional reviewer provider (a Spawn Option id)
/// picked at the Start Review control; it becomes the run's reviewer provider
/// for the built-in review preset (the reviewer agent spawns on it). Authored
/// Circuits ignore it — their reviewer provider is part of the circuit graph.
/// A blank value means "inherit"; Terminal is rejected (see
/// [`crate::db::circuit::ledger::create_node_circuit_run`]).
///
/// Retrying for an agent that already owns a live run returns that run's id and
/// does not apply the new `reviewer_provider` or `max_rounds` — the first
/// writer wins. The source agent must have a live process and a turn-completion
/// signal, matching the title-bar control's eligibility check.
pub fn start_review(
    store: Option<&PreferencesStore>,
    app: Option<&AppHandle>,
    node_id: i64,
    circuit_id: Option<i64>,
    max_rounds: i32,
    reviewer_provider: Option<String>,
    reviewer_login_id: Option<String>,
) -> Result<i64, String> {
    if !(1..=10).contains(&max_rounds) {
        return Err("Review rounds must be between 1 and 10.".into());
    }
    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        return Err("Resume the agent before starting a review.".into());
    }
    let node = crate::db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    if node.provider == "terminal" {
        return Err("Review loops require an AI agent.".into());
    }
    let source = crate::agent::provider::SpawnOptionId::from(node.provider.as_str());
    if crate::circuit::compatibility::reviewer_harness_reason(source.harness_id()).is_some() {
        return Err("This agent's harness has no turn-completion signal. Start the review from an agent with a supported turn hook, transcript watcher or result-file handoff.".into());
    }
    let run_id = crate::db::create_node_circuit_run_with_login(
        store,
        node_id,
        circuit_id,
        max_rounds,
        reviewer_provider,
        reviewer_login_id,
    )?;
    crate::circuit::evaluator::register_circuit(node_id);
    let state = crate::db::get_circuit_run(run_id)
        .map_err(|e| e.to_string())?
        .map(|run| run.state)
        .unwrap_or_else(|| "pending".into());
    crate::services::circuit_worker::emit_run_updated(app, run_id, &state);
    crate::services::circuit_worker::wake_circuit_worker();
    Ok(run_id)
}

/// Add bounded review rounds to the failed run on its retained work. Resuming the source uses
/// the same lifecycle lease as Archive, so cleanup and process launch cannot
/// own the agent concurrently.
pub async fn continue_review(
    app: &AppHandle,
    run_id: i64,
    additional_rounds: i32,
) -> Result<i64, String> {
    if !(1..=10).contains(&additional_rounds) {
        return Err("Additional review rounds must be between 1 and 10.".into());
    }
    if let Some(existing) = crate::blocking::run_blocking("existing_review_target", move || {
        crate::db::circuit::recovery::existing_review_target(run_id).map_err(String::from)
    })
    .await?
    {
        return Ok(existing);
    }
    let source_id = crate::blocking::run_blocking("review_recovery_source", move || {
        crate::db::circuit::recovery::review_recovery_source(run_id).map_err(String::from)
    })
    .await?;
    let reviewer_id = crate::blocking::run_blocking("review_recovery_reviewer", move || {
        crate::db::circuit::recovery::review_recovery_reviewer(run_id).map_err(String::from)
    })
    .await?;
    if let Some(reviewer_id) = reviewer_id {
        if !crate::agent::process::PROCESS_REGISTRY.is_alive(&reviewer_id) {
            use crate::agent::spawn::{ResumeCause, SpawnIntent, SpawnRequest};
            crate::agent::spawn::spawn_with_intent(
                app,
                SpawnRequest::new(reviewer_id, SpawnIntent::Resume { cause: ResumeCause::Explicit }, Default::default())
                    .with_lifecycle_lease(),
            ).await.map_err(|error| format!("Could not resume the reviewer: {error}. Open the reviewer to resolve this before continuing."))?;
            if !crate::agent::process::PROCESS_REGISTRY.is_alive(&reviewer_id) {
                return Err(
                    "The reviewer is still being stopped or resumed. Try Review again in a moment."
                        .into(),
                );
            }
        }
    }
    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&source_id) {
        use crate::agent::spawn::{ResumeCause, SpawnIntent, SpawnRequest};
        crate::agent::spawn::spawn_with_intent(app,
            SpawnRequest::new(source_id, SpawnIntent::Resume { cause: ResumeCause::Explicit }, Default::default())
                .with_lifecycle_lease()).await.map_err(|error|
                    format!("Could not resume the implementation agent: {error}. Open the agent to resolve this, or recover from the PR branch and start a new review."))?;
        if !crate::agent::process::PROCESS_REGISTRY.is_alive(&source_id) {
            return Err("The implementation agent is still being stopped or resumed. Try Review again in a moment.".into());
        }
    }
    let (next_id, state) = crate::blocking::run_blocking("extend_failed_review", move || {
        let next_id =
            crate::db::circuit::recovery::extend_failed_review(run_id, additional_rounds)?;
        let state = crate::db::get_circuit_run(next_id)
            .map_err(|e| e.to_string())?
            .map(|run| run.state)
            .unwrap_or_else(|| "pending".into());
        Ok((next_id, state))
    })
    .await?;
    crate::circuit::evaluator::register_circuit(source_id);
    if let Some(reviewer_id) = reviewer_id {
        crate::circuit::evaluator::register_circuit(reviewer_id);
    }
    crate::services::circuit_worker::wake_circuit_worker();
    crate::services::circuit_worker::emit_run_updated(Some(app), next_id, &state);
    Ok(next_id)
}
