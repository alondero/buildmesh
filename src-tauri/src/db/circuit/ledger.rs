//! Autopilot Circuits ledger: blueprint/run/step CRUD and the engine's
//! atomic [`commit_circuit_advance`] seam (spec #1205).

use rusqlite::{Connection, OptionalExtension, params};

use crate::autopilot::circuit::vocabulary::{RunState, StepStatus};
use crate::db::SqlResult;
use crate::models::{AutopilotCircuit, AutopilotCircuitRun, AutopilotCircuitRunStep};

// Circuits — CRUD for the blueprint rows.
// ---------------------------------------------------------------------------

/// Normalise the title-bar reviewer override into the stored run-context value.
///
/// The id is a Spawn Option id — `<harness>` or the composite
/// `harness:provider_id`. A blank value collapses to `None` (inherit the
/// app-wide Reviewer provider, then the source agent).
///
/// The gate itself lives in `autopilot::compatibility`
/// ([`validate_reviewer_provider_id`](crate::autopilot::compatibility::validate_reviewer_provider_id)):
/// a reviewer must be able to **yield a turn**, or the `verdict` gate parks
/// forever — the reviewer's status has to reach `awaiting_input` / `ready` /
/// `completed` before `classify_step_turn` will do anything at all
/// (`if !yielded { return None; }`). This is the harness half of
/// `autopilot::compatibility::evaluate`, minus its fail-closed unknown arm
/// (see `reviewer_harness_reason`) — not a Terminal-only denylist.
#[cfg(test)]
fn normalize_reviewer_provider(value: Option<String>) -> Result<Option<String>, String> {
    normalize_prepared_reviewer(value, &crate::preferences::AppPreferences::default())
}

fn validate_prepared_reviewer(value: &str, preferences: &crate::preferences::AppPreferences) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() { return Ok(()); }
    let option = match preferences.spawn_configurations.iter().find(|c| c.id == value) {
        Some(configuration) => configuration.spawn_option_id.as_str(),
        None if value.starts_with("launch/") => return Err("Launch Configuration no longer exists; select another configuration".into()),
        None => value,
    };
    let id = crate::agent::provider::SpawnOptionId::from(option);
    match crate::autopilot::compatibility::reviewer_harness_reason(id.harness_id()) {
        Some(reason) => Err(crate::autopilot::compatibility::reviewer_refusal_message(&reason)),
        None => Ok(()),
    }
}

fn normalize_prepared_reviewer(value: Option<String>, preferences: &crate::preferences::AppPreferences) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    validate_prepared_reviewer(trimmed, preferences)?;
    Ok(Some(trimmed.to_string()))
}

/// Effective reviewer decision for the built-in review preset path (issue
/// #1816). The per-run override wins when non-blank; otherwise the run
/// inherits the stored app-wide value (snapshotted below by
/// `with_app_reviewer_provider`). Whichever wins must pass the
/// attention-compatibility gate: an ineligible override is refused with
/// the harness-named reason, while an ineligible *stored* value is refused
/// with the same reason plus Settings guidance — the settings command
/// validates new writes, but a value stored before the gate existed (or
/// hand-edited into `preferences.json`) must refuse here rather than mint
/// a run that can never reach a verdict.
///
/// Pure (no DB, no prefs reads): the caller supplies the stored app-wide
/// value so this stays unit-testable without global state.
fn resolve_preset_reviewer(
    reviewer_provider: Option<String>,
    stored_app_wide: Option<String>,
    preferences: &crate::preferences::AppPreferences,
) -> Result<Option<String>, String> {
    let picked = normalize_prepared_reviewer(reviewer_provider, preferences)?;
    if picked.is_some() {
        return Ok(picked);
    }
    if let Some(stored) = stored_app_wide.as_deref() {
        validate_prepared_reviewer(stored, preferences).map_err(|message| {
            format!("{message} It is stored as the app-wide Reviewer provider — pick another in Settings.")
        })?;
    }
    Ok(None)
}

/// Atomically claim a source agent and create its review run. The source id is
/// stored relationally on the run; the context copy remains for graph
/// template expansion and backwards-compatible diagnostics.
///
/// `reviewer_provider` is the per-run reviewer provider (a Spawn Option id,
/// possibly a composite `harness:provider`) chosen at the title-bar Start
/// Review control. When set it overrides the app-wide Reviewer provider
/// snapshot in this run's context, so the reviewer agent spawns on the
/// provider the user picked. It applies to the built-in review preset only:
/// an authored Circuit carries its own reviewer provider in its graph and
/// ignores this value entirely — including for validation (validating an
/// ignored value would reject authored calls over a stale string, issue
/// #1816 review). On the preset path the *effective* reviewer (override
/// when set, else the stored app-wide snapshot) must pass the
/// attention-compatibility gate via [`resolve_preset_reviewer`].
///
/// **First writer wins.** If the source agent already owns a live run, the
/// early-return below hands back that run's id and `max_rounds` /
/// `reviewer_provider` are not applied — the dialog hides the form in this
/// state, so the only way here is a retry or IPC race. The dedupe check
/// runs BEFORE the readiness gate (#1792) so a retry on an unobserved
/// source still returns the existing run id (issue #1660 dedupe wins
/// over the gate, otherwise the gate would silently strip the live
/// borrower's run id on every retry).
///
/// `allow_unobserved` (issue #1792) is the explicit override for the
/// source-agent readiness gate enforced by [`assert_source_observed`].
/// The built-in review preset refuses to mint a run on a never-observed
/// source (no `cli_session_id`, no readable `assistant_report`); recovery
/// and explicit user-selected circuits stay permissive because both paths
/// have already proven the source is observed in another context. The
/// override is recorded on the run's `context_json`
/// (`source.review_allow_unobserved = "1"`) for audit.
///
/// The readiness gate runs **before** the writer mutex is acquired —
/// `assistant_report` does filesystem I/O and the writer mutex cannot be
/// released mid-call (issue #1228). The locked helper trusts the caller
/// and does not re-check.
pub fn create_node_circuit_run(
    node_id: i64,
    selected_circuit_id: Option<i64>,
    max_rounds: i32,
    reviewer_provider: Option<String>,
    allow_unobserved: bool,
) -> Result<i64, String> {
    // Pre-gate decisions: dedupe-before-gate then gate. Done under the
    // process-global reader so the writer mutex is not held during the
    // `assistant_report` filesystem I/O (issue #1228). The composition
    // is testable on a per-test private in-memory DB via
    // `create_node_circuit_run_pre_gate` (see below).
    {
        let db = crate::db::read_conn();
        if let Some(existing) =
            create_node_circuit_run_pre_gate(&db, node_id, selected_circuit_id, allow_unobserved)?
        {
            return Ok(existing);
        }
    }
    let preferences = crate::preferences::load()?;
    // Mint (acquires the writer mutex).
    let mut db = crate::db::write_conn();
    create_node_circuit_run_with_recovery_locked(
        &mut db, node_id, selected_circuit_id, max_rounds, (reviewer_provider, Some(&preferences)), None, allow_unobserved,
    )
}

/// Per-test isolated variant of [`create_node_circuit_run`] (issue #1691).
/// The public function locks the process-global writer; this helper
/// takes an explicit `&mut Connection` so parallel tests can each
/// operate against their own in-memory DB. `reviewer_provider` is
/// resolved *inside* the helper so the validation is not duplicated
/// between the public wrapper and the test path (issue #1691 review
/// cleanup) — an ineligible effective reviewer is rejected before any DB
/// work happens, on the built-in preset path only.
///
/// The helper trusts the caller: it assumes the readiness gate has
/// already been enforced by [`assert_source_observed`] (the public
/// wrapper runs that before `write_conn()`) or that the override /
/// recovery / explicit-circuit carve-out applies. Tests that call the
/// locked helper directly model the "caller has already verified"
/// state — they do not need to stamp `cli_session_id` to satisfy a
/// gate that is no longer in this function.
#[cfg(test)]
pub(crate) fn create_node_circuit_run_locked(
    db: &mut Connection,
    node_id: i64,
    selected_circuit_id: Option<i64>,
    max_rounds: i32,
    reviewer_provider: Option<String>,
    allow_unobserved: bool,
) -> Result<i64, String> {
    let preferences = crate::preferences::load().unwrap_or_default();
    create_node_circuit_run_with_recovery_locked(
        db, node_id, selected_circuit_id, max_rounds, (reviewer_provider, Some(&preferences)), None, allow_unobserved,
    )
}

/// Recovery path: the source is already known to be observed (it has a
/// previous run whose evidence the recovery plan reads), so the readiness
/// check is irrelevant. The locked helper records `allow_unobserved = true`
/// purely so the audit field is *not* emitted on this path — the
/// `if selected_circuit_id.is_none() && recovery.is_none()` guard below
/// already drops the audit on recovery, but the helper still uses
/// `allow_unobserved = true` so any future "always audit" tweak lands on
/// the right side of the carve-out.
pub(crate) fn create_node_circuit_run_recovery_locked(
    db: &mut Connection,
    recovery: super::recovery::ReviewRecovery,
    max_rounds: i32,
) -> Result<i64, String> {
    let recovery = match super::recovery::continuation_target_inner(db, recovery.run_id)? {
        super::recovery::ContinuationTarget::Existing(id) => return Ok(id),
        super::recovery::ContinuationTarget::Failed(id) if id != recovery.run_id =>
            super::recovery::review_recovery_inner(db, id, max_rounds)?,
        _ => recovery,
    };
    create_node_circuit_run_with_recovery_locked(
        db, recovery.source_id, None, max_rounds, (None, None), Some(recovery), true,
    )
}

/// Exact reason returned when the built-in review preset is asked to mint
/// a run on a source agent that has produced no observable evidence yet
/// (no captured `cli_session_id`, no readable `assistant_report` revision).
/// Pinned by `node_review_refuses_unstarted_source_with_exact_message`
/// (issue #1792).
pub(crate) const SOURCE_NOT_YET_OBSERVED_MESSAGE: &str =
    "Source agent has not started yet — wait for its first turn before starting a review.";

/// First-writer-wins dedupe (issue #1660). Returns the id of the source's
/// currently-live run if any. Driven from
/// [`create_node_circuit_run_pre_gate`] before the readiness gate so a
/// retry on an unobserved source still hands back the live borrower's
/// run id. The locked helper re-checks this under the writer transaction
/// as defense in depth for concurrent inserts.
pub(crate) fn find_live_run_for_source_inner(
    conn: &Connection,
    node_id: i64,
) -> SqlResult<Option<i64>> {
    conn.query_row(
        &format!(
            "SELECT id FROM autopilot_circuit_runs
             WHERE source_agent_node_id = ?1 AND state IN ({})
             LIMIT 1",
            RunState::SQL_IN_LIVE
        ),
        params![node_id], |row| row.get(0),
    )
    .optional()
}

/// Source-agent readiness gate (issue #1792). Lock-free: callers MUST run
/// this before acquiring the writer mutex because `assistant_report` does
/// filesystem I/O. Refuses to mint a run on a source agent that has
/// produced no observable evidence yet — neither a non-empty
/// `cli_session_id` nor a readable `assistant_report` revision.
///
/// Carve-outs (the gate is permissive):
/// - `allow_unobserved` — the user explicitly opted in to the override
///   on the Start Review dialog.
/// - `has_selected_circuit` — the user is asking for a specific
///   authored blueprint, not the built-in review preset.
///
/// Note: today's `assistant_report` reader returns `None` whenever
/// `cli_session_id` is `None` (every harness stores the session id
/// alongside the report, so the report branch never produces
/// independent evidence). The check is kept for the spec's
/// `cli_session_id OR assistant_report` contract and to defend against
/// future harness adapters that may diverge.
pub(crate) fn assert_source_observed(
    node: &crate::models::AgentNode,
    allow_unobserved: bool,
    has_selected_circuit: bool,
) -> Result<(), String> {
    if allow_unobserved || has_selected_circuit {
        return Ok(());
    }
    if node.cli_session_id.as_deref().is_some_and(|s| !s.is_empty()) {
        return Ok(());
    }
    // `assistant_report` is filesystem I/O. The caller must run this
    // helper before acquiring any DB writer mutex (issue #1228).
    if crate::coordinator::enrichment::assistant_report(node).is_some() {
        return Ok(());
    }
    Err(SOURCE_NOT_YET_OBSERVED_MESSAGE.into())
}

/// Pre-gate composition for the public wrapper (#1792). Returns:
/// - `Ok(Some(existing_run_id))` when first-writer-wins dedupe (#1660)
///   wins — a retry on an unobserved source returns the live borrower's
///   run id instead of re-firing the refusal.
/// - `Ok(None)` when the gate passes (or is permissive), and the caller
///   should proceed to the locked helper.
/// - `Err(msg)` when the gate refuses the source agent.
///
/// This is the single decision point the wrapper relies on. The
/// recovery path bypasses it entirely via `create_node_circuit_run_recovery_locked`
/// (the recovery source is already observed via its previous run),
/// so the helper has no `is_recovery` carve-out — keeping the helper's
/// parameters honest about the only flags the public wrapper passes.
///
/// Lock-free: takes `&Connection` so the caller controls transaction /
/// mutex scope. Tests drive this on a per-test private in-memory DB
/// (issue #1691) so the dedupe-before-gate ordering is covered
/// end-to-end without touching the process-global writer.
pub(crate) fn create_node_circuit_run_pre_gate(
    conn: &Connection,
    node_id: i64,
    selected_circuit_id: Option<i64>,
    allow_unobserved: bool,
) -> Result<Option<i64>, String> {
    // First-writer-wins dedupe (#1660). Runs BEFORE the readiness
    // gate (#1792) so a retry on an unobserved source hands back the
    // live borrower's run id without re-firing the refusal.
    if let Some(existing) = find_live_run_for_source_inner(conn, node_id)
        .map_err(|e| e.to_string())?
    {
        return Ok(Some(existing));
    }
    // Load the source node (cheap DB read on the same connection).
    let node = crate::db::agent_node::get_agent_node_by_id_inner(conn, node_id)
        .map_err(|e| e.to_string())?;
    // Source-agent readiness gate (#1792). The wrapper always calls
    // this with the real flags — no conditional call at the call site
    // (which would be the same lie the round-3 review flagged).
    assert_source_observed(
        &node,
        allow_unobserved,
        selected_circuit_id.is_some(),
    )?;
    Ok(None)
}

fn create_node_circuit_run_with_recovery_locked(
    db: &mut Connection,
    node_id: i64,
    selected_circuit_id: Option<i64>,
    max_rounds: i32,
    reviewer: (Option<String>, Option<&crate::preferences::AppPreferences>),
    recovery: Option<super::recovery::ReviewRecovery>,
    allow_unobserved: bool,
) -> Result<i64, String> {
    let (reviewer_provider, preferences) = reviewer;
    let app_reviewer = preferences.and_then(|p| p.reviewer_provider.clone());
    let reviewer_override = if selected_circuit_id.is_none() && recovery.is_none() {
        resolve_preset_reviewer(reviewer_provider, app_reviewer.clone(), preferences.expect("fresh review preferences prepared before locking"))?
    } else {
        None
    };
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let node = crate::db::agent_node::get_agent_node_by_id_inner(&tx, node_id).map_err(|e| e.to_string())?;
    if crate::db::legacy_retirement::pending_inner(&tx, node_id).map_err(|error| error.to_string())? {
        return Err("Legacy retirement is still stopping the source node. Retry after cleanup finishes.".into());
    }
    if recovery.is_some() {
        // An already-live source can still be owned by terminal cleanup.
        // Check under the same writer transaction that installs the borrower:
        // existing cleanup wins; subsequent cleanup sees the live borrower.
        let cleaning: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_node_lifecycle_leases WHERE node_id=?1 AND cleanup_generation IS NOT NULL)",
            params![node_id], |row| row.get(0),
        ).map_err(|e| e.to_string())?;
        if cleaning {
            return Err("The implementation agent is still being stopped. Try Continue review again in a moment.".into());
        }
    }
    let existing: Option<i64> = find_live_run_for_source_inner(&tx, node_id)
        .map_err(|e| e.to_string())?;
    if let Some(id) = existing { return Ok(id); }
    let owned: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM autopilot_circuit_run_steps s \
             JOIN autopilot_circuit_runs r ON r.id = s.run_id \
             WHERE s.agent_node_id = ?1 AND r.state IN ({})) \
             OR EXISTS(SELECT 1 FROM autopilot_runs WHERE node_id = ?1 \
             AND state IN ('implementing','finishing','suffix_pending'))",
            RunState::SQL_IN_LIVE
        ),
        params![node_id], |row| row.get(0),
    ).map_err(|e| e.to_string())?;
    if owned { return Err("This agent is already controlled by an active Autopilot run.".into()); }
    if !matches!(node.status, crate::models::SessionStatus::Running | crate::models::SessionStatus::AwaitingInput | crate::models::SessionStatus::Completed | crate::models::SessionStatus::Ready) {
        return Err("Resume the agent before starting a review.".into());
    }
    let review_config: Option<(Option<String>, Option<String>)> = if selected_circuit_id.is_none() {
        Some(tx.query_row(
            "SELECT NULLIF(TRIM(model), ''), NULLIF(TRIM(effort), '') FROM meshes WHERE id = ?1",
            params![node.mesh_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let (circuit_id, name) = if let Some(recovery) = &recovery {
        super::recovery::recovery_circuit_inner(&tx, node.mesh_id, recovery)?
    } else if let Some(id) = selected_circuit_id {
        let circuit = get_autopilot_circuit_inner(&tx, id).map_err(|e| e.to_string())?
            .ok_or("Circuit no longer exists")?;
        let graph = crate::autopilot::circuit::model::CircuitGraph::from_json(&circuit.graph_json)?;
        graph.validate()?;
        if circuit.mesh_id != node.mesh_id || graph.roots().is_empty()
            || graph.roots().iter().any(|n| !matches!(n.kind, crate::autopilot::circuit::model::CircuitNodeKind::Manual)) {
            return Err("Select a manual Circuit from this agent's Mesh.".into());
        }
        (id, circuit.name)
    } else {
        let (review_model, review_effort) = review_config.clone().unwrap_or_default();
        let graph = crate::autopilot::circuit::model::CircuitGraph::agent_review(
            review_model.clone(),
            review_effort.clone(),
            max_rounds,
        );
        graph.validate()?;
        let name = format!("Review agent {}", node_id);
        let description = "Review an existing agent and return findings until approved";
        let existing: Option<(i64, String)> = tx.query_row(
            "SELECT id, name FROM autopilot_circuits
             WHERE mesh_id = ?1 AND is_preset = 1
             ORDER BY id LIMIT 1",
            params![node.mesh_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|e| e.to_string())?;
        if let Some((id, existing_name)) = existing {
            (id, existing_name)
        } else {
            tx.execute(
                "INSERT INTO autopilot_circuits
                 (mesh_id, name, description, enabled, concurrency_limit, graph_json, is_preset)
                 VALUES (?1, ?2, ?3, 0, 2, ?4, 1)",
                params![node.mesh_id, name, description, graph.to_json()?],
            ).map_err(|e| e.to_string())?;
            (tx.last_insert_rowid(), name)
        }
    };
    let mut context = crate::autopilot::circuit::context::CircuitContext::new();
    context.with_circuit(circuit_id, &name, node.mesh_id);
    context.set("review.provider", app_reviewer.as_deref().unwrap_or(""));
    context.set("source.agent_id", node_id.to_string());
    context.set("source.name", &node.name);
    context.set("source.path", crate::env::node_working_path(&node).spawn_path);
    let base_ref: String = tx.query_row("SELECT base_ref FROM meshes WHERE id = ?1", params![node.mesh_id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    context.set("source.base_ref", base_ref);
    if selected_circuit_id.is_none() && recovery.is_none() {
        if allow_unobserved {
            // Audit field for the readiness-gate override (issue #1792):
            // a user who knowingly reviewed an unobserved source leaves a
            // permanent breadcrumb on the run. Absent means the source
            // either had a captured `cli_session_id` or a readable
            // `assistant_report` at create time.
            context.set("source.review_allow_unobserved", "1");
        }
        if let Some(provider) = reviewer_override.as_deref() {
            context.set("review.provider", provider);
        }
        context.set("source.review_preset", "1");
        context.set("source.provider", node.launch_configuration.as_ref().map_or(node.provider.as_str(), |c| c.id.as_str()));
        context.set(
            "source.model",
            node.launch_configuration.as_ref().map(|c| c.model.as_deref()).unwrap_or_else(|| review_config
                .as_ref()
                .and_then(|(model, _)| model.as_deref()))
                .unwrap_or(""),
        );
        context.set(
            "source.effort",
            node.launch_configuration.as_ref().map(|c| c.effort.as_deref()).unwrap_or_else(|| review_config
                .as_ref()
                .and_then(|(_, effort)| effort.as_deref()))
                .unwrap_or(""),
        );
    }
    if let Some(recovery) = &recovery {
        for (key, value) in &recovery.frozen_launches { context.set(key, value); }
    } else if let Some(preferences) = preferences {
        pin_review_launches(&tx, circuit_id, node.launch_configuration.as_ref().map_or(node.provider.as_str(), |c| c.id.as_str()), node.launch_configuration.as_ref(), preferences, &mut context)?;
    }
    context.set("retry.attempt", "1");
    // Capture the failed run before `recovery` is consumed: the successor's
    // `recovery.from_run_id` context and the predecessor's recovery history
    // entry must agree (issue #1909).
    let predecessor_run_id = recovery.as_ref().map(|recovery| recovery.run_id);
    if let Some(recovery) = &recovery {
        context.set("recovery.from_run_id", recovery.run_id.to_string());
    }
    context.set("retry.max_retries", max_rounds.to_string());
    tx.execute(
        "INSERT INTO autopilot_circuit_runs
         (circuit_id, mesh_id, trigger_identity, context_json, queue_position, source_agent_node_id)
         VALUES (?1, ?2, ?3, ?4,
                 (SELECT COALESCE(MAX(queue_position),0)+1 FROM autopilot_circuit_runs WHERE mesh_id=?2),
                 ?5)",
        params![circuit_id, node.mesh_id, format!("manual:agent:{node_id}:{}", uuid::Uuid::new_v4()), context.to_json()?, node_id],
    ).map_err(|e| e.to_string())?;
    let run_id = tx.last_insert_rowid();
    // A continuation is an operator recovery action; record it on the failed
    // predecessor so its history names the successor run and disposition.
    if let Some(predecessor) = predecessor_run_id {
        super::evidence::append_history(
            &tx,
            predecessor,
            None,
            None,
            "recovery",
            &serde_json::json!({"successor_run_id": run_id, "rounds": max_rounds}).to_string(),
            Some(super::evidence::SOURCE_OPERATOR),
            Some(super::evidence::DISPOSITION_APPLIED),
        )
        .map_err(|e| e.to_string())?;
    }
    super::evidence::pin_graph(&tx, run_id).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(run_id)
}

fn pin_review_launches(
    db: &Connection,
    circuit_id: i64,
    source_provider: &str,
    source_configuration: Option<&crate::preferences::spawn_configurations::SpawnConfiguration>,
    preferences: &crate::preferences::AppPreferences,
    context: &mut crate::autopilot::circuit::context::CircuitContext,
) -> Result<(), String> {
    use crate::autopilot::circuit::model::{CircuitGraph, CircuitNodeKind};
    use crate::preferences::launch_configurations::{capture, snapshot, LaunchOverrides};
    let inherited_configuration = source_configuration.filter(|source| !preferences.spawn_configurations.iter().any(|c| c.id == source.id));
    let mut preferences = preferences.clone();
    if let Some(configuration) = inherited_configuration { preferences.spawn_configurations.push(configuration.clone()); }
    let circuit = get_autopilot_circuit_inner(db, circuit_id).map_err(|e| e.to_string())?.ok_or("Circuit no longer exists")?;
    let graph = CircuitGraph::from_json(&circuit.graph_json)?;
    for node in &graph.nodes {
        let CircuitNodeKind::SpawnAgentNode { provider, model, effort, extra_args, .. } = &node.kind else { continue; };
        if !graph.nodes.iter().any(|gate| matches!(&gate.kind, CircuitNodeKind::ReviewVerdict { target_node_id } if target_node_id.as_deref() == Some(&node.id))) { continue; }
        let preset = context.get("source.review_preset") == Some("1");
        let parent_selection = graph.nearest_upstream_agent_step(&node.id).and_then(|id| graph.node(&id)).and_then(|parent| match &parent.kind {
            CircuitNodeKind::SpawnAgentNode { provider, .. } => provider.as_deref().filter(|p| !p.trim().is_empty()),
            _ => None,
        });
        let selection = provider.as_deref().filter(|_| !preset).filter(|p| !p.trim().is_empty())
            .or_else(|| context.get("review.provider").filter(|p| !p.trim().is_empty()))
            .or_else(|| context.get("source.provider")).or(parent_selection).unwrap_or(source_provider);
        let overrides = LaunchOverrides {
            model: model.clone().filter(|_| !preset), effort: effort.clone().filter(|_| !preset), extra_args: extra_args.clone(),
        };
        let mut launch_preferences = preferences.clone();
        if let Some(plan) = inherited_configuration.filter(|c| c.id == selection).and_then(|c| c.resolved.as_ref()) {
            launch_preferences.harness_profiles.retain(|h| h.id != plan.harness.id);
            launch_preferences.harness_profiles.push(plan.harness.clone());
            if let Some(route) = &plan.route {
                launch_preferences.provider_pairings.retain(|r| r.harness_id != route.harness_id || r.provider_id != route.provider_id);
                launch_preferences.provider_pairings.push(route.clone());
            }
        }
        let plan = capture(&launch_preferences, selection, &overrides)?;
        let key = format!("review.launch.{}", node.id);
        context.set(&key, serde_json::to_string(&snapshot(plan)).map_err(|e| e.to_string())?);
    }
    Ok(())
}

pub fn create_autopilot_circuit(
    mesh_id: i64,
    name: &str,
    description: &str,
    concurrency_limit: i64,
    graph_json: &str,
) -> SqlResult<AutopilotCircuit> {
    let db = crate::db::write_conn();
    create_autopilot_circuit_inner(&db, mesh_id, name, description, concurrency_limit, graph_json)
}

pub fn copy_review_blueprint(circuit_id: i64, name: &str) -> Result<AutopilotCircuit, String> {
    let mut db = crate::db::write_conn();
    copy_review_blueprint_locked(&mut db, circuit_id, name)
}

pub(crate) fn copy_review_blueprint_locked(db: &mut Connection, circuit_id: i64, name: &str) -> Result<AutopilotCircuit, String> {
    if name.trim().is_empty() { return Err("Give the Circuit a name.".into()); }
    let tx = db.transaction().map_err(|error| error.to_string())?;
    let original = get_autopilot_circuit_inner(&tx,circuit_id).map_err(|error| error.to_string())?
        .filter(|circuit| circuit.is_preset).ok_or("Select a built-in Review Blueprint to copy.")?;
    let mut graph = crate::autopilot::circuit::model::CircuitGraph::from_json(&original.graph_json)?;
    let roots: Vec<String> = graph.roots().iter().map(|node| node.id.clone()).collect();
    for node in &mut graph.nodes {
        if roots.contains(&node.id) { node.kind = crate::autopilot::circuit::model::CircuitNodeKind::Manual; }
    }
    graph.validate()?;
    let copied = create_autopilot_circuit_inner(&tx,original.mesh_id,name.trim(),"Independent copy of the Review Blueprint",original.concurrency_limit,&graph.to_json()?)
        .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(copied)
}

/// Per-test isolated variant of [`create_autopilot_circuit`] (issue #1691).
/// The public function locks the process-global writer; this helper
/// takes an explicit `&Connection` so parallel tests can each operate
/// against their own in-memory DB.
pub(crate) fn create_autopilot_circuit_inner(
    db: &Connection,
    mesh_id: i64,
    name: &str,
    description: &str,
    concurrency_limit: i64,
    graph_json: &str,
) -> SqlResult<AutopilotCircuit> {
    // Draft-first (issue #1356): new blueprints start disabled so the
    // GitHub/interval pollers cannot fire while the user is still
    // authoring. Trigger Now still mints a run against a disabled row.
    // `enabled` is written explicitly so existing v34 DBs whose column
    // default is still 1 cannot silently enable a fresh circuit.
    db.execute(
        "INSERT INTO autopilot_circuits \
             (mesh_id, name, description, enabled, concurrency_limit, graph_json) \
         VALUES (?1, ?2, ?3, 0, ?4, ?5)",
        params![mesh_id, name, description, concurrency_limit, graph_json],
    )?;
    get_autopilot_circuit_inner(db, db.last_insert_rowid())?
        .ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)
}

pub(crate) fn get_autopilot_circuit_inner(
    conn: &Connection,
    id: i64,
) -> SqlResult<Option<AutopilotCircuit>> {
    let mut stmt = conn.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at, is_preset \
         FROM autopilot_circuits WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], map_circuit_row)?;
    rows.next().transpose()
}

pub fn get_autopilot_circuit(id: i64) -> SqlResult<Option<AutopilotCircuit>> {
    let db = crate::db::read_conn();
    get_autopilot_circuit_inner(&db, id)
}

fn map_circuit_row(row: &rusqlite::Row<'_>) -> SqlResult<AutopilotCircuit> {
    Ok(AutopilotCircuit {
        id: row.get(0)?,
        mesh_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        concurrency_limit: row.get(5)?,
        graph_json: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        is_preset: row.get::<_, i64>(9)? != 0,
    })
}

pub fn list_autopilot_circuits(mesh_id: i64) -> SqlResult<Vec<AutopilotCircuit>> {
    let db = crate::db::read_conn();
    list_autopilot_circuits_inner(&db, mesh_id)
}

/// Per-test isolated variant of [`list_autopilot_circuits`] (issue #1691).
pub(crate) fn list_autopilot_circuits_inner(
    db: &Connection,
    mesh_id: i64,
) -> SqlResult<Vec<AutopilotCircuit>> {
    let mut stmt = db.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at, is_preset \
         FROM autopilot_circuits WHERE mesh_id = ?1 AND is_preset = 0 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![mesh_id], map_circuit_row)?;
    rows.collect()
}

/// Every enabled circuit across ALL meshes — the GitHub poll and
/// interval trigger passes' input (issue #1208). Circuits are not
/// mesh-scoped at the trigger layer: a circuit carries its own mesh_id,
/// so one query serves the whole worker pass.
pub fn list_enabled_circuits() -> SqlResult<Vec<AutopilotCircuit>> {
    let db = crate::db::read_conn();
    list_enabled_circuits_inner(&db)
}

/// Per-test isolated variant of [`list_enabled_circuits`] (issue #1691).
pub(crate) fn list_enabled_circuits_inner(db: &Connection) -> SqlResult<Vec<AutopilotCircuit>> {
    let mut stmt = db.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at, is_preset \
         FROM autopilot_circuits WHERE enabled = 1 AND is_preset = 0 ORDER BY id",
    )?;
    let rows = stmt.query_map([], map_circuit_row)?;
    rows.collect()
}

/// `created_at` of the circuit's newest run — the interval trigger's
/// cooldown anchor (issue #1208). `None` when the circuit never fired;
/// SQLite's datetime strings sort lexicographically, so MAX is correct.
/// Deliberately trigger-kind agnostic: ANY run (manual Trigger Now
/// included) restarts the cadence, because the user just intervened.
pub fn latest_circuit_run_created_at(circuit_id: i64) -> SqlResult<Option<String>> {
    let db = crate::db::read_conn();
    latest_circuit_run_created_at_inner(&db, circuit_id)
}

/// Per-test isolated variant of [`latest_circuit_run_created_at`] (issue #1691).
pub(crate) fn latest_circuit_run_created_at_inner(
    db: &Connection,
    circuit_id: i64,
) -> SqlResult<Option<String>> {
    db.query_row(
        "SELECT MAX(created_at) FROM autopilot_circuit_runs WHERE circuit_id = ?1",
        params![circuit_id],
        |row| row.get(0),
    )
}

/// Every `trigger_identity` ever recorded for this circuit — the GitHub
/// poll pass's pre-filter set (issue #1208). The schema's UNIQUE
/// constraint stays the authoritative backstop; this just keeps the pass
/// from rewriting identical rows every cycle.
pub fn list_circuit_trigger_identities(circuit_id: i64) -> SqlResult<Vec<String>> {
    let db = crate::db::read_conn();
    list_circuit_trigger_identities_inner(&db, circuit_id)
}

/// Per-test isolated variant of [`list_circuit_trigger_identities`] (issue #1691).
pub(crate) fn list_circuit_trigger_identities_inner(
    db: &Connection,
    circuit_id: i64,
) -> SqlResult<Vec<String>> {
    let mut stmt = db.prepare(
        "SELECT trigger_identity FROM autopilot_circuit_runs WHERE circuit_id = ?1",
    )?;
    let rows = stmt.query_map(params![circuit_id], |row| row.get(0))?;
    rows.collect()
}

/// One run plus its step ledger, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct CircuitRunLedger {
    pub run: AutopilotCircuitRun,
    pub steps: Vec<AutopilotCircuitRunStep>,
}

/// Ensure the inspectable built-in exists before any review run is created.
pub(super) fn ensure_review_blueprint(mesh_id: i64) -> SqlResult<()> {
    let db = crate::db::write_conn();
    ensure_review_blueprint_inner(&db, mesh_id)
}

pub(crate) fn ensure_review_blueprint_inner(db: &Connection, mesh_id: i64) -> SqlResult<()> {
    let graph = crate::autopilot::circuit::model::CircuitGraph::agent_review(None, None, 3)
        .to_json().map_err(rusqlite::Error::InvalidParameterName)?;
    db.execute("INSERT INTO autopilot_circuits(mesh_id,name,description,enabled,concurrency_limit,graph_json,is_preset)
        SELECT ?1,'Built-in Review Blueprint','Review an existing agent and return findings until approved',0,2,?2,1
        WHERE EXISTS(SELECT 1 FROM meshes WHERE id=?1)
        AND NOT EXISTS(SELECT 1 FROM autopilot_circuits WHERE mesh_id=?1 AND is_preset=1)", params![mesh_id,graph])?;
    Ok(())
}

/// User circuits and the inspectable built-in, with active and bounded terminal history.
pub fn list_circuits_with_recent_runs(
    mesh_id: i64,
    runs_per_circuit: i64,
) -> SqlResult<Vec<(AutopilotCircuit, Vec<CircuitRunLedger>)>> {
    ensure_review_blueprint(mesh_id)?;
    let db = crate::db::read_conn();
    list_circuits_with_recent_runs_inner(&db, mesh_id, runs_per_circuit)
}

pub(crate) fn list_circuits_with_recent_runs_inner(
    db: &Connection,
    mesh_id: i64,
    runs_per_circuit: i64,
) -> SqlResult<Vec<(AutopilotCircuit, Vec<CircuitRunLedger>)>> {
    let mut stmt = db.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at, is_preset \
         FROM autopilot_circuits
         WHERE mesh_id = ?1
         ORDER BY id",
    )?;
    let circuits: Vec<AutopilotCircuit> =
        stmt.query_map(params![mesh_id], map_circuit_row)?.collect::<SqlResult<_>>()?;
    if circuits.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<String> = circuits.iter().map(|c| c.id.to_string()).collect();
    // Keep both ordinary history and recovery history bounded in SQLite. A
    // failed run or a node with an outstanding cleanup request gets a small
    // recovery window even when it is older than the ordinary history cap;
    // presets are not an excuse to stream their entire lifetime ledger.
    const RECOVERY_RUNS_PER_CIRCUIT: i64 = 50;
    let mut stmt = db.prepare(&format!(
        "WITH terminal AS ( \
             SELECT r.id, r.circuit_id, r.mesh_id, r.trigger_identity, r.state, \
                    r.context_json, r.source_agent_node_id, r.created_at, r.updated_at, \
                    c.is_preset, c.graph_json, ROW_NUMBER() OVER (PARTITION BY r.circuit_id ORDER BY r.id DESC) AS history_rank \
             FROM autopilot_circuit_runs r JOIN autopilot_circuits c ON c.id=r.circuit_id \
             WHERE r.circuit_id IN ({}) \
               AND r.state IN ({}) \
         ), attention_candidates AS ( \
             SELECT DISTINCT terminal.id, terminal.circuit_id, terminal.mesh_id, terminal.trigger_identity, terminal.state, \
                    terminal.context_json, terminal.source_agent_node_id, terminal.created_at, terminal.updated_at \
             FROM terminal \
             LEFT JOIN autopilot_circuit_run_steps s ON s.run_id = terminal.id \
             LEFT JOIN agent_node_lifecycle_leases l ON l.node_id = s.agent_node_id \
             LEFT JOIN json_each(CASE WHEN json_valid(terminal.graph_json) THEN terminal.graph_json ELSE '{{}}' END, '$.nodes') review_node ON json_extract(review_node.value, '$.type.type') = 'review_verdict' \
             LEFT JOIN autopilot_circuit_run_steps review_step ON review_step.run_id = terminal.id \
                 AND review_step.node_id = json_extract(review_node.value, '$.id') \
             WHERE terminal.state = 'failed' OR l.cleanup_requested = 1 \
                OR (review_step.id IS NOT NULL AND COALESCE(review_step.outcome, '') <> 'completed') \
         ), attention AS ( \
             SELECT attention_candidates.*, ROW_NUMBER() OVER (PARTITION BY circuit_id ORDER BY id DESC) AS attention_rank \
             FROM attention_candidates \
         ), visible AS ( \
             SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                    context_json, source_agent_node_id, created_at, updated_at \
             FROM autopilot_circuit_runs \
             WHERE circuit_id IN ({}) AND state IN ({}) \
             UNION ALL \
             SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                    context_json, source_agent_node_id, created_at, updated_at \
             FROM terminal WHERE history_rank <= ?1 \
             UNION \
             SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                    context_json, source_agent_node_id, created_at, updated_at \
             FROM attention WHERE attention_rank <= ?2 \
         ) \
         SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                context_json, source_agent_node_id, created_at, updated_at \
         FROM visible ORDER BY circuit_id, id DESC",
        ids.join(","),
        RunState::SQL_IN_TERMINAL,
        ids.join(","),
        RunState::SQL_IN_ADMITTED
    ))?;
    let visible_runs: Vec<AutopilotCircuitRun> = stmt
        .query_map(params![runs_per_circuit.max(0), RECOVERY_RUNS_PER_CIRCUIT], |row| {
            Ok(AutopilotCircuitRun {
                id: row.get(0)?,
                circuit_id: row.get(1)?,
                mesh_id: row.get(2)?,
                trigger_identity: row.get(3)?,
                state: row.get(4)?,
                context_json: row.get(5)?,
                source_agent_node_id: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?
        .collect::<SqlResult<_>>()?;

    let mut runs_by_circuit: std::collections::HashMap<i64, Vec<AutopilotCircuitRun>> =
        std::collections::HashMap::new();
    for run in visible_runs {
        runs_by_circuit.entry(run.circuit_id).or_default().push(run);
    }

    let run_ids: Vec<i64> = runs_by_circuit.values().flatten().map(|run| run.id).collect();
    let mut steps_by_run: std::collections::HashMap<i64, Vec<AutopilotCircuitRunStep>> = std::collections::HashMap::new();
    if !run_ids.is_empty() {
        let placeholders = std::iter::repeat_n("?", run_ids.len()).collect::<Vec<_>>().join(",");
        let mut step_stmt = db.prepare(&format!(
            "SELECT id, run_id, node_id, agent_node_id, status, attempt, \
                    outcome, error_message, started_at, completed_at \
             FROM autopilot_circuit_run_steps WHERE run_id IN ({placeholders}) ORDER BY run_id, id"
        ))?;
        let step_rows = step_stmt.query_map(rusqlite::params_from_iter(run_ids.iter()), map_step_row)?;
        for step in step_rows {
            let step = step?;
            steps_by_run.entry(step.run_id).or_default().push(step);
        }
    }

    let mut out = Vec::with_capacity(circuits.len());
    for circuit in circuits {
        let runs = runs_by_circuit.remove(&circuit.id).unwrap_or_default();
        let ledgers = runs.into_iter().map(|run| CircuitRunLedger {
            steps: steps_by_run.remove(&run.id).unwrap_or_default(),
            run,
        }).collect();
        out.push((circuit, ledgers));
    }
    Ok(out)
}

pub fn set_autopilot_circuit_enabled(id: i64, enabled: bool) -> SqlResult<()> {
    let db = crate::db::write_conn();
    set_autopilot_circuit_enabled_inner(&db, id, enabled)
}

/// Per-test isolated variant of [`set_autopilot_circuit_enabled`] (issue #1691).
pub(crate) fn set_autopilot_circuit_enabled_inner(
    db: &Connection,
    id: i64,
    enabled: bool,
) -> SqlResult<()> {
    let changed = db.execute(
        "UPDATE autopilot_circuits SET enabled = ?2, updated_at = datetime('now') WHERE id = ?1 AND is_preset = 0",
        params![id, i64::from(enabled)],
    )?;
    if changed == 0 { return Err(rusqlite::Error::QueryReturnedNoRows); }
    Ok(())
}

/// Persist a circuit's step-slot budget — the canvas editor's per-circuit
/// concurrency control. The IPC boundary clamps to the blueprint's
/// `[floor, ceiling]` range; this accessor only writes. Errors when the
/// row doesn't exist so a stale editor can't silently no-op. `updated_at`
/// stamps so the Probe list shows fresh edit times.
///
/// Takes an explicit connection (issue #1691): the command already holds
/// the writer guard for the read that derives the blueprint, so this shares
/// that connection rather than re-locking the process-global writer.
pub(crate) fn set_autopilot_circuit_concurrency_limit_inner(
    db: &Connection,
    id: i64,
    concurrency_limit: i64,
) -> SqlResult<()> {
    let changed = db.execute(
        "UPDATE autopilot_circuits SET concurrency_limit = ?2, updated_at = datetime('now') WHERE id = ?1 AND is_preset = 0",
        params![id, concurrency_limit],
    )?;
    if changed == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// Persist a new blueprint AST for one circuit — the canvas editor's
/// save seam (issue #1209). The IPC boundary validates the JSON parses
/// AND passes semantic checks; this accessor only writes. Errors when
/// the row doesn't exist (a stale editor must not silently no-op).
/// `updated_at` stamps so the Probe list shows fresh edit times.
pub fn update_autopilot_circuit_graph(id: i64, graph_json: &str) -> SqlResult<()> {
    let db = crate::db::write_conn();
    update_autopilot_circuit_graph_inner(&db, id, graph_json)
}

/// Per-test isolated variant of [`update_autopilot_circuit_graph`] (issue #1691).
pub(crate) fn update_autopilot_circuit_graph_inner(
    db: &Connection,
    id: i64,
    graph_json: &str,
) -> SqlResult<()> {
    let changed = db.execute(
        "UPDATE autopilot_circuits SET graph_json = ?2, updated_at = datetime('now') WHERE id = ?1 AND is_preset = 0",
        params![id, graph_json],
    )?;
    if changed == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// Delete one circuit and ALL its descendants (runs, steps) in one
/// transaction. Explicit child deletes even though the schema declares
/// `ON DELETE CASCADE`: enforcement depends on the connection's
/// `foreign_keys` pragma, which is on for the bundled SQLite build but
/// off-by-default for a system-libsqlite link — the same defensive rule
/// `delete_mesh` follows for `warm_worktrees`.
pub fn delete_autopilot_circuit(id: i64) -> SqlResult<()> {
    let mut db = crate::db::write_conn();
    delete_autopilot_circuit_locked(&mut db, id)
}

/// Per-test isolated variant of [`delete_autopilot_circuit`] (issue #1691).
pub(crate) fn delete_autopilot_circuit_locked(db: &mut Connection, id: i64) -> SqlResult<()> {
    let tx = db.transaction()?;
    if get_autopilot_circuit_inner(&tx, id)?.is_some_and(|circuit| circuit.is_preset) {
        return Err(rusqlite::Error::InvalidParameterName("Built-in Review Blueprints are read-only".into()));
    }
    for table in ["circuit_run_history", "circuit_effects", "circuit_run_snapshots"] {
        tx.execute(&format!("DELETE FROM {table} WHERE run_id IN (SELECT id FROM autopilot_circuit_runs WHERE circuit_id=?1)"), [id])?;
    }
    tx.execute(
        "DELETE FROM autopilot_circuit_run_steps WHERE run_id IN \
             (SELECT id FROM autopilot_circuit_runs WHERE circuit_id = ?1)",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id IN \
             (SELECT id FROM autopilot_circuit_runs WHERE circuit_id = ?1)",
        params![id],
    )?;
    tx.execute("DELETE FROM autopilot_circuit_runs WHERE circuit_id = ?1", params![id])?;
    tx.execute("DELETE FROM autopilot_circuits WHERE id = ?1", params![id])?;
    tx.commit()
}

/// Delete every circuit (and its runs/steps) belonging to a mesh.
/// Called from [`super::delete_mesh`] inside ITS mutex acquisition —
/// `_inner(&Connection)` discipline, no second lock.
pub(crate) fn delete_circuits_for_mesh_inner(conn: &Connection, mesh_id: i64) -> SqlResult<()> {
    for table in ["circuit_run_history", "circuit_effects", "circuit_run_snapshots"] {
        conn.execute(&format!("DELETE FROM {table} WHERE run_id IN (SELECT id FROM autopilot_circuit_runs WHERE mesh_id=?1)"), [mesh_id])?;
    }
    conn.execute(
        "DELETE FROM autopilot_circuit_run_steps WHERE run_id IN \
             (SELECT id FROM autopilot_circuit_runs WHERE mesh_id = ?1)",
        params![mesh_id],
    )?;
    conn.execute(
        "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id IN \
             (SELECT id FROM autopilot_circuit_runs WHERE mesh_id = ?1)",
        params![mesh_id],
    )?;
    conn.execute("DELETE FROM autopilot_circuit_runs WHERE mesh_id = ?1", params![mesh_id])?;
    conn.execute("DELETE FROM autopilot_circuits WHERE mesh_id = ?1", params![mesh_id])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Runs + steps — the execution ledger the circuit worker drives.
// ---------------------------------------------------------------------------

/// Create a fresh `pending` run seeded with its template context
/// (`circuit.*`, pre-populated by the caller; see
/// [`CircuitContext::with_circuit`]).
///
/// Deduplication is enforced by the schema: `UNIQUE (circuit_id,
/// trigger_identity)` means re-reporting the same trigger identity
/// returns the EXISTING run id instead of minting a duplicate (spec:
/// dedupe scoped per-circuit, so two circuits may process the same
/// source independently). Manual identities embed a millisecond
/// timestamp, so Trigger Now effectively always mints a fresh run.
pub fn create_circuit_run(
    circuit_id: i64,
    mesh_id: i64,
    trigger_identity: &str,
    context_json: &str,
) -> SqlResult<i64> {
    let preferences = crate::preferences::load().map_err(rusqlite::Error::InvalidParameterName)?;
    let mut db = crate::db::write_conn();
    create_circuit_run_prepared_locked(&mut db, circuit_id, mesh_id, trigger_identity, context_json, &preferences)
}

/// Per-test isolated variant of [`create_circuit_run`] (issue #1691).
/// Opens its own transaction on `db`; the `_locked` suffix distinguishes
/// this from the `_inner` helpers that operate inside an externally-managed
/// transaction.
#[cfg(test)]
pub(crate) fn create_circuit_run_locked(
    db: &mut Connection,
    circuit_id: i64,
    mesh_id: i64,
    trigger_identity: &str,
    context_json: &str,
) -> SqlResult<i64> {
    create_circuit_run_prepared_locked(db, circuit_id, mesh_id, trigger_identity, context_json, &crate::preferences::AppPreferences::default())
}

pub(crate) fn create_circuit_run_prepared_locked(
    db: &mut Connection,
    circuit_id: i64,
    mesh_id: i64,
    trigger_identity: &str,
    context_json: &str,
    preferences: &crate::preferences::AppPreferences,
) -> SqlResult<i64> {
    let tx = db.transaction()?;
    if let Some(id) = tx.query_row("SELECT id FROM autopilot_circuit_runs WHERE circuit_id=?1 AND trigger_identity=?2",
        params![circuit_id, trigger_identity], |row| row.get::<_, i64>(0)).optional()? { return Ok(id); }
    let mut context = crate::autopilot::circuit::context::CircuitContext::from_json(context_json).map_err(rusqlite::Error::InvalidParameterName)?;
    if let Some(source) = context.source_agent_id() {
        if crate::db::legacy_retirement::pending_inner(&tx, source)? { return Err(rusqlite::Error::InvalidQuery); }
    }
    let default_provider: Option<String> = tx.query_row("SELECT COALESCE(NULLIF(TRIM(autopilot_provider), ''), default_provider) FROM meshes WHERE id=?1", [mesh_id], |row| row.get(0))?;
    let source_provider = crate::preferences::resolve_default_provider(None, default_provider, preferences.default_provider.clone());
    if context.get("review.provider").is_none() {
        if let Some(provider) = preferences.reviewer_provider.as_deref() { context.set("review.provider", provider); }
    }
    pin_review_launches(&tx, circuit_id, &source_provider, None, preferences, &mut context).map_err(rusqlite::Error::InvalidParameterName)?;
    let context_json = context.to_json().map_err(rusqlite::Error::InvalidParameterName)?;
    let next_position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(queue_position), 0) + 1 FROM autopilot_circuit_runs WHERE mesh_id = ?1",
        params![mesh_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO autopilot_circuit_runs \
             (circuit_id, mesh_id, trigger_identity, context_json, queue_position) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![circuit_id, mesh_id, trigger_identity, context_json, next_position],
    )?;
    let id = tx.query_row(
        "SELECT id FROM autopilot_circuit_runs \
         WHERE circuit_id = ?1 AND trigger_identity = ?2",
        params![circuit_id, trigger_identity],
        |row| row.get(0),
    )?;
    super::evidence::pin_graph(&tx, id)?;
    tx.commit()?;
    Ok(id)
}
/// Atomically terminalise one active run and return its attached Agent Nodes
/// so the command layer can retire their processes/worktrees after the DB
/// stops the worker from driving the run. A missing row is already gone
/// (deleted between render and click) and returns an empty agent list —
/// callers must not string-match the driver error for this case.
pub fn cancel_circuit_run(run_id: i64) -> SqlResult<Vec<i64>> {
    let mut db = crate::db::write_conn();
    cancel_circuit_run_locked(&mut db, run_id)
}

/// Per-test isolated variant of [`cancel_circuit_run`] (issue #1691).
/// The public function locks the process-global writer; this helper
/// takes an explicit `&Connection` so parallel tests can each operate
/// against their own in-memory DB.
pub(crate) fn cancel_circuit_run_locked(conn: &mut Connection, run_id: i64) -> SqlResult<Vec<i64>> {
    let tx = conn.transaction()?;
    let result = cancel_circuit_run_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(result.agents)
}

/// One run's cancel writes against an already-open transaction. Shared by
/// the single and batch paths so both observe identical state transitions.
struct CancelRunWrite {
    agents: Vec<i64>,
    source: Option<i64>,
    cancelled: bool,
}

fn cancel_circuit_run_inner(tx: &Connection, run_id: i64) -> SqlResult<CancelRunWrite> {
    let row: Option<(String, Option<i64>)> = tx.query_row(
        "SELECT state, source_agent_node_id FROM autopilot_circuit_runs WHERE id = ?1",
        params![run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((state, source)) = row else {
        return Ok(CancelRunWrite { agents: vec![], source: None, cancelled: false });
    };
    let agents = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT agent_node_id FROM autopilot_circuit_run_steps \
             WHERE run_id = ?1 AND agent_node_id IS NOT NULL ORDER BY agent_node_id",
        )?;
        let rows = stmt
            .query_map(params![run_id], |row| row.get(0))?
            .collect::<SqlResult<Vec<i64>>>()?;
        rows
    };
    if RunState::is_live_db_str(&state) {
        tx.execute(
            &format!(
                "UPDATE autopilot_circuit_runs SET state = ?2, context_json = json_remove(context_json, '$.\"cleanup.pending\"'), updated_at = datetime('now') \
                 WHERE id = ?1 AND state IN ({})",
                RunState::SQL_IN_LIVE
            ),
            params![run_id, RunState::Cancelled.as_db_str()],
        )?;
        tx.execute(
            "INSERT INTO agent_node_lifecycle_leases (node_id, cleanup_requested)
             SELECT DISTINCT s.agent_node_id, 1
             FROM autopilot_circuit_run_steps s
             JOIN agent_nodes a ON a.id = s.agent_node_id
             WHERE s.run_id = ?1 AND s.agent_node_id IS NOT NULL
               AND s.agent_node_id IS NOT (SELECT source_agent_node_id FROM autopilot_circuit_runs WHERE id = ?1)
             ON CONFLICT(node_id) DO UPDATE SET
               cleanup_requested = 1, retired = 0, updated_at = unixepoch()",
            params![run_id],
        )?;
    }
    // Terminalise the ledger in the same transaction as the run state. A
    // stale worker commit is rejected after this point, so incomplete steps
    // must not remain frozen as `running`/`queued` in the audit UI.
    if !RunState::is_terminal_db_str(&state) {
        tx.execute(
            &format!(
                "UPDATE autopilot_circuit_run_steps \
                 SET status = ?2, outcome = ?2, completed_at = datetime('now') \
                 WHERE run_id = ?1 AND status IN ({})",
                StepStatus::SQL_IN_IN_FLIGHT
            ),
            params![run_id, StepStatus::Cancelled.as_db_str()],
        )?;
    }
    tx.execute(
        "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
        params![run_id],
    )?;
    let cancelled = RunState::is_live_db_str(&state);
    Ok(CancelRunWrite { agents, source, cancelled })
}

/// Batch cancel for queue/activity hygiene: every listed run is
/// terminalised in ONE transaction, so a 50-run "Cancel all" costs one
/// write txn, one worker wake, and one UI event — not an N+1 storm.
/// Missing rows are skipped (already gone between render and click).
/// Returns attached agent ids, source node ids to unregister, and the ids
/// actually transitioned to cancelled.
pub struct BatchCancelResult {
    pub agents: Vec<i64>,
    pub sources: Vec<i64>,
    pub cancelled: Vec<i64>,
}

pub fn cancel_circuit_runs(run_ids: &[i64]) -> SqlResult<BatchCancelResult> {
    let mut db = crate::db::write_conn();
    cancel_circuit_runs_locked(&mut db, run_ids)
}

/// Per-test isolated variant of [`cancel_circuit_runs`] (issue #1691).
/// The public function locks the process-global writer; this helper
/// takes an explicit `&Connection` so parallel tests can each operate
/// against their own in-memory DB.
pub(crate) fn cancel_circuit_runs_locked(
    conn: &mut Connection,
    run_ids: &[i64],
) -> SqlResult<BatchCancelResult> {
    let tx = conn.transaction()?;
    let mut agents: Vec<i64> = Vec::new();
    let mut sources: Vec<i64> = Vec::new();
    let mut cancelled: Vec<i64> = Vec::new();
    // De-duplicate the payload so one id cannot double-count agents.
    let mut seen = std::collections::HashSet::with_capacity(run_ids.len());
    for run_id in run_ids {
        if !seen.insert(run_id) {
            continue;
        }
        let write = cancel_circuit_run_inner(&tx, *run_id)?;
        agents.extend(write.agents);
        if let Some(source) = write.source {
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
        if write.cancelled {
            cancelled.push(*run_id);
        }
    }
    agents.sort_unstable();
    agents.dedup();
    sources.sort_unstable();
    sources.dedup();
    cancelled.sort_unstable();
    tx.commit()?;
    Ok(BatchCancelResult { agents, sources, cancelled })
}

/// Runs whose attached agents may still need retiring while a circuit is
/// deleted. Terminal rows are included so a deletion can be retried after a
/// transient process/worktree cleanup failure without orphaning retained
/// agents from a completed or failed run.
pub fn list_circuit_run_ids_for_cleanup(circuit_id: i64) -> SqlResult<Vec<i64>> {
    let db = crate::db::read_conn();
    list_circuit_run_ids_for_cleanup_inner(&db, circuit_id)
}

/// Per-test isolated variant of [`list_circuit_run_ids_for_cleanup`] (issue #1691).
pub(crate) fn list_circuit_run_ids_for_cleanup_inner(
    db: &Connection,
    circuit_id: i64,
) -> SqlResult<Vec<i64>> {
    let mut stmt = db.prepare(
        "SELECT id FROM autopilot_circuit_runs \
         WHERE circuit_id = ?1 AND state IN ('pending', 'running', 'paused', 'completed', 'failed', 'cancelled') \
         ORDER BY id",
    )?;
    let ids = stmt
        .query_map(params![circuit_id], |row| row.get(0))?
        .collect();
    ids
}

/// One active (pending/running) run joined with the fields its worker
/// pass needs from the owning circuit.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveCircuitRun {
    pub run: AutopilotCircuitRun,
    pub circuit_enabled: bool,
    pub circuit_concurrency_limit: i64,
    pub circuit_graph_json: String,
    pub circuit_name: String,
}

pub fn list_active_circuit_runs() -> SqlResult<Vec<ActiveCircuitRun>> {
    let db = crate::db::read_conn();
    list_active_circuit_runs_inner(&db)
}

/// Per-test isolated variant of [`list_active_circuit_runs`] (issue #1691).
pub(crate) fn list_active_circuit_runs_inner(db: &Connection) -> SqlResult<Vec<ActiveCircuitRun>> {
    let mut stmt = db.prepare(
        "SELECT r.id, r.circuit_id, r.mesh_id, r.trigger_identity, r.state, \
                r.context_json, r.source_agent_node_id, r.created_at, r.updated_at, \
                c.enabled, c.concurrency_limit, COALESCE(snapshot.graph_json, c.graph_json), c.name \
         FROM autopilot_circuit_runs r \
         JOIN autopilot_circuits c ON c.id = r.circuit_id \
         LEFT JOIN circuit_run_snapshots snapshot ON snapshot.run_id = r.id \
         WHERE r.state IN ('pending', 'running', 'paused') \
         ORDER BY r.mesh_id, CASE WHEN r.state = 'pending' THEN 1 ELSE 0 END, r.queue_position, r.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ActiveCircuitRun {
            run: AutopilotCircuitRun {
                id: row.get(0)?,
                circuit_id: row.get(1)?,
                mesh_id: row.get(2)?,
                trigger_identity: row.get(3)?,
                state: row.get(4)?,
                context_json: row.get(5)?,
                source_agent_node_id: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            },
            circuit_enabled: row.get::<_, i64>(9)? != 0,
            circuit_concurrency_limit: row.get(10)?,
            circuit_graph_json: row.get(11)?,
            circuit_name: row.get(12)?,
        })
    })?;
    rows.collect()
}

pub fn list_circuit_runs(circuit_id: i64, limit: i64) -> SqlResult<Vec<AutopilotCircuitRun>> {
    let db = crate::db::read_conn();
    list_circuit_runs_inner(&db, circuit_id, limit)
}

/// Per-test isolated variant of [`list_circuit_runs`] (issue #1691).
pub(crate) fn list_circuit_runs_inner(
    db: &Connection,
    circuit_id: i64,
    limit: i64,
) -> SqlResult<Vec<AutopilotCircuitRun>> {
    let mut stmt = db.prepare(
        "SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                context_json, source_agent_node_id, created_at, updated_at \
         FROM autopilot_circuit_runs WHERE circuit_id = ?1 \
         ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![circuit_id, limit], |row| {
        Ok(AutopilotCircuitRun {
            id: row.get(0)?,
            circuit_id: row.get(1)?,
            mesh_id: row.get(2)?,
            trigger_identity: row.get(3)?,
            state: row.get(4)?,
            context_json: row.get(5)?,
            source_agent_node_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;
    rows.collect()
}

/// Test helper for direct live-state setup. Production pause/resume uses the
/// compare-and-set transition below.
#[cfg(test)]
pub fn set_circuit_run_state(run_id: i64, state: &str) -> SqlResult<()> {
    let db = crate::db::write_conn();
    set_circuit_run_state_inner(&db, run_id, state)
}

/// Per-test isolated variant of [`set_circuit_run_state`] (issue #1691).
#[cfg(test)]
pub(crate) fn set_circuit_run_state_inner(
    db: &Connection,
    run_id: i64,
    state: &str,
) -> SqlResult<()> {
    db.execute(
        "UPDATE autopilot_circuit_runs SET state = ?2, updated_at = datetime('now') \
         WHERE id = ?1 AND state IN ('pending', 'running', 'paused')",
        params![run_id, state],
    )?;
    Ok(())
}

/// Compare-and-set a live run state. Pause/resume commands use this instead
/// of a read followed by an unconditional write, so cancellation cannot win
/// between those operations and then be overwritten by the stale command.
pub fn transition_circuit_run_state(
    run_id: i64,
    expected_state: &str,
    next_state: &str,
) -> SqlResult<bool> {
    let db = crate::db::write_conn();
    transition_circuit_run_state_inner(&db, run_id, expected_state, next_state)
}

/// Per-test isolated variant of [`transition_circuit_run_state`] (issue #1691).
pub(crate) fn transition_circuit_run_state_inner(
    db: &Connection,
    run_id: i64,
    expected_state: &str,
    next_state: &str,
) -> SqlResult<bool> {
    let updated = db.execute(
        "UPDATE autopilot_circuit_runs SET state = ?3, updated_at = datetime('now') \
         WHERE id = ?1 AND state = ?2 AND state IN ('pending', 'running', 'paused')",
        params![run_id, expected_state, next_state],
    )?;
    Ok(updated > 0)
}

/// Is this DB string a terminal run state? The three terminal values
/// (`completed` / `failed` / `cancelled`) each release one
/// circuit-run-admission slot exactly once. `paused` is deliberately
/// NOT terminal: paused runs retain their slot (the user-chosen
/// semantics in #1467 planning).
pub fn is_terminal_run_state(state: &str) -> bool {
    crate::autopilot::circuit::stepper::RunState::from_db_str(state).is_terminal()
}

/// One run row by id, or `None` when the id is unknown.
pub fn get_circuit_run(run_id: i64) -> SqlResult<Option<AutopilotCircuitRun>> {
    let db = crate::db::read_conn();
    get_circuit_run_inner(&db, run_id)
}

/// Per-test isolated variant of [`get_circuit_run`] (issue #1691).
pub(crate) fn get_circuit_run_inner(
    db: &Connection,
    run_id: i64,
) -> SqlResult<Option<AutopilotCircuitRun>> {
    let mut stmt = db.prepare(
        "SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                context_json, source_agent_node_id, created_at, updated_at \
         FROM autopilot_circuit_runs WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![run_id], |row| {
        Ok(AutopilotCircuitRun {
            id: row.get(0)?,
            circuit_id: row.get(1)?,
            mesh_id: row.get(2)?,
            trigger_identity: row.get(3)?,
            state: row.get(4)?,
            context_json: row.get(5)?,
            source_agent_node_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;
    rows.next().transpose()
}

pub fn list_circuit_run_steps(run_id: i64) -> SqlResult<Vec<AutopilotCircuitRunStep>> {
    let db = crate::db::read_conn();
    list_circuit_run_steps_inner(&db, run_id)
}

/// Per-test isolated variant of [`list_circuit_run_steps`] (issue #1691).
pub(crate) fn list_circuit_run_steps_inner(
    db: &Connection,
    run_id: i64,
) -> SqlResult<Vec<AutopilotCircuitRunStep>> {
    let mut stmt = db.prepare(
        "SELECT id, run_id, node_id, agent_node_id, status, attempt, \
                outcome, error_message, started_at, completed_at \
         FROM autopilot_circuit_run_steps WHERE run_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![run_id], map_step_row)?;
    rows.collect()
}

fn map_step_row(row: &rusqlite::Row<'_>) -> SqlResult<AutopilotCircuitRunStep> {
    Ok(AutopilotCircuitRunStep {
        id: row.get(0)?,
        run_id: row.get(1)?,
        node_id: row.get(2)?,
        agent_node_id: row.get(3)?,
        status: row.get(4)?,
        attempt: row.get(5)?,
        outcome: row.get(6)?,
        error_message: row.get(7)?,
        started_at: row.get(8)?,
        completed_at: row.get(9)?,
    })
}

/// One step mutation inside a [`commit_circuit_advance`] transaction.
/// Mirrors the stepper's `StepWrite`; for both `outcome` and `error`, outer
/// `None` means "leave as-is" while `Some(None)` explicitly clears the stored
/// value.
#[derive(Debug, Clone, PartialEq)]
pub struct CircuitStepOp {
    pub node_id: String,
    pub status: String,
    pub outcome: Option<Option<String>>,
    pub error: Option<Option<String>>,
    pub agent_node_id: Option<i64>,
    /// The step's execution count after this write (#1207 retry
    /// bookkeeping). Written on both insert and update.
    pub attempt: i32,
    /// A retried execution: clear outcome/error, restamp started_at.
    pub fresh_attempt: bool,
}

/// The engine's atomic commit point. Applies an optional run-state and/or
/// context update plus any number of step upserts in ONE transaction on
/// ONE mutex acquisition, so a crash mid-apply can never leave a
/// half-applied stepper decision behind. A `context_json` without a
/// `run_state` still persists (the worker's run-id seeding rides any
/// other write).
///
/// Step rows are upserted by `(run_id, node_id)` (UNIQUE constraint);
/// insert stamps `started_at`, terminal statuses stamp `completed_at`,
/// and a `fresh_attempt` op clears the previous round's outcome/error
/// and restamps `started_at` for the retried execution.
///
/// **Terminal-state single-release idempotency (issue #1467, ADR-0028).**
/// When `run_state` is `Some(completed|failed|cancelled)` (per
/// [`is_terminal_run_state`]), the run-state UPDATE uses an extra
/// `WHERE state IN ('pending', 'running', 'paused')` clause. A row
/// already in a terminal state matches zero rows, so the update is a
/// no-op and **no** capacity is double-decremented. Three failure modes
/// this guards against:
///
/// 1. **Concurrent terminal writes** — the stepper's
///    `finish_run_if_done` flushing `completed` racing an effect-
///    failure path to `failed`. The first commit wins (terminal row
///    matches zero rows for the second, so no overwrite).
/// 2. **Crash after commit, before wake** — the next worker pass
///    retries the wake and re-evaluates pending runs cleanly.
/// 3. **Retry path** — a `RetryLimit` reseting a failed step keeps the
///    run's terminal state untouched (the WHERE filter blocks the
///    reschedule from clobbering it to `running`).
///
/// On a successful terminal-state commit (`rows_updated > 0`), this
/// function wakes the circuit worker so the next pass promotes the
/// next FIFO pending run into the freed slot. Wakes are idempotent
/// (condvar-only).
pub fn commit_circuit_advance(
    run_id: i64,
    run_state: Option<&str>,
    context_json: Option<&str>,
    step_ops: &[CircuitStepOp],
) -> SqlResult<()> {
    let mut db = crate::db::write_conn();
    let tx = db.transaction()?;
    let woke = commit_circuit_advance_inner(
        &tx, run_id, run_state, context_json, step_ops,
    )?;
    tx.commit()?;
    drop(db);
    if woke {
        crate::services::circuit_worker::wake_circuit_worker();
    }
    Ok(())
}

/// Per-test isolated wrapper of [`commit_circuit_advance`] (issue #1691).
/// Opens a transaction on the supplied `&mut Connection`, runs the
/// atomic commit logic, and commits — all on the test's private
/// in-memory DB. The public function locks the process-global writer;
/// this helper lets parallel tests run without contending on the global
/// mutex.
#[cfg(test)]
pub(crate) fn commit_circuit_advance_locked(
    conn: &mut Connection,
    run_id: i64,
    run_state: Option<&str>,
    context_json: Option<&str>,
    step_ops: &[CircuitStepOp],
) -> SqlResult<()> {
    let tx = conn.transaction()?;
    let _woke = commit_circuit_advance_inner(
        &tx, run_id, run_state, context_json, step_ops,
    )?;
    tx.commit()?;
    Ok(())
}

/// Per-test isolated variant of [`commit_circuit_advance`] (issue #1691).
/// The public function locks the process-global writer; this helper
/// takes an explicit `&Connection` so parallel tests can each operate
/// against their own in-memory DB. Returns `true` when a terminal-state
/// commit actually fired so the caller can wake the worker (production
/// only — tests can ignore the boolean).
///
/// Does NOT commit the transaction — the caller must commit so it can
/// drop the writer guard before waking the circuit worker.
pub(crate) fn commit_circuit_advance_inner(
    tx: &Connection,
    run_id: i64,
    run_state: Option<&str>,
    context_json: Option<&str>,
    step_ops: &[CircuitStepOp],
) -> SqlResult<bool> {
    let durable_state = tx
        .query_row(
            "SELECT state FROM autopilot_circuit_runs WHERE id = ?1",
            params![run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    // A worker may have loaded this run just before cancellation or circuit
    // deletion. The first transaction to acquire the writer lock wins: once
    // terminal (or deleted), stale context/step writes are discarded together
    // and can never resurrect the run.
    if durable_state
        .as_deref()
        .map(is_terminal_run_state)
        .unwrap_or(true)
    {
        // A crash or an older worker may have left a lease row behind after
        // the run became terminal. It is no longer counted for admission, but
        // remove the durable record while this writer transaction is already
        // holding the run lock.
        tx.execute(
            "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
            params![run_id],
        )?;
        return Ok(false);
    }
    let prior_context_json = if run_state.map(is_terminal_run_state).unwrap_or(false) {
        tx.query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id = ?1",
            params![run_id],
            |row| row.get::<_, String>(0),
        ).optional()?
    } else {
        None
    };
    let mut terminal_woke = false;
    if let Some(state) = run_state {
        if durable_state.as_deref() != Some(state) {
            super::evidence::append_history(tx, run_id, None, None, "run_transition", state,
                Some(super::evidence::SOURCE_CIRCUIT_WORKER), Some(super::evidence::DISPOSITION_APPLIED))?;
        }
    }
    if let Some(context) = context_json {
        super::evidence::record_wait_changes(tx, run_id, context)?;
    }
    match (run_state, context_json) {
        (Some(state), ctx) => {
            let rows_updated = if is_terminal_run_state(state) {
                tx.execute(
                    "UPDATE autopilot_circuit_runs \
                     SET state = ?2, context_json = json_remove(COALESCE(?3, context_json), '$.\"cleanup.pending\"'), updated_at = datetime('now') \
                     WHERE id = ?1 AND state IN ('pending', 'running', 'paused')",
                    params![run_id, state, ctx],
                )?
            } else {
                tx.execute(
                    "UPDATE autopilot_circuit_runs \
                     SET state = ?2, context_json = json_remove(COALESCE(?3, context_json), '$.\"cleanup.pending\"'), updated_at = datetime('now') \
                     WHERE id = ?1",
                    params![run_id, state, ctx],
                )?
            };
            // Terminal committed AT LEAST ONCE this round — wake so the
            // next pass can re-evaluate pending runs against the freed
            // slot (FIFO promotion). The wake is recorded here; the
            // actual call happens after tx.commit() so a crash mid-tx
            // doesn't wake the worker spuriously.
            terminal_woke = is_terminal_run_state(state) && rows_updated > 0;
        }
        (None, Some(ctx)) => {
            tx.execute(
                "UPDATE autopilot_circuit_runs \
                 SET context_json = json_remove(?2, '$.\"cleanup.pending\"'), updated_at = datetime('now') \
                 WHERE id = ?1",
                params![run_id, ctx],
            )?;
        }
        (None, None) => {}
    }
    for op in step_ops {
        super::evidence::append_history(tx, run_id, Some(&op.node_id), Some(op.attempt), "step_transition", &op.status,
            Some(super::evidence::SOURCE_CIRCUIT_WORKER), Some(super::evidence::DISPOSITION_APPLIED))?;
        let effect_state = match op.status.as_str() {
            "completed" => Some("acknowledged"),
            "unverified" => Some("uncertain"),
            _ => None,
        };
        if let Some(state) = effect_state {
            let changed = tx.execute("UPDATE circuit_effects SET state=?4 WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND state='possible_dispatch'",
                params![run_id, op.node_id, op.attempt, state])?;
            if changed > 0 {
                super::evidence::append_history(tx, run_id, Some(&op.node_id), Some(op.attempt), "effect_result", state,
                    Some(super::evidence::SOURCE_CIRCUIT_WORKER), Some(state))?;
            }
        }
        let outcome_val = op.outcome.clone().flatten();
        let outcome_changed = op.outcome.is_some();
        let error_val = op.error.clone().flatten();
        let error_changed = op.error.is_some();
        let terminal = outcome_val
            .as_deref()
            .map(crate::autopilot::circuit::model::StepOutcome::is_terminal_db_str)
            .unwrap_or(false);
        tx.execute(
            "INSERT INTO autopilot_circuit_run_steps \
                 (run_id, node_id, status, attempt, outcome, error_message, agent_node_id, started_at, completed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), \
                  CASE WHEN ?12 THEN datetime('now') ELSE NULL END) \
             ON CONFLICT(run_id, node_id) DO UPDATE SET \
                 status = excluded.status, \
                 attempt = excluded.attempt, \
                  outcome = CASE WHEN ?8 THEN NULL \
                      WHEN ?9 THEN excluded.outcome \
                      ELSE autopilot_circuit_run_steps.outcome END, \
                   error_message = CASE WHEN ?8 THEN NULL \
                      WHEN ?10 THEN excluded.error_message \
                      ELSE autopilot_circuit_run_steps.error_message END, \
                  agent_node_id = COALESCE(excluded.agent_node_id, autopilot_circuit_run_steps.agent_node_id), \
                  started_at = CASE WHEN ?11 THEN datetime('now') \
                      ELSE autopilot_circuit_run_steps.started_at END, \
                   completed_at = CASE WHEN ?12 THEN datetime('now') WHEN ?11 THEN NULL \
                      ELSE autopilot_circuit_run_steps.completed_at END",
            params![
                run_id,
                op.node_id,
                op.status,
                op.attempt,
                outcome_val,
                error_val,
                op.agent_node_id,
                op.fresh_attempt,
                outcome_changed,
                error_changed,
                op.fresh_attempt,
                terminal,
            ],
        )?;
    }
    if terminal_woke {
        // Cleanup ownership is node-scoped and survives run retention. The
        // legacy context marker is consumed as an input only; it is never
        // persisted back into the historical ledger.
        let state = run_state.unwrap_or_default();
        let cleanup_requested = state == "failed" || state == "cancelled" || context_json.or(prior_context_json.as_deref())
            .and_then(|ctx| serde_json::from_str::<serde_json::Value>(ctx).ok())
            .and_then(|ctx| ctx.get("cleanup.pending").and_then(|v| v.as_str()).map(|v| v == "1"))
            .unwrap_or(false);
        if cleanup_requested {
            tx.execute(
                "INSERT INTO agent_node_lifecycle_leases (node_id, cleanup_requested)
                 SELECT DISTINCT s.agent_node_id, 1
                 FROM autopilot_circuit_run_steps s
                 JOIN autopilot_circuit_runs r ON r.id = s.run_id
                 JOIN agent_nodes a ON a.id = s.agent_node_id
                 WHERE s.run_id = ?1 AND s.agent_node_id IS NOT NULL
                   AND s.agent_node_id IS NOT r.source_agent_node_id
                 ON CONFLICT(node_id) DO UPDATE SET
                   cleanup_requested = 1, retired = 0, updated_at = unixepoch()",
                params![run_id],
            )?;
        }
        tx.execute(
            "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
            params![run_id],
        )?;
    }
    Ok(terminal_woke)
}

/// Attach a spawned agent and its optional presentation parent to a step.
/// Parentage is supplied by the circuit domain/worker at the point the
/// relationship is known; the persistence layer stores it without knowing
/// anything about blueprint names or step roles.
pub fn set_circuit_step_agent_node_with_parent(
    run_id: i64,
    node_id: &str,
    agent_node_id: i64,
    parent_agent_node_id: Option<i64>,
) -> SqlResult<bool> {
    let db = crate::db::write_conn();
    set_circuit_step_agent_node_with_parent_inner(
        &db, run_id, node_id, agent_node_id, parent_agent_node_id,
    )
}

/// Per-test isolated variant of [`set_circuit_step_agent_node_with_parent`] (issue #1691).
pub(crate) fn set_circuit_step_agent_node_with_parent_inner(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    agent_node_id: i64,
    parent_agent_node_id: Option<i64>,
) -> SqlResult<bool> {
    let updated = db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = ?3, parent_agent_node_id = ?4 \
         WHERE run_id = ?1 AND node_id = ?2",
        params![run_id, node_id, agent_node_id, parent_agent_node_id],
    )?;
    Ok(updated > 0)
}
/// Clear an agent association after a CloseAgentNode effect succeeds. The
/// circuit step remains an audit record, but a retired reviewer must no
/// longer consume mesh/global agent capacity on the run's remaining steps.
pub fn clear_circuit_step_agent_node(run_id: i64, node_id: &str) -> SqlResult<()> {
    let db = crate::db::write_conn();
    clear_circuit_step_agent_node_inner(&db, run_id, node_id)
}

/// Per-test isolated variant of [`clear_circuit_step_agent_node`] (issue #1691).
pub(crate) fn clear_circuit_step_agent_node_inner(
    db: &Connection,
    run_id: i64,
    node_id: &str,
) -> SqlResult<()> {
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = NULL \
         WHERE run_id = ?1 AND node_id = ?2",
        params![run_id, node_id],
    )?;
    Ok(())
}

/// Clear an association by the newly-created Agent Node id. This is the
/// abort seam for an async spawn that loses a cancellation/delete race after
/// the worker has attached the node but before the task has launched it.
pub fn clear_circuit_step_agent_node_by_agent_id(run_id: i64, agent_node_id: i64) -> SqlResult<()> {
    let db = crate::db::write_conn();
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = NULL \
         WHERE run_id = ?1 AND agent_node_id = ?2",
        params![run_id, agent_node_id],
    )?;
    Ok(())
}
// Concurrency counters — the inputs to the stepper's capacity snapshot.
// ---------------------------------------------------------------------------

/// Steps currently Running across this circuit's active runs — compared
/// against `autopilot_circuits.concurrency_limit`. Paused runs count:
/// their steps still hold real agents even though the graph is parked.
pub fn count_running_circuit_steps(circuit_id: i64) -> SqlResult<i64> {
    let db = crate::db::read_conn();
    count_running_circuit_steps_inner(&db, circuit_id)
}

/// Per-test isolated variant of [`count_running_circuit_steps`] (issue #1691).
pub(crate) fn count_running_circuit_steps_inner(
    db: &Connection,
    circuit_id: i64,
) -> SqlResult<i64> {
    db.query_row(
        "SELECT COUNT(*) FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         WHERE r.circuit_id = ?1 AND r.state IN ('running', 'paused') AND s.status = 'running'",
        params![circuit_id],
        |row| row.get(0),
    )
}

/// **Admitted** circuit runs on this mesh (issue #1467) — the input to
/// the run-admission gate. Counts runs in `running` or `paused` only.
/// Deliberately excludes `pending`: a pending run has NOT yet claimed
/// a circuit-run slot, and the gate exists precisely to decide whether
/// a pending run gets to claim one.
///
/// Why exclude `pending` — counting pending runs toward the cap would
/// self-deadlock: on a mesh with 3 pending runs and cap=2, every
/// pending run's count read sees itself + peers, so 3 < 2 = false and
/// no run ever admits. The fix is the FIFO-faithful shape here: only
/// **admitted** (i.e. `running` or `paused`) runs consume a slot at
/// admission time, and iteration order (`ORDER BY r.id` in the worker's
/// `list_active_circuit_runs`) provides the FIFO promotion — the next
/// un-admitted pending run is admitted the moment the count drops
/// below the cap, with no orphaned admits at the boundary.
///
/// State semantics:
///   * `running` holds capacity (an admitted run's steps may fan out to
///     many agents; the run keeps its one slot regardless of fan-out).
///   * `paused` holds capacity (matches the existing
///     `paused_runs_stay_active_and_counters_count_them` invariant —
///     pause preserves the in-flight agents so resume continues
///     cleanly; the user-chosen semantics in #1467 planning explicitly
///     retain the slot on pause).
///   * `pending` does NOT count (not yet admitted; the gate is the
///     admission decision).
///   * Terminal runs (`completed`/`failed`) do NOT count: a terminal
///     `commit_circuit_advance` transitions the row out of this set in a
///     single `UPDATE`, and a repeated terminal signal is a no-op (so we
///     never double-decrement capacity).
///
/// One unit = one admitted run regardless of how many agent nodes the
/// blueprint fans out to. This is the seam that fixes the two-overlap
/// PR-review deadlock (issue #1355 / runs 3+4 of circuit 5) where the
/// agent-node count saturated on the implementation agent and parked
/// the reviewer step in `pending_slot` indefinitely.
pub fn count_active_circuit_runs(mesh_id: i64) -> SqlResult<i64> {
    let db = crate::db::read_conn();
    count_active_circuit_runs_inner(&db, mesh_id)
}

/// Per-test isolated variant of [`count_active_circuit_runs`] (issue #1691).
pub(crate) fn count_active_circuit_runs_inner(db: &Connection, mesh_id: i64) -> SqlResult<i64> {
    db.query_row(
        "SELECT COUNT(*) FROM autopilot_circuit_runs \
         WHERE mesh_id = ?1 AND state IN ('running', 'paused')",
        params![mesh_id],
        |row| row.get(0),
    )
}

#[cfg(test)]
mod reviewer_tests {
    use super::*;
    use crate::agent::capabilities::capabilities_for;
    use crate::autopilot::compatibility::harness_has_turn_signal;
    use crate::models::Provider;

    #[test]
    fn circuit_review_inherits_source_snapshot_without_a_saved_configuration() {
        use crate::preferences::launch_configurations::{capture, snapshot};
        use crate::autopilot::circuit::context::CircuitContext;
        for harness in ["codex", "retained-codex"] {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        let mesh = crate::db::create_mesh_inner(&db, "source snapshot", "/tmp/source-snapshot").unwrap();
        let source = crate::db::create_agent_node_inner(&db, mesh.id, "Source", &mesh.path, "main", crate::models::EnvType::Windows,
            harness, None, None, None, None, false, None, None, None).unwrap();
        crate::db::update_agent_node_status_inner(&db, source.id, crate::models::SessionStatus::Ready).unwrap();
        let mut prefs = crate::preferences::AppPreferences::default();
        prefs.harness_profiles.push(crate::preferences::HarnessProfile { id: harness.into(), name: "Retained Codex".into(), harness: "codex".into(), runtime: None, wsl_distro: None, executable: None });
        let launch = snapshot(capture(&prefs, harness, &crate::preferences::launch_configurations::LaunchOverrides {
            model: Some("gpt-6-luna".into()), effort: Some("low".into()), extra_args: None,
        }).unwrap());
        assert_eq!(launch.id, format!("launch/{harness}"));
        prefs.harness_profiles.clear();
        assert!(prefs.spawn_configurations.is_empty());
        db.execute("UPDATE agent_nodes SET spawn_configuration=?1 WHERE id=?2", params![serde_json::to_string(&launch).unwrap(), source.id]).unwrap();
        let run = create_node_circuit_run_with_recovery_locked(&mut db, source.id, None, 2, (None, Some(&prefs)), None, true).unwrap();
        let stored = get_circuit_run_inner(&db, run).unwrap().unwrap();
        let context = CircuitContext::from_json(&stored.context_json).unwrap();
        let configuration: crate::preferences::spawn_configurations::SpawnConfiguration = serde_json::from_str(context.get("review.launch.reviewer").unwrap()).unwrap();
        assert_eq!(configuration.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(configuration.spawn_option_id, harness);
        assert_eq!(configuration.resolved.unwrap().harness.name, "Retained Codex");
        }
    }

    #[test]
    fn circuit_issue_review_pins_parent_selection_with_autopilot_precedence() {
        use crate::autopilot::circuit::{context::CircuitContext, model::{CircuitGraph, CircuitNodeKind}};
        for explicit in [None, Some("kimi")] {
            let mut db = Connection::open_in_memory().unwrap();
            crate::db::init_schema(&db).unwrap();
            let mesh = crate::db::create_mesh_inner(&db, "parent precedence", "/tmp/parent-precedence").unwrap();
            db.execute("UPDATE meshes SET default_provider='claude', autopilot_provider='codex' WHERE id=?1", [mesh.id]).unwrap();
            let mut graph = CircuitGraph::issue_driven_autopilot_review("autopilot");
            if let CircuitNodeKind::SpawnAgentNode { provider, .. } = &mut graph.nodes.iter_mut().find(|n| n.id == "implementer").unwrap().kind { *provider = explicit.map(str::to_string); }
            let circuit = create_autopilot_circuit_inner(&db, mesh.id, "Review", "", 2, &graph.to_json().unwrap()).unwrap();
            let run = create_circuit_run_prepared_locked(&mut db, circuit.id, mesh.id, "issue:17", "{}", &Default::default()).unwrap();
            let stored = get_circuit_run_inner(&db, run).unwrap().unwrap();
            let context = CircuitContext::from_json(&stored.context_json).unwrap();
            let configuration: crate::preferences::spawn_configurations::SpawnConfiguration = serde_json::from_str(context.get("review.launch.reviewer").unwrap()).unwrap();
            assert_eq!(configuration.spawn_option_id, explicit.unwrap_or("codex"));
        }
    }

    #[test]
    fn circuit_trigger_snapshot_is_atomic_and_duplicate_trigger_keeps_original_settings() {
        use crate::autopilot::circuit::{context::CircuitContext, model::CircuitGraph};
        use crate::preferences::spawn_configurations::SpawnConfiguration;
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        let mesh = crate::db::create_mesh_inner(&db, "trigger snapshot", "/tmp/trigger-snapshot").unwrap();
        let graph = CircuitGraph::issue_driven_autopilot_review("autopilot");
        let circuit = create_autopilot_circuit_inner(&db, mesh.id, "Review", "", 2, &graph.to_json().unwrap()).unwrap();
        let mut prefs = crate::preferences::AppPreferences::default();
        prefs.reviewer_provider = Some("codex".into());
        prefs.harness_defaults.insert("codex".into(), crate::preferences::HarnessConfigValue { model: Some("gpt-6-luna".into()), effort: Some("low".into()) });
        let run = create_circuit_run_prepared_locked(&mut db, circuit.id, mesh.id, "issue:17", "{}", &prefs).unwrap();
        prefs.reviewer_provider = Some("launch/deleted".into());
        assert_eq!(create_circuit_run_prepared_locked(&mut db, circuit.id, mesh.id, "issue:17", "{}", &prefs).unwrap(), run);
        assert!(create_circuit_run_prepared_locked(&mut db, circuit.id, mesh.id, "issue:18", "{}", &prefs).is_err());
        let rows: i64 = db.query_row("SELECT COUNT(*) FROM autopilot_circuit_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "invalid configuration must not leave a runnable partial run");
        let stored = get_circuit_run_inner(&db, run).unwrap().unwrap();
        let context = CircuitContext::from_json(&stored.context_json).unwrap();
        let configuration: SpawnConfiguration = serde_json::from_str(context.get("review.launch.reviewer").unwrap()).unwrap();
        assert_eq!(configuration.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(configuration.resolved.unwrap().harness.harness, "codex");
    }

    #[test]
    fn circuit_review_pins_effective_configuration_and_continuation_keeps_it() {
        use crate::autopilot::circuit::context::CircuitContext;
        use crate::preferences::spawn_configurations::SpawnConfiguration;
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        let mesh = crate::db::create_mesh_inner(&db, "frozen review", "/tmp/frozen-review").unwrap();
        let source = crate::db::create_agent_node_inner(&db, mesh.id, "Source", &mesh.path, "main", crate::models::EnvType::Windows,
            "codex", None, None, None, None, false, None, None, None).unwrap();
        crate::db::update_agent_node_status_inner(&db, source.id, crate::models::SessionStatus::Ready).unwrap();
        let mut preferences = crate::preferences::AppPreferences::default();
        preferences.reviewer_provider = Some("codex".into());
        preferences.harness_defaults.insert("codex".into(), crate::preferences::HarnessConfigValue { model: Some("gpt-6-luna".into()), effort: Some("low".into()) });
        let first = create_node_circuit_run_with_recovery_locked(&mut db, source.id, None, 2, (None, Some(&preferences)), None, true).unwrap();
        let read_snapshot = |db: &Connection, id| {
            let run = get_circuit_run_inner(db, id).unwrap().unwrap();
            let context = CircuitContext::from_json(&run.context_json).unwrap();
            serde_json::from_str::<SpawnConfiguration>(context.get("review.launch.reviewer").expect("snapshot at run creation")).unwrap()
        };
        let frozen = read_snapshot(&db, first);
        assert_eq!(frozen.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(frozen.effort.as_deref(), Some("low"));
        assert_eq!(frozen.resolved.as_ref().unwrap().harness.harness, "codex");
        preferences.harness_defaults.get_mut("codex").unwrap().model = Some("changed-default".into());
        commit_circuit_advance_locked(&mut db, first, Some("failed"), None, &[crate::db::CircuitStepOp {
            node_id: "verdict".into(), status: "failed".into(), outcome: None, error: None, agent_node_id: None, attempt: 1, fresh_attempt: false,
        }]).unwrap();
        let recovery = super::super::recovery::review_recovery_inner(&db, first, 2).unwrap();
        let successor = create_node_circuit_run_recovery_locked(&mut db, recovery, 2).unwrap();
        // Recovery history (issue #1909): the failed predecessor records the
        // continuation as an operator action naming the successor run.
        let (detail, recorded_source, disposition): (String, Option<String>, Option<String>) = db.query_row(
            "SELECT detail, source, disposition FROM circuit_run_history WHERE run_id=?1 AND kind='recovery'",
            params![first], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!((recorded_source.as_deref(), disposition.as_deref()), (Some("operator"), Some("applied")));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&detail).unwrap()["successor_run_id"].as_i64(), Some(successor));
        let successor_context = CircuitContext::from_json(&get_circuit_run_inner(&db, successor).unwrap().unwrap().context_json).unwrap();
        assert_eq!(successor_context.get("recovery.from_run_id"), Some(first.to_string().as_str()));
        assert_eq!(serde_json::to_value(read_snapshot(&db, successor)).unwrap(), serde_json::to_value(&frozen).unwrap());
        assert_eq!(serde_json::to_value(read_snapshot(&db, first)).unwrap(), serde_json::to_value(&frozen).unwrap());
        cancel_circuit_run_locked(&mut db, successor).unwrap();
        let fresh = create_node_circuit_run_with_recovery_locked(&mut db, source.id, None, 2, (None, Some(&preferences)), None, true).unwrap();
        assert_eq!(read_snapshot(&db, fresh).model.as_deref(), Some("changed-default"));
    }

    /// Issue #1816: every harness with neither an attention hook nor a
    /// passive turn watcher is rejected as a reviewer provider, with a
    /// reason naming the harness; every eligible harness is accepted.
    /// The expectation is derived from the live capability descriptors
    /// through the *shared* predicate (not a re-spelled copy) so the test
    /// agrees with the frontend's `blocksReviewCircuit` predicate by
    /// construction — a harness that gains a turn signal flips both sides
    /// together. The hardcoded pins below (not this loop) are what would
    /// catch wrong capability data.
    #[test]
    fn reviewer_gate_matches_attention_compatibility_for_every_harness() {
        for provider in Provider::all() {
            let caps = capabilities_for(provider.adapter());
            let blocked =
                caps.is_plain_terminal || !harness_has_turn_signal(&caps);
            let id = provider.to_string();
            match normalize_reviewer_provider(Some(id.clone())) {
                Ok(_) => assert!(
                    !blocked,
                    "{id:?} was accepted but has no turn-completion signal"
                ),
                Err(err) => {
                    assert!(
                        blocked,
                        "{id:?} was rejected but can yield a turn: {err:?}"
                    );
                    if caps.is_plain_terminal {
                        assert!(
                            err.contains("Terminal"),
                            "{id:?} must keep the distinct Terminal message, got {err:?}"
                        );
                    } else {
                        assert!(
                            err.contains("turn-completion"),
                            "{id:?} must name the missing capability, got {err:?}"
                        );
                    }
                }
            }
        }
    }

    /// Terminal keeps its existing distinct error message (bare, padded,
    /// composite, and case variants all route through the harness half).
    #[test]
    fn reviewer_terminal_keeps_distinct_message() {
        for picked in ["terminal", "  terminal  ", "terminal:minimax", "Terminal", "TERMINAL:foo"] {
            let err = normalize_reviewer_provider(Some(picked.into())).unwrap_err();
            assert_eq!(
                err,
                "Terminal cannot be used as the reviewer provider.",
                "{picked:?} must keep the Terminal message"
            );
        }
    }

    /// The issue's named cases: `dsh` and `freebuff` are rejected with the
    /// Inspector/docs label naming the harness (not the Terminal message),
    /// including through a composite `harness:provider` id and uppercase
    /// input (lookup normalises case). Issue #1775 gave `cline` a file hook,
    /// so it is now eligible (see [`reviewer_accepts_eligible_unknown_and_blank`]).
    #[test]
    fn reviewer_rejects_harnesses_without_turn_signal() {
        for (picked, name) in [
            ("dsh", "DeepSeek Harness"),
            ("DSH", "DeepSeek Harness"),
            ("freebuff", "Freebuff"),
            ("FREEBUFF", "Freebuff"),
            ("freebuff:minimax", "Freebuff"),
            ("  freebuff  ", "Freebuff"),
        ] {
            let err = normalize_reviewer_provider(Some(picked.into())).unwrap_err();
            assert!(
                err.contains(name),
                "{picked:?} must name the harness ({name}), got {err:?}"
            );
            assert!(
                err.contains("turn-completion"),
                "{picked:?} must name the missing capability, got {err:?}"
            );
        }
    }

    /// Eligible harnesses pass through untouched (value preserved), the
    /// `claude` profile alias and the legacy frontend aliases resolve to
    /// eligible adapters, unknown harness profile ids stay permissive
    /// (the spawn seam resolves them; mirrors the frontend gate), and
    /// blanks collapse to `None` (inherit) exactly as before.
    #[test]
    fn reviewer_accepts_eligible_unknown_and_blank() {
        for picked in [
            "claude", "anthropic", "claude_code", "codex", "agy", "antigravity",
            "opencode", "commandcode", "cmd", "muse", "codex:minimax",
            // Issue #1775: Cline now provisions an attention hook.
            "cline", "cline:minimax",
        ] {
            let got = normalize_reviewer_provider(Some(picked.into())).unwrap();
            assert_eq!(got.as_deref(), Some(picked), "{picked:?} must pass through");
        }
        // Unknown / user-defined harness profile ids stay permissive.
        for picked in ["my-custom-harness", "made-up:provider"] {
            assert!(
                normalize_reviewer_provider(Some(picked.into())).is_ok(),
                "{picked:?} (unknown) must stay permissive"
            );
        }
        assert_eq!(normalize_reviewer_provider(None).unwrap(), None);
        assert_eq!(normalize_reviewer_provider(Some("   ".into())).unwrap(), None);
    }

    /// Issue #1816 review (inherit path): on a blank override the run
    /// inherits the stored app-wide value, so an ineligible *stored*
    /// value must refuse with Settings guidance instead of minting an
    /// unwinnable run. A non-blank override wins and the stored value is
    /// not consulted; an eligible stored value (or none) inherits
    /// silently. Pure: the stored value is threaded in, no global prefs.
    #[test]
    fn preset_reviewer_gates_override_then_stored_app_wide_value() {
        // Override wins; stored value never consulted.
        assert_eq!(
            resolve_preset_reviewer(Some("codex".into()), Some("freebuff".into()), &Default::default()).unwrap().as_deref(),
            Some("codex")
        );
        // Ineligible override refused with the harness-named reason.
        let err = resolve_preset_reviewer(Some("freebuff".into()), None, &Default::default()).unwrap_err();
        assert!(err.contains("Freebuff"), "got {err:?}");
        assert!(!err.contains("Settings"), "override refusal must not blame Settings, got {err:?}");
        // Blank override + ineligible stored value: refuse with guidance.
        for blank in [None, Some("   ".to_string())] {
            let err = resolve_preset_reviewer(blank, Some("freebuff".into()), &Default::default()).unwrap_err();
            assert!(err.contains("Freebuff"), "got {err:?}");
            assert!(err.contains("Settings"), "stale-value refusal must name Settings, got {err:?}");
        }
        // Blank override + eligible stored value: inherit (None).
        assert_eq!(
            resolve_preset_reviewer(Some("  ".into()), Some("codex".into()), &Default::default()).unwrap(),
            None
        );
        // Blank override + no stored value: inherit (None).
        assert_eq!(resolve_preset_reviewer(None, None, &Default::default()).unwrap(), None);
    }
}
