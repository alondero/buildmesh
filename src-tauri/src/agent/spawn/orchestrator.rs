//! High-level spawn pipeline: intent dispatch plus the four phase calls.
//!
//! `spawn_with_intent` is the public seam and the sole owner of
//! `node-spawn-failed` / session-lifecycle Error (so Resume can call
//! `on_resume_failed`). `spawn_with_intent` acquires the in-flight claim,
//! then coordinates prepare → provision → launch → streams. Prepare
//! returns workspace params and launch params separately; provision
//! never sees PTY size or cascade overrides. Command construction,
//! process sandboxing, and attention-hook writes stay in their
//! dedicated modules.

use super::launch::launch_process;
use super::prepare::{prepare_context, PrepareOutcome, SpawnInFlightClaim};
use super::process::is_agent_already_running;
use super::provision::provision_workspace;
use super::reader::SpawnTimer;
use super::streams::start_streams;
use super::{ExplicitSpawnOverrides, ResumeCause, SpawnIntent, SpawnOutcome, SpawnRequest};
use crate::agent::session_lifecycle;
use crate::db;
use tauri::Emitter;

pub(crate) use super::prepare::SpawnOptions;

/// Pure decision for "given the stored CLI session id, the resume cause,
/// and whether the adapter auto-resumes on startup, what should
/// `spawn_with_intent` do?". The Skip variants are the regression-pin
/// for issue #949: a future refactor that re-introduces an `on_idle`
/// call inside the Skip arms fails review by virtue of the decision
/// being a single enum variant.
///
/// Startup discovery includes missing identities for recovery. If recovery
/// cannot find one, keep the node Suspended for explicit Regenerate.
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
        worktree_policy,
    } = request;
    // Bind the type name so the `ExplicitSpawnOverrides` re-export stays
    // live at the module scope (the destructure pattern alone doesn't
    // count as a use). The value flows through to `SpawnOptions` below;
    // the annotation is the only thing this line adds.
    let explicit: ExplicitSpawnOverrides = explicit;
    // Claim before reading/clearing identity: a duplicate Fresh request must
    // not erase the winner's identity or move its recovery timestamp.
    let Some(claim) = SpawnInFlightClaim::try_claim(node_id) else {
        let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
        return Ok(SpawnOutcome::Skipped(node));
    };
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    if matches!(intent, SpawnIntent::Resume { cause: ResumeCause::Startup })
        && node.status != crate::models::SessionStatus::Suspended
    {
        return Ok(SpawnOutcome::Skipped(node));
    }
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
        &claim,
        SpawnOptions {
            session_id: node_id,
            provider,
            resume,
            rows: terminal_size.rows,
            cols: terminal_size.cols,
            prefill,
            node: Some(node.clone()),
            // Issue #1358: per-spawn extra_args ride the explicit layer
            // through to launch, where `resolve_spawn_config`
            // capability-masks them against `HarnessCapabilities
            // .supports_extra_args` (Terminal drops; every interactive
            // harness keeps).
            explicit_extra_args: explicit.extra_args,
            // Cascade layer-1 overrides flow through verbatim. Empty /
            // whitespace-only values are normalised at `cascade_inputs_for`
            // in `command` so the cascade falls through to the next layer
            // rather than forwarding a synthetic blank arg to the harness
            // (issue #1148 AC #32 + #1155 AC #3).
            explicit_model: explicit.model,
            explicit_effort: explicit.effort,
            worktree_policy,
        },
    )
    .await;

    let outcome = match result {
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
    };
    drop(claim);
    outcome
}

/// Whether this request intentionally discards the node's prior conversation.
/// A Resume request can still launch a fresh process for a non-resumable
/// adapter, but that is not user intent to replace the captured identity.
pub(super) fn intent_replaces_conversation(intent: &SpawnIntent) -> bool {
    !matches!(intent, SpawnIntent::Resume { .. })
}

/// Transitional implementation retained while transport callers migrate to
/// [`spawn_with_intent`]. It borrows the caller's in-flight claim and sequences
/// the four spawn phases. Phase modules return `Result` and do not emit
/// `node-spawn-failed` or write session-lifecycle Error — that stays here
/// via [`spawn_with_intent`].
pub(crate) async fn spawn_agent_inner(
    app: &tauri::AppHandle,
    _claim: &SpawnInFlightClaim,
    opts: SpawnOptions,
) -> Result<(), String> {
    tracing::info!(
        "spawn_agent_inner: session_id={}, provider={:?}, resume={:?}, size={}x{}",
        opts.session_id,
        opts.provider,
        opts.resume,
        opts.cols,
        opts.rows
    );

    let timer = SpawnTimer::new(opts.session_id);

    match prepare_context(app, opts, &timer).await? {
        PrepareOutcome::Skipped => {}
        PrepareOutcome::Ready(phases) => {
            let provisioned = provision_workspace(app, phases.workspace, &timer).await?;
            let launched = launch_process(app, provisioned, phases.launch, &timer).await?;
            start_streams(app, launched, &timer).await?;
        }
    }

    tracing::info!("spawn_agent_inner: complete");
    timer.total();
    Ok(())
}
