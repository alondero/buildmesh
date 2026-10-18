use crate::agent::managed_config;
use crate::agent::process::{AgentProcess, AgentProcessParams, PROCESS_REGISTRY};
use portable_pty::{CommandBuilder, PtyPair};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
/// **Additive merge** (issue #1370). The pre-#1370 implementation did
/// `settings["hooks"] = expected_hooks` and so wiped any pre-existing
/// `Notification` / `Stop` / `PermissionRequest` / `UserPromptSubmit` /
/// `PreToolUse` matcher groups the user had authored. The merge now:
///
/// * iterates per event (`Notification`, `Stop`),
/// * locates a Buildmesh-owned handler via a canonical-anchor marker
///   predicate (the `command` field carries
///   `BUILDMESH_PORT` + `BUILDMESH_SESSION_ID` + `/api/attention/` — the
///   same anchors Codex/Grok use),
/// * updates the Buildmesh entry in place when the shape drifts, OR
/// * appends a fresh matcher group when none exists yet.
///
/// User-authored matcher groups, sibling handlers, and other events
/// (`PreToolUse`, `UserPromptSubmit`, …) survive byte-for-byte across the
/// merge. Repeated injection adds no duplicates. The write is the shared
/// `managed_config::atomic_write` (temp file + `sync_all` + rename); a
/// malformed existing file returns `Err` rather than silently overwriting
/// the user's data.
///
/// **PermissionRequest is intentionally absent.** The adapter advertises
/// `AttentionLaunchMode::SkipPermissions` (`--dangerously-skip-permissions`),
/// so Claude Code never raises a permission prompt and a `PermissionRequest`
/// hook entry would be misleading by construction (issue #1370 §2). The
/// `Notification` catch-all still fires on `permission_prompt` notifications
/// if a future launch flips the mode; under today's SkipPermissions the
/// signal is `idle_prompt` and MCP-elicitations only.
///
/// This is the Claude-harness implementation behind
/// `AnthropicAdapter::provision_attention_hooks` (issue #886); the mesh commands
/// also call it directly to pre-provision the default harness's hook at mesh
/// creation, before any node/provider exists. The spawn path surfaces the
/// `Err` as a `SignalHealth::Unavailable` lifecycle event (see
/// `spawn::provision::run_provider_provisioning`), so a malformed
/// settings.local.json is never silently masked.
pub fn inject_attention_hook(project_path: &Path) -> Result<(), String> {
    let claude_dir = project_path.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .map_err(|e| format!("failed to create .claude dir: {e}"))?;

    let settings_path = claude_dir.join("settings.local.json");
    // Resolve the port from $BUILDMESH_PORT at hook-run time (set per-agent in
    // spawn_environment) rather than baking a literal. This keeps the hook
    // correct across the 1992→1994 fallback and routes a dev-profile agent's
    // attention to the dev instance (2992), not the stable hub.
    // `--data-binary @-` forwards the hook's stdin JSON ({hook_event_name,
    // transcript_path, …}) as the POST body (issue #878). The backend uses it
    // to tell "turn ended, user needed" from "turn ended, waiting on
    // background tasks"; an empty body degrades to always-mark.
    let curl = crate::env::unix_attention_curl();
    let hook_command = serde_json::json!({
        "type": "command",
        "command": format!("{curl} -sf --connect-timeout 1 --max-time 2 -o /dev/null -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true"),
    });
    ensure_hooks_json(&settings_path, &hook_command)
}

/// Anchors that identify a Buildmesh-owned Claude Code handler. The
/// `command` field is the only documented Claude hook-handler payload, and
/// this substring set is the guaranteed-preserved token triple. Substring
/// (not equality) match so a future URL refactor (adding `?token=…`, swapping
/// `localhost` for `127.0.0.1`) keeps the merge stable — only the canonical
/// anchors matter, and they must not drift because they are what recognises
/// an already-provisioned config as ours.
const BUILDMESH_HANDLER_ANCHORS: &[&str] =
    &["/api/attention/", "BUILDMESH_PORT", "BUILDMESH_SESSION_ID"];

pub(super) fn is_buildmesh_handler(handler: &serde_json::Value) -> bool {
    handler
        .get("command")
        .and_then(|value| value.as_str())
        .is_some_and(|command| managed_config::carries_anchors(command, BUILDMESH_HANDLER_ANCHORS))
}

/// Write the Buildmesh attention hooks into
/// `{project}/.claude/settings.local.json`, additive-merging any existing
/// user-authored matcher groups under `Notification` and `Stop`.
///
/// The merge refuses to silently overwrite a malformed user-authored file
/// (trailing comma, partial edit, …) — the shared
/// `managed_config::read_document` refusal treats parse failure as an
/// explicit `Err` so the spawn path surfaces a provision error and the
/// user's data survives intact (issue #1370 §1). A missing file is the
/// happy path (fresh install); only an existing-but-unparseable file is
/// the failure case.
fn ensure_hooks_json(path: &Path, new_handler: &serde_json::Value) -> Result<(), String> {
    let mut document = managed_config::read_document(path).map_err(|error| error.to_string())?;
    let hooks_obj = managed_config::object_entry(
        managed_config::root_object(&mut document, "settings.local.json top-level")?,
        "hooks",
        "settings.local.json `hooks`",
    )?;

    // The empty matcher is Claude Code's documented "match-all" form for
    // `Notification` (a matcher on `idle_prompt` alone misses every
    // permission prompt). It is built from the same `String` as the other
    // matchers so the merge appends the same literal the pre-#1370 tests pin.
    let matcher_all = String::new();

    let mut changed = false;
    for (event, matcher) in [
        ("Notification", Some(matcher_all)),
        ("Stop", None),
        ("Elicitation", None),
        ("ElicitationResult", None),
        ("StopFailure", None),
        ("UserPromptSubmit", None),
        ("SubagentStart", None),
        ("SubagentStop", None),
        (
            "PreToolUse",
            Some("^(AskUserQuestion|ExitPlanMode)$".to_string()),
        ),
        (
            "PostToolUse",
            Some("^(AskUserQuestion|ExitPlanMode)$".to_string()),
        ),
        (
            "PostToolUseFailure",
            Some("^(AskUserQuestion|ExitPlanMode)$".to_string()),
        ),
    ] {
        let matcher = matcher.map(serde_json::Value::String);
        let fields: Vec<(&str, &serde_json::Value)> =
            matcher.iter().map(|value| ("matcher", value)).collect();
        changed |= managed_config::upsert_matcher_group(
            managed_config::array_entry(
                hooks_obj,
                event,
                &format!("settings.local.json event `{event}`"),
            )?,
            &is_buildmesh_handler,
            new_handler,
            &fields,
        );
    }

    if !changed {
        return Ok(());
    }

    let content = document
        .to_pretty_json()
        .map_err(|e| format!("serialize failed: {e}"))?;
    managed_config::atomic_write(path, &content)
        .map_err(|e| format!("failed to write settings.local.json: {e}"))?;
    tracing::info!("inject_attention_hook: wrote hook at {:?}", path);
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
/// `take()`n by `AgentProcess::close_input` / kill_session, issue
/// #1531) or when the underlying write returns an error (broken
/// pipe — the channel is then disconnected, subsequent `try_send`s
/// return `Disconnected`, and `write_bytes` surfaces "Agent not
/// running" to the caller).
/// Zeroes a queue gauge on every exit path from [`pty_writer_thread`].
///
/// The thread can leave in three ways — the channel closed cleanly, a
/// `write_all` error, or a `flush` error — and on the error paths the buffers
/// still sitting in `rx` are dropped when the receiver is, without their byte
/// accounting ever being released. Without this guard those bytes would stay on
/// the `DIAG` line as a phantom backlog for as long as the registry entry
/// survives, which outlives the writer thread (issue #1530 review).
struct ResetGaugeOnDrop(std::sync::Arc<crate::agent::process::InputQueueGauge>);

impl Drop for ResetGaugeOnDrop {
    fn drop(&mut self) {
        self.0.reset();
    }
}

pub(super) fn pty_writer_thread(
    session_id: i64,
    mut writer: Box<dyn std::io::Write + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    queue: std::sync::Arc<crate::agent::process::InputQueueGauge>,
) {
    // Armed before the first `recv`, so every `return` below resets the gauge.
    let _reset_on_drop = ResetGaugeOnDrop(std::sync::Arc::clone(&queue));
    while let Ok(bytes) = rx.recv() {
        // Release the byte accounting as the buffer is handed off, not after
        // the write returns: `write_all` on a full ConPTY pipe can block for a
        // long time, and the queue should read as drained the moment the
        // writer has taken ownership (issue #1530).
        queue.release(bytes.len());
        if let Err(e) = writer.write_all(&bytes) {
            tracing::warn!(session_id, "PTY writer thread exiting on write error: {e}");
            return;
        }
        if let Err(e) = writer.flush() {
            tracing::warn!(session_id, "PTY writer thread exiting on flush error: {e}");
            return;
        }
    }
    // Channel closed cleanly (`close_input` dropped the sender). The
    // writer's `Drop` closes the underlying PTY pipe, so the agent's
    // stdin EOFs and the agent CLI exits cleanly. The drop guard zeroes
    // anything still counted as buffered.
    tracing::debug!(session_id, "PTY writer thread exiting (channel closed)");
}

/// Capacity of the bounded `SyncSender` channel between the async
/// Tauri command and the dedicated PTY writer thread. The bound counts
/// messages: one `write_to_agent` call is one message, and a paste
/// stays one message even when it is tens of kilobytes (issue #1498
/// traced a 17,508-byte paste). Do not split a paste to manufacture
/// smaller messages.
///
/// A message count is not a memory bound, so it is paired with
/// [`crate::agent::process::PTY_INPUT_QUEUE_BYTE_CAPACITY`] — see
/// `InputQueueGauge` for why the byte ceiling is tracked explicitly
/// (issue #1530). A refusal is reported to the caller as
/// [`crate::agent::process::InputDisposition::Backpressured`]; it is
/// never dropped behind a success. The alternative — blocking the async
/// runtime on a full bounded channel — would defeat the whole reason the
/// dedicated thread exists.
pub const PTY_WRITER_CHANNEL_CAPACITY: usize = 64;

/// The session-scoped state the registry insert and the reader thread both
/// need. Built once in [`super::streams::start_streams`] so the two cannot
/// drift apart: `reader_alive` and `deliberate_kill` are one flag instance
/// shared three ways (the registry entry — `kill_session` sets it — the reader
/// thread's epilogue reads it, and nothing else), and `spawn_start` is the
/// original `SpawnTimer.start` clone.
#[derive(Clone)]
pub(super) struct SpawnedSession {
    pub session_id: i64,
    pub reader_alive: Arc<AtomicBool>,
    pub deliberate_kill: Arc<AtomicBool>,
    pub spawn_start: std::time::Instant,
    pub mesh_id: i64,
}

/// Insert the agent into [`PROCESS_REGISTRY`] and return the generation the
/// registry assigned to the entry.
pub(super) fn register_agent(
    session: SpawnedSession,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    job: Option<crate::process_util::JobHandle>,
) -> u64 {
    let SpawnedSession {
        session_id,
        reader_alive,
        deliberate_kill,
        spawn_start,
        mesh_id,
    } = session;
    // Issue #1122: spawn the dedicated PTY writer thread and stand up
    // the bounded channel *before* the registry `insert` so a concurrent
    // `write_bytes` call (visible the moment the entry exists) can never
    // race against a missing channel. The writer thread owns the
    // `Box<dyn Write + Send>` exclusively; the registry holds the
    // sender side.
    let (writer_tx, writer_rx) =
        std::sync::mpsc::sync_channel::<Vec<u8>>(PTY_WRITER_CHANNEL_CAPACITY);
    // The gauge is shared, not moved: the registry entry keeps one handle and
    // the writer thread the other, so the enqueue side can reserve and the
    // drain side can release (issue #1530).
    let queue = std::sync::Arc::new(crate::agent::process::InputQueueGauge::default());
    let writer_queue = std::sync::Arc::clone(&queue);
    let writer_handle = std::thread::Builder::new()
        .name(format!("pty-writer-{session_id}"))
        .spawn(move || pty_writer_thread(session_id, writer, writer_rx, writer_queue))
        .expect("failed to spawn PTY writer thread");

    PROCESS_REGISTRY.insert(
        session_id,
        // The reader thread gets its own handles to the shared flags, so the
        // registry entry and the reader keep observing one flag instance.
        AgentProcess::new(AgentProcessParams {
            child,
            writer_tx,
            queue,
            writer_handle: Some(writer_handle),
            master,
            reader_alive: Arc::clone(&reader_alive),
            deliberate_kill: Arc::clone(&deliberate_kill),
            job,
            reader_handle: None,
            spawn_start,
            mesh_id,
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::agent::process::PROCESS_REGISTRY;
    use crate::agent::provider::{SpawnRecipe, WindowsShell};
    use crate::agent::spawn_environment;
    use crate::models::EnvType;

    /// Records every `write` and signals when `flush` runs, so the test
    /// observes the production writer thread's `write_all` boundary.
    struct RecordingWriter {
        chunks: Arc<Mutex<Vec<Vec<u8>>>>,
        flushed: std::sync::mpsc::Sender<()>,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.chunks.lock().unwrap().push(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let _ = self.flushed.send(());
            Ok(())
        }
    }

    struct KillSession(i64);

    impl Drop for KillSession {
        fn drop(&mut self) {
            PROCESS_REGISTRY.kill_session(self.0);
        }
    }

    /// Issue #1498 traced a 17,508-byte Windows paste. The body contains
    /// newlines so a line-pacing workaround fails this contract.
    fn large_bracketed_paste() -> Vec<u8> {
        const PASTE_LEN: usize = 17_508;
        let open = b"\x1b[200~";
        let close = b"\x1b[201~";
        let line = b"paste line\n";
        let body_len = PASTE_LEN - open.len() - close.len();
        let mut body = Vec::with_capacity(body_len);
        while body.len() + line.len() <= body_len {
            body.extend_from_slice(line);
        }
        body.resize(body_len, b'x');
        let mut paste = Vec::with_capacity(PASTE_LEN);
        paste.extend_from_slice(open);
        paste.extend_from_slice(&body);
        paste.extend_from_slice(close);
        assert_eq!(paste.len(), PASTE_LEN);
        paste
    }

    /// `write_to_agent` forwards the whole IPC string through
    /// `PROCESS_REGISTRY.write_bytes` before any newline handling.
    /// Calling that registry method on an agent installed by
    /// `register_agent` is the PTY half of the command: the production
    /// `pty_writer_thread` must `write_all` the paste as one buffer.
    /// A newline payload also consults the process database after the
    /// write, which is a separate attention-signal path.
    #[test]
    fn write_to_agent_delivers_a_large_paste_as_one_pty_write() {
        let session_id = -915_1498;
        let paste = large_bracketed_paste();
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let (flushed_tx, flushed_rx) = std::sync::mpsc::channel();

        let cwd = std::env::current_dir().unwrap();
        let recipe = SpawnRecipe {
            binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
            base_args: if cfg!(windows) {
                vec!["/c".into(), "exit".into(), "0".into()]
            } else {
                vec!["-c".into(), "exit 0".into()]
            },
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        };
        let cmd = spawn_environment::wrap(
            recipe,
            EnvType::Windows,
            None,
            None,
            &cwd.to_string_lossy(),
            session_id,
            false,
        )
        .expect("an unsandboxed command always assembles");
        let pair = super::super::open_pty_pair(24, 80).expect("open pty pair");
        let child = super::spawn_child(&pair, cmd).expect("spawn child");
        drop(pair.slave);
        drop(pair.master.take_writer().expect("take writer"));

        super::register_agent(
            super::SpawnedSession {
                session_id,
                reader_alive: Arc::new(AtomicBool::new(true)),
                deliberate_kill: Arc::new(AtomicBool::new(false)),
                spawn_start: std::time::Instant::now(),
                mesh_id: 0,
            },
            child,
            Box::new(RecordingWriter {
                chunks: chunks.clone(),
                flushed: flushed_tx,
            }),
            pair.master,
            None,
        );
        let _stop = KillSession(session_id);

        PROCESS_REGISTRY
            .write_bytes(session_id, &paste)
            .expect("enqueue the paste");

        flushed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("pty writer thread did not flush the paste");

        let recorded = chunks.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "a large paste must stay one PTY write, saw {} writes (first {} bytes)",
            recorded.len(),
            recorded.first().map(|chunk| chunk.len()).unwrap_or(0),
        );
        assert_eq!(recorded[0], paste);
    }
}
