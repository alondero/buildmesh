//! Circuit spawn overrides and two-stage agent launch (issue #1660).

use tauri::{AppHandle, Emitter};

use crate::agent::spawn::ExplicitSpawnOverrides;
use crate::autopilot::circuit::model::CircuitNodeKind;
use crate::autopilot::circuit::stepper::RunView;
use crate::db;
use crate::models::SessionStatus;

use super::{begin_circuit_spawn, run_accepts_effects, CircuitSpawnPermit};

pub(super) fn resolve_circuit_spawn_inputs(
    kind: &CircuitNodeKind,
) -> Result<ResolvedCircuitSpawn, String> {
    let CircuitNodeKind::SpawnAgentNode {
        prompt,
        name,
        provider,
        model,
        effort,
        extra_args,
    } = kind
    else {
        return Err(format!(
            "node is not a spawn node (got {:?})",
            std::mem::discriminant(kind)
        ));
    };
    // Provider: preserve the user-authored string for the `agent_nodes.provider`
    // row column. An unknown id stays as-is — the row carries the
    // user-authored value, and `Provider::from_db_str`'s Anthropic
    // fallback in `spawn_with_intent` handles legacy / mistyped ids.
    let provider_str = provider.clone();
    let prompt = prompt.clone();
    let name = name.clone();
    let explicit = ExplicitSpawnOverrides {
        model: model
            .as_deref()
            .and_then(non_empty_trim)
            .map(str::to_string),
        effort: effort
            .as_deref()
            .and_then(non_empty_trim)
            .map(str::to_string),
        extra_args: extra_args
            .as_deref()
            .and_then(non_empty_trim)
            .map(str::to_string),
    };
    Ok(ResolvedCircuitSpawn {
        prompt,
        name,
        provider_str,
        explicit,
    })
}

/// Mirrors `cascade_inputs_for`'s whitespace trim so the seam collapses
/// "   " / "\t\n" / "" to `None` before reaching the cascade (issue
/// #1148 AC #32). Inline rather than reaching into `crate::agent::spawn`
/// — this is the seam boundary for the cascade, not a place to deepen
/// the dependency surface.
pub(super) fn non_empty_trim(s: &str) -> Option<&str> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub(super) fn inherited_review_provider(
    explicit: Option<&str>,
    parent_provider: Option<&str>,
) -> Option<String> {
    explicit
        .and_then(non_empty_trim)
        .or_else(|| parent_provider.and_then(non_empty_trim))
        .map(str::to_string)
}

/// Apply the reviewer-specific part of the spawn cascade. Circuit-authored
/// values remain the highest-precedence layer; node-started review context is
/// the next layer, and the reviewed agent's provider is the final reviewer
/// fallback before the ordinary mesh/application cascade.
pub(super) fn resolve_review_spawn_inputs(
    view: &RunView,
    node_id: &str,
    provider: Option<String>,
    mut explicit: ExplicitSpawnOverrides,
    parent_provider: Option<&str>,
) -> (Option<String>, ExplicitSpawnOverrides) {
    if !is_review_spawn_step(view, node_id) {
        return (provider, explicit);
    }

    let source_provider = view
        .context
        .get("source.provider")
        .and_then(non_empty_trim)
        .map(str::to_string);
    let source_model = view
        .context
        .get("source.model")
        .and_then(non_empty_trim)
        .map(str::to_string);
    let source_effort = view
        .context
        .get("source.effort")
        .and_then(non_empty_trim)
        .map(str::to_string);

    let provider = provider
        .filter(|value| !value.trim().is_empty())
        .or(source_provider)
        .or_else(|| inherited_review_provider(None, parent_provider));
    explicit.model = explicit.model.or(source_model);
    explicit.effort = explicit.effort.or(source_effort);
    (provider, explicit)
}

pub(super) struct ReviewSpawnResolution {
    pub(super) parent_agent_node_id: Option<i64>,
    pub(super) provider: Option<String>,
    pub(super) explicit: ExplicitSpawnOverrides,
}

/// Resolve all reviewer-specific spawn state in one place. Parent provider
/// observation is supplied by the orchestration layer so this decision seam
/// stays independent of SQLite and can be tested with an in-memory view.
pub(super) fn resolve_review_spawn_configuration(
    view: &RunView,
    node_id: &str,
    provider: Option<String>,
    explicit: ExplicitSpawnOverrides,
    parent_provider: Option<&str>,
) -> ReviewSpawnResolution {
    let parent_agent_node_id = resolve_step_parent_agent_id(view, node_id);
    let (provider, explicit) =
        resolve_review_spawn_inputs(view, node_id, provider, explicit, parent_provider);
    ReviewSpawnResolution {
        parent_agent_node_id,
        provider,
        explicit,
    }
}

pub(super) fn is_review_spawn_step(view: &RunView, node_id: &str) -> bool {
    ((view.context.get("source.review_preset") == Some("1")
        || view.graph.is_issue_driven_autopilot_review())
        && node_id == "reviewer")
        || view.graph.nodes.iter().any(|node| {
            matches!(
                &node.kind,
                CircuitNodeKind::ReviewVerdict { target_node_id }
                    if target_node_id.as_deref() == Some(node_id)
            )
        })
}

/// Resolve the activity parent agent for a circuit step from its upstream
/// agent step, falling back to the borrowed source for review steps.
pub(super) fn resolve_step_parent_agent_id(view: &RunView, node_id: &str) -> Option<i64> {
    view.graph
        .nearest_upstream_agent_step(node_id)
        .and_then(|parent_step| view.step(&parent_step).and_then(|step| step.agent_node_id))
        .or_else(|| {
            is_review_spawn_step(view, node_id)
                .then(|| view.context.source_agent_id())
                .flatten()
        })
}

/// The output of [`resolve_circuit_spawn_inputs`]: the prompt + name
/// carried through verbatim, the optional per-step provider string for
/// `create_pending`, the layer-1 cascade override for
/// `SpawnRequest::with_explicit(...)`. The orchestrator doesn't
/// surface a resolved `Provider` here because `spawn_with_intent`
/// recomputes it from the `agent_nodes.provider` row it just wrote —
/// carrying a duplicate would be speculative generality.
#[derive(Debug)]
pub(super) struct ResolvedCircuitSpawn {
    /// Author-authored prompt, carried verbatim — Mustache resolution
    /// happens in the wrapper against `view.context`.
    pub(super) prompt: String,
    /// Author-authored agent node name, carried verbatim.
    pub(super) name: Option<String>,
    /// User-authored provider string for the `agent_nodes.provider`
    /// column. `None` = fall through to the mesh's default at spawn.
    pub(super) provider_str: Option<String>,
    /// Per-step cascade layer-1 override; passed to
    /// `SpawnRequest::with_explicit(...)`.
    pub(super) explicit: ExplicitSpawnOverrides,
}

/// The SpawnAgentNode effect: create the pending row (stage-1), wire it
/// to the step, then schedule stage-2 in the background — mirroring the
/// autopilot launch order minus the GitHub ledger.
pub(super) fn circuit_spawn_intent(
    delivery: crate::autopilot::launch::InitialPromptDelivery,
    prompt: &str,
) -> crate::agent::spawn::SpawnIntent {
    use crate::agent::spawn::SpawnIntent;
    use crate::autopilot::launch::InitialPromptDelivery;

    match delivery {
        InitialPromptDelivery::Prefill => SpawnIntent::Loop {
            initial_prompt: prompt.to_string(),
        },
        InitialPromptDelivery::Fresh | InitialPromptDelivery::InjectAfterSpawn => {
            SpawnIntent::Fresh
        }
    }
}

pub(super) fn deliver_circuit_initial_prompt(
    app: &AppHandle,
    node_id: i64,
    prompt: &str,
    delivery: crate::autopilot::launch::InitialPromptDelivery,
) {
    use crate::autopilot::launch::InitialPromptDelivery;

    let result = match delivery {
        InitialPromptDelivery::Prefill => Ok(()),
        InitialPromptDelivery::InjectAfterSpawn => {
            crate::autopilot::pipeline::write_prompt_to_pty(node_id, prompt, app)
        }
        InitialPromptDelivery::Fresh => Ok(()),
    };

    if let Err(error) = result {
        tracing::error!(
            "circuits: fallback prompt injection for agent {} failed: {}",
            node_id,
            error
        );
        let _ = crate::agent::session_lifecycle::on_error(
            &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
            node_id,
        );
    }
}

pub(super) fn schedule_circuit_initial_prompt(
    app: &AppHandle,
    node_id: i64,
    prompt: &str,
    delivery: crate::autopilot::launch::InitialPromptDelivery,
) {
    if delivery == crate::autopilot::launch::InitialPromptDelivery::Prefill {
        crate::autopilot::launch::watch_and_submit_for_circuit(app.clone(), node_id, prompt);
    }
}

async fn run_accepts_effects_async(run_id: i64) -> bool {
    tauri::async_runtime::spawn_blocking(move || run_accepts_effects(run_id).unwrap_or(false))
        .await
        .unwrap_or(false)
}

async fn abort_circuit_spawn_async(run_id: i64, node_id: i64) {
    let _ =
        tauri::async_runtime::spawn_blocking(move || abort_circuit_spawn(run_id, node_id)).await;
}

/// Inputs the background spawn task needs. Bundled to keep
/// `spawn_circuit_agent_in_background` under clippy::too_many_arguments.
/// Owned fields because the value moves into a `'static` async task.
pub(super) struct CircuitBackgroundSpawn {
    pub run_id: i64,
    pub node_id: i64,
    pub permit: CircuitSpawnPermit,
    pub explicit: ExplicitSpawnOverrides,
    pub worktree_policy: crate::agent::spawn::WorktreePolicy,
    pub prompt: String,
    pub delivery: crate::autopilot::launch::InitialPromptDelivery,
}

pub(super) fn spawn_circuit_agent_in_background(app: &AppHandle, spawn: CircuitBackgroundSpawn) {
    let CircuitBackgroundSpawn {
        run_id,
        node_id,
        permit,
        explicit,
        worktree_policy,
        prompt,
        delivery,
    } = spawn;
    let app_for_spawn = app.clone();
    tauri::async_runtime::spawn(async move {
        // Keep the permit alive until every post-launch cancellation check
        // and compensation path has completed. Merely accepting it as an
        // unused parameter would drop it when this function returns.
        let _permit = permit;
        // The node is attached before this task is queued. Re-check the
        // durable run state inside the task as well as in the worker effect
        // loop: cancellation may have won while the task was waiting for a
        // runtime worker. A cancelled run must never start a new process.
        if !run_accepts_effects_async(run_id).await {
            abort_circuit_spawn_async(run_id, node_id).await;
            return;
        }
        let intent = circuit_spawn_intent(delivery, &prompt);
        if let Err(error) = crate::agent::spawn::spawn_with_intent(
            &app_for_spawn,
            crate::agent::spawn::SpawnRequest::new(node_id, intent, Default::default())
                .with_explicit(explicit)
                .with_worktree_policy(worktree_policy)
                .with_lifecycle_lease(),
        )
        .await
        {
            if !run_accepts_effects_async(run_id).await {
                abort_circuit_spawn_async(run_id, node_id).await;
            }
            tracing::error!("circuits: agent node {} failed: {}", node_id, error);
            return;
        }
        // Cancellation can race with the process launch itself. Retire the
        // process and clear the step association after the launch completes
        // if the durable state flipped while the async spawn was in flight.
        // This compensating check also handles a ledger deletion that raced
        // before the task acquired the DB row.
        if !run_accepts_effects_async(run_id).await {
            abort_circuit_spawn_async(run_id, node_id).await;
            return;
        }
        schedule_circuit_initial_prompt(&app_for_spawn, node_id, &prompt, delivery);
        if !run_accepts_effects_async(run_id).await {
            abort_circuit_spawn_async(run_id, node_id).await;
            return;
        }
        deliver_circuit_initial_prompt(&app_for_spawn, node_id, &prompt, delivery);
    });
}

/// Retire a circuit spawn that lost a cancellation/delete race. The row may
/// already have been removed by the command layer, so process-registry cleanup
/// is deliberately attempted even when the normal Agent Node delete cannot
/// reload the row.
pub(super) fn abort_circuit_spawn(run_id: i64, node_id: i64) {
    crate::agent::process::PROCESS_REGISTRY.kill_session(node_id);
    let retired = match db::get_agent_node_by_id(node_id) {
        Ok(_) => {
            crate::services::agent_node::delete(node_id, true).map_err(|error| error.to_string())
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = retired {
        // Keep the step association intact: the command-side cleanup retry
        // needs this durable owner id if an OS/worktree lock is transient.
        tracing::warn!(
            "circuits: could not retire aborted spawn {} for run {}: {}",
            node_id,
            run_id,
            error
        );
        return;
    }
    let _ = db::clear_circuit_step_agent_node_by_agent_id(run_id, node_id);
}

/// Attach a newly-created agent to its circuit step and persist the activity
/// parent in the same transaction-owned DB seam used by the worker. Keeping
/// this write beside the in-memory attachment makes the parentage contract
/// testable without manufacturing circuit rows with ad-hoc SQL.
pub(super) fn attach_spawned_agent(
    run_id: i64,
    view: &mut RunView,
    node_id: &str,
    agent_node_id: i64,
    parent_agent_node_id: Option<i64>,
) -> Result<(), String> {
    if !db::set_circuit_step_agent_node_with_parent(
        run_id,
        node_id,
        agent_node_id,
        parent_agent_node_id,
    )
    .map_err(|error| format!("could not attach agent to step: {}", error))?
    {
        return Err("could not attach agent to step: step row no longer exists".to_string());
    }
    view.attach_agent_node(node_id, agent_node_id);
    Ok(())
}

pub(super) fn spawn_step_agent(
    app: &AppHandle,
    run_id: i64,
    mesh_id: i64,
    view: &mut RunView,
    node_id: &str,
) -> Result<(), String> {
    use crate::agent::spawn::WorktreePolicy;

    let kind = view
        .graph
        .node(node_id)
        .map(|n| n.kind.clone())
        .ok_or_else(|| format!("node {} not in blueprint", node_id))?;
    // Pure seam (issue #1358): translate the AST kind into the
    // provider column value + cascade layer-1 overrides that the spawn
    // pipeline consumes downstream. Whitespace-only model/effort/extra_args
    // collapse to `None` here so the cascade falls through to the mesh or
    // application layer (mirrors `cascade_inputs_for`'s trim behaviour).
    let ResolvedCircuitSpawn {
        prompt,
        name,
        provider_str,
        explicit,
    } = resolve_circuit_spawn_inputs(&kind)?;
    // Activity parentage is derived once from the circuit graph and persisted
    // with the step association. The DB layer does not inspect graph JSON or
    // infer special step names.
    let parent_agent_node_id = resolve_step_parent_agent_id(view, node_id);
    let source_provider = view.context.get("source.provider").and_then(non_empty_trim);
    // The parent provider is an observation, not part of the pure resolver's
    // policy. Avoid the lookup whenever a circuit or source provider already
    // determines the reviewer harness.
    let parent_provider = if is_review_spawn_step(view, node_id)
        && provider_str.as_deref().and_then(non_empty_trim).is_none()
        && source_provider.is_none()
    {
        parent_agent_node_id.and_then(|parent_id| {
            db::get_agent_node_by_id(parent_id)
                .ok()
                .map(|parent| parent.provider)
        })
    } else {
        None
    };
    let ReviewSpawnResolution {
        parent_agent_node_id,
        provider: provider_str,
        explicit,
    } = resolve_review_spawn_configuration(
        view,
        node_id,
        provider_str,
        explicit,
        parent_provider.as_deref(),
    );

    let resolved_prompt = view.context.resolve(&prompt);
    let source_issue = view
        .context
        .get("issue.number")
        .and_then(|number| number.parse::<i64>().ok());
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let provider = provider_str
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crate::services::autopilot::configured_autopilot_provider(&mesh));
    let prompt_delivery =
        crate::autopilot::launch::initial_prompt_delivery(&provider, &resolved_prompt);
    let worktree_policy =
        if source_issue.is_some() || view.context.get("source.review_preset") == Some("1") {
            WorktreePolicy::ForceBranched
        } else {
            WorktreePolicy::RespectMesh
        };
    let use_worktree_override = match worktree_policy {
        WorktreePolicy::ForceBranched => Some(true),
        WorktreePolicy::RespectMesh => None,
    };

    // Issue-triggered circuit runs share the legacy Autopilot trust boundary:
    // resolve the same harness/provider chain and reject an incompatible mesh
    // before a pending Agent Node row is created. Manual circuits remain a
    // general-purpose graph feature and are intentionally not subject to the
    // Autopilot compatibility gate.
    if source_issue.is_some() {
        let verdict = crate::autopilot::compatibility::compute_for_mesh(
            Some(provider.as_str()),
            mesh.default_provider.as_deref(),
            crate::preferences::default_provider().as_deref(),
            mesh.use_worktree,
        );
        if !verdict.allowed {
            return Err(format!(
                "Autopilot circuit cannot spawn on mesh {}: incompatible provider/worktree configuration ({:?})",
                mesh_id, verdict.reasons
            ));
        }
    }

    // If step already has an agent node attached (e.g. from an earlier loop iteration/retry)
    if let Some(existing_agent_id) = view.step(node_id).and_then(|s| s.agent_node_id) {
        if crate::agent::process::PROCESS_REGISTRY.is_alive(&existing_agent_id) {
            tracing::info!(
                "circuits: submitting new turn to live agent {} for step {} (run {})",
                existing_agent_id,
                node_id,
                run_id
            );
            let _ = db::update_agent_node_status(existing_agent_id, SessionStatus::Running);
            crate::autopilot::evaluator::note_turn_start(existing_agent_id);
            crate::autopilot::pipeline::write_prompt_to_pty(
                existing_agent_id,
                &resolved_prompt,
                app,
            )
            .map_err(|e| format!("PTY write failed on retry: {}", e))?;
            return Ok(());
        }

        // Process is dead/exited: reuse its worktree path/branch to spawn a fresh process
        if let Ok(old_node) = db::get_agent_node_by_id(existing_agent_id) {
            tracing::info!(
                "circuits: respawning agent for step {} in existing worktree {}",
                node_id,
                old_node.path
            );
            let Some(spawn_permit) = begin_circuit_spawn(run_id)? else {
                return Ok(());
            };
            let new_node = crate::services::agent_node::create_pending_with_worktree_override(
                mesh_id,
                &old_node.path,
                &old_node.branch,
                Some(provider.as_str()),
                source_issue,
                name.as_deref(),
                use_worktree_override,
            )
            .map_err(|e| e.to_string())?;

            if let Err(error) = crate::agent::session_lifecycle::on_created(
                &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
                new_node.id,
            ) {
                let _ = crate::services::agent_node::delete(new_node.id, true);
                return Err(error.to_string());
            }

            if let Err(error) =
                attach_spawned_agent(run_id, view, node_id, new_node.id, parent_agent_node_id)
            {
                // Deletion can win the race after create_pending. If its
                // cascade removed the run, retire the unattached node instead
                // of leaking a process/worktree outside the circuit ledger.
                let _ = crate::services::agent_node::delete(new_node.id, true);
                return Err(error);
            }
            if !run_accepts_effects(run_id)? {
                if let Some(step) = view.step_mut(node_id) {
                    step.agent_node_id = None;
                }
                abort_circuit_spawn(run_id, new_node.id);
                return Ok(());
            }
            crate::autopilot::evaluator::register_circuit(new_node.id);
            crate::autopilot::evaluator::note_turn_start(new_node.id);
            let _ = app.emit(
                "node-created",
                crate::commands::agent::NodeCreatedPayload { id: new_node.id },
            );
            spawn_circuit_agent_in_background(
                app,
                CircuitBackgroundSpawn {
                    run_id,
                    node_id: new_node.id,
                    permit: spawn_permit,
                    explicit,
                    worktree_policy,
                    prompt: resolved_prompt,
                    delivery: prompt_delivery,
                },
            );
            return Ok(());
        }
    }

    let branch = crate::commands::git::get_default_branch_blocking(mesh.path.clone())
        .unwrap_or_else(|_| "main".to_string());

    let Some(spawn_permit) = begin_circuit_spawn(run_id)? else {
        return Ok(());
    };
    let node = crate::services::agent_node::create_pending_with_worktree_override(
        mesh.id,
        &mesh.path,
        &branch,
        // Issue #1358: per-node provider override flows here.
        Some(provider.as_str()),
        source_issue,
        name.as_deref(),
        use_worktree_override,
    )
    .map_err(|e| e.to_string())?;

    if let Err(error) = crate::agent::session_lifecycle::on_created(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node.id,
    ) {
        let _ = crate::services::agent_node::delete(node.id, true);
        return Err(error.to_string());
    }

    if let Err(error) = attach_spawned_agent(run_id, view, node_id, node.id, parent_agent_node_id) {
        let _ = crate::services::agent_node::delete(node.id, true);
        return Err(error);
    }
    if !run_accepts_effects(run_id)? {
        if let Some(step) = view.step_mut(node_id) {
            step.agent_node_id = None;
        }
        abort_circuit_spawn(run_id, node.id);
        return Ok(());
    }

    // Track output times for this piloted node (the PTY submit watcher
    // and future classifiers read them).
    crate::autopilot::evaluator::register_circuit(node.id);
    crate::autopilot::evaluator::note_turn_start(node.id);

    let _ = app.emit(
        "node-created",
        crate::commands::agent::NodeCreatedPayload { id: node.id },
    );
    tracing::info!(
        "circuits: spawned agent node {} for run {} (step {})",
        node.id,
        run_id,
        node_id
    );

    // Stage-2 in the background — same two-stage contract as every
    // other spawn path. An empty prompt starts fresh; a non-empty prompt
    // uses prefill when supported and otherwise is injected after spawn.
    // Issue #1358: per-step model / effort / extra_args ride the explicit
    // layer through to `spawn_with_intent`, where capability masking occurs.
    spawn_circuit_agent_in_background(
        app,
        CircuitBackgroundSpawn {
            run_id,
            node_id: node.id,
            permit: spawn_permit,
            explicit,
            worktree_policy,
            prompt: resolved_prompt,
            delivery: prompt_delivery,
        },
    );

    Ok(())
}
