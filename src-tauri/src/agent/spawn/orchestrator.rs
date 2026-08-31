use super::provision::{
    emit_sync_outcome_event, resolve_base_ref_for_spawn, run_provider_provisioning,
    SpawnInFlightClaim, DEFAULT_WORKTREE_MODE,
};
use super::reader::{
    build_spawn_command_prepared, is_agent_already_running, open_pty_pair,
    reader_should_capture_session_id, register_agent, sandbox_spawn, spawn_child, start_reader,
    SessionIdMode, SpawnTimer, EARLY_EXIT_WINDOW,
};
use super::{
    AgentSpawnedPayload, ExplicitSpawnOverrides, MeshSyncOutcome, MeshSyncWarningPayload,
    ProviderErrorPayload, ResumeCause, SpawnIntent, SpawnOutcome, SpawnRequest,
};
use crate::agent::process::PROCESS_REGISTRY;
use crate::agent::session_lifecycle;
use crate::git::worktree::provision::{
    fork_remote_alias, locked_fetch_pr_head, provision_for_spawn, read_origin_ref_sha,
    AppHandleSink, ProvisionHooks, SpawnContext, SpawnSource,
};
use crate::models::{AgentNode, Provider};
use crate::{db, env};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::Emitter;

/// Options for spawning or resuming an agent process.
pub struct SpawnOptions {
    pub session_id: i64,
    pub provider: Provider,
    pub resume: Option<String>,
    pub rows: u16,
    pub cols: u16,
    pub prefill: Option<String>,
    /// Pre-fetched node to avoid a redundant DB read when the caller already has it.
    pub node: Option<AgentNode>,
    /// Cascade layer-1 model override (issue #1155). Highest precedence
    /// in the spawn-config cascade — wins over the Mesh row and the
    /// application default. `None` or whitespace-only collapses to
    /// absent at [`cascade_inputs_for`] so the cascade falls through.
    pub explicit_model: Option<String>,
    /// Cascade layer-1 effort / reasoning override (issue #1155). Same
    /// semantics as [`Self::explicit_model`] — independent field, only
    /// matters when the harness's capability descriptor declares effort
    /// support (otherwise the resolver mask drops it).
    pub explicit_effort: Option<String>,
    /// Cascade layer-1 verbatim CLI flag string (issue #1358). No mesh
    /// / application layer carries per-spawn flags, so this is the only
    /// layer of supply. Capability-masked downstream — a harness whose
    /// descriptor reports `supports_extra_args = false` (Terminal is
    /// the only one) silently drops the value at the resolver rather
    /// than splicing a synthetic flag into its argv.
    pub explicit_extra_args: Option<String>,
}

/// Pure decision for "given the stored CLI session id, the resume cause,
/// and whether the adapter auto-resumes on startup, what should
/// `spawn_with_intent` do?". The Skip variants are the regression-pin
/// for issue #949: a future refactor that re-introduces an `on_idle`
/// call inside the Skip arms fails review by virtue of the decision
/// being a single enum variant.
///
/// Empty-string defense: legacy writes can leave an empty string in
/// `agent_nodes.cli_session_id`. `db::list_suspended_nodes`'s SQL
/// `IS NOT NULL` filter only catches NULL, so the empty case is
/// defended here.
pub(crate) fn decide_startup_resume(
    cli_session_id: Option<&str>,
    cause: ResumeCause,
    auto_resume_on_startup: bool,
) -> ResumeSkipDecision {
    let stored = cli_session_id.filter(|s| !s.is_empty());
    match (cause, stored) {
        (ResumeCause::Startup, None) => ResumeSkipDecision::SkipSuspended,
        (_, None) => ResumeSkipDecision::NoSessionId,
        (ResumeCause::Startup, Some(_id)) if !auto_resume_on_startup => {
            ResumeSkipDecision::SkipAdapterDeclines
        }
        (_, Some(id)) => ResumeSkipDecision::Proceed(id.to_string()),
    }
}

/// Decision surface for the Startup resume-skip path (issue #949 /
/// PR #1121). See [`decide_startup_resume`] for the full rationale —
/// in short: every `Skip*` variant MUST be paired with a
/// `SpawnOutcome::Skipped(node)` return path that does NOT call any
/// `sink.write_status`. The node stays `Suspended` so the user's
/// Resume / Regenerate affordances remain reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeSkipDecision {
    /// The Startup resume is not viable (missing or empty
    /// `cli_session_id`); return `SpawnOutcome::Skipped(node)` without
    /// touching `agent_nodes.status`. The node stays `Suspended`.
    SkipSuspended,
    /// The Startup resume is not viable because the adapter declines
    /// (`auto_resume_on_startup() == false`); return
    /// `SpawnOutcome::Skipped(node)` without touching
    /// `agent_nodes.status`. The node stays `Suspended`.
    SkipAdapterDeclines,
    /// The Explicit (user-driven) resume is not viable because there
    /// is no captured session id; the caller surfaces this as an
    /// `Err`. Distinct from `SkipSuspended` because the user expects
    /// an error toast, not a silent no-op.
    NoSessionId,
    /// The resume IS viable; the caller continues the spawn flow and
    /// the captured `cli_session_id` is returned.
    Proceed(String),
}

// ---------------------------------------------------------------------------
// Public Tauri command interface
// ---------------------------------------------------------------------------

/// Start an Agent Node from a domain intent.
///
/// The durable node is the source of truth for provider, path, and session
/// identity. Callers only select the reason for starting it and the initial
/// terminal size; low-level `SpawnOptions` stays inside this module while
/// existing callers migrate to the intent seam.
///
/// ## Resume-skip decision (issue #949, PR #1121)
///
/// `spawn_with_intent` previously wrote `Idle` for every Startup-resume
/// branch it couldn't honour, which silently dropped Suspended nodes with
/// no UI recovery affordance. The fix was to short-circuit these branches
/// to `SpawnOutcome::Skipped` WITHOUT touching `agent_nodes.status` —
/// leaving them as `Suspended` so the user can drive the new Resume /
/// Regenerate affordances.
///
/// [`decide_startup_resume`] is the testable surface of that contract:
/// it takes only the three facts the decision depends on (the stored
/// `cli_session_id`, the cause, and whether the adapter auto-resumes on
/// startup) and returns the decided outcome. The Skip variants in
/// particular must NEVER trigger a sink write — the regression test in
/// `mod tests` pins the decision matrix.
pub(crate) async fn spawn_with_intent(
    app: &tauri::AppHandle,
    request: SpawnRequest,
) -> Result<SpawnOutcome, String> {
    let SpawnRequest {
        node_id,
        intent,
        terminal_size,
        explicit,
    } = request;
    // Bind the type name so the `ExplicitSpawnOverrides` re-export stays
    // live at the module scope (the destructure pattern alone doesn't
    // count as a use). The value flows through to `SpawnOptions` below;
    // the annotation is the only thing this line adds.
    let explicit: ExplicitSpawnOverrides = explicit;
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    let provider = crate::preferences::resolve_harness_provider(&node.provider);
    let adapter = provider.adapter();
    let is_resume_intent = matches!(intent, SpawnIntent::Resume { .. });

    let resume = match &intent {
        SpawnIntent::Resume { cause } => {
            let decision = decide_startup_resume(
                node.cli_session_id.as_deref(),
                *cause,
                adapter.auto_resume_on_startup(),
            );
            match decision {
                // Startup resume with no cli_session_id: there is nothing
                // for us to resume. DO NOT write Idle -- that was the
                // silent-drop bug that stranded Suspended OpenCode /
                // Terminal nodes with no UI recovery affordance.
                // Leaving the status as Suspended means the user can
                // click the new Resume button in the sidebar / header
                // to retry with ResumeCause::Explicit. The
                // auto_resume_agent_nodes caller in commands/agent.rs
                // queries db::list_suspended_nodes so the row is
                // always already Suspended here; the prior on_idle
                // was redundant at best and silently destructive.
                ResumeSkipDecision::SkipSuspended
                // Startup resume but the adapter declines (OpenCode,
                // Terminal -- they have no --resume flag and no
                // auto-resume). DO NOT write Idle (same rationale as
                // the cli_session_id-missing branch above): the node
                // stays Suspended so the user's new Resume button can
                // retry later, or the node can be regenerated to a
                // different provider. The Explicit branch
                // (ResumeCause::Explicit from user-driven Resume /
                // Regenerate) is not affected -- the explicit user's
                // expectation is that we try the captured session id
                // via `supports_resume()`, not that we silently skip.
                | ResumeSkipDecision::SkipAdapterDeclines => {
                    return Ok(SpawnOutcome::Skipped(node));
                }
                ResumeSkipDecision::NoSessionId => {
                    return Err(format!(
                        "cannot resume node {}: no CLI session ID is stored",
                        node.id
                    ));
                }
                // Adapter cannot honour a resume arg (OpenCode,
                // Terminal -- no --resume flag) under an Explicit
                // cause: fall through to a fresh process launch while
                // retaining the captured id. Unlike an explicit Fresh
                // intent, this preserves the identity so a future
                // Regenerate to a resumable harness can still pick it up.
                // Without this, the user-driven Resume button on a
                // Suspended OpenCode node would surface a toast instead
                // of starting fresh on the same worktree.
                ResumeSkipDecision::Proceed(id) => Some(id),
            }
        }
        _ => None,
    };

    // Issue #1180 — the prefill comes from `SpawnIntent::initial_prompt`,
    // the single source of truth shared with the desktop draft response
    // and the Autopilot watcher. `into_string()` consumes the
    // `InitialPrompt` wrapper, giving us an owned `String` without an
    // extra `as_str().to_string()` re-allocation. A supporting harness
    // forwards the same string the user already saw on the draft
    // response (byte-identical).
    let prefill = intent
        .initial_prompt()
        .map(|prompt| prompt.into_string())
        .filter(|prefill| {
            if adapter.supports_prefill() {
                true
            } else {
                tracing::warn!(
                    "spawn_with_intent: provider '{}' does not support prefill; skipping {} bytes",
                    node.provider,
                    prefill.len()
                );
                false
            }
        });

    if is_agent_already_running(&node_id) {
        return Ok(SpawnOutcome::AlreadyActive(node));
    }

    if intent_replaces_conversation(&intent) {
        // Every non-resume intent is a deliberate new conversation, so no old harness identity
        // may survive it. In particular, self-assigning providers persist
        // their new id fill-only after launch; retaining an old id here would
        // make the next startup resume the wrong conversation.
        db::clear_cli_session_id(node_id).map_err(|e| e.to_string())?;
    }

    let result = spawn_agent_inner(
        app,
        SpawnOptions {
            session_id: node_id,
            provider,
            resume,
            rows: terminal_size.rows,
            cols: terminal_size.cols,
            prefill,
            node: Some(node.clone()),
            // Issue #1358: per-spawn extra_args ride the explicit layer
            // through to `spawn_agent_inner`, where `resolve_spawn_config`
            // capability-masks them against `HarnessCapabilities
            // .supports_extra_args` (Terminal drops; every interactive
            // harness keeps).
            explicit_extra_args: explicit.extra_args,
            // Cascade layer-1 overrides flow through verbatim. Empty /
            // whitespace-only values are normalised at `cascade_inputs_for`
            // inside `spawn_agent_inner` so the cascade falls through to
            // the next layer rather than forwarding a synthetic blank
            // arg to the harness (issue #1148 AC #32 + #1155 AC #3).
            explicit_model: explicit.model,
            explicit_effort: explicit.effort,
        },
    )
    .await;

    match result {
        Ok(()) => {
            let refreshed = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
            let _ = app.emit(
                "node-spawn-completed",
                crate::commands::agent::NodeSpawnCompletedPayload { node_id },
            );
            Ok(SpawnOutcome::Started(refreshed))
        }
        Err(error) => {
            let sink = session_lifecycle::AppSessionLifecycleSink { app };
            if is_resume_intent {
                let _ = session_lifecycle::on_resume_failed(&sink, node_id, &error);
            } else {
                let _ = session_lifecycle::on_error(&sink, node_id);
            }
            let _ = app.emit(
                "node-spawn-failed",
                crate::commands::agent::NodeSpawnFailedPayload {
                    node_id,
                    error: error.clone(),
                },
            );
            Err(error)
        }
    }
}

/// Whether this request intentionally discards the node's prior conversation.
/// A Resume request can still launch a fresh process for a non-resumable
/// adapter, but that is not user intent to replace the captured identity.
pub(super) fn intent_replaces_conversation(intent: &SpawnIntent) -> bool {
    !matches!(intent, SpawnIntent::Resume { .. })
}

/// Build the per-field cascade inputs the spawn pipeline hands to
/// [`crate::agent::capabilities::resolve_agent_config`] (issue #1155).
///
/// Pure helper so the wiring is testable independent of the resolver —
/// the resolver already has unit tests for its cascade order
/// (`resolver_cascade_prefers_explicit_over_mesh_over_application`), but
/// that test never proves the spawn pipeline *populates* the explicit
/// slot from `SpawnOptions`. This helper is the seam every future spawn
/// site writes to if it wants layer-1 precedence, and the unit tests in
/// `mod tests` below pin both the field-by-field wiring AND the cascade
/// precedence when fed through the resolver.
///
/// Whitespace-only / empty strings on the explicit slot collapse to
/// `None` here (closer to the transport — mobile HTTP / autopilot / UI
/// — than the resolver) so the cascade falls through to the next layer
/// regardless of whether the caller or the resolver did the trimming.
/// Mirrors `resolve_field`'s `normalize_non_empty` (issue #1148 AC #32,
/// #1155 AC #3).
///
/// `mesh_*` and the `application` slot borrow from the mesh row /
/// preferences cache and pass straight through; the resolver normalises
/// those layers at its seam.
pub(crate) fn cascade_inputs_for<'a>(
    explicit_model: Option<&'a str>,
    explicit_effort: Option<&'a str>,
    mesh_model: Option<&'a str>,
    mesh_effort: Option<&'a str>,
    app_default: Option<&'a crate::preferences::HarnessConfigValue>,
    mesh_override: Option<&'a crate::preferences::HarnessConfigValue>,
) -> crate::agent::capabilities::AgentConfigInputs<'a> {
    /// Trim; collapse empty / whitespace-only to `None`. Mirrors
    /// `capabilities::normalize_non_empty` at the spawn seam (issue
    /// #1148 AC #32 + #1155 AC #3). Inline closure hoisted here so the
    /// model + effort legs share the same shape.
    fn non_empty_trim(s: &str) -> Option<&str> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
    crate::agent::capabilities::AgentConfigInputs {
        model: crate::agent::capabilities::FieldInputs {
            explicit: explicit_model.and_then(non_empty_trim),
            mesh_override: mesh_override.and_then(|v| v.model.as_deref()),
            mesh: mesh_model,
            application: app_default.and_then(|v| v.model.as_deref()),
        },
        effort: crate::agent::capabilities::FieldInputs {
            explicit: explicit_effort.and_then(non_empty_trim),
            mesh_override: mesh_override.and_then(|v| v.effort.as_deref()),
            mesh: mesh_effort,
            application: app_default.and_then(|v| v.effort.as_deref()),
        },
    }
}

/// Pure seam for the spawn orchestrator's resolver call (issue #1157).
/// Composes `capabilities_for(provider.adapter())` +
/// `cascade_inputs_for` + `resolve_agent_config` into a single pure
/// function so the integration test for issue #1155 AC #4 ("Regression
/// tests must verify layer-1 behavior at a real spawn site, not just
/// resolver unit tests") can drive the full `SpawnRequest → resolver`
/// path through the same call shape `spawn_agent_inner` uses — without
/// standing up a Tauri runtime, a preferences cache, or a DB.
///
/// `app_default` is the ALREADY-LOOKED-UP value for the harness
/// profile. The orchestrator parses the composite `node.provider` id
/// (`"<harness>:<provider>"` for Proxied rows) and resolves the harness
/// default at its seam so this helper stays free of
/// `preferences::load()` (which would force the test to populate the
/// in-process preferences cache).
pub(crate) fn resolve_spawn_config(
    provider: Provider,
    explicit_model: Option<&str>,
    explicit_effort: Option<&str>,
    explicit_extra_args: Option<&str>,
    app_default: Option<&crate::preferences::HarnessConfigValue>,
    mesh_override: Option<&crate::preferences::HarnessConfigValue>,
) -> crate::agent::capabilities::ResolvedAgentConfig {
    let capabilities = crate::agent::capabilities::capabilities_for(provider.adapter());
    // Issue #1358: all three cascaded fields (model / effort /
    // extra-args) flow into one resolver call so `ResolvedAgentConfig`
    // is constructed atomically (issue #1362 code review). The extra-args
    // mask lives inside `resolve_agent_config` next to the others.
    crate::agent::capabilities::resolve_agent_config(
        &capabilities,
        cascade_inputs_for(
            explicit_model,
            explicit_effort,
            None,
            None,
            app_default,
            mesh_override,
        ),
        explicit_extra_args,
    )
}

/// Transitional implementation retained while transport callers migrate to
/// [`spawn_with_intent`]. It is private to the agent module once migration is
/// complete.
pub(crate) async fn spawn_agent_inner(
    app: &tauri::AppHandle,
    opts: SpawnOptions,
) -> Result<(), String> {
    let SpawnOptions {
        session_id,
        provider,
        resume,
        rows,
        cols,
        prefill,
        node: preloaded_node,
        explicit_model,
        explicit_effort,
        explicit_extra_args,
    } = opts;

    tracing::info!(
        "spawn_agent_inner: session_id={}, provider={:?}, resume={:?}, size={}x{}",
        session_id,
        provider,
        resume,
        cols,
        rows
    );

    let timer = SpawnTimer::new(session_id);

    // 0. Claim the session for the WHOLE pipeline. `is_agent_already_running`
    //    below only sees registered processes, and registration is seconds
    //    away (git fetch + worktree provisioning) — without this claim a
    //    concurrent duplicate call (backend stage-2 vs frontend Terminal
    //    auto-spawn) passes that check and its step-2 stale-kill destroys
    //    THIS call's freshly-booted process. Returning Ok mirrors the
    //    already-running short-circuit: the node is being brought up, the
    //    caller has nothing further to do.
    let _spawn_claim = match SpawnInFlightClaim::try_claim(session_id) {
        Some(claim) => claim,
        None => {
            tracing::info!(
                "spawn_agent_inner: spawn already in flight for session {}, skipping duplicate call",
                session_id
            );
            return Ok(());
        }
    };

    // 1. Check if already running
    if is_agent_already_running(&session_id) {
        return Ok(());
    }

    // 2. Kill any stale process for this session
    tracing::debug!(
        "spawn_agent_inner: killing stale processes for session {}",
        session_id
    );
    crate::agent::process::kill_agent(session_id).await.ok();

    // 3. Get node and resolve paths (skip DB read if caller provided the node)
    let node = match preloaded_node {
        Some(n) => n,
        None => db::get_agent_node_by_id(session_id).map_err(|e| {
            let err = format!(
                "spawn_agent: failed to get agent node {}: {}",
                session_id, e
            );
            tracing::error!("{}", err);
            err
        })?,
    };
    tracing::info!(
        "spawn_agent_inner: node path={}, env={:?}",
        node.path,
        node.env
    );
    timer.checkpoint("after_node_db_read");

    let adapter = provider.adapter();

    // 4. Determine session ID mode
    let session_id_mode = if adapter.supports_resume() {
        match resume {
            Some(ref id) if !id.is_empty() => SessionIdMode::Resume(id.clone()),
            _ => {
                if adapter.self_assigns_session_id() {
                    SessionIdMode::None
                } else {
                    let cli_uuid = uuid::Uuid::new_v4().to_string();
                    db::update_cli_session_id(session_id, &cli_uuid).map_err(|e| e.to_string())?;
                    tracing::info!("spawn_agent_inner: assigned cli_session_id={}", cli_uuid);
                    SessionIdMode::Assign(cli_uuid)
                }
            }
        }
    } else {
        SessionIdMode::None
    };

    // 5. Read mesh row for use_worktree / worktree_mode (legacy
    // model/effort columns are no longer read as active spawn
    // configuration — the v33 migration copied any non-empty legacy
    // values into the new map; see issue #1151 acceptance criteria 6).
    let row = env::mesh_row(&std::path::PathBuf::from(&node.path));
    let use_worktree = row.as_ref().map(|r| r.use_worktree).unwrap_or(true);
    // OS-level sandbox toggle (macOS Seatbelt #497, Windows AppContainer #498).
    // Off by default; the per-OS spawn policy is decided in `spawn_environment::wrap`
    // and `crate::sandbox::spawn::spawn_sandboxed`.
    let sandbox = row.as_ref().map(|r| r.sandbox).unwrap_or(false);
    let worktree_mode = row
        .as_ref()
        .and_then(|r| r.worktree_mode.as_deref())
        .unwrap_or(DEFAULT_WORKTREE_MODE);
    // Autopilot enforcement (issue #482, PRD #480): auto-spawned nodes must
    // always work on a real branch (and in a worktree) — the wrap-up sequence
    // pushes a branch and opens a PR, which a detached-HEAD worktree or a
    // shared mesh root cannot do. The ledger row is written before stage-2
    // starts, so this read is ordered correctly. The node row itself already
    // carries `use_worktree = true` (spawn override in `services::autopilot`).
    let is_autopilot = db::get_autopilot_run(session_id).ok().flatten().is_some();
    let use_worktree = use_worktree || is_autopilot;
    let worktree_mode = if is_autopilot {
        "branched"
    } else {
        worktree_mode
    };
    let base_ref =
        resolve_base_ref_for_spawn(&node.path, row.as_ref().and_then(|r| r.base_ref.as_deref()));

    timer.checkpoint("after_mesh_row_read");

    // 6. Compute spawn path. The pool claim (issue #609/#612) decides whether
    //    the spawn adopts a pre-warmed worktree (Manual: pool slug IS the
    //    node name; Issue/PR: `git worktree move` the pool dir onto the
    //    `gh{N}-`/`pr{N}-` target) or falls through to a cold create. A
    //    claim failure is non-fatal — the spawn falls back to cold; it
    //    only fails on an actual worktree-create error.
    let mesh_id = db::get_mesh_by_path(&node.path).map(|m| m.id).unwrap_or(-1);
    // `is_rename_spawn` selects between the two warm-pool adoption modes
    // downstream: Manual adopts the pool's slug as the node name (issue #609);
    // Issue/PR keep their own `gh{N}-`/`pr{N}-` name and move the pool dir
    // to match (issue #612). Consumed by the post-spawn name adoption
    // (further below) and by the SpawnContext built at phase 7.
    let is_rename_spawn = node.source_issue.is_some() || node.source_pr.is_some();
    let mut warm_claimed: Option<crate::services::warm_pool::ClaimedWarmEntry> = None;
    // Issue #653: a successful `try_claim` that the use-site recheck later
    // dropped still drained the pool by one row — `warm_claimed` is None
    // (the spawn fell back to cold), but the mesh's pool inventory is one
    // short. Track "we claimed at least once this spawn" so the post-spawn
    // refill still fires (otherwise the pool stays at target-1 until the
    // next reconcile). Distinct from `warm_claimed` because `warm_claimed`
    // tracks "we adopted the warm entry as this node's worktree" — that's
    // what `forget_after_spawn` and the manual name adoption gate on.
    let mut pool_was_drained_by_this_spawn = false;
    if use_worktree {
        // The path the node resolves to WITHOUT a pool claim. If it's already
        // on disk this spawn is a resume / handover / re-spawn reusing an
        // existing worktree — never claim a pool entry for it (that would
        // re-point the node at a different directory and abandon its work).
        let existing = env::resolve_agent_path(&node.path, node.worktree_name.as_deref());
        let existing_present = std::path::Path::new(&existing.host_path).exists();
        if mesh_id > 0 && crate::services::warm_pool::should_claim_for_spawn(existing_present) {
            match crate::services::warm_pool::try_claim(app, mesh_id) {
                Ok(Some(entry)) => {
                    tracing::info!(
                        "spawn_agent_inner: claimed warm pool entry id={} path={} slug={} base_sha={}",
                        entry.id,
                        entry.path,
                        entry.preassigned_name,
                        entry.base_sha.as_deref().unwrap_or("none"),
                    );
                    // Issue #653 use-site guard: `try_claim` just checked
                    // the directory exists, but the spawn then waits
                    // seconds inside `fetch_origin` + git worktree move;
                    // another thread can delete the directory in that gap.
                    // Re-check immediately before committing to the warm
                    // path. On false, `recheck_after_claim` already dropped
                    // the row + tombstone; we just leave `warm_claimed`
                    // None so the existing `spawn_worktree_name` fallback
                    // resolves to the throwaway slug and the cold-create
                    // block runs naturally for both spawn modes (Issue/PR
                    // and manual).
                    if crate::services::warm_pool::recheck_after_claim(entry.id, &entry.path) {
                        warm_claimed = Some(entry);
                        pool_was_drained_by_this_spawn = true;
                    } else {
                        // Note: `recheck_after_claim` already logs the
                        // reason (claimed row N's directory disappeared...),
                        // so don't duplicate that WARN here.
                        // warm_claimed stays None — do NOT adopt. The row
                        // was already dropped by recheck_after_claim, but
                        // the pool inventory is still down by one; the
                        // post-spawn refill below must run regardless of
                        // the local `did_claim_warm` flag (which checks
                        // `warm_claimed.is_some()`, not the DB).
                        pool_was_drained_by_this_spawn = true;
                    }
                }
                Ok(None) => {
                    tracing::info!(
                        "spawn_agent_inner: warm pool empty for mesh {}; cold spawn",
                        mesh_id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "spawn_agent_inner: warm pool claim failed (non-fatal, falling back to cold): {}",
                        e
                    );
                }
            }
        }
    }

    // The effective spawn_worktree_name + path.
    //
    //  * Manual warm claim (`!is_rename_spawn`): adopt the pool's preassigned
    //    slug as the node's `worktree_name`, so the rest of the pipeline
    //    resolves straight onto the already-on-disk pool directory (#609).
    //  * Issue/PR warm claim (`is_rename_spawn`): keep the node's own
    //    `gh{N}-`/`pr{N}-` `worktree_name`. It resolves to a path that does
    //    NOT exist yet, so we enter the cold-create block below — where the
    //    PR-head fetch runs — and there `git worktree move` the pool directory
    //    onto this target instead of a cold `git worktree add` (#612).
    //  * No claim: fall back to whatever the node row carries (resumes, or a
    //    cold issue/PR spawn).
    //
    // Owned (`Option<String>`, not `Option<&str>`) on purpose: the Issue/PR
    // path mutates `warm_claimed` (take / re-assign) inside the worktree block
    // below, so `spawn_worktree_name` must not hold a borrow into it. The slugs
    // are short, so the clone is negligible.
    let spawn_worktree_name: Option<String> = if let Some(ref entry) = warm_claimed {
        if is_rename_spawn {
            node.worktree_name.clone()
        } else {
            Some(entry.preassigned_name.clone())
        }
    } else if use_worktree {
        node.worktree_name.clone()
    } else {
        tracing::info!("spawn_agent_inner: use_worktree=false, using repo root directly");
        None
    };

    let resolved = env::resolve_agent_path(&node.path, spawn_worktree_name.as_deref());
    tracing::info!(
        "spawn_agent_inner: resolved spawn_path={}, host_path={}, env={:?}",
        resolved.spawn_path,
        resolved.host_path,
        resolved.env_type
    );

    // For a Manual warm claim, the pool's preassigned slug IS the node's
    // `worktree_name` once the spawn completes — the post-spawn DB write
    // (below, before `register_agent`) persists that, but `provision_for_spawn`
    // needs the right branch name in the Spawn Context NOW so the manual
    // `Upgraded` branch's `git checkout -B <branch>` targets the pool's slug
    // rather than the node's stage-1 throwaway. Mutate `node.worktree_name`
    // in place here; `node.clone()` carries the value into the Spawn Context.
    let mut node = node;
    if let (false, Some(ref entry)) = (is_rename_spawn, &warm_claimed) {
        node.worktree_name = Some(entry.preassigned_name.clone());
    }

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
            // Auto-sync the parent **Mesh** before we cut a new worktree
            // (issue #213). The sync is best-effort: a network failure or
            // a non-fast-forwardable history is surfaced as a `mesh-sync-
            // warning` Tauri event so the frontend can show a non-fatal
            // toast, but spawn always proceeds from the local HEAD.
            // Skips (dirty parent, no remote, already up to date) are
            // silent — the user doesn't need to know about them.
            //
            // The remote is derived from the mesh's `base_ref` (issue
            // #276), so a Mesh with `base_ref = "upstream/main"` syncs
            // against `upstream` rather than hardcoded `origin`. We move
            // `base_ref` into the closure because `spawn_blocking` needs
            // a `'static` closure.
            // Freshness skip (ADR 0020): the background mesh sync in
            // `services::pool_worker` (and any recent spawn / manual Sync)
            // stamps `services::fetch_freshness` on every successful fetch.
            // When the mesh was synced within `SPAWN_FETCH_TTL`, this whole
            // network round-trip is redundant — the remote-tracking ref the
            // worktree is cut from is already current to within minutes —
            // so we skip it and the spawn goes straight to provisioning.
            // `ref_advanced_for_pool` stays false: whichever path recorded
            // the fresh fetch already ran the warm-pool freshness pass.
            // The manual Sync button remains the "I need the latest RIGHT
            // NOW" override — it fetches unconditionally.
            if crate::services::fetch_freshness::spawn_can_skip_fetch(&node.path) {
                tracing::info!(
                    "spawn_agent_inner: skipping auto-sync for session {} — mesh {} was synced {}s ago (< TTL)",
                    session_id,
                    node.path,
                    crate::services::fetch_freshness::time_since_success(&node.path).as_secs()
                );
                timer.checkpoint("fetch_origin_skipped_fresh");
            } else {
                let root = node.path.clone();
                let base_ref_owned = base_ref.to_string();
                timer.checkpoint("before_fetch_origin");
                // Issue #652 — per-Mesh serialization. Without this lock, N
                // concurrent spawns against the same Mesh race on
                // .git/FETCH_HEAD, .git/index.lock, and refs/heads/<branch>.lock:
                // one git fetch wins, the others fail with "another git process"
                // and the spawn lands on a stale ref. The lock is *blocking*
                // (not try_lock-or-skip), so caller #2 waits for caller #1 to
                // populate the refs and then reuses them (its natural outcome
                // is UpToDate, which is correct).
                //
                // Issue #709 — the wrap is consolidated into
                // `git::sync::locked_fetch_origin` so the lock-acquisition
                // shape is identical to the manual `git_sync`'s
                // `locked_do_sync`, the PR-spawn's `locked_fetch_pr_head`,
                // and the prune's `locked_prune_remote_tracking`. The
                // `tokio::task::spawn_blocking` + `with_mesh_sync_lock`
                // pair used to live inline here.
                let sync_result = crate::git::sync::locked_fetch_origin(root, base_ref_owned).await;
                timer.checkpoint("after_fetch_origin");
                // Ref-freshness (issue #613 AC3): if the fetch actually pulled new
                // commits, the mesh's base ref has moved, so any OTHER warm pool
                // entries for this mesh are now parked on a stale SHA and must be
                // `git reset --hard`ed onto the new commit. Only `Synced` /
                // `FetchedButDiverged` advance the ref — `UpToDate` / skipped means
                // nothing moved. We record the fact here and let the single
                // post-spawn maintenance task (at the end of this fn) run the
                // freshness pass, so refresh and refill share one fill-lock
                // acquisition instead of racing on two threads (issue #613 review).
                ref_advanced_for_pool = sync_result
                    .as_ref()
                    .map(|o| o.advanced_ref())
                    .unwrap_or(false);
                emit_sync_outcome_event(app, session_id, &node.path, sync_result);
            } // end freshness-gated fetch_origin block

            // Worktree adoption for PR-spawned nodes (issue #420, extended
            // by #443 for fork PRs). When the node carries a `source_pr`,
            // the head ref stored in `node.branch` is the PR's actual source
            // branch (e.g. `feat/420-pr-spawn`), and the worktree needs to
            // be cut from `<remote>/<head_ref>` so the agent lands on the
            // same commits the PR is built from. Two cases:
            //
            //  - Same-repo PRs (`head_repo_owner` is `None`): the head
            //    lives on `origin` — we call `locked_fetch_pr_head` and
            //    use `origin/<head_ref>` (the #420 path).
            //  - Fork PRs (`head_repo_owner` is `Some`): the head lives on
            //    the fork's clone URL — `locked_fetch_pr_head` calls
            //    `fetch_fork_head`, which registers the fork as a remote
            //    (`fork-<login>`) and fetches from there (issue #443,
            //    follow-up to #36). The worktree base_ref becomes
            //    `fork-<login>/<head_ref>`.
            //
            // The fetch is best-effort: a network failure or stale local ref
            // falls back to the mesh's `base_ref` (the ADR 0001 offline
            // pattern), and the user sees the agent spawn on the wrong
            // commits rather than a hard error — strictly worse than a clean
            // spawn on the right commits, but a strict-error spawn is
            // brittle to the very first offline session.
            //
            // Even so, the fallback MUST surface to the user: the spawn
            // otherwise reports success, the dock closes, and the agent
            // silently lands on the wrong commits. We piggy-back on the
            // existing `mesh-sync-warning` event (the same non-fatal channel
            // the auto-sync path uses) with a `pr_head_unfetchable` or
            // `pr_fork_unfetchable` outcome — the App.tsx listener already
            // renders a toast for that event, so no frontend change is
            // required.
            if node.source_pr.is_some() {
                let head_ref_owned = node.branch.clone();
                let root = node.path.clone();
                let fork_owner_owned = node.head_repo_owner.clone();
                let fork_url_owned = node.head_repo_clone_url.clone();
                timer.checkpoint("before_fetch_pr_head");
                let fetch_ok = tokio::task::spawn_blocking(move || {
                    // Issue #698 — per-Mesh serialization for the PR-spawn
                    // fetch. The match lives inside `locked_fetch_pr_head`
                    // so both branches share one lock acquisition keyed on
                    // `&root` (the mesh's DB-stored path, same key
                    // `fetch_origin` uses two steps above). Without the
                    // lock, two concurrent PR-spawns (or a PR-spawn racing
                    // the manual `git_sync` from #680) collide on
                    // `.git/FETCH_HEAD` / `refs/remotes/<remote>/<ref>.lock`
                    // and the losing spawn silently falls back to `base_ref`.
                    // The fork branch additionally writes `git remote add/
                    // set-url` config that the next caller must observe,
                    // so the lock covers both remote registration and
                    // fetch in one critical section.
                    locked_fetch_pr_head(
                        &root,
                        &head_ref_owned,
                        fork_owner_owned.as_deref(),
                        fork_url_owned.as_deref(),
                    )
                })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("spawn_agent_inner: fetch task panicked: {}", e);
                    false
                });
                timer.checkpoint("after_fetch_pr_head");
                if fetch_ok {
                    // Pick the right remote-name prefix for the base_ref
                    // string the worktree will be cut from. Same-repo PRs
                    // use `origin/<head_ref>` (matches the mesh's default
                    // `base_ref`); fork PRs use `fork-<login>/<head_ref>`.
                    // The mesh's `base_ref` is overwritten to use the fork
                    // remote so a future head-branch push is picked up by
                    // the same fetch the auto-sync path runs.
                    let remote_name = match node.head_repo_owner.as_deref() {
                        Some(owner) => fork_remote_alias(owner),
                        None => "origin".to_string(),
                    };
                    let remote_ref = format!("{}/{}", remote_name, node.branch);

                    // Issue #444 — exact-pinning: after a successful fetch,
                    // compare the local SHA at the remote ref we just
                    // populated to the `source_pr_pinned_sha` we stored at
                    // spawn time. On mismatch (PR was force-pushed / rebased
                    // between click-time and spawn-time) emit a non-fatal
                    // `pr_sha_drift` warning via the same `mesh-sync-warning`
                    // channel the offline-fallback path uses. The worktree
                    // proceeds on the new tip — strict-fail would block
                    // legitimate rebase-and-merge workflows for one stale
                    // click. The drift check is a no-op for v15-and-earlier
                    // PR-spawned rows where `source_pr_pinned_sha` is None
                    // (the column was added in v16) and for any empty
                    // GitHub response: read_origin_ref_sha returns None
                    // for a missing ref, and a None expected/actual pair
                    // is treated as "no SHA to compare" and skipped.
                    let root_for_sha = node.path.clone();
                    let head_ref_for_sha = remote_ref.clone();
                    let expected_sha = node.source_pr_pinned_sha.clone();
                    let actual_sha = tokio::task::spawn_blocking(move || {
                        read_origin_ref_sha(&root_for_sha, &head_ref_for_sha)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "spawn_agent_inner: read_origin_ref_sha task panicked: {}",
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
                            tracing::warn!("spawn_agent_inner: {} (node {})", message, session_id,);
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
                    // Distinguish the two failure modes in the toast: a
                    // fork fetch failure is more likely to be permanent
                    // (the user renamed or deleted the fork) than a same-
                    // repo failure (usually transient network).
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
                    tracing::warn!("spawn_agent_inner: {} (node {})", message, session_id,);
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
            // Path already exists (resume / handover / re-spawn). No
            // auto-sync, no PR-head-fetch — the existing worktree's tree IS
            // the spawn point.
            base_ref.to_string()
        }
    } else {
        // Root Node (`use_worktree = false`) — no worktree, no base_ref
        // resolution needed; `provision_for_spawn` short-circuits to `Reused`.
        base_ref.to_string()
    };

    // 7. Provision the Worktree Node via `provision_for_spawn` (issue #677).
    //    The seam deepened: the provisioner now owns the four-way decision
    //    (Reused / Adopted / Upgraded / Created), the warm-failure cold
    //    fallback, the post-success pool row cleanup (`forget_after_spawn`),
    //    the Manual name-adoption DB write, and the `post_spawn_maintenance`
    //    thread trigger. This orchestrator hands it:
    //      * a SpawnContext (data only),
    //      * ProvisionHooks (decision inputs: ref-advanced / pool-drained),
    //      * an AppHandleSink (side-effect surface),
    //    then awaits the call and propagates the result.
    //
    //    CRITICAL CORRECTNESS:
    //    * `ctx.base_ref` is `worktree_base_ref` (post-fetch for PR/Issue,
    //      the mesh base otherwise). Setting this AFTER the PR-head-fetch
    //      block — not the original `base_ref` — is what makes every PR
    //      spawn land on the freshly fetched PR head rather than going
    //      cold. For Resume / Root Node it's `base_ref` (no fetch ran).
    //    * `warm_claimed.take()` moves the claim into the context; on a warm
    //      failure the provisioner cleans both possible paths up, forgets the
    //      row, and re-cuts cold — all internally. This orchestrator no
    //      longer threads the entry back out.
    //    * `is_rename_spawn` is preserved unchanged — the pre-provision
    //      `spawn_worktree_name` resolution still reads it.
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
    // The provisioner owns its own post-success bookkeeping (`forget_after_spawn`,
    // Manual name adoption, `post_spawn_maintenance` thread) — the orchestrator
    // only needs to know whether the worktree is on disk (Ok) or whether the
    // provisioner gave up entirely (Err, already combined warm+cold strings).
    match provision_result {
        Ok(_outcome) => {}
        Err(e) => {
            tracing::error!("spawn_agent_inner: provision_for_spawn failed: {}", e);
            let sink = session_lifecycle::AppSessionLifecycleSink { app };
            let _ = session_lifecycle::on_error(&sink, session_id);
            let _ = app.emit(
                "node-spawn-failed",
                crate::commands::agent::NodeSpawnFailedPayload {
                    node_id: session_id,
                    error: e.clone(),
                },
            );
            return Err(e);
        }
    }

    // Fix WSL/Windows path mismatches in the worktree's .git file — without
    // this, agent commands run inside the worktree see a broken gitlink on
    // Windows-side shells. Best-effort: a failure is logged, never fatal.
    if let Err(e) =
        crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type)
    {
        tracing::warn!(
            "spawn_agent_inner: failed to sanitize worktree .git file: {}",
            e
        );
    }

    // 8-9. Build the command, then spawn it — either normally (portable-pty)
    //       or, when the mesh opts in on Windows, inside an AppContainer sandbox
    //       (issue #498). The sandbox path owns its ConPTY spawn but returns the
    //       same `Child`/`MasterPty` trait objects, so everything downstream
    //       (Job Object containment, reader thread, resize, kill) is identical.
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
            // Verification failures are fail-closed. Schedule a runtime-specific
            // refresh so a later launch can proceed without a settings round-trip.
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

    let emit_provider_error = |message: &str| {
        let _ = app.emit(
            "provider-error",
            ProviderErrorPayload {
                session_id,
                provider,
                message: message.to_string(),
            },
        );
    };

    // Trust is a launch prerequisite, independent of attention hooks. The
    // prepared routing carries the exact runtime identity for Codex proxy
    // launches so trust and child execution use one WSL distro/home. Both
    // trust and hook provisioning are blocking filesystem/process work, so
    // keep their ordering while moving them off the Tokio worker thread.
    let launch_runtime = routing.launch_runtime();
    let provisioning_resolved = resolved.clone();
    let provisioning_runtime = launch_runtime.clone();
    let needs_attention_hook = adapter.requires_attention_hook();
    let provisioning = crate::commands::run_blocking("provider_provisioning", move || {
        Ok(run_provider_provisioning(
            || adapter.ensure_workspace_trusted(&provisioning_resolved, &provisioning_runtime),
            || adapter.provision_attention_hooks(&provisioning_resolved, &provisioning_runtime),
            needs_attention_hook,
        ))
    })
    .await;
    match provisioning {
        Ok((trust, hooks)) => {
            if let Err(e) = trust {
                tracing::warn!(
                    "spawn_agent_inner: workspace trust provisioning failed for session {}: {}",
                    session_id,
                    e
                );
                emit_provider_error(&format!("workspace trust unavailable: {e}"));
            }
            if let Err(e) = hooks {
                tracing::warn!(
                    "spawn_agent_inner: attention hook provisioning failed for session {}: {}",
                    session_id,
                    e
                );
                emit_provider_error(&format!("attention hooks unavailable: {e}"));
            }
        }
        Err(error) => {
            tracing::warn!(
                "spawn_agent_inner: provider provisioning task failed for session {}: {}",
                session_id,
                error
            );
            emit_provider_error(&format!("provider provisioning unavailable: {error}"));
        }
    }
    timer.checkpoint("after_workspace_trust");

    // Resolve configuration values through the per-field cascade (issue
    // #1149 prefactor; #1150 fills the application slot; #1151 fills the
    // per-Mesh override slot). The resolver applies the capability mask,
    // so `build_spawn_command` receives values the harness actually accepts
    // — unsupported values never reach the harness process regardless of
    // which layer supplied them. The application slot reads the latest
    // in-process preferences cache (no disk read on the spawn hot path);
    // the validator already removed any value the harness couldn't accept
    // at save time, so the resolver's mask here is the second-and-final gate.
    //
    // `node.provider` for a Proxied Provider row is the composite id
    // `"<harness>:<provider>"` (e.g. `"claude:minimax"`, `"codex:minimax"`).
    // The per-Mesh override map and the application-defaults map are both
    // keyed by the harness *profile* id (the half before the first `:`),
    // so a raw lookup would miss every Proxied spawn — failing AC #12
    // ("Native and Proxied Provider Spawn Options consume the same
    // application-default layer"). Split the composite id through
    // `parse_spawn_option_id` before both lookups so native and Proxied
    // rows hit the same map key.
    let (harness_id_for_default, _) = crate::agent::provider::parse_spawn_option_id(&node.provider);
    let mesh_override = crate::db::get_mesh_harness_overrides(node.mesh_id)
        .ok()
        .flatten()
        .and_then(|m| m.get(harness_id_for_default).cloned());
    let app_default = match crate::preferences::load() {
        Ok(prefs) => crate::preferences::harness_default_for(&prefs, harness_id_for_default),
        Err(e) => {
            tracing::warn!(
                "spawn_agent_inner: harness-default load failed, treating as absent: {e}"
            );
            None
        }
    };
    let resolved_config = resolve_spawn_config(
        provider,
        explicit_model.as_deref(),
        explicit_effort.as_deref(),
        // Cascade layer-1 verbatim CLI flags from the v2 SpawnAgentNode
        // explicit slot (issue #1358). The resolver capability-masks
        // this against `HarnessCapabilities.supports_extra_args` —
        // Terminal drops it; every interactive harness keeps it. The
        // `non_empty_trim` collapse happens inside `resolve_agent_config`
        // / `resolve_extra_args` so whitespace-only inputs cascade-fall.
        explicit_extra_args.as_deref(),
        // Legacy `meshes.model` / `meshes.effort` columns are physically
        // present for positional row compatibility but are no longer
        // read as active spawn configuration — the v33 one-shot
        // migration copied any non-empty legacy values into the
        // `claude` override entry of the new map (issue #1151 acceptance
        // criteria 6). On a healthy v33+ DB this slot is always `None`.
        app_default.as_ref(),
        mesh_override.as_ref(),
    );
    let cmd = build_spawn_command_prepared(
        &resolved,
        provider,
        &routing,
        &session_id_mode,
        session_id,
        &resolved_config,
        prefill.as_deref(),
        sandbox,
    );

    let (child, master): (
        Box<dyn portable_pty::Child + Send + Sync>,
        Box<dyn portable_pty::MasterPty + Send>,
    ) = if crate::sandbox::sandbox_enabled(sandbox) {
        tracing::info!(
            "spawn_agent_inner: spawning session {} inside AppContainer sandbox",
            session_id
        );
        sandbox_spawn(&cmd, session_id, &resolved.host_path, rows, cols)
            .inspect_err(|e| emit_provider_error(e))?
    } else {
        let pair = open_pty_pair(rows, cols)?;
        let child = spawn_child(&pair, cmd).inspect_err(|e| emit_provider_error(e))?;
        (child, pair.master)
    };

    tracing::info!("spawn_agent_inner: process spawned successfully");
    timer.checkpoint("after_pty_spawn");

    // Contain the whole process tree in a Job Object straight away, before the
    // shell launches the agent CLI — so any process the agent later detaches
    // (e.g. a dev server it backgrounds) is still killed on close, even when its
    // parent has exited and `taskkill /T` could no longer reach it.
    let job = child
        .process_id()
        .and_then(crate::process_util::JobHandle::contain);
    if job.is_none() {
        tracing::warn!(
            "spawn_agent_inner: could not contain session {} in a Job Object; \
             close will fall back to taskkill (detached children may survive)",
            session_id
        );
    }

    // 10. Setup IO
    let reader = master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = master.take_writer().map_err(|e| e.to_string())?;
    let reader_alive = Arc::new(AtomicBool::new(true));

    // 11. Register BEFORE starting the reader thread. The pre-#300 order
    //     (register-then-start) is the one that closes the TOCTOU window
    //     in `is_agent_already_running`: a concurrent spawn for the
    //     same session_id sees the entry and bails. The `reader_handle`
    //     is stashed via a setter after the thread is spawned — the
    //     tiny window between insert and setter is benign (kill_session
    //     arriving then sees `reader_handle = None` and skips the join,
    //     matching the natural-exit test path).
    tracing::info!(
        "spawn_agent_inner: storing agent process for session {}",
        session_id
    );
    // Slug adoption (`name` + `worktree_name`) is NOT done here. It belongs to
    // the provisioner, which applies it via `ProvisionSink::adopt_manual_slug`
    // before this point — see `git::worktree::provision`.
    //
    // This is where a compensating `set_agent_node_worktree_name` used to
    // live. #1057 moved the claim into `SpawnContext` via
    // `warm_claimed.take()` a few hundred lines above, which silently made
    // this block's `Some(entry)` guard unmatchable — `None` is a perfectly
    // valid value to pattern-test, so nothing failed to compile and no test
    // covered it. The row kept its stage-1 throwaway slug, and every close
    // then queued a directory that had never existed (#1080). Do not
    // reintroduce a second adoption site here: one owner, in the provisioner.
    // One flag instance shared three ways: the registry entry (kill_session
    // sets it), the reader thread (its epilogue reads it), and nothing else.
    let deliberate_kill = Arc::new(AtomicBool::new(false));
    register_agent(
        session_id,
        child,
        writer,
        master,
        reader_alive.clone(),
        job,
        timer.start(),
        mesh_id,
        deliberate_kill.clone(),
    );
    tracing::info!("spawn_agent_inner: stored agent process");

    // 13. Start reader thread
    let spawned_at = std::time::Instant::now();
    // `spawn_start` is the original SpawnTimer reference, used by the
    // reader-thread `first_pty_output` checkpoint log for timeline
    // alignment with every other `spawn_timing:` line. Distinct from
    // `spawned_at` (process-creation time) which the early-exit
    // heuristic needs — see `start_reader` doc comment.
    let spawn_start = timer.start();
    tracing::debug!(
        "spawn_agent_inner: starting reader thread for session {}",
        session_id
    );
    crate::http_server::ensure_pty_channel(session_id);
    // Issue #651: derive the reader-capture gate from `session_id_mode`
    // (the orchestrator's authoritative decision) rather than from
    // `adapter.self_assigns_session_id() && node.cli_session_id.is_none()`
    // (a derived condition that could drift if a future adapter violates the
    // "Assign => !self_assigns" invariant). The two writes — orchestrator
    // pre-write at step 4 and reader capture at `start_reader` — are
    // unsynchronised; only one path must own the column for any given spawn.
    let needs_session_capture =
        reader_should_capture_session_id(&session_id_mode, adapter.captures_session_id_from_pty());
    let reader_handle = start_reader(
        app.clone(),
        session_id,
        needs_session_capture,
        reader,
        spawned_at,
        reader_alive,
        adapter.is_plain_terminal(),
        spawn_start,
        mesh_id,
        deliberate_kill,
    );

    // 13b. Start natural-exit watcher (issue #287). On Windows ConPTY
    //      10.0.28120 the master read pipe no longer EOFs on child
    //      exit, so the reader thread stays blocked in `read()` until
    //      the pseudoconsole itself is closed. This poller drops the
    //      master within ~500ms of the child exiting, EOFing the
    //      reader, which then sets `reader_alive = false` and flips
    //      the node status to `Idle`. The watcher uses `try_wait` +
    //      `try_lock` on the child so it never blocks kill_session
    //      (which also locks that mutex).
    if let Some(entry) = PROCESS_REGISTRY.get(&session_id) {
        crate::agent::process::watch_child_exit(entry.child.clone(), entry.master.clone());
    }

    // 14. Stash the JoinHandle on the registered entry. `kill_session`
    //     reads it under a Mutex so the concurrent kill_session path
    //     is race-free (see `process.rs::kill_session`).
    if let Some(entry) = PROCESS_REGISTRY.get(&session_id) {
        entry.set_reader_handle(reader_handle);
    }

    if matches!(session_id_mode, SessionIdMode::None) {
        adapter.after_fresh_spawn(session_id, &resolved.spawn_path, resolved.env_type);
    }

    tracing::info!("spawn_agent_inner: reader thread spawned, updating node status");
    // Issue #654 — close the post-spawn status + early-exit race. The
    // `NOT IN (Error, Archived)` guard is the symmetric race fix: prevents
    // the orchestrator from resurrecting a reader-written Error back to
    // Spawning (which would let the delayed promotion later write Running
    // onto a dead node — same ghost-Running bug, other direction). Routes
    // through SessionLifecycle (issue #132) so the `unless_in` predicate
    // lives in one place.
    let sink = session_lifecycle::AppSessionLifecycleSink { app };
    session_lifecycle::on_spawn_started(&sink, session_id).map_err(|e| e.to_string())?;
    let app_for_promotion = app.clone();
    std::thread::spawn(move || {
        // Promote to Running iff the reader hasn't already written Error.
        // Both delay and reader check must share `EARLY_EXIT_WINDOW`.
        std::thread::sleep(EARLY_EXIT_WINDOW);
        let promotion_sink = session_lifecycle::AppSessionLifecycleSink {
            app: &app_for_promotion,
        };
        if let Err(e) = session_lifecycle::on_spawn_complete(&promotion_sink, session_id) {
            tracing::warn!(
                "spawn_agent_inner: conditional Running promotion failed for session {}: {}",
                session_id,
                e
            );
        }
    });

    // Warm-pool post-claim housekeeping (issue #609) and the post-spawn
    // maintenance task (issue #613) live inside `provision_for_spawn` now
    // — the provisioner owns the warm-failure cold fallback, the warm-row
    // `forget_after_spawn`, the Manual name adoption (DB write +
    // `node-renamed` event), and the single thread that runs refresh +
    // refill under one fill-lock acquisition. This orchestrator just gets
    // back the final `ProvisionOutcome`; see `git::worktree::provision`
    // for the seam contract.

    // Emit the post-spawn reconcile trigger (issue #332). Async-spawn paths
    // (auto-resume on startup, fresh auto-spawn, handover, etc.) race the
    // frontend's attach-fit: term.onResize fires `resize_agent(real cols)`
    // before the agent process exists, so the IPC returns "Agent not
    // running" and is silently swallowed. The PTY was created at the
    // caller-supplied `rows`/`cols` (80x24 for auto_resume_sessions), and
    // because term.cols is already the fitted value no further onResize
    // fires — the PTY stays at the spawn-time size and the agent wraps
    // its first lines of output inside a wider pane. By emitting here
    // (after the agent is registered AND the DB status flips to
    // `Spawning` — the transient state between process launch and the
    // conditional `Spawning → Running` promotion 3s later; issue #654),
    // we give the frontend a definitive "agent is up, push the real
    // size now" signal that closes the race uniformly for all three
    // paths. Frontend consumer: TerminalRegistry listens and calls
    // syncPtySize, which is self-guarding (no-op on detached/missing
    // terminals) and swallows the "Agent not running" rejection.
    let _ = app.emit(
        "agent-spawned",
        AgentSpawnedPayload {
            session_id,
            rows: rows as i32,
            cols: cols as i32,
        },
    );

    tracing::info!("spawn_agent_inner: complete");
    timer.total();
    Ok(())
}
