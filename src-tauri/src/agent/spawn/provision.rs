//! Provision-workspace phase of Agent Node spawn.
//!
//! Fetches the mesh / PR head, cuts or adopts the worktree, sanitizes the
//! gitlink, then runs provider preflight and workspace-trust / attention-hook
//! provisioning. Command construction stays in `command`; process sandboxing
//! stays in `process`.

use super::reader::SpawnTimer;
use super::wire::emit_provider_error;
use super::{MeshSyncOutcome, MeshSyncWarningPayload};
use crate::agent::session_lifecycle::SessionLifecycleSink as _;
use crate::git::worktree::provision::{
    fork_remote_alias, locked_fetch_pr_head, provision_for_spawn, read_origin_ref_sha,
    AppHandleSink, ProvisionHooks, SpawnContext, SpawnSource,
};
use crate::models::{AgentNode, Provider};
use tauri::Emitter;

/// Git/workspace inputs for this phase. Launch knobs (PTY size, prefill,
/// cascade overrides) stay on [`super::launch::LaunchParams`].
pub(super) struct WorkspaceToProvision {
    pub session_id: i64,
    pub provider: Provider,
    pub node: AgentNode,
    pub use_worktree: bool,
    pub worktree_mode: String,
    pub base_ref: String,
    pub warm_claimed: Option<crate::services::warm_pool::ClaimedWarmEntry>,
    pub pool_was_drained_by_this_spawn: bool,
    pub spawn_worktree_name: Option<String>,
    pub resolved: crate::env::ResolvedPath,
}

/// Disk/runtime state after fetch, worktree provision, and provider
/// preflight. Command-construction knobs are not here — the orchestrator
/// passes [`super::launch::LaunchParams`] alongside this to launch.
pub(super) struct ProvisionedWorkspace {
    pub session_id: i64,
    pub provider: Provider,
    pub resolved: crate::env::ResolvedPath,
    pub routing: crate::agent::launch_routing::PreparedLaunchRouting,
}

/// Run the two provider-owned launch prerequisites in order while preserving
/// independent failures. Hook provisioning must still run when trust setup
/// reports an error, and a provider without attention hooks should not invoke
/// its hook seam. The caller supplies this synchronous body to the blocking
/// pool because adapters may inspect or mutate files and invoke WSL.
pub(super) fn run_provider_provisioning<Trust, Hooks>(
    ensure_trusted: Trust,
    provision_hooks: Hooks,
    needs_attention_hook: bool,
) -> (Result<(), String>, Result<(), String>)
where
    Trust: FnOnce() -> Result<(), String>,
    Hooks: FnOnce() -> Result<(), String>,
{
    let trust = ensure_trusted();
    let hooks = if needs_attention_hook {
        provision_hooks()
    } else {
        Ok(())
    };
    (trust, hooks)
}

/// Map an `crate::git::sync::fetch_origin` outcome to either a silent `tracing` log
/// or a `mesh-sync-warning` Tauri event. The frontend's `App.tsx`
/// listens for the event and shows a non-fatal warning toast.
///
/// Per issue #213:
/// - `FetchedButDirty`, `SkippedNoRemote`, `UpToDate`, `Synced` are silent.
/// - `FetchedButDiverged`, `FetchFailed`, `RepoUnusable` emit a
///   warning so the user knows the spawn fell back to local HEAD.
///
/// Spawn proceeds either way; the event is purely informational.
pub(super) fn emit_sync_outcome_event(
    app: &tauri::AppHandle,
    session_id: i64,
    mesh_path: &str,
    outcome: Result<crate::git::sync::FetchOutcome, crate::git::sync::FetchError>,
) {
    let payload = match outcome {
        Ok(crate::git::sync::FetchOutcome::FetchedButDirty { new_commits }) => {
            // Silent, like Synced/UpToDate: the fetch reached the remote and
            // advanced the tracking refs the worktree is cut from — the new
            // node IS fresh. Only the parent checkout's fast-forward was
            // skipped, and the user already knows their own tree is dirty.
            tracing::info!(
                "provision_workspace: auto-sync fetched {} commit(s) but skipped the pull \
                 (parent dirty) for session {}",
                new_commits,
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::SkippedNoRemote) => {
            tracing::info!(
                "provision_workspace: auto-sync skipped (no origin) for session {}",
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::UpToDate) => {
            tracing::info!(
                "provision_workspace: auto-sync up-to-date for session {}",
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::Synced { new_commits }) => {
            tracing::info!(
                "provision_workspace: auto-sync pulled {} commit(s) for session {}",
                new_commits,
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::FetchedButDiverged {
            new_commits,
            reason,
        }) => {
            // Diverged is informational, not an error — the fetch
            // succeeded, the new commits are visible locally, we just
            // can't auto-apply them without a real merge. The user
            // should know so they can decide whether to `git pull`
            // themselves or rebase.
            let message = format!(
                "Fetched {} new commit(s) from origin, but local history has diverged ({}). Spawning from local HEAD — pull manually to sync.",
                new_commits, reason
            );
            tracing::warn!("provision_workspace: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::Diverged,
                new_commits: Some(new_commits),
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
        Err(crate::git::sync::FetchError::RepoUnusable(reason)) => {
            let message = format!(
                "Couldn't auto-sync the mesh — repository is unusable: {}. Spawning from local HEAD instead.",
                reason
            );
            tracing::warn!("provision_workspace: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::RepoUnusable,
                new_commits: None,
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
        Err(crate::git::sync::FetchError::FetchFailed(reason)) => {
            // The most common case: network down. We don't try to
            // distinguish "no network" from "auth failure" — both look
            // the same to `git fetch`. The user knows whether they
            // have connectivity; we just tell them we couldn't sync.
            let message = if reason.is_empty() {
                "Couldn't auto-sync the mesh (fetch failed). Spawning from local HEAD instead."
                    .to_string()
            } else {
                format!(
                    "Couldn't auto-sync the mesh ({}). Spawning from local HEAD instead.",
                    reason
                )
            };
            tracing::warn!("provision_workspace: {}", message);
            Some(MeshSyncWarningPayload {
                node_id: session_id,
                mesh_path: mesh_path.to_string(),
                outcome: MeshSyncOutcome::FetchFailed,
                new_commits: None,
                pr_number: None,
                head_ref: None,
                expected_sha: None,
                actual_sha: None,
                fallback_base_ref: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                message,
            })
        }
    };
    if let Some(payload) = payload {
        let _ = app.emit("mesh-sync-warning", payload);
    }
}

// The worktree-provision helpers — `fetch_single_ref`, `locked_fetch_pr_head`,
// `fork_remote_alias`, `fetch_fork_head`, `read_origin_ref_sha`,
// `upgrade_warm_to_mode`, `adopt_warm_worktree_by_move`,
// `checkout_worktree_to_base`, `run_git_checkout` — live in
// `crate::git::worktree::provision` (ADR 0007 consolidation, issue #677, plus
// #698's `locked_fetch_pr_head` wrapper).

/// Fetch, cut or adopt the worktree, then run provider preflight and
/// workspace-trust / attention-hook provisioning.
///
/// Failures return `Err` to the orchestrator. This phase must not emit
/// `node-spawn-failed` or write session-lifecycle Error — `spawn_with_intent`
/// is the sole owner of those so Resume can call `on_resume_failed`.
pub(super) async fn provision_workspace(
    app: &tauri::AppHandle,
    workspace: WorkspaceToProvision,
    timer: &SpawnTimer,
) -> Result<ProvisionedWorkspace, String> {
    let WorkspaceToProvision {
        session_id,
        provider,
        node,
        use_worktree,
        worktree_mode,
        base_ref,
        mut warm_claimed,
        pool_was_drained_by_this_spawn,
        spawn_worktree_name,
        resolved,
    } = workspace;
    let adapter = provider.adapter();

    // Set true when the spawn-time fetch advances the mesh's base ref, so the
    // single post-spawn pool-maintenance task at the end runs the ref-freshness
    // pass (issue #613 AC3). Carried to the end rather than firing its own
    // thread here so refresh + refill share ONE fill-lock acquisition and can
    // never lose a lock race to each other (issue #613 review).
    let mut ref_advanced_for_pool = false;

    // Auto-sync (issue #213) + PR-head-fetch (#420/#443) + worktree_base_ref
    // resolution only run when the host path doesn't exist yet — for resume /
    // handover / re-spawn the existing worktree's tree IS the agent's starting
    // point and re-syncing would churn refs unnecessarily. Root Nodes
    // (`use_worktree = false`) skip both auto-sync and the PR-head-fetch by
    // virtue of `spawn_worktree_name` being None.
    let host_path_exists = std::path::Path::new(&resolved.host_path).exists();
    let worktree_base_ref = if spawn_worktree_name.is_some() {
        if !host_path_exists {
            if crate::services::fetch_freshness::spawn_can_skip_fetch(&node.path) {
                tracing::info!(
                    "provision_workspace: skipping auto-sync for session {} — mesh {} was synced {}s ago (< TTL)",
                    session_id,
                    node.path,
                    crate::services::fetch_freshness::time_since_success(&node.path).as_secs()
                );
                timer.checkpoint("fetch_origin_skipped_fresh");
            } else {
                let root = node.path.clone();
                let base_ref_owned = base_ref.to_string();
                timer.checkpoint("before_fetch_origin");
                let sync_result = crate::git::sync::locked_fetch_origin(root, base_ref_owned).await;
                timer.checkpoint("after_fetch_origin");
                ref_advanced_for_pool = sync_result
                    .as_ref()
                    .map(|o| o.advanced_ref())
                    .unwrap_or(false);
                emit_sync_outcome_event(app, session_id, &node.path, sync_result);
            }

            if node.source_pr.is_some() {
                let head_ref_owned = node.branch.clone();
                let root = node.path.clone();
                let fork_owner_owned = node.head_repo_owner.clone();
                let fork_url_owned = node.head_repo_clone_url.clone();
                timer.checkpoint("before_fetch_pr_head");
                let fetch_ok = tokio::task::spawn_blocking(move || {
                    locked_fetch_pr_head(
                        &root,
                        &head_ref_owned,
                        fork_owner_owned.as_deref(),
                        fork_url_owned.as_deref(),
                    )
                })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("provision_workspace: fetch task panicked: {}", e);
                    false
                });
                timer.checkpoint("after_fetch_pr_head");
                if fetch_ok {
                    let remote_name = match node.head_repo_owner.as_deref() {
                        Some(owner) => fork_remote_alias(owner),
                        None => "origin".to_string(),
                    };
                    let remote_ref = format!("{}/{}", remote_name, node.branch);

                    let root_for_sha = node.path.clone();
                    let head_ref_for_sha = remote_ref.clone();
                    let expected_sha = node.source_pr_pinned_sha.clone();
                    let actual_sha = tokio::task::spawn_blocking(move || {
                        read_origin_ref_sha(&root_for_sha, &head_ref_for_sha)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "provision_workspace: read_origin_ref_sha task panicked: {}",
                            e
                        );
                        None
                    });
                    if let (Some(expected), Some(actual)) =
                        (expected_sha.as_deref(), actual_sha.as_deref())
                    {
                        if expected != actual {
                            let pr_number = node.source_pr.unwrap_or(-1);
                            let head_ref = node.branch.clone();
                            let message = format!(
                                "PR #{} was force-pushed or rebased after you clicked Spawn                                  (expected {}, now {} on {}). Spawning on the new tip —                                  re-spawn to pin to a fresh SHA.",
                                pr_number, expected, actual, remote_ref,
                            );
                            tracing::warn!(
                                "provision_workspace: {} (node {})",
                                message,
                                session_id,
                            );
                            let _ = app.emit(
                                "mesh-sync-warning",
                                MeshSyncWarningPayload {
                                    node_id: session_id,
                                    mesh_path: node.path.clone(),
                                    outcome: MeshSyncOutcome::PrShaDrift,
                                    new_commits: None,
                                    pr_number: Some(pr_number),
                                    head_ref: Some(head_ref.clone()),
                                    expected_sha: Some(expected.to_string()),
                                    actual_sha: Some(actual.to_string()),
                                    fallback_base_ref: None,
                                    head_repo_owner: None,
                                    head_repo_clone_url: None,
                                    message,
                                },
                            );
                        }
                    }
                    remote_ref
                } else {
                    let pr_number = node.source_pr.unwrap_or(-1);
                    let head_ref = node.branch.clone();
                    let is_fork = node.head_repo_owner.is_some();
                    let source_label = if is_fork {
                        let alias = node
                            .head_repo_owner
                            .as_deref()
                            .map(fork_remote_alias)
                            .unwrap_or_else(|| "fork".to_string());
                        format!("the fork remote '{}'", alias)
                    } else {
                        "origin".to_string()
                    };
                    let message = format!(
                        "Could not fetch PR #{} head ref '{}' from {};                          spawning from the mesh's base ref '{}' instead.                          The agent may land on stale commits — re-spawn                          when the network is back to retry.",
                        pr_number, head_ref, source_label, base_ref,
                    );
                    tracing::warn!("provision_workspace: {} (node {})", message, session_id,);
                    let mut head_repo_owner_str: Option<String> = None;
                    let mut head_repo_clone_url_str: Option<String> = None;
                    if let (Some(owner), Some(url)) = (
                        node.head_repo_owner.as_deref(),
                        node.head_repo_clone_url.as_deref(),
                    ) {
                        head_repo_owner_str = Some(owner.to_string());
                        head_repo_clone_url_str = Some(url.to_string());
                    }
                    let outcome_enum = if is_fork {
                        MeshSyncOutcome::PrForkUnfetchable
                    } else {
                        MeshSyncOutcome::PrHeadUnfetchable
                    };
                    let _ = app.emit(
                        "mesh-sync-warning",
                        MeshSyncWarningPayload {
                            node_id: session_id,
                            mesh_path: node.path.clone(),
                            outcome: outcome_enum,
                            new_commits: None,
                            pr_number: Some(pr_number),
                            head_ref: Some(head_ref.clone()),
                            expected_sha: None,
                            actual_sha: None,
                            fallback_base_ref: Some(base_ref.to_string()),
                            head_repo_owner: head_repo_owner_str,
                            head_repo_clone_url: head_repo_clone_url_str,
                            message,
                        },
                    );
                    base_ref.to_string()
                }
            } else {
                base_ref.to_string()
            }
        } else {
            base_ref.to_string()
        }
    } else {
        base_ref.to_string()
    };

    // Provision the Worktree Node via `provision_for_spawn` (issue #677).
    //    CRITICAL CORRECTNESS:
    //    * `ctx.base_ref` is `worktree_base_ref` (post-fetch for PR/Issue,
    //      the mesh base otherwise). Setting this AFTER the PR-head-fetch
    //      block — not the original `base_ref` — is what makes every PR
    //      spawn land on the freshly fetched PR head rather than going
    //      cold. For Resume / Root Node it's `base_ref` (no fetch ran).
    //    * `warm_claimed.take()` moves the claim into the context; on a warm
    //      failure the provisioner cleans both possible paths up, forgets the
    //      row, and re-cuts cold — all internally.
    let provision_ctx = SpawnContext {
        node: node.clone(),
        source: SpawnSource::from_node(&node),
        base_ref: worktree_base_ref.clone(),
        worktree_mode: worktree_mode.to_string(),
        use_worktree,
        warm_entry: warm_claimed.take(),
        host_path: resolved.host_path.clone(),
    };

    let provision_hooks = ProvisionHooks {
        ref_advanced_for_pool,
        pool_was_drained_by_this_spawn,
    };
    let provision_sink = AppHandleSink { app: app.clone() };
    timer.checkpoint("before_provision");
    let provision_result = tokio::task::spawn_blocking(move || {
        provision_for_spawn(provision_ctx, &provision_hooks, &provision_sink)
    })
    .await
    .unwrap_or_else(|e| Err(format!("provision_for_spawn task panicked: {}", e)));
    timer.checkpoint("after_provision");
    if let Err(e) = provision_result {
        tracing::error!("provision_workspace: provision_for_spawn failed: {}", e);
        return Err(e);
    }

    if let Err(e) =
        crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type)
    {
        tracing::warn!(
            "provision_workspace: failed to sanitize worktree .git file: {}",
            e
        );
    }

    timer.checkpoint("before_provider_preflight");
    let routing_harness_id = node.provider.clone();
    let routing_resolved = resolved.clone();
    let routing = match crate::commands::run_blocking("prepare_provider_routing", move || {
        crate::agent::launch_routing::prepare(&routing_harness_id, provider, &routing_resolved)
    })
    .await
    {
        Ok(routing) => routing,
        Err(error) => {
            if provider == Provider::Codex {
                if let Ok(Some((pairing, _))) =
                    crate::preferences::resolve_stored_pairing_and_account(&node.provider)
                {
                    crate::commands::preferences::schedule_pairing_verification_for_runtime(
                        app.clone(),
                        pairing.harness_id,
                        pairing.provider_id,
                        resolved.env_type,
                    );
                }
            }
            timer.checkpoint("provider_preflight_failed");
            return Err(format!("spawn preflight failed: {error}"));
        }
    };
    timer.checkpoint("after_provider_preflight");

    let emit_signal_unavailable = |message: &str| {
        if let Some(app) = crate::http::app_handle() {
            let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app };
            let detail = crate::agent::session_lifecycle::HookSignalDetail {
                provider: Some(provider.to_string()),
                message: Some(message.to_string()),
                signal_health: crate::agent::session_lifecycle::SignalHealth::Unavailable,
                ..Default::default()
            };
            let payload = crate::agent::session_lifecycle::LifecycleChangedPayload::new(
                session_id,
                crate::agent::session_lifecycle::LifecycleKind::SignalUnavailable,
                crate::models::SessionStatus::Running,
                &detail,
                message,
            );
            sink.emit_lifecycle_changed(payload);
        }
    };

    let launch_runtime = routing.launch_runtime();
    let provisioning_resolved = resolved.clone();
    let provisioning_runtime = launch_runtime.clone();
    let needs_attention_hook = adapter.requires_attention_hook();
    let provisioning = crate::commands::run_blocking("provider_provisioning", move || {
        Ok(run_provider_provisioning(
            || adapter.ensure_workspace_trusted(&provisioning_resolved, &provisioning_runtime),
            || {
                adapter.provision_attention_hooks(
                    &provisioning_resolved,
                    &provisioning_runtime,
                    session_id,
                )
            },
            needs_attention_hook,
        ))
    })
    .await;
    match provisioning {
        Ok((trust, hooks)) => {
            if let Err(e) = trust {
                tracing::warn!(
                    "provision_workspace: workspace trust provisioning failed for session {}: {}",
                    session_id,
                    e
                );
                emit_provider_error(
                    app,
                    session_id,
                    provider,
                    &format!("workspace trust unavailable: {e}"),
                );
            }
            if let Err(e) = hooks {
                tracing::warn!(
                    "provision_workspace: attention hook provisioning failed for session {}: {}",
                    session_id,
                    e
                );
                emit_provider_error(
                    app,
                    session_id,
                    provider,
                    &format!("attention hooks unavailable: {e}"),
                );
                let _ = crate::db::update_agent_node_signal_health(
                    session_id,
                    Some(crate::agent::session_lifecycle::SignalHealth::Unavailable),
                );
                emit_signal_unavailable(&format!("attention hooks unavailable: {e}"));
            } else if needs_attention_hook {
                let _ = crate::db::update_agent_node_signal_health(
                    session_id,
                    Some(crate::agent::session_lifecycle::SignalHealth::Ok),
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                "provision_workspace: provider provisioning task failed for session {}: {}",
                session_id,
                error
            );
            emit_provider_error(
                app,
                session_id,
                provider,
                &format!("provider provisioning unavailable: {error}"),
            );
            if needs_attention_hook {
                let _ = crate::db::update_agent_node_signal_health(
                    session_id,
                    Some(crate::agent::session_lifecycle::SignalHealth::Unavailable),
                );
                emit_signal_unavailable(&format!("provider provisioning unavailable: {error}"));
            }
        }
    }
    timer.checkpoint("after_workspace_trust");

    Ok(ProvisionedWorkspace {
        session_id,
        provider,
        resolved,
        routing,
    })
}
