use crate::agent::process::{AgentProcess, PROCESS_REGISTRY};
use portable_pty::{CommandBuilder, PtyPair};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Spawn the child process.
pub fn spawn_child(
    pair: &PtyPair,
    cmd: CommandBuilder,
) -> Result<Box<dyn portable_pty::Child + Send + Sync>, String> {
    pair.slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn agent: {}", e))
}

/// Spawn `cmd` inside the Windows agent sandbox. Returns the same
/// `Child`/`MasterPty` trait objects as the normal path. Windows-only; the
/// non-Windows stub exists only so `spawn_agent_inner` compiles cross-platform
/// (the `sandbox_enabled` seam never selects this branch off Windows).
///
/// Uses the **restricted-token** primitive (ADR-0014), not the AppContainer:
/// the AppContainer's object-namespace isolation hung `claude.exe` at libuv's
/// named-pipe creation (#528) and blocked loopback (#533). The §4 spike proved
/// the restricted token fixes both. It is launched **permissive**
/// (`include_user_sid = true`) — read-confinement is *not* delivered here (a
/// same-user token can't deny home reads while MSYS `bash` runs; see the spike's
/// `tradeoff` test and ADR-0014 §Spike result), so home grants are unnecessary
/// (`grant_home = false`). Deny-by-default reads are a tracked follow-up
/// (separate-user principal / WSL).
#[cfg(target_os = "windows")]
#[allow(clippy::type_complexity)]
pub(super) fn sandbox_spawn(
    cmd: &CommandBuilder,
    session_id: i64,
    host_path: &str,
    rows: u16,
    cols: u16,
) -> Result<
    (
        Box<dyn portable_pty::Child + Send + Sync>,
        Box<dyn portable_pty::MasterPty + Send>,
    ),
    String,
> {
    crate::sandbox::spawn::spawn_sandboxed_restricted(
        cmd, session_id, host_path, rows, cols, false, true,
    )
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::type_complexity)]
pub(super) fn sandbox_spawn(
    _cmd: &CommandBuilder,
    _session_id: i64,
    _host_path: &str,
    _rows: u16,
    _cols: u16,
) -> Result<
    (
        Box<dyn portable_pty::Child + Send + Sync>,
        Box<dyn portable_pty::MasterPty + Send>,
    ),
    String,
> {
    Err("process sandbox is only supported on Windows".to_string())
}

/// Ensures the Claude Code attention hooks exist in
/// `{project}/.claude/settings.local.json`.
///
/// Writes a catch-all `Notification` hook (fires on permission prompts, idle
/// prompts, MCP elicitations — every type that means "the user is needed") plus
/// a `Stop` hook (fires the instant a turn ends). Both POST to the local
/// attention endpoint. Idempotent: re-runs no-op once the config matches, and
/// migrate an older `idle_prompt`-only config on the next spawn.
///
/// This is the Claude-harness implementation behind
/// `AnthropicAdapter::provision_attention_hooks` (issue #886); the mesh commands
/// also call it directly to pre-provision the default harness's hook at mesh
/// creation, before any node/provider exists.
pub fn inject_attention_hook(project_path: &std::path::Path) -> Result<(), String> {
    let claude_dir = project_path.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .map_err(|e| format!("failed to create .claude dir: {e}"))?;

    let settings_path = claude_dir.join("settings.local.json");
    let mut settings: serde_json::Value = match std::fs::read_to_string(&settings_path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };

    // Resolve the port from $BUILDMESH_PORT at hook-run time (set per-agent in
    // spawn_environment) rather than baking a literal. This keeps the hook
    // correct across the 1992→1994 fallback and routes a dev-profile agent's
    // attention to the dev instance (2992), not the stable hub.
    // `--data-binary @-` forwards the hook's stdin JSON ({hook_event_name,
    // transcript_path, …}) as the POST body (issue #878). The backend uses it
    // to tell "turn ended, user needed" from "turn ended, waiting on
    // background tasks"; an empty body degrades to always-mark.
    let hook_command =
        "curl -sf -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true"
            .to_string();

    // We register two hooks so the user is told the *instant* their input is
    // needed, not just when the agent goes idle:
    //   - Notification with an empty (catch-all) matcher fires on every
    //     notification type — crucially `permission_prompt` (the agent is asking
    //     to run a tool / answer a question) as well as `idle_prompt`. Matching
    //     only `idle_prompt` (the old behaviour) missed every permission prompt,
    //     so the user was never alerted when an agent paused to ask something.
    //   - Stop fires the moment the agent finishes a turn, so "agent is waiting
    //     for you" lands immediately instead of after Claude Code's idle timer.
    // Both POST to the same attention endpoint; `mark_attention` is idempotent.
    let notification_hook = serde_json::json!({
        "type": "command",
        "command": hook_command
    });
    let expected_hooks = serde_json::json!({
        "Notification": [{
            "matcher": "",
            "hooks": [notification_hook.clone()]
        }],
        "Stop": [{
            "hooks": [notification_hook]
        }]
    });

    if settings.get("hooks") == Some(&expected_hooks) {
        return Ok(());
    }

    settings["hooks"] = expected_hooks;

    let content =
        serde_json::to_string_pretty(&settings).map_err(|e| format!("serialize failed: {e}"))?;
    std::fs::write(&settings_path, content).map_err(|e| format!("failed to write: {e}"))?;
    tracing::info!("inject_attention_hook: wrote hook at {:?}", settings_path);
    Ok(())
}

/// Check if an agent is already running for this session.
pub fn is_agent_already_running(session_id: &i64) -> bool {
    if let Some(agent) = PROCESS_REGISTRY.get(session_id) {
        if agent.reader_alive.load(Ordering::SeqCst) {
            tracing::info!(
                "spawn_agent: session {} is already running, skipping spawn",
                session_id
            );
            return true;
        }
    }
    false
}

/// Register the agent process in the registry.
///
/// `spawn_start` is the original `SpawnTimer.start` clone — used by
/// `record_first_input_if_first` (via `AgentProcess.spawn_start`) to
/// timestamp the `first_user_input` log line against the same reference
/// as every other `spawn_timing:` checkpoint.
/// Issue #1122: per-agent dedicated PTY writer thread. The thread
/// owns the `Box<dyn Write + Send>` exclusively (no mutex on the
/// hot path) and drains a `std::sync::mpsc::SyncSender` channel that
/// `AgentProcessRegistry::write_bytes` enqueues bytes into from the
/// async runtime. The thread exits when the channel closes (sender
/// dropped) or when the underlying write returns an error (broken
/// pipe — the channel is then disconnected, subsequent `try_send`s
/// return `Disconnected`, and `write_bytes` surfaces "Agent not
/// running" to the caller).
pub(super) fn pty_writer_thread(
    session_id: i64,
    mut writer: Box<dyn std::io::Write + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
) {
    while let Ok(bytes) = rx.recv() {
        if let Err(e) = writer.write_all(&bytes) {
            tracing::warn!(session_id, "PTY writer thread exiting on write error: {e}");
            return;
        }
        if let Err(e) = writer.flush() {
            tracing::warn!(session_id, "PTY writer thread exiting on flush error: {e}");
            return;
        }
    }
    // Channel closed cleanly (kill_session dropped the sender). The
    // writer's `Drop` closes the underlying PTY pipe, so the agent's
    // stdin EOFs and the agent CLI exits cleanly.
    tracing::debug!(session_id, "PTY writer thread exiting (channel closed)");
}

/// Capacity of the bounded `SyncSender` channel between the async
/// Tauri command and the dedicated PTY writer thread. 64 entries ×
/// ~tens of bytes per entry is a few KB of in-flight data — comfortably
/// within the PTY pipe buffer (64 KB on Linux, similar on Windows
/// ConPTY) yet bounded enough that a stuck agent can't grow memory
/// without limit. A full channel surfaces as a `warn!` log and the
/// bytes are dropped (the user can re-type); the alternative — blocking
/// the async runtime on a full bounded channel — would defeat the
/// whole reason the dedicated thread exists.
pub(super) const PTY_WRITER_CHANNEL_CAPACITY: usize = 64;

#[allow(clippy::too_many_arguments)]
pub(super) fn register_agent(
    session_id: i64,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    reader_alive: Arc<AtomicBool>,
    job: Option<crate::process_util::JobHandle>,
    spawn_start: std::time::Instant,
    mesh_id: i64,
    deliberate_kill: Arc<AtomicBool>,
) {
    // Issue #1122: spawn the dedicated PTY writer thread and stand up
    // the bounded channel *before* the registry `insert` so a concurrent
    // `write_bytes` call (visible the moment the entry exists) can never
    // race against a missing channel. The writer thread owns the
    // `Box<dyn Write + Send>` exclusively; the registry holds the
    // sender side.
    let (writer_tx, writer_rx) =
        std::sync::mpsc::sync_channel::<Vec<u8>>(PTY_WRITER_CHANNEL_CAPACITY);
    let writer_handle = std::thread::Builder::new()
        .name(format!("pty-writer-{session_id}"))
        .spawn(move || pty_writer_thread(session_id, writer, writer_rx))
        .expect("failed to spawn PTY writer thread");

    PROCESS_REGISTRY.insert(
        session_id,
        AgentProcess {
            child: Arc::new(Mutex::new(child)),
            writer_tx,
            writer_handle: Mutex::new(Some(writer_handle)),
            // Wrap the master in `Some` so `kill_session` can `take()` it
            // out to drop the pseudoconsole (issue #300).
            master: Arc::new(Mutex::new(Some(master))),
            reader_alive,
            // Shared with the reader thread (started right after this
            // insert) so a `kill_session` teardown is distinguishable
            // from the child dying on its own — see the field docs.
            deliberate_kill,
            job,
            // The handle is set after the reader thread is spawned, via
            // `AgentProcess::set_reader_handle`. We insert first so a
            // concurrent `is_agent_already_running` sees the entry; the
            // window between insert and setter is benign (see process.rs).
            reader_handle: Mutex::new(None),
            spawn_start,
            // First-write gate: starts false, flipped true exactly once
            // by `record_first_input_if_first` on the first successful
            // `write_bytes` call for this session. Plain `AtomicBool` —
            // the field lives inside `Arc<AgentProcess>` already, so no
            // inner Arc is needed (the reader thread doesn't share this
            // flag).
            first_user_input_logged: AtomicBool::new(false),
            // Issue #634: stored at registration so `write_bytes` and the
            // PTY read loop can record per-mesh activity without a DB
            // lookup on every chunk. `mesh_id` was already resolved at
            // `prepare_context` via `db::get_mesh_by_path(&node.path)`
            // — the value is in scope here.
            mesh_id,
        },
    );
}
