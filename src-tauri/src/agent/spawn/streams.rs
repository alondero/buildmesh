//! Register-and-start-streams phase of Agent Node spawn.
//!
//! Inserts the process into the registry, starts the PTY reader, watches
//! natural exit, promotes Spawning → Running after the early-exit window,
//! and emits `agent-spawned`.

use super::launch::LaunchedProcess;
use super::process::register_agent;
use super::reader::{
    reader_should_capture_session_id, start_reader, SessionIdMode, SpawnTimer, EARLY_EXIT_WINDOW,
};
use super::AgentSpawnedPayload;
use crate::agent::process::PROCESS_REGISTRY;
use crate::agent::session_lifecycle;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::Emitter;

/// Register the child, start the reader thread, and emit the post-spawn
/// events that close the attach-fit race.
pub(super) async fn start_streams(
    app: &tauri::AppHandle,
    launched: LaunchedProcess,
    timer: &SpawnTimer,
) -> Result<(), String> {
    let LaunchedProcess {
        session_id,
        provider,
        rows,
        cols,
        session_id_mode,
        resolved,
        mesh_id,
        child,
        master,
        reader,
        writer,
        job,
    } = launched;
    let adapter = provider.adapter();
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
        adapter.after_fresh_spawn(session_id, &resolved.spawn_path, resolved.env_type, app);
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
    if let Err(error) = session_lifecycle::on_spawn_started(&sink, session_id) {
        adapter.on_process_terminated(session_id);
        return Err(error.to_string());
    }
    adapter.on_spawn_activated(session_id);
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

    Ok(())
}
