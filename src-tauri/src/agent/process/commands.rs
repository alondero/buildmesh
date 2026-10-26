//! Live-process commands and submission/interrupt orchestration; blocking seams stay explicit.

use super::*;
// ---------------------------------------------------------------------------
// Tauri command surface — process lifecycle home
//
// `commands::agent::kill_agent / resize_agent / write_to_agent / send_to_agent /
//  is_agent_running / debug_list_agents / debug_crash_snapshot` moved here as
// part of issue #1052 (extract process-lifecycle home) and are now
// `agent::process::kill_agent` etc. The deep module owns
// the full surface so a Tauri command-registration update in `lib.rs` is the
// only seam changes the move requires. `commands::agent::spawn_agent`,
// `create_issue_node`, `create_pr_node`, `spawn_issue_agent`,
// `spawn_handover_agent`, `auto_resume_agent_nodes`
// stay in `commands::agent` — those are spawn orchestration, the legitimate
// role of the commands layer.
//
// Sync cores are `pub(crate)` so production call sites in other modules
// (`services/agent_node.rs:580`, `agent/spawn.rs:1314`) and the test handle in
// `commands/test.rs:367` can reach them via `agent::process::kill_agent`
// directly — the dependency arrow now points at the deep module rather than
// the transport-named commands layer.
// ---------------------------------------------------------------------------

/// Kill all running agent processes. Used during graceful shutdown.
/// Renamed from `kill_all_agents` for naming consistency with the existing
/// `kill_session` (registry-level primitive) — the rename lands in the same
/// PR so callers and the lib.rs shutdown handler update at once.
pub fn kill_all_sessions() {
    for id in PROCESS_REGISTRY.session_ids() {
        PROCESS_REGISTRY.kill_session(id);
        crate::http_server::clear_scrollback(id);
        tracing::info!("kill_all_sessions: killed agent for session {}", id);
    }
}

#[command]
pub async fn resize_agent(session_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    PROCESS_REGISTRY.resize_pty(session_id, cols, rows)
}

#[command]
pub async fn write_to_agent(
    app: AppHandle,
    session_id: i64,
    data: String,
) -> Result<InputOutcome, String> {
    // Issue #1122 (progressive text entry latency).
    //
    // The previous implementation offloaded the entire body — PTY write
    // + disarm + DB read + DB write — onto `spawn_blocking` for every
    // keystroke. The blocking pool is shared with git/diff/GitHub probes;
    // at peak, a single queued probe could park 10-50ms of keystroke
    // latency, and the symptom surfaced as the whole TUI feeling sluggish
    // after a long session.
    //
    // Split the body into a fast path and a slow path instead:
    //
    // 1. Fast path: PTY write + autoclear disarm. Both are short,
    //    non-blocking (Mutex<Box<dyn Write>> + in-memory map lookup) and
    //    never hold a lock across an `.await`, so they go on the async
    //    runtime. Every keystroke — letters, arrows, backspace, paste —
    //    takes this path.
    // 2. Slow path: only when the streaming decoder observes a submission
    //    outside bracketed paste. Terminal replies and pasted newlines cannot
    //    clear attention. Then we ask the DB whether the node is a plain
    //    shell (no LLM attention to clear) and, if not, record the
    //    attention-cleared transition. The DB work goes through
    //    `spawn_blocking` because it's IO-bound and the convention
    //    (added by #761) keeps `reqwest`/rusqlite off the async worker
    //    pool.
    //
    // The PTY write happens *first* even on the slow path — a failed
    // write must not claim "user input accepted", otherwise the
    // post-exit detector would see no signal for the dead child and the
    // status flip would land on a session that never received the byte.
    //
    // The typed outcome is returned to the caller rather than flattened to
    // `Ok(())` (issue #1530): a `Backpressured` result is the client's cue to
    // hold the exact bytes in its ordered retry buffer instead of believing
    // the prompt was delivered.
    let outcome = PROCESS_REGISTRY.write_input(session_id, data.as_bytes())?;
    // Any accepted keystroke means the user is engaged with this node —
    // the stale-mark hypothesis behind auto-clear (issue #878) no longer
    // holds, and the keystroke's own echo must not count toward the
    // resume burst. A refused write engaged nobody, so it must not disarm.
    if outcome.activity.user_input {
        crate::attention_autoclear::disarm(session_id);
    }
    if outcome.is_accepted() {
        watch_for_interrupt(session_id, data.as_bytes());
    }
    if !outcome.is_accepted() || !outcome.activity.submitted {
        return Ok(outcome);
    }
    let published = crate::commands::run_blocking("write_to_agent_signal", move || {
        write_to_agent_signal_blocking(session_id)
    })
    .await?;
    if let Some(payload) = published {
        let sink = session_lifecycle::AppSessionLifecycleSink { app: &app };
        publish_submitted_input(&sink, payload);
    }
    Ok(outcome)
}

/// Publish a resume envelope that was already committed on the blocking pool.
///
/// The legacy `attention-cleared` event does not carry a status. Desktop
/// ignores it for the badge, so a node whose list snapshot was still
/// `spawning` stays on "Starting…" after the user sends a prompt unless this
/// also publishes `agent-lifecycle`.
pub(crate) fn publish_submitted_input(
    sink: &dyn crate::agent::session_lifecycle::SessionLifecycleSink,
    payload: crate::agent::session_lifecycle::LifecycleChangedPayload,
) {
    let session_id = payload.session_id;
    sink.emit_lifecycle_changed(payload);
    sink.emit_attention_cleared(session_id);
}

/// Commit a submitted keystroke's resume on the blocking pool.
///
/// `session_lifecycle::on_user_submitted` decides whether the submission
/// resumes anything (a plain shell's Enter does not — issue #535). Returns the
/// stored envelope so the async caller can publish it. `None` means the
/// keystroke was shell input, or the row rejected the transition. A commit
/// error is logged and treated as "nothing to publish": the PTY bytes were
/// already accepted.
pub(crate) fn write_to_agent_signal_blocking(
    session_id: i64,
) -> Result<Option<crate::agent::session_lifecycle::LifecycleChangedPayload>, String> {
    match commit_submitted_input(&crate::agent::session_lifecycle::DbOnlySink, session_id) {
        Ok(payload) => Ok(payload),
        Err(error) => {
            tracing::warn!(session_id, %error, "submitted input could not commit a lifecycle");
            Ok(None)
        }
    }
}

/// Commit a submitted keystroke as resumed work — the desktop and mobile
/// input paths share it — and, when the node was idling at its prompt under
/// a harness that reports a prompt starting a turn, check after
/// [`SUBMIT_CONFIRM_WINDOW`](crate::agent::session_lifecycle::SUBMIT_CONFIRM_WINDOW)
/// that the harness confirmed it. An unconfirmed Enter started no turn.
pub(crate) fn commit_submitted_input(
    sink: &dyn crate::agent::session_lifecycle::SessionLifecycleSink,
    session_id: i64,
) -> Result<Option<crate::agent::session_lifecycle::LifecycleChangedPayload>, String> {
    use crate::agent::session_lifecycle as lifecycle;
    if sink.is_plain_terminal(session_id) {
        return Ok(None);
    }
    // One row read answers both questions about this node's harness, and both
    // collapse to plain values so the confirmation thread below carries no DB
    // handle across its 30-second sleep.
    let (awaits_confirmation, replays_submit) = match db::get_agent_node_by_id(session_id) {
        Ok(node) => (
            lifecycle::submit_needs_confirmation(
                node.status,
                harness_confirms_prompt_submission(&node.provider),
            ),
            harness_replays_unconfirmed_submit(&node.provider),
        ),
        Err(_) => (false, false),
    };
    let committed = lifecycle::on_user_submitted(sink, session_id)?;
    if let Some(submitted) = committed.clone().filter(|_| awaits_confirmation) {
        std::thread::spawn(move || {
            std::thread::sleep(lifecycle::SUBMIT_CONFIRM_WINDOW);
            let reverted = match crate::http::state::app_handle() {
                Some(app) => lifecycle::on_submit_unconfirmed(
                    &lifecycle::AppSessionLifecycleSink { app },
                    &submitted,
                ),
                None => lifecycle::on_submit_unconfirmed(&lifecycle::DbOnlySink, &submitted),
            };
            match reverted {
                Ok(true) => {
                    tracing::info!(
                        session_id,
                        "submitted input was not confirmed by the harness; node is back at its prompt"
                    );
                    replay_dropped_submit(session_id, replays_submit);
                }
                Ok(false) => {
                    // Some observation won the race, so the prompt was accepted
                    // after all. Hand the next submit its own replay rather than
                    // leaving this incarnation with none (issue #2344).
                    if replays_submit {
                        if let Some(agent) = PROCESS_REGISTRY.get(&session_id) {
                            agent.rearm_submit_replay();
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(session_id, %error, "unconfirmed-submit revert failed")
                }
            }
        });
    }
    Ok(committed)
}

/// Re-send the prompt behind a submit the harness never acknowledged
/// (issue #2344).
///
/// Runs only after the revert committed, so the node is sitting at an empty
/// prompt and the TUI — for OpenCode, long since attached its input reader —
/// can accept what the first send lost. Failure is logged and dropped: the
/// bytes are gone, the user is back at their prompt, and the status has
/// already been corrected.
pub(super) fn replay_dropped_submit(session_id: i64, harness_opted_in: bool) {
    let Some(agent) = PROCESS_REGISTRY.get(&session_id) else {
        tracing::info!(
            session_id,
            "agent left the registry before its prompt could be re-sent"
        );
        return;
    };
    match agent.replay_unconfirmed_submit(harness_opted_in) {
        SubmitReplayOutcome::Sent => {
            tracing::info!(
                session_id,
                "re-sent a prompt the harness had dropped before its TUI was reading"
            );
            // A re-send is submitted work, not a silent repair. Publish it the
            // same way the first submit was published, so the UI leaves Ready
            // at the moment the TUI starts running instead of sitting on an
            // idle-looking node until OpenCode's own hook arrives. Re-arming
            // the confirmation window also means a replay that goes nowhere is
            // caught: the next revert returns the node to Ready, and the
            // already-spent one-shot keeps it from replaying again.
            if let Err(error) = match crate::http::state::app_handle() {
                Some(app) => commit_submitted_input(
                    &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
                    session_id,
                ),
                None => {
                    commit_submitted_input(&crate::agent::session_lifecycle::DbOnlySink, session_id)
                }
            } {
                tracing::warn!(
                    session_id,
                    %error,
                    "re-sent prompt left the node Ready: resumed work could not be published"
                );
            }
        }
        SubmitReplayOutcome::Skipped(skip) => {
            tracing::info!(session_id, ?skip, "no prompt to re-send")
        }
        SubmitReplayOutcome::Refused(error) => {
            tracing::warn!(session_id, %error, "the PTY refused the re-sent prompt")
        }
    }
}

/// The bytes a terminal sends for the interrupt keys: Esc, and Ctrl+C.
pub(super) fn is_interrupt_key(data: &[u8]) -> bool {
    matches!(data, [0x1b] | [0x03])
}

/// What one look at an interrupted node found.
#[derive(Clone, Copy, Debug)]
pub(super) enum InterruptProbe {
    /// The harness recorded the interruption.
    Recorded,
    /// Not recorded yet; look again.
    NotYet,
    /// The node left the observed `Running` revision by another path.
    Moved,
}

/// Look up to `attempts` times, `poll` apart, until the interruption is
/// recorded (`true`) or the node moves on (`false`).
pub(super) fn await_interrupt_record(
    attempts: u32,
    poll: std::time::Duration,
    mut probe: impl FnMut() -> InterruptProbe,
) -> bool {
    for _ in 0..attempts {
        std::thread::sleep(poll);
        match probe() {
            InterruptProbe::Recorded => return true,
            InterruptProbe::Moved => return false,
            InterruptProbe::NotYet => {}
        }
    }
    false
}

/// An interrupt key reached a node. Claude Code ends an interrupted turn
/// without any hook, so after the harness has had time to record the
/// interruption, a node still `Running` whose transcript shows it is settled
/// back to `Ready`. Anything else — no transcript evidence, a newer
/// observation, a harness that reports interrupts natively — leaves it alone.
pub(crate) fn watch_for_interrupt(session_id: i64, data: &[u8]) {
    if !is_interrupt_key(data) {
        return;
    }
    let pressed_at = std::time::SystemTime::now();
    std::thread::spawn(move || {
        use crate::agent::session_lifecycle as lifecycle;
        let Ok((crate::models::SessionStatus::Running, observed)) =
            db::agent_node::agent_status_revision(session_id)
        else {
            return;
        };
        let recorded = await_interrupt_record(
            lifecycle::INTERRUPT_ATTEMPTS,
            lifecycle::INTERRUPT_POLL,
            || {
                match db::agent_node::agent_status_revision(session_id) {
                    Ok((crate::models::SessionStatus::Running, current)) if current == observed => {
                    }
                    _ => return InterruptProbe::Moved,
                }
                match db::get_agent_node_by_id(session_id) {
                    Ok(node)
                        if crate::coordinator::enrichment::interrupted_turn(&node, pressed_at) =>
                    {
                        InterruptProbe::Recorded
                    }
                    Ok(_) => InterruptProbe::NotYet,
                    Err(_) => InterruptProbe::Moved,
                }
            },
        );
        if !recorded {
            return;
        }
        let settled = match crate::http::state::app_handle() {
            Some(app) => lifecycle::on_turn_interrupted(
                &lifecycle::AppSessionLifecycleSink { app },
                session_id,
                &observed,
            ),
            None => lifecycle::on_turn_interrupted(&lifecycle::DbOnlySink, session_id, &observed),
        };
        match settled {
            Ok(true) => tracing::info!(
                session_id,
                "interrupted turn settled; node is back at its prompt"
            ),
            Ok(false) => {}
            Err(error) => tracing::warn!(session_id, %error, "interrupted-turn settle failed"),
        }
    });
}

/// Whether the node's harness reports a prompt that starts a turn.
pub(in crate::agent) fn harness_confirms_prompt_submission(provider: &str) -> bool {
    crate::preferences::resolve_harness_provider(provider)
        .adapter()
        .confirms_prompt_submission()
}

/// Whether the node's harness re-sends a prompt it never acknowledged
/// (issue #2344).
pub(in crate::agent) fn harness_replays_unconfirmed_submit(provider: &str) -> bool {
    crate::preferences::resolve_harness_provider(provider)
        .adapter()
        .replays_unconfirmed_submit()
}



#[command]
pub async fn send_to_agent(
    app: AppHandle,
    session_id: i64,
    input: String,
) -> Result<InputOutcome, String> {
    write_to_agent(app, session_id, format!("{}\n", input)).await
}

/// Reject a handover that carries nothing. Split from [`handover_to_agent`]
/// because the command takes an `AppHandle` only the real app can construct,
/// and this is the half with a user-facing rule in it.
pub(super) fn validate_handover_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Nothing to hand over — select text in the source terminal first".to_string());
    }
    Ok(())
}

pub(super) fn prepare_existing_node_handover(
    target_node_id: i64,
    text: &str,
    app_data_dir: Option<&std::path::Path>,
) -> Result<crate::agent::handover::PreparedHandover, String> {
    validate_handover_text(text)?;
    let node = db::get_agent_node_by_id(target_node_id).map_err(|error| error.to_string())?;
    let provider = crate::agent::provider::resolve_node_provider(&node);
    crate::agent::handover::prepare(&node, provider, text, app_data_dir)
}

pub(super) fn deliver_prepared_handover(
    prepared: crate::agent::handover::PreparedHandover,
    deliver: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    match deliver(prepared.prompt()) {
        Ok(()) => Ok(()),
        Err(error) => {
            prepared.discard();
            Err(error)
        }
    }
}

/// Hand the text selected in one node's terminal over to another **running**
/// node: stage it in the target's PTY input box and submit it. The
/// existing-node sibling of `commands::agent::spawn_handover_agent`, and the
/// backing command for the terminal context menu's "Handover to node" rows.
///
/// Delegates to [`crate::circuit::delivery::write_prompt_to_pty`] instead of
/// writing the bytes here, because the submit is the hard part: a multi-line
/// buffer has to arrive as ONE bracketed paste and the Enter has to land as its
/// own write at an idle input box, or an ink TUI swallows the keystroke into
/// the paste and the prompt sits staged forever (issue #874). That primitive
/// also settles before the Enter when the target is one the turn evaluator does
/// *not* buffer — the ordinary case here, since the agent a human hands work to
/// is one the user spawned rather than one Autopilot owns.
#[command]
pub async fn handover_to_agent(
    app: AppHandle,
    target_node_id: i64,
    text: String,
) -> Result<(), String> {
    let app_data_dir = crate::preferences::app_data_dir();
    crate::commands::run_blocking("handover_to_agent", move || {
        let prepared =
            prepare_existing_node_handover(target_node_id, &text, app_data_dir.as_deref())?;
        deliver_prepared_handover(prepared, |prompt| {
            crate::circuit::delivery::write_prompt_to_pty(target_node_id, prompt, &app)
        })
    })
    .await
}

#[command]
pub async fn kill_agent(session_id: i64) -> Result<(), String> {
    // Offload to the blocking pool: `kill_session` shells out to
    // `taskkill /F /T` on Windows (a synchronous `.output()` wait) and then
    // joins the PTY reader thread with a bounded 2s timeout — up to several
    // seconds parked on a Tauri async worker per call. This command runs on
    // every node close AND at step 2 of every spawn (`spawn_agent_inner`
    // kills stale processes first), so leaving it inline is exactly the
    // pool-starvation class from the Command Threading convention.
    crate::commands::run_blocking("kill_agent", move || kill_agent_blocking(session_id)).await
}

/// Sync core for [`kill_agent`] — see the `*_blocking` + `run_blocking`
/// convention in `commands/mod.rs`.
pub(crate) fn kill_agent_blocking(session_id: i64) -> Result<(), String> {
    crate::session_naming::reset_buffers(session_id);
    crate::agent::provider::notify_process_terminated(session_id);
    PROCESS_REGISTRY.kill_session(session_id);
    crate::http_server::clear_scrollback(session_id);
    // Routes through SessionLifecycle (issue #132). `DbOnlySink` because
    // the blocking core has no `AppHandle` — kill is silent on the
    // attention channel anyway (no `attention-cleared` emit here).
    session_lifecycle::on_idle(&session_lifecycle::DbOnlySink, session_id)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn is_agent_running(session_id: i64) -> bool {
    PROCESS_REGISTRY.is_alive(&session_id)
}
