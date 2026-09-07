use crate::agent::process::{AgentProcess, PROCESS_REGISTRY};
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
/// merge. Repeated injection adds no duplicates. Writes are atomic via
/// `NamedTempFile` + `sync_all` + `persist`; a malformed existing file
/// returns `Err` rather than silently overwriting the user's data.
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
    let hook_command = serde_json::json!({
        "type": "command",
        "command": "curl -sf -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true",
    });
    ensure_hooks_json(&settings_path, &hook_command)
}

/// Marker predicate: a Claude Code hook handler is Buildmesh-owned when its
/// `command` carries the canonical attention anchors. The `command` field
/// is the only documented Claude hook-handler payload (mirrors the Codex
/// precedent at `codex.rs:986-996`), and its `BUILDMESH_PORT` +
/// `BUILDMESH_SESSION_ID` + `/api/attention/` substring set is the
/// guaranteed-preserved token triple. Substring (not equality) match so a
/// future URL refactor (adding `?token=…`, swapping `localhost` for
/// `127.0.0.1`) keeps the merge stable — only the canonical anchors
/// matter.
pub(super) fn is_buildmesh_handler(handler: &serde_json::Value) -> bool {
    handler
        .get("command")
        .and_then(|v| v.as_str())
        .is_some_and(|command| {
            command.contains("BUILDMESH_PORT")
                && command.contains("BUILDMESH_SESSION_ID")
                && command.contains("/api/attention/")
        })
}

/// Add or update the Buildmesh-owned handler in a single event's
/// matcher-group array. `extra_fields` are merged into the freshly-appended
/// matcher group (e.g. `Notification` requires the documented `matcher: ""`
/// catch-all field; `Stop` does not). Returns `true` when something
/// changed (caller decides whether to rewrite the document).
fn merge_buildmesh_handler(
    groups: &mut Vec<serde_json::Value>,
    new_handler: &serde_json::Value,
    extra_fields: &[(&str, &serde_json::Value)],
) -> bool {
    for group in groups.iter_mut() {
        let Some(handlers) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        if let Some(index) = handlers.iter().position(is_buildmesh_handler) {
            if handlers[index] != *new_handler {
                handlers[index] = new_handler.clone();
                return true;
            }
            // Already correct → no-op. Caller skips the rewrite.
            return false;
        }
    }
    // No Buildmesh entry yet → append a fresh matcher group. Claude Code
    // requires `Notification` entries to carry a `matcher` field (the docs
    // note that matcher "" is the documented catch-all form); `Stop`
    // entries do not — matchers on Stop are ignored with a warning, so
    // omitting the field keeps the merge shape minimal.
    let mut group = serde_json::Map::new();
    for (key, value) in extra_fields {
        group.insert((*key).to_string(), (*value).clone());
    }
    group.insert(
        "hooks".to_string(),
        serde_json::json!([new_handler.clone()]),
    );
    groups.push(serde_json::Value::Object(group));
    true
}

/// Atomically persist `content` to `path` via `tempfile::NamedTempFile +
/// persist`. Mirrors the Codex pattern at `codex.rs:998-1007` and the
/// Grok pattern at `grok.rs:133-140`. A crash mid-rename or pre-rename
/// leaves the canonical file untouched and the orphan `.tmp` is the
/// only residue — visible at `dir.parent().join("name.*.tmp")` until the
/// OS reclaims it.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map(|_| ()).map_err(|error| error.error)
}

/// Write the Buildmesh attention hooks into
/// `{project}/.claude/settings.local.json`, additive-merging any existing
/// user-authored matcher groups under `Notification` and `Stop`.
///
/// The merge refuses to silently overwrite a malformed user-authored file
/// (trailing comma, partial edit, …) — the Codex/Grok pattern treats
/// parse failure as an explicit `Err` so the spawn path surfaces a
/// provision error and the user's data survives intact (issue #1370
/// §1). A missing file is the happy path (fresh install); only an
/// existing-but-unparseable file is the failure case.
fn ensure_hooks_json(path: &Path, new_handler: &serde_json::Value) -> Result<(), String> {
    let mut settings: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).map_err(|e| {
            format!(
                "refusing to overwrite malformed {path:?}: {e}. \
                 Repair or remove the file and retry"
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("failed to read {path:?}: {e}")),
    };
    if !settings.is_object() {
        return Err(format!("{path:?}: top-level value must be a JSON object"));
    }

    let settings_obj = settings
        .as_object_mut()
        .expect("settings coerced to object above");
    let hooks = settings_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "settings.local.json `hooks` value must be an object".to_string())?;

    // The empty matcher is Claude Code's documented "match-all" form for
    // `Notification` (a matcher on `idle_prompt` alone misses every
    // permission prompt). Build it as a `serde_json::Value` once so the
    // merge appends the same literal the pre-#1370 tests pin.
    let matcher_all = serde_json::Value::String(String::new());

    let mut changed = false;
    let notification_groups = hooks_obj
        .entry("Notification")
        .or_insert_with(|| serde_json::json!([]));
    let notification_groups_array = notification_groups
        .as_array_mut()
        .ok_or_else(|| "settings.local.json event `Notification` must be an array".to_string())?;
    changed = merge_buildmesh_handler(
        notification_groups_array,
        new_handler,
        &[("matcher", &matcher_all)],
    ) || changed;

    let stop_groups = hooks_obj
        .entry("Stop")
        .or_insert_with(|| serde_json::json!([]));
    let stop_groups_array = stop_groups
        .as_array_mut()
        .ok_or_else(|| "settings.local.json event `Stop` must be an array".to_string())?;
    changed = merge_buildmesh_handler(stop_groups_array, new_handler, &[]) || changed;

    if !changed {
        return Ok(());
    }

    let content =
        serde_json::to_string_pretty(&settings).map_err(|e| format!("serialize failed: {e}"))?;
    write_atomic(path, &content)
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
    // Channel closed cleanly (`close_input` dropped the sender). The
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
) -> u64 {
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
        AgentProcess::new(
            child,
            writer_tx,
            Some(writer_handle),
            master,
            reader_alive,
            deliberate_kill,
            job,
            None,
            spawn_start,
            mesh_id,
        ),
    )
}
