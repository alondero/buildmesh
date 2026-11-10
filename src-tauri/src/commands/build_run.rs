//! Build/Run feature — spawns a shell in a worktree and runs build/run commands

use crate::db;
use crate::env;
use crate::models::MeshRow;
use crate::pty::lifecycle::join_with_timeout;
use crate::pty::PtyRegistry;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
// Rust never constructs this: production emits the base64 `String` form and
// the object shape exists so the generated TS type stays in the contract for
// `BuildRunTerminalRegistry`'s `string | BuildRunOutputPayload` union. Deleting
// it would delete `src/types/generated/BuildRunOutputPayload.ts` with it
// (issue #2450).
#[allow(dead_code)]
pub struct BuildRunOutputPayload {
    pub data: String,
}

/// Payload of the per-session `build-run-exited-{sessionId}` Tauri event.
/// Emitted when the PTY reader sees EOF on the build/run shell. The
/// `generation` field identifies which incarnation exited (the same
/// monotonic token [`BuildRunProcess::generation`]). The event name
/// encodes the sessionId; the payload encodes the generation so the
/// frontend can tell whether the exit event applies to the current
/// instance or to a previous incarnation whose late EOF crossed paths
/// with a replacement spawn.
///
/// Generated to `src/types/generated/BuildRunExitedPayload.ts`; the TS half
/// is imported by `src/components/Terminal/BuildRunTerminalRegistry.ts`.
/// The TS half uses `i32` because `ts-rs` does not support `u64`
/// natively in TS — see `ts(as = "i32")` annotation per CLAUDE.md.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "BuildRunExitedPayload.ts")]
pub struct BuildRunExitedPayload {
    #[ts(as = "i32")]
    pub generation: u64,
}

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
    if crate::git::worktree::worktree_registered_at(&resolved.host_path, wt_name)? {
        return Ok(());
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
/// Each entry carries a per-incarnation `generation` token so the
/// natural-exit reader thread can compare-and-remove against it instead
/// of unconditionally deleting whatever happens to be at the same
/// `node_id` (issue #1532). A rapid `Build -> Build` (or `Run -> Run`)
/// replacement drops the previous Arc in `insert`; the previous reader
/// thread is still pumping and will reach EOF when its master is
/// closed, but `remove_if_current(node_id, prev_gen)` must fail to
/// match - the entry now points at the *new* incarnation.
///
/// **Note on `Arc` discipline.** `BuildRunProcess` is held in an
/// `Arc` by the `PtyRegistry`, so every field is reachable via that
/// outer `Arc`. Build/run has no separate worker thread sharing
/// `child`/`master` (unlike `AgentProcess`, which shares them with its
/// child-exit watcher), so the inner fields are plain `Mutex<...>` -
/// no `Arc<Mutex<...>>` onion. Issue #1532 review finding #4.
struct BuildRunProcess {
    /// Per-incarnation token assigned by [`BuildRunRegistry::insert`].
    /// `0` is the "not yet inserted" sentinel (matches `AgentProcess`
    /// convention); callers should pass `0` from struct literals and
    /// let `insert` overwrite.
    generation: u64,
    /// Retained child handle. The previous design `drop(child)`-ed it
    /// immediately after spawn, leaving a stale reader thread's master
    /// with no reaper. `teardown_inc` now `kill_process_tree(pid)` +
    /// `kill()` + `try_wait()`s this so a replacement spawn cannot leak
    /// a process tree (issue #1532 point #5 + review finding #3).
    /// `Option` so teardown can `take()` it; `None` is the "already
    /// reaped" state.
    child: Mutex<Option<Box<dyn Child + Send>>>,
    /// PTY master wrapped in `Option` so teardown can `take()` it and
    /// drop the pseudoconsole. On Windows ConPTY the master read pipe
    /// does not EOF on child exit — closing the master is the only way
    /// to unblock the reader thread (mirrors `AgentProcess`).
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    /// Writer for sending user input to the PTY. Always populated
    /// (even for one-shot build/run) so `write_to_build_run` can be
    /// called for any entry in the registry. Build/run never receives
    /// user input today, but storing the writer is harmless and keeps
    /// the surface uniform.
    writer: Mutex<Box<dyn Write + Send>>,
    /// PTY reader thread handle. `kill_session` joins it with a bounded
    /// timeout so the close path can never hang the UI thread on a
    /// wedged reader (issue #1532 teardown contract; mirrors
    /// `AgentProcess::reader_handle`).
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    /// Set by `teardown_inc` when the reader did not exit within the join
    /// watchdog and was detached. A wedged reader is an expected outcome once
    /// the pty master is closed, but a *teardown that ordered its locks wrongly*
    /// also shows up as this flag, which is how the deadlock regression test
    /// observes it without timing the call.
    reader_join_timed_out: AtomicBool,
    output: crate::pty::sink::OutputProducer,
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

/// Whether teardown should join the PTY reader thread or just detach it.
///
/// `Drop` is the case where the caller IS the reader (natural EOF reaping);
/// joining yourself is a guaranteed self-deadlock. `Join` is every other
/// teardown path (explicit close, replacement spawn).
///
/// **Module-local on purpose** (round-3 review finding #5). `agent::process`
/// has separate reader and writer workers, so it needs an extra `Both` /
/// `WriterOnly` distinction that build_run doesn't. Lifting the enum into
/// `pty::lifecycle` (the round-1 shape) forced one of them to carry an
/// irrelevant variant — exactly the round-3 review's complaint. Each
/// module owns its enum; `pty::lifecycle` exports only the shared
/// [`join_with_timeout`] helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinPolicy {
    /// Wait up to the caller's chosen `timeout` for the reader to exit,
    /// then detach if it hasn't.
    Join,
    /// The caller IS the reader — drop the handle without joining.
    /// `JoinHandle::drop` detaches per stdlib docs.
    Drop,
}

impl BuildRunRegistry {
    fn new() -> Self {
        Self {
            processes: PtyRegistry::new(),
        }
    }

    /// Insert `process` and return `(generation, arc)` so the caller
    /// can stash the reader thread's `JoinHandle` on the EXACT Arc
    /// just inserted.
    ///
    /// **Synchronous teardown of the previous incarnation** (issue
    /// #1532 point #2 + round-4 review finding #1). The issue
    /// explicitly says "stop/join the previous generation before
    /// publishing the replacement" — two concurrent processes in
    /// the same worktree corrupt Cargo.lock, thrash build targets,
    /// and collide on port bindings. Insert tears down the previous
    /// entry *synchronously* (it may take up to the 2 s join
    /// watchdog) before returning the new `Arc`. The caller therefore
    /// only spawns its PTY child AFTER `insert` returns — so the
    /// previous process is fully reaped before the next one writes
    /// to the worktree.
    ///
    /// **Atomic publish** (round-4 review finding #2). The previous
    /// version called `processes.remove(&node_id)` then
    /// `processes.insert(...)` in two separate lock acquisitions.
    /// That opened a race where Thread 2's `remove` saw None and
    /// Thread 1's `insert` then overwrote Thread 2's `insert`, leaving
    /// Thread 2's process orphaned with no teardown. `PtyRegistry::insert`
    /// already returns the previous entry in a single lock acquisition
    /// (see `pty/registry.rs:33`); we use it directly.
    fn insert(&self, node_id: i64, mut process: BuildRunProcess) -> (u64, Arc<BuildRunProcess>) {
        let generation = NEXT_BUILD_RUN_GENERATION.fetch_add(1, Ordering::Relaxed);
        process.generation = generation;
        let arc = Arc::new(process);
        // Atomic publish + previous capture. Returns the prior entry
        // (if any) in the same lock acquisition; no race window.
        let previous = self.processes.insert(node_id, arc.clone());
        // Synchronous teardown BEFORE returning. The 2 s watchdog
        // caps the wait; we cannot defer this to a background
        // thread because the caller will spawn its child immediately
        // after this returns, and the worktree must be free.
        if let Some(prev) = previous {
            teardown_inc(&prev, JoinPolicy::Join);
        }
        (generation, arc)
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

    /// Predicate for the test surface and for any future pre-write check.
    /// Compiled for tests alone, since no production caller asks whether a
    /// session is still registered (issue #2450).
    #[cfg(test)]
    fn contains(&self, node_id: &i64) -> bool {
        self.processes.contains(node_id)
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
        let mut writer = process.writer.lock().unwrap_or_else(|e| e.into_inner());
        writer.write_all(data).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }

    /// Resize the PTY to `cols` x `rows`. Same "not running" semantics
    /// — even if a teardown closed the master out from under us, the
    /// canonical error string is the same so the frontend's catch block
    /// (`if (err !== 'Build run not running') console.error(...)`) stays
    /// silent (issue #1532 review finding #8).
    fn resize_pty(&self, node_id: i64, cols: u16, rows: u16) -> Result<(), String> {
        let process = self
            .processes
            .get(&node_id)
            .ok_or_else(|| "Build run not running".to_string())?;
        let master = process.master.lock().unwrap_or_else(|e| e.into_inner());
        let m = master
            .as_ref()
            .ok_or_else(|| "Build run not running".to_string())?;
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
    *process
        .reader_handle
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(handle);
}

// `PtyRegistry` already wraps its inner `HashMap` in `Arc<Mutex<...>>`,
// making each entry cheap to share and the map thread-safe. The previous
// `Lazy<Arc<BuildRunRegistry>>` stacked a redundant `Arc` on top of
// that, which only made the global a hair more expensive to address
// (review finding #7). The bare `Lazy<BuildRunRegistry>` is enough —
// `BuildRunRegistry::new()` is `const`-ish and `PtyRegistry` already
// owns its concurrency primitives internally.
static BUILD_RUN_REGISTRY: once_cell::sync::Lazy<BuildRunRegistry> =
    once_cell::sync::Lazy::new(BuildRunRegistry::new);

/// Per-node spawn locks (round-5 review finding #1; round-6 polish).
///
/// `build_run_blocking` calls into `BUILD_RUN_REGISTRY.insert` to
/// reap any previous incarnation, then opens a PTY and spawns the
/// new child. Between `insert` and `spawn_command` there is a window
/// of ~20 ms during which the registry holds the new entry with
/// `child: None`. A concurrent `close_build_run` (or replacement
/// insert) arriving during that window runs `teardown_inc` against
/// the hollow entry, finds `child: None`, and tears down NOTHING —
/// then the OS spawn returns and stores a live `Child` into the
/// already-removed entry, leaving an orphan process holding the
/// worktree CWD indefinitely.
///
/// The fix: serialize the entire `build_run` start path per node.
/// Callers acquire the per-node lock with
/// `get_node_spawn_lock(node_id).lock()`; the lock is held for the
/// rest of the function and dropped at function exit. A second
/// `build_run` (or `close_build_run`) on the same node blocks
/// until the first finishes, so the reaping → opening PTY →
/// spawning child → publishing entry sequence is atomic from the
/// registry's perspective.
///
/// Implementation: a global `Mutex<HashMap<i64, Arc<parking_lot
/// ::Mutex<()>>>>` map. The outer map lock is held only for the
/// lookup / insert of the per-node `Arc`; the per-node `Mutex` is
/// acquired after dropping the outer lock so other nodes are not
/// blocked while one node is mid-spawn. `parking_lot::Mutex::lock`
/// returns an owned guard that does NOT borrow from the Mutex, so
/// callers can hold the guard in a plain local without lifetime
/// parameters or `Box::leak` (round-6 review finding: the previous
/// version used `Box::leak(Box::new(inner))` per call, leaking one
/// Box allocation per spawn).
static NODE_SPAWN_LOCKS: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<i64, Arc<parking_lot::Mutex<()>>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Get-or-create the per-node spawn lock. Caller holds the returned
/// `Arc` (keeps the Mutex alive for the caller's scope) and
/// acquires the lock with `.lock()`. Drop on function exit releases
/// the lock and decrements the `Arc` strong count, freeing the
/// per-node Mutex when the last user of this node has dropped
/// their handle. No `Box::leak`, no `'static` contortions.
fn get_node_spawn_lock(node_id: i64) -> Arc<parking_lot::Mutex<()>> {
    let mut map = NODE_SPAWN_LOCKS.lock().unwrap();
    map.entry(node_id)
        .or_insert_with(|| Arc::new(parking_lot::Mutex::new(())))
        .clone()
}

/// Shared teardown for a build/run process incarnation (issue #1532).
/// The caller must already have removed `process` from the registry so
/// only one path owns this Arc — `kill_session` and `insert` (on
/// replacement) call this after `remove`/`insert`; `reap_incarnation`
/// calls this after `remove_if_current`.
///
/// **Mutex-poison handling.** Teardown is the LAST line of defense
/// against leaking a process tree on Windows — if any step aborts,
/// the child shell and its descendants (cargo, npm dev, …) survive
/// as orphans pinning the worktree CWD. Even if a prior lock-holder
/// panicked and poisoned the mutex, `kill_process_tree` + `child.kill`
/// MUST still run — so we use `.unwrap_or_else(|e| e.into_inner())`
/// instead of `.expect("poisoned")` (round-3 review finding #6). The
/// `.expect()` form would propagate the panic and skip every step
/// after the poison site, leaking the process tree. Recovering the
/// inner mutex lets us continue.
fn teardown_inc(process: &BuildRunProcess, join: JoinPolicy) {
    process.output.cancel();
    // 1. Drop the master. `Option::take` removes the `Box<dyn MasterPty>`
    //    from the mutex; the binding falls out of scope and drops it,
    //    closing the pseudoconsole. On Windows ConPTY this is the only
    //    way to EOF the reader thread (mirror of the
    //    `AgentProcess::master` close).
    process
        .master
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();

    // 2. Kill the child handle. We use [`crate::process_util::kill_process_tree`]
    //    FIRST because on Windows `Child::kill` only signals the immediate
    //    shell (`cmd.exe` / `powershell.exe`) — the actual build tool the
    //    shell spawned (`cargo`, `tsc`, `npm run dev`) survives as an
    //    orphan, pinning the worktree directory as its CWD and blocking
    //    later worktree removal. `kill_process_tree` walks the whole tree
    //    via `taskkill /F /T` (Windows) and is a no-op on Unix where
    //    closing the PTY master already `SIGHUP`s the foreground process
    //    group. Issue #1532 review finding #3.
    //
    //    **Only kill if the child is still alive** (round-4 review
    //    finding #5). On natural exit the reader thread sees EOF
    //    and calls `reap_incarnation`; by that point the child has
    //    already exited. Unconditionally shelling out to
    //    `taskkill.exe /F /T /PID <pid>` would block 20-50 ms on
    //    every clean exit AND risk running taskkill against an
    //    innocent recycled PID.
    if let Some(mut child) = process
        .child
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let still_alive = match child.try_wait() {
            Ok(Some(_)) => false, // already exited naturally
            Ok(None) => true,     // still alive
            Err(_) => true,       // unknown — be safe and kill
        };
        if still_alive {
            if let Some(pid) = child.process_id() {
                crate::process_util::kill_process_tree(pid);
            }
            let _ = child.kill();
            let _ = child.try_wait();
        }
        // Already-dead path: skip the taskkill shell-out entirely.
    }

    match join {
        JoinPolicy::Join => {
            if let Some(handle) = process
                .reader_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                // A detached build-run reader is an expected outcome once
                // the pty master is closed, so it is recorded, not an error.
                if !join_with_timeout(handle, std::time::Duration::from_secs(2)) {
                    process.reader_join_timed_out.store(true, Ordering::Release);
                }
            }
        }
        JoinPolicy::Drop => {
            // This thread *is* the reader. Drop the handle without
            // joining — `JoinHandle::drop` detaches.
            drop(
                process
                    .reader_handle
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take(),
            );
        }
    }
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

    // 7. Acquire the per-node spawn lock (round-5 review finding #1).
    //    The lock serializes the entire build_run path: a second
    //    `build_run` on the same node blocks here until this one
    //    finishes — so the "reap previous → spawn child → publish"
    //    sequence is atomic from the registry's perspective. Without
    //    this lock, the round-4 "hollow process" race returned:
    //    `insert` published an entry with `child: None`, then
    //    `spawn_command` ran for ~20 ms while a concurrent
    //    `kill_session` saw `child: None` and tore down nothing,
    //    leaving the live child orphaned.
    let node_lock = get_node_spawn_lock(node_id);
    let _node_guard = node_lock.lock();

    // 8. Reap any previous incarnation FIRST — while holding the
    //    per-node lock, so a concurrent `close_build_run` cannot
    //    interfere. The previous child + tree are dead before we
    //    spawn our own.
    let _ = BUILD_RUN_REGISTRY.kill_session(node_id);

    // 9. Open the PTY pair + clone reader + take writer. Still NO
    //    child spawned.
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open PTY: {}", e))?;

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("failed to clone PTY reader: {}", e))?;

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("failed to take PTY writer: {}", e))?;

    // 10. Spawn the shell INTO the (now-free) worktree. After this,
    //     we have a fully-formed `BuildRunProcess` ready to publish.
    let mut cmd = build_shell_command(mode, command, resolved.env_type);
    cmd.cwd(shell_cwd);
    crate::pty::strip_git_env_vars(&mut cmd);

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn shell: {}", e))?;

    // 11. Publish the FULLY-FORMED entry (child: Some(child),
    //     master: Some, writer, reader_handle: None) to the registry.
    //     No hollow entry is ever observable by other threads.
    let process = BuildRunProcess {
        generation: 0, // overwritten by `insert`
        child: Mutex::new(Some(child)),
        master: Mutex::new(Some(pair.master)),
        writer: Mutex::new(writer),
        reader_handle: Mutex::new(None),
        reader_join_timed_out: AtomicBool::new(false),
        output: crate::pty::sink::BUILD_RUN
            .ensure(node_id)
            .producer(Arc::new(AtomicBool::new(false))),
    };
    let (generation, arc) = BUILD_RUN_REGISTRY.insert(node_id, process);

    // 12. Spawn the reader thread.
    let node_id_clone = node_id;
    let app_handle = app.clone();
    let reader_thread_name = format!("build-run-pty-reader-{node_id_clone}");
    let output = arc.output.clone();
    let spawn_result = std::thread::Builder::new()
        .name(reader_thread_name.clone())
        .spawn(move || {
            reader_thread_body(node_id_clone, generation, reader, app_handle, output);
        });
    let reader_handle = match spawn_result {
        Ok(h) => h,
        Err(e) => {
            // Thread spawn failed (resource exhaustion, etc.).
            // Generation-gated cleanup: only tear down the entry we
            // just inserted. If a concurrent replacement insert has
            // already taken our generation, `remove_if_current` is a
            // no-op (round-4 review finding #4).
            if let Some(orphan) = BUILD_RUN_REGISTRY.remove_if_current(node_id, generation) {
                teardown_inc(&orphan, JoinPolicy::Join);
            }
            return Err(format!(
                "failed to spawn reader thread {reader_thread_name}: {e}"
            ));
        }
    };
    set_reader_handle(&arc, reader_handle);

    Ok(())
}

/// Reader thread body, factored out of `build_run_blocking` so it has
/// a proper function boundary (review finding #6). Reads PTY bytes
/// until EOF / error, then calls [`reap_and_maybe_emit`] which performs
/// the compare-and-remove reaping + the optional exit-event emit.
///
/// **Channel discipline.** Production bytes ride the binary Channel
/// `pty::sink::BUILD_RUN`; the JSON `build-run-output-{id}` event path
/// is test-injection only. The Channel is session-scoped and MUST NOT
/// be unregistered here — that's the frontend dispose path
/// (`unsubscribe_build_run_output`), review finding #6 invariant.
///
/// **Why this is a free function, not a closure.** Two reasons:
/// 1. The reader body needs to be testable in isolation from the
///    `build_run_blocking` flow. With a free function, a unit test
///    can construct a fake `Box<dyn Read>` (a `Cursor<Vec<u8>>`) and
///    invoke `reader_thread_body` directly to verify the EOF
///    reaping + emission path.
/// 2. The previous closure form made the source-scraping test
///    (`process_lifecycle_does_not_unregister_build_run_output_
///    subscription`) brittle: every new thread added elsewhere in the
///    file shifted the `.split(".spawn(move || {").nth(N)` anchor.
///    With the body extracted, we can cover it behaviourally (see
///    `reader_thread_body_does_not_drop_channel`).
fn reader_thread_body(
    node_id: i64,
    generation: u64,
    reader: Box<dyn std::io::Read + Send>,
    app_handle: AppHandle,
    sink: crate::pty::sink::OutputProducer,
) {
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
    reap_and_maybe_emit(node_id, generation, &app_handle);
}

/// Reap the registry entry for `node_id` if `generation` is still
/// current, and emit the `build-run-exited-{node_id}` event iff the
/// reap succeeded. Returns `true` iff this thread's generation was
/// still current.
///
/// The emitted event carries `generation` in the payload so the
/// frontend can verify it matches the current instance (round-5
/// review finding #3: empty payload forced the frontend to rely on
/// ambient map state, which doesn't survive a torn lifecycle).
pub fn reap_and_maybe_emit(node_id: i64, generation: u64, app_handle: &AppHandle) -> bool {
    let reaped = BUILD_RUN_REGISTRY.reap_incarnation(node_id, generation);
    if reaped {
        let _ = app_handle.emit(
            &format!("build-run-exited-{node_id}"),
            BuildRunExitedPayload { generation },
        );
    }
    reaped
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
/// Delegates to [`BuildRunRegistry::kill_session`] which removes the
/// entry from the registry and tears down the underlying process. The
/// reader epilogue's `reap_incarnation` will return false once the
/// entry is gone (the close initiator owns the next state), drops the
/// PTY master to EOF the reader on Windows ConPTY, and joins the
/// reader with a 2 s timeout so a wedged reader cannot hang the UI
/// thread. Issue #1532.
///
/// **Per-node spawn lock** (round-6 review polish). Without acquiring
/// the same per-node lock `build_run` holds during its start path,
/// `close_build_run` would race against an in-flight spawn: between
/// `build_run`'s "reap previous" and "publish new entry" steps the
/// registry is briefly empty, so a concurrent `close_build_run`
/// would find nothing to kill and return `Ok`, while `build_run`
/// then inserts the new entry — the user's click is silently lost.
/// Acquiring the lock here serialises close vs. spawn so close sees
/// the live entry (or waits for the spawn to finish, then sees an
/// empty registry which is the expected end-state).
///
/// **Command Threading.** The body is sync and may take up to ~2 s
/// (kill_process_tree + the join watchdog). It MUST NOT run on a tokio
/// worker — that park stalls every other async command. Routed through
/// `crate::commands::run_blocking` (review finding #1).
#[tauri::command]
pub async fn close_build_run(node_id: i64) -> Result<(), String> {
    crate::commands::run_blocking("close_build_run", move || {
        let node_lock = get_node_spawn_lock(node_id);
        let _node_guard = node_lock.lock();
        let _ = BUILD_RUN_REGISTRY.kill_session(node_id);
        Ok(())
    })
    .await
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
pub fn unsubscribe_build_run_output(session_id: i64, channel_id: Option<u32>) {
    if let Some(channel_id) = channel_id {
        crate::pty::sink::BUILD_RUN.unregister_channel(session_id, channel_id);
    } else {
        crate::pty::sink::BUILD_RUN.unregister(session_id);
    }
}

/// Forward user keystrokes to the live build/run PTY. Currently meaningful
/// only for `BuildRunMode::Terminal`; build/run ignores input.
/// Mirrors `agent::write_to_agent` (`src-tauri/src/commands/agent.rs:291`).
#[tauri::command]
pub fn write_to_build_run(node_id: i64, data: String) -> Result<(), String> {
    BUILD_RUN_REGISTRY.write_bytes(node_id, data.as_bytes())
}

/// Resize the live build/run PTY to the given terminal grid size.
/// Mirrors `agent::resize_agent` (`src-tauri/src/commands/agent.rs:286`).
#[tauri::command]
pub fn resize_build_run(node_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    BUILD_RUN_REGISTRY.resize_pty(node_id, cols, rows)
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
#[path = "build_run_tests.rs"]
mod tests;
