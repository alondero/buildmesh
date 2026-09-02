use crate::agent::session_lifecycle;
use crate::db;
use portable_pty::{native_pty_system, PtyPair, PtySize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Threshold for the PTY reader thread's early-exit heuristic (issue #654).
/// If the reader thread exits within this window the agent is flagged
/// `Error` — typically because `--resume <uuid>` failed against an expired
/// session. The orchestrator's delayed `Spawning → Running` promotion sleeps
/// just past this same window (see `start_streams`' delayed promotion) so the two
/// sites MUST stay in sync; bumping this constant without re-checking the
/// promotion delay recreates the ghost-Running race.
/// Shared by the reader thread's early-exit heuristic and the
/// orchestrator's delayed Spawning→Running promotion sleep (#654). The two
/// MUST stay in lock-step — drifting them recreates the race in either
/// direction.
pub const EARLY_EXIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// What the PTY reader thread's epilogue should do to the node's status
/// after the read loop ends. Extracted as a pure decision so the
/// deliberate-kill / early-exit / plain-terminal matrix is unit-testable
/// without a live PTY.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PostExitAction {
    /// Natural exit — flip the node to Idle.
    MarkIdle,
    /// The process died on its own within `EARLY_EXIT_WINDOW` of its
    /// creation — almost always a `--resume <uuid>` that the CLI rejected
    /// ("No conversation found…"). Mark Error and emit `resume-failed`.
    MarkErrorResumeFailed,
    /// `kill_session` tore the PTY down deliberately (node close, spawn
    /// step-2 stale kill, app shutdown). The kill initiator owns the next
    /// status; any write from the reader would race it. The pre-fix bug:
    /// a <3s-old process killed by a respawn was stamped `Error`, which
    /// then blocked the new spawn's Spawning→Running promotion (`Error`
    /// is in that write's exclusion list) — the node showed "failed to
    /// start" while the replacing agent booted fine seconds later.
    LeaveStatusAlone,
}

pub(crate) fn post_exit_action(
    is_plain_terminal: bool,
    deliberately_killed: bool,
    elapsed_since_process_creation: std::time::Duration,
) -> PostExitAction {
    if deliberately_killed {
        return PostExitAction::LeaveStatusAlone;
    }
    if is_plain_terminal {
        // A shell exiting — `exit`, window close — is a normal Idle,
        // never an Error: a shell is not a --resume, so a fast exit
        // isn't a resume-failure signal.
        return PostExitAction::MarkIdle;
    }
    if elapsed_since_process_creation < EARLY_EXIT_WINDOW {
        PostExitAction::MarkErrorResumeFailed
    } else {
        PostExitAction::MarkIdle
    }
}

/// Per-spawn timing log. Records elapsed milliseconds at each
/// `checkpoint(name)` call and at the end via `total()`. Output goes to
/// `buildmesh.log` via the existing `tracing` setup — no extra plumbing.
///
/// Born of the spawn-latency investigation (5-10s lag between clicking
/// "Spawn" and visible UI feedback). The checkpoints proved the bottleneck
/// was NOT the hypothesised `git::sync::fetch_origin` (network) but
/// `worktree_create` — 97% of which was libgit2's checkout. That checkout
/// now shells out to `git worktree add` (~20× faster; ADR 0007 amendment),
/// so a fresh node is usable in ~2s instead of ~14s. The timer is kept as a
/// cheap spawn-latency regression guard; its only consumer is the `tracing`
/// log file.
pub(super) struct SpawnTimer {
    start: std::time::Instant,
    session_id: i64,
}

impl SpawnTimer {
    pub(super) fn new(session_id: i64) -> Self {
        Self {
            start: std::time::Instant::now(),
            session_id,
        }
    }

    pub(super) fn checkpoint(&self, name: &str) {
        tracing::info!(
            "spawn_timing: session={} checkpoint={} elapsed={}ms",
            self.session_id,
            name,
            self.start.elapsed().as_millis()
        );
    }

    pub(super) fn total(&self) {
        tracing::info!(
            "spawn_timing: session={} TOTAL elapsed={}ms",
            self.session_id,
            self.start.elapsed().as_millis()
        );
    }

    /// Original start instant — exposed `pub(crate)` so `register_agent`
    /// can clone it onto `AgentProcess.spawn_start`, giving the
    /// `first_user_input` log line the same reference as every other
    /// `spawn_timing:` checkpoint.
    pub(crate) fn start(&self) -> std::time::Instant {
        self.start
    }
}

/// Open a PTY pair using the native PTY system.
pub fn open_pty_pair(rows: u16, cols: u16) -> Result<PtyPair, String> {
    let pty_system = native_pty_system();
    pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open PTY: {}", e))
}

/// Session ID mode: either assign a new ID or resume an existing one.
pub enum SessionIdMode {
    Assign(String),
    Resume(String),
    None,
}

/// Whether the PTY reader thread should attempt to capture a session ID
/// from live PTY output (issue #651).
///
/// Two independent code paths target the same `agent_nodes.cli_session_id`
/// column: prepare's pre-write in `prepare_context` (Assign
/// mode) and the reader thread's `session_capture::try_extract_session_id`
/// match. They are unsynchronised, so a last-writer-wins race leaves the DB
/// holding either the orchestrator's UUID or a regex match — and on
/// auto-resume `claude --resume <wrong-uuid>` → "Conversation not found".
///
/// This predicate is the single source of truth for which path is allowed to
/// write for a given spawn:
///
/// * `Assign(_)` — orchestrator is authoritative; the reader MUST NOT
///   capture (the orchestrator just wrote the UUID that the agent was
///   launched with via `--session-id <uuid>`).
/// * `Resume(_)` — the resume arg is authoritative; the DB column already
///   holds the same ID from a prior spawn. A reader capture would race
///   `claude --resume <id>` with a possibly-different UUID.
/// * `None` — orchestrator did not pre-write (Codex / Agy self-assign
///   internally). Capture is allowed only if the provider's adapter
///   declares `captures_session_id_from_pty() = true`; otherwise any UUID
///   match would be spurious noise (OpenCode captures via `after_fresh_spawn`).
pub(super) fn reader_should_capture_session_id(
    session_id_mode: &SessionIdMode,
    pty_capture: bool,
) -> bool {
    pty_capture && matches!(session_id_mode, SessionIdMode::None)
}

pub fn pump_pty_output(mut reader: Box<dyn std::io::Read + Send>, mut on_chunk: impl FnMut(&[u8])) {
    let mut buf = [0u8; crate::pty::batch::PTY_READ_BUF];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                on_chunk(&buf[..n]);
            }
            Err(e) => {
                tracing::error!("PTY read error: {}", e);
                break;
            }
        }
    }
}

/// Buffer a PTY chunk for session auto-naming — every chunk for LLM
/// providers, never for a plain terminal. A terminal's rename buffer is
/// never consumed: the rename LLM only fires from `on_turn`, which only
/// the Claude stop hook calls. Ungated, each Terminal node would retain
/// up to `MAX_BUFFER_CHARS` and contend the global NAMING mutex on every
/// chunk for the node's whole lifetime (issue #296).
///
/// Extracted from `start_reader`'s pump callback so the gate is
/// unit-testable without standing up an AppHandle / PTY (same seam
/// pattern as `resolve_base_ref_for_spawn`).
pub(crate) fn maybe_buffer_for_naming(is_plain_terminal: bool, session_id: i64, text: &str) {
    if !is_plain_terminal {
        crate::session_naming::on_output(session_id, text);
    }
}

/// Start the PTY reader thread. Returns the `JoinHandle` so the caller
/// can store it on `AgentProcess` and let `kill_session` join with a
/// bounded timeout (issue #300).
///
/// Output dispatch (issue #1385): each OS `read()` still feeds capture /
/// naming / autopilot on this thread, then the bytes go through
/// `pty::batch::with_batcher` (8 ms / 32 KiB) onto a binary Tauri Channel
/// (`OutputSink::send_owned`). Production PTY bytes never share the JSON
/// `agent-output` event — that path is test injection only. The Channel
/// is node-scoped: this reader must not unregister it on exit.
///
/// Two time references are passed in, with distinct semantics — keep
/// them separate:
///
/// * `spawned_at` — process-creation time (`Instant::now()` right after
///   `spawn_child` returns). Used by the 3-second early-exit heuristic
///   to detect a likely-failed `--resume`. **Must NOT be unified with
///   `spawn_start`**: a slow 14s spawn pipeline followed by an agent
///   dying 1s after process creation must still trigger `resume-failed`,
///   and the original "3s after process creation" semantic preserves
///   that detection.
/// * `spawn_start` — the original `SpawnTimer.start` from the top of
///   `spawn_agent_inner`. Used by the `first_pty_output` checkpoint log
///   so it lines up with every other `spawn_timing:` line (all
///   measured against the same "user clicked Spawn" instant).
#[allow(clippy::too_many_arguments)]
pub(super) fn start_reader(
    app: tauri::AppHandle,
    session_id: i64,
    needs_session_capture: bool,
    reader: Box<dyn std::io::Read + Send>,
    spawned_at: std::time::Instant,
    reader_alive: Arc<AtomicBool>,
    is_plain_terminal: bool,
    spawn_start: std::time::Instant,
    mesh_id: i64,
    deliberate_kill: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let app_clone = app;
    let reader_alive_clone = reader_alive;
    // Issue #1221: stateful wrapper that stitches PTY chunks so the
    // `session id: <uuid>` regex can match a banner that straddles an
    // 8 KiB read boundary, and so multi-byte UTF-8 sequences split
    // across reads aren't corrupted to U+FFFD before being handed to
    // `session_naming::on_output` and `autopilot::evaluator::on_output`.
    // `captured` is a plain `bool` (not `AtomicBool`) because the
    // reader thread is the only writer — the `AtomicBool` here used to
    // be load-bearing for `start_reader`'s outer scope but it's now
    // folded into the wrapper. Initialise pre-armed when the caller
    // already knows no capture is needed (e.g. providers like Anthropic
    // that we pre-assigned a UUID to).
    let mut chunk_capture = crate::session_capture::ChunkCapture::default();
    if !needs_session_capture {
        // Force the latch on so post-init feeds skip the regex.
        chunk_capture.mark_captured();
    }

    std::thread::spawn(move || {
        // The SpawnTimer in the spawn pipeline stops at process *creation*
        // (`after_pty_spawn`), so the shell → agent-CLI boot tail is invisible
        // to it. Log the gap from spawn to the first byte of PTY output here —
        // that first byte is the earliest signal the agent process is actually
        // alive and producing a UI. Same `spawn_timing:` prefix so it sits
        // alongside the other checkpoints. Measured against `spawn_start` (not
        // `spawned_at`) so this elapsed time is comparable to every other
        // checkpoint in the log.
        // Issue #1385: coalesce OS reads onto a dedicated batcher thread
        // so a build-storm of tiny PTY chunks becomes one IPC dispatch
        // per 8 ms / 32 KiB. Capture / naming / autopilot still see every
        // OS read on this thread (ChunkCapture already stitches split
        // banners). `with_batcher` drops the producer before joining —
        // joining while still holding `SyncSender` deadlocks the reader
        // on every PTY exit.
        let sink = crate::agent::output::ensure(session_id);
        let batch_session_id = session_id;
        crate::pty::batch::with_batcher(
            move |batch| {
                // One transport only: Channel (or the sink's pre-subscribe
                // buffer). Never emit JSON `agent-output` for PTY bytes —
                // that path and the Channel have no ordering, so a
                // subscribe landing mid-stream would let later chunks
                // overtake earlier ones and split ANSI.
                crate::http_server::send_pty_output(batch_session_id, &batch);
                sink.send_owned(batch);
            },
            |batch_tx| {
                let mut first_chunk = true;
                pump_pty_output(reader, |data| {
                    if first_chunk {
                        first_chunk = false;
                        tracing::info!(
                            "spawn_timing: session={} checkpoint=first_pty_output elapsed={}ms (spawn start → first output; agent CLI boot tail)",
                            session_id,
                            spawn_start.elapsed().as_millis()
                        );
                    }
                    // Mark THIS MESH as active so the background warm-pool worker
                    // holds off its idle refills for this mesh's pool while an agent
                    // is actively producing output (issue #613 AC2; issue #634 scopes
                    // the activity per-mesh so a chatty agent on mesh A doesn't
                    // starve mesh B's pool). `mesh_id` is captured from the spawn
                    // context at thread start — the closure outlives the agent's
                    // registry entry, so reading it from `PROCESS_REGISTRY` inside
                    // the closure would race with `kill_session`'s `remove`.
                    crate::services::pool_worker::note_activity_for_mesh(mesh_id);

                    let (text, uuid) = chunk_capture.feed(data);
                    maybe_buffer_for_naming(is_plain_terminal, session_id, &text);
                    // Autopilot state evaluator tail (issue #483) — one in-memory
                    // set lookup for non-piloted nodes.
                    crate::autopilot::evaluator::on_output(session_id, &text);
                    // Stale-attention safety net (issue #878) — one map lookup for
                    // unarmed nodes.
                    crate::attention_autoclear::on_output(session_id, data.len());

                    if let Some(uuid) = uuid {
                        // The structured hook and Codex rollout fallback can
                        // capture the same self-assigned ID first. Do not let a
                        // delayed PTY banner replace an already-verified value.
                        let captured =
                            db::set_cli_session_id_if_missing(session_id, &uuid).unwrap_or(false);
                        if captured {
                            tracing::info!(
                                "session_capture: captured session ID {} for node {}",
                                uuid,
                                session_id
                            );
                        }
                    }

                    let _ = batch_tx.send(data.to_vec());
                });
            },
        );
        // The Channel subscription belongs to the Agent Node's persistent
        // terminal, not this process incarnation. Keep it live so retry,
        // resume, and regenerate output reaches the same xterm instance.
        tracing::debug!(
            "PTY reader loop ended for session {}, reader exiting",
            session_id
        );
        reader_alive_clone.store(false, Ordering::SeqCst);
        crate::agent::provider::notify_process_terminated(session_id);

        // `spawned_at` is process-creation time, NOT `spawn_start`: the
        // early-exit heuristic answers "did the process die almost
        // immediately after it was created?" — a slow 14s pipeline
        // followed by a 1s-later death must still read as an early exit.
        match post_exit_action(
            is_plain_terminal,
            deliberate_kill.load(Ordering::SeqCst),
            spawned_at.elapsed(),
        ) {
            PostExitAction::LeaveStatusAlone => {
                // kill_session initiated this exit; the kill initiator
                // owns the node's next status (see PostExitAction docs).
                tracing::debug!(
                    "Node {} reader exited after deliberate kill — leaving status to the kill initiator",
                    session_id
                );
            }
            PostExitAction::MarkIdle => {
                // Routes through SessionLifecycle (issue #132) — single writer
                // for `agent_nodes.status`.
                let sink = session_lifecycle::AppSessionLifecycleSink { app: &app_clone };
                let _ = session_lifecycle::on_pty_eof(&sink, session_id);
            }
            PostExitAction::MarkErrorResumeFailed => {
                tracing::warn!(
                    "Node {} reader exited after {:?} — likely resume failure",
                    session_id,
                    spawned_at.elapsed()
                );
                // Routes through SessionLifecycle (issue #132) — the
                // `unless_in(Error, Archived)` guard (#654) lives inside
                // `on_resume_failed`, and `resume-failed` is emitted from
                // exactly one place (the lifecycle sink).
                let sink = session_lifecycle::AppSessionLifecycleSink { app: &app_clone };
                let _ = session_lifecycle::on_resume_failed(
                    &sink,
                    session_id,
                    "Agent exited immediately after spawn — session may have expired",
                );
            }
        }

        tracing::debug!("PTY reader thread exited for session {}", session_id);
    })
}

// ---------------------------------------------------------------------------
// Resume decision surface (issue #949 / PR #1121)
// ---------------------------------------------------------------------------
