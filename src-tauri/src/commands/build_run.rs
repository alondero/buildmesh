//! Build/Run feature — spawns a shell in a worktree and runs build/run commands

use crate::db;
use crate::env;
use crate::models::MeshRow;
use crate::pty::PtyRegistry;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Wire types — Tauri event payloads (issue #161)
// ---------------------------------------------------------------------------

/// Payload of the per-session `build-run-output-{sessionId}` Tauri event.
/// Production PTY bytes go over a binary Channel (`subscribe_build_run_output`);
/// this JSON event is the test-injection fallback (issue #1393, matching
/// `agent-output` after #1385). `data` is a base64-encoded chunk when tests
/// emit the object form; a plain string payload is also accepted.
///
/// Generated to `src/types/generated/BuildRunOutputPayload.ts`; the TS half
/// is imported by `src/components/Terminal/BuildRunTerminalRegistry.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "BuildRunOutputPayload.ts")]
#[allow(dead_code)] // ts-rs wire type; production bytes use the Channel
pub struct BuildRunOutputPayload {
    pub data: String,
}

/// Payload of the per-session `build-run-exited-{sessionId}` Tauri event.
/// Emitted when the PTY reader sees EOF on the build/run shell. Empty
/// payload — the exit is a sentinel, not a state carrier. The event name
/// encodes the sessionId (one event family per spawned shell) so the
/// listener can match it without a payload field.
///
/// Generated to `src/types/generated/BuildRunExitedPayload.ts`; the TS half
/// is imported by `src/components/Terminal/BuildRunTerminalRegistry.ts`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "BuildRunExitedPayload.ts")]
pub struct BuildRunExitedPayload {}

// ---------------------------------------------------------------------------
// Worktree path resolution
// ---------------------------------------------------------------------------

/// Validate that the worktree directory exists on disk.
/// Returns an error if the node has no worktree_name or the directory hasn't been created yet.
fn validate_worktree_exists(
    resolved: &env::ResolvedPath,
    worktree_name: Option<&str>,
) -> Result<(), String> {
    let wt_name = worktree_name.ok_or_else(|| {
        "No worktree name set for this agent node. Spawn the agent first to create a worktree."
            .to_string()
    })?;

    // Use git2 to verify the worktree is registered in git metadata,
    // not just the directory exists on disk. This catches broken/corrupted worktrees.
    let repo = git2::Repository::open(&resolved.host_path)
        .or_else(|_| git2::Repository::discover(&resolved.host_path))
        .map_err(|e| format!("Failed to open repository: {}", e))?;

    let worktrees = repo
        .worktrees()
        .map_err(|e| format!("Failed to list worktrees: {}", e))?;

    // Check if our worktree name is in the list
    for i in 0..worktrees.len() {
        if let Some(name) = worktrees.get(i) {
            if name == wt_name {
                return Ok(());
            }
        }
    }

    // Also check if the path itself is a valid git worktree (it could be the main worktree)
    if std::path::Path::new(&resolved.host_path)
        .join(".git")
        .exists()
    {
        return Ok(());
    }

    Err(format!(
        "Worktree '{}' not found in git worktree list. Spawn the agent first to create the worktree.",
        wt_name
    ))
}

// ---------------------------------------------------------------------------
// Process management
// ---------------------------------------------------------------------------

/// A build/run process tracked separately from agents.
///
/// Each entry carries a per-incarnation `generation` token so the natural-exit
/// reader thread can compare-and-remove against it instead of unconditionally
/// deleting whatever happens to be at the same `node_id` (issue #1532). A
/// rapid `Build → Build` (or `Run → Run`) replacement drops the previous Arc
/// in `insert`; the previous reader thread is still pumping and will reach
/// EOF when its master is closed, but `remove_if_current(node_id, prev_gen)`
/// must fail to match — the entry now points at the *new* incarnation.
struct BuildRunProcess {
    /// Per-incarnation token assigned by [`BuildRunRegistry::insert`].
    /// `0` is the "not yet inserted" sentinel (matches `AgentProcess`
    /// convention); callers should pass `0` from struct literals and
    /// let `insert` overwrite.
    generation: u64,
    /// Retained child handle. The previous design `drop(child)`-ed it
    /// immediately after spawn, leaving a stale reader thread's master
    /// with no reaper. `teardown_inc` now `kill()` + `try_wait()`s this
    /// so a replacement spawn cannot leak a process tree (issue #1532
    /// point #5). `Option` so teardown can `take()` it; `None` is the
    /// "already reaped" state.
    child: Arc<Mutex<Option<Box<dyn Child + Send>>>>,
    /// PTY master wrapped in `Option` so teardown can `take()` it and
    /// drop the pseudoconsole. On Windows ConPTY the master read pipe
    /// does not EOF on child exit — closing the master is the only way
    /// to unblock the reader thread (mirrors `AgentProcess`).
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    /// Writer for sending user input to the PTY. Always populated (even
    /// for one-shot build/run) so `write_to_build_run` can be called for
    /// any entry in the registry. Build/run never receives user input
    /// today, but storing the writer is harmless and keeps the surface
    /// uniform.
    writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    /// PTY reader thread handle. `kill_session` joins it with a bounded
    /// timeout so the close path can never hang the UI thread on a
    /// wedged reader (issue #1532 teardown contract; mirrors
    /// `AgentProcess::reader_handle`).
    reader_handle: Mutex<Option<JoinHandle<()>>>,
}

/// Thread-safe registry for build/run processes. Mirrors the
/// incarnation-safe shape of `AgentProcessRegistry`: insertion assigns a
/// monotonic generation token and tears down the previous entry under
/// the same key, and reader-cleanup uses compare-and-remove.
struct BuildRunRegistry {
    processes: PtyRegistry<i64, BuildRunProcess>,
}

/// Monotonic token source for [`BuildRunProcess::generation`]. Starts at
/// `1` so a literal `generation: 0` is visibly "not yet inserted"
/// (matches `NEXT_PROCESS_GENERATION` in `agent::process`).
static NEXT_BUILD_RUN_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Whether teardown should join the PTY reader thread. Natural-exit
/// reaping runs *on* the reader itself, so it must not join itself
/// (mirrors `agent::process::JoinPolicy`).
#[derive(Debug, Clone, Copy)]
enum JoinPolicy {
    /// `kill_session` / `insert` replacement path — wait up to 2 s for
    /// the reader to exit before detaching.
    Join,
    /// `reap_incarnation` from inside the reader — drop the handle without
    /// joining.
    Drop,
}

impl BuildRunRegistry {
    fn new() -> Self {
        Self {
            processes: PtyRegistry::new(),
        }
    }

    /// Insert `process` and return the generation token assigned to this
    /// incarnation. A previous entry under the same node id is torn
    /// down so `insert` cannot leak a child/PTY/writer (issue #1532
    /// point #2 — serialize starts per node).
    fn insert(&self, node_id: i64, mut process: BuildRunProcess) -> u64 {
        let generation = NEXT_BUILD_RUN_GENERATION.fetch_add(1, Ordering::Relaxed);
        process.generation = generation;
        let previous = self.processes.insert(node_id, Arc::new(process));
        if let Some(prev) = previous {
            teardown_inc(&prev, JoinPolicy::Join);
        }
        generation
    }

    /// Drop the registry entry only if it is still this incarnation. A
    /// replacement spawn under the same node id keeps its entry (issue
    /// #1532).
    fn remove_if_current(&self, node_id: i64, generation: u64) -> Option<Arc<BuildRunProcess>> {
        self.processes
            .remove_if(&node_id, |process| process.generation == generation)
    }

    /// Reap a naturally-exited process incarnation. Returns `true` iff
    /// the registry entry was actually removed — the caller uses the
    /// boolean to decide whether to emit the
    /// `build-run-exited-{node_id}` event. Compare-and-remove first so
    /// only one teardown owns the Arc: a replacement spawn or a
    /// concurrent `kill_session` wins the other path.
    fn reap_incarnation(&self, node_id: i64, generation: u64) -> bool {
        let Some(process) = self.remove_if_current(node_id, generation) else {
            return false;
        };
        teardown_inc(&process, JoinPolicy::Drop);
        true
    }

    /// Explicit close initiated by the user (X-button, `close_build_run`).
    /// Mirrors `AgentProcessRegistry::kill_session`: removes the entry
    /// and joins the reader with a bounded timeout. Returns true if a
    /// process was actually reaped (used by the test surface to assert
    /// close-vs-replacement isolation).
    fn kill_session(&self, node_id: i64) -> bool {
        let Some(process) = self.processes.remove(&node_id) else {
            return false;
        };
        teardown_inc(&process, JoinPolicy::Join);
        true
    }

    /// Predicate exposed for the test surface and for any future
    /// pre-write checks. Marked `#[allow(dead_code)]` so clippy on
    /// `--lib` doesn't flag it — the tests under `mod tests` exercise
    /// it, but `--lib` analysis does not see test-only usage.
    #[allow(dead_code)]
    fn contains(&self, node_id: &i64) -> bool {
        self.processes.contains(node_id)
    }

    fn get(&self, node_id: &i64) -> Option<Arc<BuildRunProcess>> {
        self.processes.get(node_id)
    }

    /// Write bytes (user keystrokes) to the PTY master of a live process.
    /// Returns `Err("Build run not running")` if the node has no live process
    /// — matches the agent registry's error string shape so the frontend can
    /// safely ignore the "not running" case.
    fn write_bytes(&self, node_id: i64, data: &[u8]) -> Result<(), String> {
        let process = self
            .processes
            .get(&node_id)
            .ok_or_else(|| "Build run not running".to_string())?;
        let mut writer = process.writer.lock().unwrap();
        writer.write_all(data).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }

    /// Resize the PTY to `cols` x `rows`. Same "not running" semantics.
    fn resize_pty(&self, node_id: i64, cols: u16, rows: u16) -> Result<(), String> {
        let process = self
            .processes
            .get(&node_id)
            .ok_or_else(|| "Build run not running".to_string())?;
        let master = process.master.lock().unwrap();
        let Some(m) = master.as_ref() else {
            return Err("PTY master already closed".to_string());
        };
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
    }
}

/// Stash the reader thread's `JoinHandle` on the registry entry. The
/// window between `insert` and this setter is benign — a `kill_session`
/// arriving in that window sees `reader_handle = None` and skips the
/// join (the thread is detached when the registry entry drops; the
/// channel close will terminate its loop).
fn set_reader_handle(process: &Arc<BuildRunProcess>, handle: JoinHandle<()>) {
    *process.reader_handle.lock().unwrap() = Some(handle);
}

static BUILD_RUN_REGISTRY: once_cell::sync::Lazy<Arc<Mutex<BuildRunRegistry>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(BuildRunRegistry::new())));

/// Shared teardown for a build/run process incarnation (issue #1532).
/// The caller must already have removed `process` from the registry so
/// only one path owns this Arc — `kill_session` and `insert` (on
/// replacement) call this after `remove`/`insert`; `reap_incarnation`
/// calls this after `remove_if_current`.
fn teardown_inc(process: &BuildRunProcess, join: JoinPolicy) {
    // 1. Drop the master. `Option::take` removes the `Box<dyn MasterPty>`
    //    from the mutex; the binding falls out of scope and drops it,
    //    closing the pseudoconsole. On Windows ConPTY this is the only
    //    way to EOF the reader thread (mirror of the
    //    `AgentProcess::master` close).
    if let Ok(mut master_guard) = process.master.lock() {
        master_guard.take();
    }

    // 2. Kill the child handle. `child.kill()` + `try_wait()` reaps the
    //    shell handle portable-pty owns. Issue #1532 point #5: the
    //    previous design `drop(child)`-ed the handle at the bottom of
    //    `build_run_blocking`, leaving the process tree un-reapable
    //    once a replacement spawn needed to kill it. Now retained for
    //    explicit teardown.
    if let Ok(mut child_guard) = process.child.lock() {
        if let Some(mut child) = child_guard.take() {
            let _ = child.kill();
            let _ = child.try_wait();
        }
    }

    match join {
        JoinPolicy::Join => {
            if let Some(handle) = process.reader_handle.lock().unwrap().take() {
                join_with_timeout(handle, std::time::Duration::from_secs(2));
            }
        }
        JoinPolicy::Drop => {
            // This thread *is* the reader. Drop the handle without
            // joining — `JoinHandle::drop` detaches.
            drop(process.reader_handle.lock().unwrap().take());
        }
    }
}

/// Join a thread, detaching it (via a watchdog) if it hasn't returned in
/// `timeout`. Mirrors `agent::process::join_with_timeout` so a wedged
/// reader cannot hang `kill_session` on the UI thread.
///
/// We can't use `JoinHandle::join_timeout` (the method doesn't exist on
/// stable), so we run the join on a watchdog thread and wait on a
/// oneshot channel. The watchdog outlives the timeout only if the
/// reader is genuinely stuck; the thread itself is cheap (it's idle in
/// `join`) and exits as soon as the reader does. The inner
/// `JoinHandle` is dropped at the end of the watchdog's closure, which
/// detaches the reader per `JoinHandle::drop` docs.
fn join_with_timeout(handle: JoinHandle<()>, timeout: std::time::Duration) {
    if handle.is_finished() {
        let _ = handle.join();
        return;
    }
    let watch_name = match handle.thread().name() {
        Some(name) => format!("join-watch-{name}"),
        None => "join-watch-buildrun-reader".to_string(),
    };
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let _watchdog = std::thread::Builder::new()
        .name(watch_name)
        .spawn(move || {
            let _ = handle.join();
            // If the receiver is gone (we timed out), the send errors
            // silently; the `JoinHandle` is still dropped on closure
            // exit, detaching the reader thread.
            let _ = tx.send(());
        })
        .expect("failed to spawn join watchdog");
    let _ = rx.recv_timeout(timeout);
}

// ---------------------------------------------------------------------------
// Shell selection
// ---------------------------------------------------------------------------

/// Build the `CommandBuilder` for either a one-shot build/run command or an
/// interactive shell. Terminal mode drops the `-c` / `-e` / `/c` flag and
/// the command argument — we want a long-running shell, not a one-shot.
///
/// Shell choice per platform (terminal mode):
/// - Wsl → `wsl.exe` (default user's login shell, cwd is the WSL path)
/// - macOS / native Linux → `sh` (any non-Windows host: no WSL, no PowerShell)
/// - Windows → `powershell.exe` (per user preference; ANSI renders in PTY)
fn build_shell_command(
    mode: BuildRunMode,
    command: &str,
    env_type: crate::models::EnvType,
) -> CommandBuilder {
    if mode == BuildRunMode::Terminal {
        if env_type == crate::models::EnvType::Wsl {
            CommandBuilder::new("wsl.exe")
        } else if !cfg!(target_os = "windows") {
            // macOS and native Linux: POSIX `sh` (no WSL, no PowerShell).
            CommandBuilder::new("sh")
        } else {
            CommandBuilder::new("powershell.exe")
        }
    } else if env_type == crate::models::EnvType::Wsl {
        let mut c = CommandBuilder::new("wsl.exe");
        c.arg("-e");
        c.arg(command);
        c
    } else if !cfg!(target_os = "windows") {
        // macOS and native Linux: POSIX `sh -c <command>`.
        let mut c = CommandBuilder::new("sh");
        c.arg("-c");
        c.arg(command);
        c
    } else {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg(command);
        c
    }
}

// ---------------------------------------------------------------------------
// Tauri command
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildRunMode {
    Build,
    Run,
    /// Interactive shell spawned in the worktree directory. The user types
    /// into it via `write_to_build_run`; output is streamed on the same
    /// binary Channel (`subscribe_build_run_output`) as build/run.
    Terminal,
}

#[tauri::command]
pub async fn build_run(node_id: i64, mode: BuildRunMode, app: AppHandle) -> Result<(), String> {
    // Offload: the body opens the repo with git2 (worktree-registration
    // check — a stat-heavy walk on large repos / WSL UNC paths) and then
    // opens a ConPTY + spawns a shell. Both are blocking calls that must
    // not park a Tauri async worker (Command Threading convention).
    crate::commands::run_blocking("build_run", move || build_run_blocking(node_id, mode, app)).await
}

/// Sync core for [`build_run`] — see the `*_blocking` + `run_blocking`
/// convention in `commands/mod.rs`.
fn build_run_blocking(node_id: i64, mode: BuildRunMode, app: AppHandle) -> Result<(), String> {
    // 1. Get agent node (node.path == mesh.path for all nodes)
    let node = db::get_agent_node_by_id(node_id)
        .map_err(|e| format!("failed to get agent node {}: {}", node_id, e))?;

    // 2. Read canonical mesh row from DB (for build/run command strings).
    // The worktree-vs-root decision below does NOT consult `row.use_worktree`
    // — the per-node override (`SpawnButtonCluster`'s alt-click path
    // bypasses the mesh setting and persists `use_worktree=false` on the
    // node row) means the mesh-level flag is the wrong authority here.
    let mesh = db::get_mesh_by_path(&node.path)
        .map_err(|e| format!("failed to get mesh for path {}: {}", node.path, e))?;
    let row = MeshRow::from(&mesh);

    // 3. Resolve cwd via the canonical Node Working Directory rule
    //    (`resolve_build_run_cwd` → `env::node_working_path`). Gates on
    //    `node.use_worktree` + non-empty trimmed `worktree_name`, so a Root
    //    Node spawned in a worktree-enabled mesh resolves to the mesh root
    //    — the bug the user reported surfaced here as "No worktree name set
    //    for this agent node." for any Build/Run/Terminal click on a Root
    //    Node.
    let resolved = resolve_build_run_cwd(&node);

    // 4. Validate + sanitize the worktree only for Worktree Nodes. Root
    //    Nodes have no worktree to inspect and no `.git` worktree-link to
    //    sanitize. Pulling `wt_name` from `env::worktree_segment` (not from
    //    `node.worktree_name` directly) keeps the trim invariant in one
    //    place — a DB row with stray whitespace around the name would
    //    otherwise bypass the trim and fail the git2 worktree-list compare.
    if let Some(wt_name) = env::worktree_segment(&node) {
        validate_worktree_exists(&resolved, Some(wt_name))?;

        // Sanitize .git file to ensure proper worktree isolation across environments
        if let Err(e) =
            crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type)
        {
            tracing::warn!("build_run: failed to sanitize worktree .git file: {}", e);
        }
    }

    // 5. Resolve the command for this (mode, context) tuple (issue #802).
    //    Root context prefers the per-context `root_build_command` /
    //    `root_run_command` and falls back to `build_command` /
    //    `run_command`; worktree context always uses the latter. Terminal
    //    mode spawns an interactive shell directly, so its command is empty.
    //    `is_root` comes from `env::worktree_segment` — the SAME signal
    //    `resolve_build_run_cwd` uses to choose the cwd — so the command
    //    always matches the directory the shell actually spawns in.
    let is_root = env::worktree_segment(&node).is_none();
    let command = resolve_build_run_command(mode, is_root, &row).ok_or_else(|| {
        match mode {
            BuildRunMode::Run => "run command not configured".to_string(),
            // Terminal always resolves to Some(""), so only Build reaches here.
            _ => "build command not configured".to_string(),
        }
    })?;

    // 6. Get shell working directory from resolved path
    let shell_cwd = &resolved.spawn_path;

    // 7. Spawn PTY with shell
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open PTY: {}", e))?;

    let mut cmd = build_shell_command(mode, command, resolved.env_type);
    cmd.cwd(shell_cwd);
    crate::pty::strip_git_env_vars(&mut cmd);

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn shell: {}", e))?;

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("failed to clone PTY reader: {}", e))?;

    // Take writer up-front so the registry can serve `write_to_build_run`
    // for terminal mode. `take_writer()` is a one-shot — must be called
    // before the master is moved into the registry.
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("failed to take PTY writer: {}", e))?;

    // 8. Store process in registry. The child handle is RETAINED here
    //    (was previously `drop(child)`-ed on exit) so a replacement
    //    spawn can `kill()` + `try_wait()` it via `teardown_inc`. See
    //    issue #1532 point #5.
    let process = BuildRunProcess {
        generation: 0, // overwritten by `insert`
        child: Arc::new(Mutex::new(Some(child))),
        master: Arc::new(Mutex::new(Some(pair.master))),
        writer: Arc::new(Mutex::new(writer)),
        reader_handle: Mutex::new(None),
    };
    let generation = BUILD_RUN_REGISTRY
        .lock()
        .unwrap()
        .insert(node_id, process);

    // 9. Spawn reader thread to stream output to frontend. Capture the
    //    generation so the EOF epilogue can `remove_if_current` instead
    //    of unconditionally wiping the entry (issue #1532 point #3).
    let node_id_clone = node_id;
    let app_handle = app.clone();
    let reader_handle = std::thread::Builder::new()
        .name(format!("build-run-pty-reader-{node_id_clone}"))
        .spawn(move || {
            // Issue #1393: coalesce OS reads onto the same batcher the
            // agent path uses (8 ms / 32 KiB) and push raw bytes over a
            // binary Channel. Production bytes never share the JSON
            // `build-run-output-{id}` event — that path is test
            // injection only. The Channel is session-scoped: this reader
            // must not unregister it on exit. `with_batcher` drops the
            // producer before joining — joining while still holding
            // `SyncSender` deadlocks the reader on every PTY exit.
            let sink = crate::pty::sink::BUILD_RUN.ensure(node_id_clone);
            crate::pty::batch::with_batcher(
                move |batch| {
                    sink.send_owned(batch);
                },
                |batch_tx| {
                    let mut r = reader;
                    let mut buf = [0u8; crate::pty::batch::PTY_READ_BUF];
                    loop {
                        match r.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if batch_tx.send(buf[..n].to_vec()).is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::error!("build_run PTY read error: {}", e);
                                break;
                            }
                        }
                    }
                },
            );
            // EOF / read error — process is gone. Compare-and-remove
            // against this thread's generation so a stale reader cannot
            // delete a replacement process (issue #1532). `reaped`
            // distinguishes the three lifecycle exits:
            //
            // - `true`  → this thread's generation is still current, so
            //             the natural exit is real. Emit the
            //             `build-run-exited-{node_id}` event so the
            //             frontend can flip `ptyAlive=false`.
            // - `false` → a replacement `insert` or a `kill_session`
            //             already removed this entry; the close /
            //             replacement initiator owns the next state and
            //             emitting would mis-fire onto a live
            //             replacement's terminal.
            let reaped = BUILD_RUN_REGISTRY
                .lock()
                .unwrap()
                .reap_incarnation(node_id_clone, generation);
            if reaped {
                let _ = app_handle.emit(
                    &format!("build-run-exited-{}", node_id_clone),
                    BuildRunExitedPayload {},
                );
            }
        })
        .map_err(|e| format!("failed to spawn reader thread: {}", e))?;
    // Stash the JoinHandle on the registry entry so `kill_session` /
    // a replacement `insert` can join it. The window between `insert`
    // and this setter is benign — a teardown arriving in that window
    // sees `reader_handle = None` and skips the join.
    if let Some(process) = BUILD_RUN_REGISTRY.lock().unwrap().get(&node_id) {
        set_reader_handle(&process, reader_handle);
    }

    Ok(())
}

#[tauri::command]
pub async fn get_mesh_row(mesh_id: i64) -> Result<MeshRow, String> {
    crate::commands::run_blocking("get_mesh_row", move || {
        let mesh = db::get_mesh_by_id(mesh_id)
            .map_err(|e| format!("failed to get mesh {}: {}", mesh_id, e))?;
        Ok(MeshRow::from(&mesh))
    })
    .await
}

/// Close a build/run terminal for a node.
///
/// Kills the PTY only. The binary output Channel is session-scoped and
/// must survive process exit so a replacement spawn (mode switch after
/// X-close is a new subscribe; natural EOF keeps the xterm showing
/// `[process exited]`). Frontend `dispose` calls
/// `unsubscribe_build_run_output` separately.
///
/// Delegates to [`BuildRunRegistry::kill_session`] which marks the
/// teardown deliberate (so the reader epilogue does NOT emit
/// `build-run-exited-{node_id}` — the close initiator owns the next
/// state), drops the PTY master to EOF the reader on Windows ConPTY,
/// and joins the reader with a 2 s timeout so a wedged reader cannot
/// hang the UI thread. Issue #1532.
#[tauri::command]
pub async fn close_build_run(node_id: i64) -> Result<(), String> {
    let registry = BUILD_RUN_REGISTRY.lock().unwrap();
    let _ = registry.kill_session(node_id);
    Ok(())
}

/// Subscribe this webview to raw Build/Run PTY bytes for `session_id`.
/// Replaces any previous Channel for the same session. Fast in-memory
/// map insert — plain sync command so it does not occupy a tokio worker
/// (issue #1380).
#[tauri::command]
pub fn subscribe_build_run_output(session_id: i64, on_chunk: Channel<InvokeResponseBody>) {
    crate::pty::sink::BUILD_RUN.register(session_id, on_chunk);
}

/// Drop the binary Channel for `session_id`. Idempotent.
#[tauri::command]
pub fn unsubscribe_build_run_output(session_id: i64) {
    crate::pty::sink::BUILD_RUN.unregister(session_id);
}

/// Forward user keystrokes to the live build/run PTY. Currently meaningful
/// only for `BuildRunMode::Terminal`; build/run ignores input.
/// Mirrors `agent::write_to_agent` (`src-tauri/src/commands/agent.rs:291`).
#[tauri::command]
pub fn write_to_build_run(node_id: i64, data: String) -> Result<(), String> {
    let registry = BUILD_RUN_REGISTRY.lock().unwrap();
    registry.write_bytes(node_id, data.as_bytes())
}

/// Resize the live build/run PTY to the given terminal grid size.
/// Mirrors `agent::resize_agent` (`src-tauri/src/commands/agent.rs:286`).
#[tauri::command]
pub fn resize_build_run(node_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    let registry = BUILD_RUN_REGISTRY.lock().unwrap();
    registry.resize_pty(node_id, cols, rows)
}

// ---------------------------------------------------------------------------
// Path decision
// ---------------------------------------------------------------------------

/// Where the build/run shell should be spawned for `node`.
///
/// Delegates to the canonical Node Working Directory rule
/// (`env::node_working_path`), which gates on `node.use_worktree` +
/// non-empty trimmed `worktree_name` — NOT on `mesh.use_worktree`. The
/// previous inline implementation used `row.use_worktree`, which broke for
/// Root Nodes spawned in a worktree-enabled mesh (`SpawnButtonCluster`'s
/// alt-click path bypasses the mesh setting and persists `use_worktree=false`
/// on the node row). The resulting `spawn_worktree_name = None` then hit
/// `validate_worktree_exists` and surfaced as "No worktree name set for this
/// agent node. Spawn the agent first to create a worktree." — exactly the
/// regression the user reported for Build/Run/Terminal on a Root Node.
fn resolve_build_run_cwd(node: &crate::models::AgentNode) -> env::ResolvedPath {
    env::node_working_path(node)
}

/// Resolve the command string for a `(mode, context)` pair (issue #802).
///
/// Root context (`is_root == true`) prefers the per-context
/// `root_build_command` / `root_run_command` and falls back to
/// `build_command` / `run_command`; worktree context always uses the latter.
/// Terminal mode needs no command, so it resolves to `Some("")` — never
/// `None` — and callers spawn an interactive shell instead of running a
/// one-shot command.
///
/// The fallback (`.or(build_command)`) preserves PR #801's behaviour: a mesh
/// that never sets the `root_*` columns runs the same command in both
/// contexts, exactly as before this feature.
fn resolve_build_run_command(mode: BuildRunMode, is_root: bool, row: &MeshRow) -> Option<&str> {
    match (mode, is_root) {
        (BuildRunMode::Build, false) => row.build_command.as_deref(),
        (BuildRunMode::Build, true) => row
            .root_build_command
            .as_deref()
            .or(row.build_command.as_deref()),
        (BuildRunMode::Run, false) => row.run_command.as_deref(),
        (BuildRunMode::Run, true) => row
            .root_run_command
            .as_deref()
            .or(row.run_command.as_deref()),
        (BuildRunMode::Terminal, _) => Some(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AgentNode;

    fn node_fixture(use_worktree: bool, worktree_name: Option<&str>) -> AgentNode {
        AgentNode {
            path: "/home/user/my-repo".to_string(),
            worktree_name: worktree_name.map(str::to_string),
            use_worktree,
            ..Default::default()
        }
    }

    /// Regression: a Root Node (use_worktree=false, worktree_name=None) must
    /// resolve to the mesh root even when the mesh has use_worktree=true. The
    /// previous inline implementation used `row.use_worktree`, then fed
    /// `spawn_worktree_name = None` into `validate_worktree_exists`, which
    /// surfaced "No worktree name set for this agent node." for any Build/Run/
    /// Terminal click on a Root Node inside a worktree-enabled mesh — the
    /// bug the user reported.
    ///
    /// Asserts on `raw_path` (the input-pass-through POSIX form, mirrored by
    /// the frontend's `getNodeGitPath`) rather than `host_path` — `host_path`
    /// goes through `to_host_path`, which converts a `/home/...` fixture to
    /// a WSL UNC path on Windows. The raw form is the contract this helper
    /// is responsible for (issue #409).
    #[test]
    fn resolve_build_run_cwd_root_node_resolves_to_mesh_root() {
        let resolved = resolve_build_run_cwd(&node_fixture(false, None));
        assert_eq!(resolved.raw_path, "/home/user/my-repo");
        assert!(
            !resolved.raw_path.contains("worktrees"),
            "Root Node raw_path must not contain a worktree subdir, got: {}",
            resolved.raw_path
        );
    }

    /// A stale `worktree_name` on a Root Node is ignored — same canonical
    /// rule `env::node_working_path` uses everywhere else (issue #383).
    #[test]
    fn resolve_build_run_cwd_root_node_ignores_stale_worktree_name() {
        let resolved = resolve_build_run_cwd(&node_fixture(false, Some("stale-name")));
        assert!(
            !resolved.raw_path.contains("worktrees"),
            "stale worktree_name on Root Node must not leak into raw_path: {}",
            resolved.raw_path
        );
    }

    /// A Worktree Node still resolves into its `.claude/worktrees/<name>`
    /// subdir — the canonical behaviour, preserved.
    #[test]
    fn resolve_build_run_cwd_worktree_node_resolves_worktree_subdir() {
        let resolved = resolve_build_run_cwd(&node_fixture(true, Some("gentle-fox")));
        assert_eq!(
            resolved.raw_path,
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// The worktree validator gate must skip Root Nodes — otherwise we'd
    /// call `git2::Repository::open` on the mesh root and look for a
    /// non-existent worktree, tripping the user-facing "Worktree '...' not
    /// found" error. This pins `env::worktree_segment`'s contract at the
    /// call site: `Some(_)` only when the node is a Worktree Node.
    #[test]
    fn worktree_segment_is_some_only_for_worktree_nodes() {
        assert!(env::worktree_segment(&node_fixture(false, None)).is_none());
        assert!(env::worktree_segment(&node_fixture(false, Some("ignored"))).is_none());
        assert_eq!(
            env::worktree_segment(&node_fixture(true, Some("gentle-fox"))),
            Some("gentle-fox"),
        );
        assert!(env::worktree_segment(&node_fixture(true, None)).is_none());
        assert!(env::worktree_segment(&node_fixture(true, Some("   "))).is_none());

        // Trim invariant (issue #383 — Root Node + stale `worktree_name`):
        // the trimmed segment is what the git2 worktree-list compare sees.
        assert_eq!(
            env::worktree_segment(&node_fixture(true, Some("  gentle-fox  "))),
            Some("gentle-fox"),
        );
    }

    #[test]
    fn build_run_mode_serializes_lowercase() {
        for (variant, expected) in [
            (BuildRunMode::Build, "\"build\""),
            (BuildRunMode::Run, "\"run\""),
            (BuildRunMode::Terminal, "\"terminal\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "serialize {:?}", variant);

            let round: BuildRunMode = serde_json::from_str(&json).unwrap();
            assert_eq!(round, variant, "round-trip {:?}", variant);
        }
    }

    #[test]
    fn build_run_registry_write_bytes_to_dead_session() {
        let registry = BuildRunRegistry::new();
        let result = registry.write_bytes(42, b"hello");
        assert!(result.is_err());
        // Frontend matches on this substring to swallow the "not running"
        // case silently — keep the contract stable.
        assert!(result.unwrap_err().contains("not running"));
    }

    #[test]
    fn build_run_registry_resize_pty_to_dead_session() {
        let registry = BuildRunRegistry::new();
        let result = registry.resize_pty(42, 80, 24);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
    }

    // --- Generation-safety tests (issue #1532) ---------------------------
    //
    // These exercise the lifecycle contract: a rapid Build→Build (or any
    // spawn→spawn) replacement must (a) tear down the previous incarnation
    // under the same node_id without leaking the entry, and (b) make the
    // replacement immune to the previous reader thread's late EOF. The
    // tests operate on the registry directly — no real PTY is opened;
    // `dummy_process` constructs a stand-in BuildRunProcess whose
    // child/master slots are empty (so `teardown_inc` no-ops on those
    // paths but still exercises the generation bookkeeping).

    use std::io::Cursor;

    /// Construct a `BuildRunProcess` stand-in for tests. `child` and
    /// `master` are `None` so `teardown_inc` no-ops on them; `writer`
    /// is a real `Cursor<Vec<u8>>` (Send + Write) so `write_bytes` can
    /// actually push bytes through the entry.
    fn dummy_process() -> BuildRunProcess {
        BuildRunProcess {
            generation: 0,
            child: Arc::new(Mutex::new(None)),
            master: Arc::new(Mutex::new(None)),
            writer: Arc::new(Mutex::new(Box::new(Cursor::new(Vec::new())))),
            reader_handle: Mutex::new(None),
        }
    }

    /// Insert assigns strictly-monotonic generation tokens so a stale
    /// reader can never be mistaken for a current one (issue #1532
    /// point #1).
    #[test]
    fn insert_assigns_monotonic_generations() {
        let registry = BuildRunRegistry::new();
        let g1 = registry.insert(-915_1533, dummy_process());
        let g2 = registry.insert(-915_1534, dummy_process());
        let g3 = registry.insert(-915_1533, dummy_process()); // same node_id, new generation
        assert!(g1 < g2, "g2 must be strictly greater than g1");
        assert!(g2 < g3, "g3 must be strictly greater than g2 even on the same node_id");
        assert_ne!(g1, g2);
        assert_ne!(g2, g3);
    }

    /// `insert` returns the generation assigned to the new entry, not
    /// the previous one — the reader thread must capture THIS value to
    /// drive `reap_incarnation`.
    #[test]
    fn insert_returns_assigned_generation() {
        let registry = BuildRunRegistry::new();
        let g1 = registry.insert(-915_1535, dummy_process());
        let arc1 = registry.get(&-915_1535).unwrap();
        assert_eq!(arc1.generation, g1);

        let g2 = registry.insert(-915_1535, dummy_process());
        let arc2 = registry.get(&-915_1535).unwrap();
        assert_eq!(arc2.generation, g2);
        assert_ne!(arc1.generation, arc2.generation);
    }

    /// A previous reader's `remove_if_current` against the SAME
    /// `node_id` must NOT delete a replacement incarnation
    /// (issue #1532 point #3 + AC "stale A exit does not alter B").
    /// This is the canonical race the issue describes.
    #[test]
    fn stale_generation_cannot_remove_replacement() {
        let registry = BuildRunRegistry::new();
        let node_id = -915_1536;

        let g_a = registry.insert(node_id, dummy_process());
        // B replaces A — `insert` tears down A's process handle (no-op
        // on the dummy process) and assigns a fresh generation.
        let g_b = registry.insert(node_id, dummy_process());
        assert_ne!(g_a, g_b);

        // A's reader reaches EOF after B has been published. It calls
        // `reap_incarnation(node_id, g_a)` — must be a no-op so B
        // stays current.
        let reaped = registry.reap_incarnation(node_id, g_a);
        assert!(!reaped, "stale generation must not reap a replacement");
        assert!(
            registry.contains(&node_id),
            "replacement B must survive A's late EOF"
        );
        assert_eq!(
            registry.get(&node_id).unwrap().generation,
            g_b,
            "replacement's generation must be unchanged"
        );

        // B remains writable — the user's keystrokes land on the live
        // PTY (issue #1532 AC "B remains current and writable").
        registry
            .write_bytes(node_id, b"hello from replacement")
            .expect("replacement must accept writes after stale reap");
    }

    /// `reap_incarnation` on a never-inserted generation is a no-op.
    /// Covers the late-EOF-from-old-restart case where the generation
    /// counter has wrapped or was never issued.
    #[test]
    fn reap_incarnation_unknown_generation_is_noop() {
        let registry = BuildRunRegistry::new();
        let node_id = -915_1537;
        let g_real = registry.insert(node_id, dummy_process());

        let reaped = registry.reap_incarnation(node_id, g_real + 999);
        assert!(!reaped);

        // Original entry is untouched.
        assert_eq!(registry.get(&node_id).unwrap().generation, g_real);
    }

    /// `reap_incarnation` returns true and removes the entry when the
    /// generation still matches — the natural-exit happy path the
    /// reader thread takes on its own EOF.
    #[test]
    fn reap_incarnation_current_generation_removes_entry() {
        let registry = BuildRunRegistry::new();
        let node_id = -915_1538;
        let g = registry.insert(node_id, dummy_process());
        assert!(registry.contains(&node_id));

        let reaped = registry.reap_incarnation(node_id, g);
        assert!(reaped, "current-generation reap must succeed");
        assert!(
            !registry.contains(&node_id),
            "current-generation reap must remove the entry"
        );
    }

    /// `kill_session` is a deliberate teardown — mirrors user X-click.
    /// It must reap the current incarnation and return true; subsequent
    /// calls are no-ops (issue #1532 AC "closing current B
    /// removes/reaps B only").
    #[test]
    fn kill_session_tears_down_only_current() {
        let registry = BuildRunRegistry::new();
        let node_id = -915_1539;

        let g_a = registry.insert(node_id, dummy_process());
        let g_b = registry.insert(node_id, dummy_process()); // replaces

        // kill_session removes the CURRENT (B) entry — A is already gone.
        assert!(registry.kill_session(node_id));
        assert!(!registry.contains(&node_id));

        // A's late reap (had it survived in the reader thread) would
        // also be a no-op now.
        assert!(!registry.reap_incarnation(node_id, g_a));
        assert!(!registry.reap_incarnation(node_id, g_b));

        // Calling kill_session on an empty registry is a no-op.
        assert!(!registry.kill_session(node_id));
    }

    /// AC: "Concurrent starts settle on exactly one live generation."
    /// Two sequential inserts on the same node_id end with exactly one
    /// entry — the second insert both (a) assigns a new generation and
    /// (b) tears down the first. After both calls settle, the registry
    /// contains only g_b.
    #[test]
    fn sequential_replacements_leave_one_live_entry() {
        let registry = BuildRunRegistry::new();
        let node_id = -915_1540;

        let _g_a = registry.insert(node_id, dummy_process());
        let _g_b = registry.insert(node_id, dummy_process());
        let _g_c = registry.insert(node_id, dummy_process());

        assert_eq!(registry.processes.len(), 1, "only one entry per node_id");

        // No residue: stale reaps for any earlier generation are silent.
        assert!(!registry.reap_incarnation(node_id, _g_a));
        assert!(!registry.reap_incarnation(node_id, _g_b));
        // Current reap reaps cleanly.
        assert!(registry.reap_incarnation(node_id, _g_c));
        assert!(!registry.contains(&node_id));
    }

    /// AC: "No process/reader remains after repeated replacements."
    /// A three-spawn cycle followed by a final kill_session leaves an
    /// empty registry — no zombie entries, no panics on a follow-up
    /// reap_incarnation.
    #[test]
    fn repeated_replacements_followed_by_close_yields_empty_registry() {
        let registry = BuildRunRegistry::new();
        let node_id = -915_1541;

        for _ in 0..5 {
            let _ = registry.insert(node_id, dummy_process());
        }
        // After the storm, exactly one entry remains.
        assert_eq!(registry.processes.len(), 1);

        assert!(registry.kill_session(node_id));
        assert!(registry.processes.is_empty());

        // Stale reaps do not panic, do not resurrect the entry.
        assert!(!registry.reap_incarnation(node_id, u64::MAX));
        assert!(registry.processes.is_empty());
    }

    /// A first-generation reap, then a second-generation insert, then a
    /// close: the close must operate on B (the current entry), not on
    /// the already-reaped A. Mirrors the user's mental model of "I
    /// clicked Build twice quickly, then X-closed the second one."
    #[test]
    fn reap_then_replace_then_close_only_touches_current() {
        let registry = BuildRunRegistry::new();
        let node_id = -915_1542;

        let g_a = registry.insert(node_id, dummy_process());
        // A naturally exits.
        assert!(registry.reap_incarnation(node_id, g_a));
        assert!(!registry.contains(&node_id));

        // B is inserted after A has been reaped.
        let g_b = registry.insert(node_id, dummy_process());
        assert!(registry.contains(&node_id));

        // User closes — kill_session operates on B.
        assert!(registry.kill_session(node_id));
        assert!(!registry.contains(&node_id));

        // B's reap would now be a no-op (entry already gone).
        assert!(!registry.reap_incarnation(node_id, g_b));
    }

    /// The reader-facing contract: `write_bytes` keeps working on the
    /// replacement AFTER a stale reap. Pinned explicitly because the
    /// original bug (issue #1532) was "B is now missing from the
    /// registry or shown as exited even though it is alive."
    #[test]
    fn replacement_remains_writable_after_stale_reap() {
        let registry = BuildRunRegistry::new();
        let node_id = -915_1543;

        let g_a = registry.insert(node_id, dummy_process());
        let g_b = registry.insert(node_id, dummy_process());
        assert_ne!(g_a, g_b);

        // Stale A reap.
        assert!(!registry.reap_incarnation(node_id, g_a));

        // B is still the live entry; writes still succeed.
        assert!(registry.write_bytes(node_id, b"alive").is_ok());

        // Sanity: B's reap reaps correctly when its own reader reaches EOF.
        assert!(registry.reap_incarnation(node_id, g_b));
        assert!(registry.write_bytes(node_id, b"dead").is_err());
    }

    // --- Per-context command resolution (issue #802) --------------------

    /// Build a `MeshRow` with only the four command columns set — every other
    /// field defaults. Goes through `MeshRow::from` so the mapping is exercised
    /// alongside the resolver.
    fn row(
        build: Option<&str>,
        run: Option<&str>,
        root_build: Option<&str>,
        root_run: Option<&str>,
    ) -> MeshRow {
        MeshRow::from(&crate::models::Mesh {
            build_command: build.map(str::to_string),
            run_command: run.map(str::to_string),
            root_build_command: root_build.map(str::to_string),
            root_run_command: root_run.map(str::to_string),
            ..Default::default()
        })
    }

    /// Worktree Build context always uses `build_command`, even when a
    /// `root_build_command` is set — the root command must not leak into a
    /// Worktree Node's build.
    #[test]
    fn resolve_command_build_worktree_uses_build_command() {
        let r = row(
            Some("npm run build"),
            None,
            Some("cargo build --workspace"),
            None,
        );
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Build, false, &r),
            Some("npm run build")
        );
    }

    /// Root Build context prefers `root_build_command` when set.
    #[test]
    fn resolve_command_build_root_uses_root_build_command() {
        let r = row(
            Some("npm run build"),
            None,
            Some("cargo build --workspace"),
            None,
        );
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Build, true, &r),
            Some("cargo build --workspace")
        );
    }

    /// Root Build with no `root_build_command` falls back to `build_command`
    /// — PR #801's behaviour, unchanged for meshes without the new field.
    #[test]
    fn resolve_command_build_root_falls_back_to_build_command() {
        let r = row(Some("npm run build"), None, None, None);
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Build, true, &r),
            Some("npm run build")
        );
    }

    /// Worktree Run context always uses `run_command`.
    #[test]
    fn resolve_command_run_worktree_uses_run_command() {
        let r = row(None, Some("npm run dev"), None, Some("cargo run -p app"));
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Run, false, &r),
            Some("npm run dev")
        );
    }

    /// Root Run context prefers `root_run_command` when set.
    #[test]
    fn resolve_command_run_root_uses_root_run_command() {
        let r = row(None, Some("npm run dev"), None, Some("cargo run -p app"));
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Run, true, &r),
            Some("cargo run -p app")
        );
    }

    /// Root Run with no `root_run_command` falls back to `run_command`.
    #[test]
    fn resolve_command_run_root_falls_back_to_run_command() {
        let r = row(None, Some("npm run dev"), None, None);
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Run, true, &r),
            Some("npm run dev")
        );
    }

    /// A mesh with no command configured at all resolves to `None` so the
    /// caller surfaces the "not configured" error.
    #[test]
    fn resolve_command_unconfigured_is_none() {
        let r = row(None, None, None, None);
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Build, true, &r),
            None
        );
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Build, false, &r),
            None
        );
        assert_eq!(resolve_build_run_command(BuildRunMode::Run, true, &r), None);
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Run, false, &r),
            None
        );
    }

    /// Terminal mode needs no command in either context — always `Some("")`.
    #[test]
    fn resolve_command_terminal_is_empty_in_both_contexts() {
        let r = row(Some("b"), Some("r"), Some("rb"), Some("rr"));
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Terminal, true, &r),
            Some("")
        );
        assert_eq!(
            resolve_build_run_command(BuildRunMode::Terminal, false, &r),
            Some("")
        );
    }

    /// The start_reader pattern: pump inside `with_batcher`. If the
    /// producer isn't dropped before join, this hangs on EOF.
    #[test]
    fn pump_inside_with_batcher_exits_cleanly_on_reader_eof() {
        let reader: Box<dyn std::io::Read + Send> =
            Box::new(std::io::Cursor::new(b"hello from pty\n"));
        let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let g = got.clone();
        let started = std::time::Instant::now();
        crate::pty::batch::with_batcher(
            move |batch| g.lock().unwrap().extend_from_slice(&batch),
            |tx| {
                let mut r = reader;
                let mut buf = [0u8; crate::pty::batch::PTY_READ_BUF];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let _ = tx.send(buf[..n].to_vec());
                        }
                        Err(_) => break,
                    }
                }
            },
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "reader+batcher hung after PTY EOF — producer was not dropped"
        );
        assert_eq!(&*got.lock().unwrap(), b"hello from pty\n");
    }

    #[test]
    fn production_reader_does_not_emit_json_or_base64() {
        let src = include_str!("build_run.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(
            production.contains("with_batcher"),
            "Build/Run reader must go through pty::batch::with_batcher"
        );
        assert!(
            !production.contains("base64::") && !production.contains("use base64"),
            "production Build/Run PTY bytes must not be Base64-encoded"
        );
        assert!(
            !production.contains("build-run-output-{}"),
            "production Build/Run PTY bytes must not ride the JSON event; that path is test injection only"
        );
        assert!(
            production.contains("subscribe_build_run_output"),
            "Build/Run must expose subscribe_build_run_output"
        );
    }

    #[test]
    fn process_lifecycle_does_not_unregister_build_run_output_subscription() {
        let src = include_str!("build_run.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);

        // Extract the reader thread body. Anchored on `.spawn(move || {`
        // (the open) and the `.map_err(...)?` that wraps the Builder
        // result — the body is the only text between them (issue #1532
        // changed `std::thread::spawn` to `std::thread::Builder` so the
        // previous split markers no longer match).
        //
        // `nth(2)` rather than `nth(1)` because the production text has
        // TWO `.spawn(move || {` patterns: the watchdog in
        // `join_with_timeout` (line ~363) and the actual reader (line
        // ~565). `nth(0)` is before the watchdog, `nth(1)` is between
        // the watchdog and the reader, `nth(2)` is the reader body.
        // Without this offset, the assertion could pass on the
        // watchdog's body and miss a regression in the real reader.
        let reader = production
            .split(".spawn(move || {")
            .nth(2)
            .and_then(|rest| rest.split(".map_err(").next())
            .expect("reader thread body");
        assert!(
            !reader.contains("BUILD_RUN.unregister") && !reader.contains("unregister_output"),
            "PTY reader / EOF must not drop the session-scoped Channel"
        );

        let close = production
            .split("pub async fn close_build_run")
            .nth(1)
            .and_then(|rest| rest.split("#[tauri::command]").next())
            .expect("close_build_run body");
        assert!(
            !close.contains("BUILD_RUN.unregister") && !close.contains("unregister_output"),
            "close_build_run kills the PTY only; Channel unsubscribe is frontend dispose"
        );

        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let agent_node = std::fs::read_to_string(src_root.join("services").join("agent_node.rs"))
            .expect("agent_node.rs");
        let agent_prod = agent_node.split("#[cfg(test)]").next().unwrap_or(&agent_node);
        assert!(
            agent_prod.contains("pty::sink::unregister_node_sinks("),
            "Agent Node deletion must release the Build/Run output subscription via the unified sink helper"
        );
    }
}
