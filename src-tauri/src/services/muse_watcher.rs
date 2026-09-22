//! Passive turn detection for Meta Muse's durable session log (issue #1709).
//!
//! Muse 1.1.1 exposes no interactive attention-hook registration: `muse --help`
//! has no hook/event flag and there is no workspace or global hook config file
//! (unlike AGY's `.agents/hooks.json`, Codex's `.codex/hooks.json`, or Grok's
//! global HTTP hooks). Its MSP method index (`muse schema generate-json-schema`)
//! proves the agent loop *has* `turn/*`, `approval/*`, and `userInput/*` events,
//! but those arrive only on the `muse serve` stdio plane — a separate headless
//! architecture that does not render the interactive TUI Buildmesh PTY-spawns.
//!
//! What the interactive TUI *does* leave behind is the durable session log at
//! `~/.local/share/muse/sessions/YYYY/MM/DD/<uuid>/session.jsonl` (unless
//! launched with `--no-session-log`, which Buildmesh never passes). That log
//! appends one `runtime.session` record per run boundary:
//!
//! ```json
//! {"payload_type":"runtime.session","payload":{
//!   "kind":"run","run_id":"<uuid>","event":{"kind":"started","prompt":"…"}}}
//! {"payload_type":"runtime.session","payload":{
//!   "kind":"run","run_id":"<uuid>","event":{"kind":"terminal","terminal":"completed","reason":null,"turn_duration_ms":1027}}}
//! ```
//!
//! `event.kind == "terminal"` is the `turn/completed` fact. This watcher tails
//! that file and publishes a Node Turn on each terminal record, giving Muse the
//! same turn signal the Command Code transcript watcher provides.
//!
//! **Launch mode is `SkipPermissions`.** Buildmesh launches with
//! `--disable-approval` (the sandbox flag is issue #1788's separate concern); every
//! observed `approval_disabled` session log carries
//! zero `approval/requested` records, so a `PermissionRequested` signal is
//! impossible by construction and is deliberately not classified here.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tauri::AppHandle;

struct ActiveWatcher {
    session_id: String,
    /// Keeps notify's backend and callback alive for this node.
    _watcher: RecommendedWatcher,
    /// Coordinates activation and teardown without polling from the worker.
    signal: Arc<WorkerSignal>,
}

struct WorkerState {
    active: bool,
    activated: bool,
}

struct WorkerSignal {
    state: Mutex<WorkerState>,
    wake: Condvar,
}

struct ActivationGuard {
    node_id: i64,
    armed: bool,
}

impl ActivationGuard {
    fn new(node_id: i64, resume: bool) -> Self {
        Self {
            node_id,
            armed: resume,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ActivationGuard {
    fn drop(&mut self) {
        if self.armed {
            clear_activation(self.node_id);
        }
    }
}

impl WorkerSignal {
    fn new(activated: bool) -> Self {
        Self {
            state: Mutex::new(WorkerState {
                active: true,
                activated,
            }),
            wake: Condvar::new(),
        }
    }

    fn cancel(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = false;
            self.wake.notify_all();
        }
    }

    fn activate(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.activated = true;
            self.wake.notify_all();
        }
    }

    fn wait_until_ready(&self, node_id: i64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        while state.active
            && (!state.activated || !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id))
        {
            state = match self.wake.wait(state) {
                Ok(state) => state,
                Err(_) => return false,
            };
        }
        state.active
    }

    fn is_active(&self) -> bool {
        self.state.lock().map(|state| state.active).unwrap_or(false)
    }
}

static WATCHERS: once_cell::sync::Lazy<Mutex<HashMap<i64, ActiveWatcher>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

/// Nodes whose initial `Spawning` lifecycle write has completed. Kept
/// independently of a watcher: a resumed watcher is armed before launch and
/// must not consume records until the lifecycle sink is available.
static ACTIVATED_NODES: once_cell::sync::Lazy<Mutex<HashSet<i64>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashSet::new()));

/// A durable `run/terminal` record — the Muse turn boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnTerminal {
    pub run_id: String,
    /// `completed` | `failed` | `cancelled` (Muse's `TurnTerminal`, open enum).
    pub terminal: String,
    /// The runtime's free-text terminal reason, when present.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunState {
    Active,
    Terminal,
}

/// Stateful session-log classifier which emits each terminal turn once.
///
/// A terminal is only meaningful while its run is the current one: if a newer
/// run has already `started`, the older terminal belongs to a turn the node
/// moved past and must not be published.
#[derive(Default)]
pub struct MuseTurnTracker {
    state: Option<RunState>,
    emitted_run: Option<String>,
}

impl MuseTurnTracker {
    pub fn observe_session_log_line(&mut self, line: &str) -> Option<TurnTerminal> {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("payload_type")?.as_str()? != "runtime.session" {
            return None;
        }
        let payload = value.get("payload")?;
        if payload.get("kind")?.as_str()? != "run" {
            return None;
        }
        let event = payload.get("event")?;
        let run_id = payload.get("run_id")?.as_str()?.to_string();
        match event.get("kind")?.as_str()? {
            "started" => {
                self.state = Some(RunState::Active);
                None
            }
            "terminal" => {
                self.state = Some(RunState::Terminal);
                if self.emitted_run.as_deref() == Some(run_id.as_str()) {
                    return None;
                }
                self.emitted_run = Some(run_id.clone());
                Some(TurnTerminal {
                    run_id,
                    terminal: event
                        .get("terminal")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("completed")
                        .to_string(),
                    reason: event
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                })
            }
            _ => None,
        }
    }
}

/// Incremental reader for an append-only Muse session log.
///
/// It leaves an unterminated final line in place for a later retry: `notify`
/// can fire while Muse is still writing a record, and consuming that fragment
/// would make a terminal turn disappear permanently.
#[derive(Default)]
struct SessionLogTail {
    offset: u64,
    tracker: MuseTurnTracker,
    pending: Option<TurnTerminal>,
}

impl SessionLogTail {
    fn from_offset(offset: u64) -> Self {
        Self {
            offset,
            ..Default::default()
        }
    }

    fn read_turns(&mut self, path: &Path) -> Result<Vec<TurnTerminal>, String> {
        let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let file_len = file
            .metadata()
            .map_err(|e| format!("stat {}: {e}", path.display()))?
            .len();
        if file_len < self.offset {
            self.offset = 0;
            self.tracker = MuseTurnTracker::default();
            self.pending = None;
        }

        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(self.offset))
            .map_err(|e| format!("seek {}: {e}", path.display()))?;
        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            if bytes == 0 {
                break;
            }
            if !line.ends_with('\n') {
                // The suffix could carry a new run boundary. Keep the
                // candidate pending until we can inspect the complete record.
                return Ok(vec![]);
            }
            self.offset += bytes as u64;
            if let Some(turn) = self.tracker.observe_session_log_line(&line) {
                self.pending = Some(turn);
            }
        }
        // A late observer can read several turns at once. Only the current
        // terminal state may be published; an earlier completion must not mark
        // a node ready while a newer turn is already running.
        if !matches!(self.tracker.state, Some(RunState::Terminal)) {
            self.pending = None;
        }
        Ok(self.pending.take().into_iter().collect())
    }
}

/// Resolve the host-side path to a session's durable log. Muse's index maps a
/// session UUID to its log path, but a live session has no index row yet, so
/// the lookup also scans the on-disk session tree
/// ([`crate::services::muse_sessions`]).
fn session_log_path(session_id: &str, spawn_path: &str) -> Option<PathBuf> {
    let home = crate::services::muse_sessions::data_root(spawn_path)?;
    session_log_path_in(&home, session_id)
}

/// Resolve a session log against an already-resolved Muse data root. Split out
/// so the lookup is unit-testable without a real WSL home.
fn session_log_path_in(home: &Path, session_id: &str) -> Option<PathBuf> {
    crate::services::muse_sessions::log_path(home, session_id)
}

/// Start observing a fresh Muse session from the beginning of its log. Its
/// first turn may already have completed before capture; replaying the log
/// surfaces that terminal instead of losing it.
pub fn start_for_session(
    node_id: i64,
    session_id: &str,
    spawn_path: &str,
    app: &AppHandle,
) -> Result<(), String> {
    if WATCHERS
        .lock()
        .map_err(|_| "Muse watcher registry lock poisoned".to_string())?
        .get(&node_id)
        .is_some_and(|watcher| watcher.session_id == session_id)
    {
        return Ok(());
    }
    let log_path = session_log_path(session_id, spawn_path)
        .ok_or_else(|| format!("no Muse session log for {session_id}"))?;
    start(node_id, session_id, log_path, app, None)
}

/// Start observing a resumed session from its exact pre-spawn EOF. The offset
/// is captured before the child starts, so a prior completed turn is never
/// replayed as the resumed node's own signal, and a fast first resumed turn is
/// still caught.
pub fn start_for_resumed_session(
    node_id: i64,
    session_id: &str,
    spawn_path: &str,
    app: &AppHandle,
) -> Result<(), String> {
    let log_path = session_log_path(session_id, spawn_path)
        .ok_or_else(|| format!("no Muse session log for {session_id}"))?;
    let offset = std::fs::metadata(&log_path)
        .map_err(|e| format!("stat {}: {e}", log_path.display()))?
        .len();
    start(node_id, session_id, log_path, app, Some(offset))
}

/// Async boundary for resume setup. The exact pre-spawn snapshot and watcher
/// registration stay synchronous on the blocking pool before the child launches.
pub async fn start_for_resumed_session_async(
    node_id: i64,
    session_id: &str,
    spawn_path: &str,
    app: AppHandle,
) -> Result<(), String> {
    let session_id = session_id.to_string();
    let spawn_path = spawn_path.to_string();
    crate::blocking::run_blocking("muse watcher resume", move || {
        start_for_resumed_session(node_id, &session_id, &spawn_path, &app)
    })
    .await
}

fn start(
    node_id: i64,
    session_id: &str,
    log_path: PathBuf,
    app: &AppHandle,
    initial_offset: Option<u64>,
) -> Result<(), String> {
    // Fresh discovery runs after spawn activation. A transient file/watch
    // failure must not erase that milestone and strand the next retry.
    let mut activation_guard = ActivationGuard::new(node_id, initial_offset.is_some());
    if !log_path.is_file() {
        return Err(format!(
            "Muse session log does not exist: {}",
            log_path.display()
        ));
    }
    // Fresh-session capture happens after the PTY reader has registered. Do
    // not attach a replaying watcher to a process that already exited while
    // the capture poll was in flight. Resume watchers intentionally start
    // before registration and are handled by the worker's wait below.
    if initial_offset.is_none() && !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        return Err(format!("Muse node {node_id} is no longer running"));
    }

    let (tx, rx) = mpsc::sync_channel(1);
    let path_for_callback = log_path.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result| match result {
            Ok(_) => {
                let _ = tx.try_send(());
            }
            Err(error) => tracing::warn!(
                "muse watcher: notify error for {}: {error}",
                path_for_callback.display()
            ),
        },
        Config::default().with_poll_interval(std::time::Duration::from_secs(2)),
    )
    .map_err(|e| format!("create Muse watcher: {e}"))?;
    watcher
        .watch(&log_path, RecursiveMode::NonRecursive)
        .map_err(|e| format!("watch {}: {e}", log_path.display()))?;

    let Some(signal) = register_watcher(node_id, session_id, watcher, initial_offset.is_some())?
    else {
        activation_guard.disarm();
        return Ok(());
    };

    // Close the registration/reader-exit race: if the process died between the
    // first liveness check and map insertion, cancel this exact watcher before
    // its worker can replay the terminal log.
    if initial_offset.is_none() && !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        stop_if_current(node_id, &signal, false);
        return Err(format!("Muse node {node_id} is no longer running"));
    }

    let app = app.clone();
    let path_for_worker = log_path.clone();
    let session_id = session_id.to_string();
    let worker_signal = signal.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("muse-watcher-{node_id}"))
        .spawn(move || {
            // A resumed watcher is installed immediately before child spawn, so
            // wait until the process is registered before consuming its
            // post-baseline records.
            if !worker_signal.wait_until_ready(node_id) {
                return;
            }

            let mut tail = initial_offset
                .map(SessionLogTail::from_offset)
                .unwrap_or_default();
            emit_turns(
                node_id,
                &session_id,
                &path_for_worker,
                &app,
                &worker_signal,
                tail.read_turns(&path_for_worker),
            );

            while rx.recv().is_ok() {
                while rx.try_recv().is_ok() {}
                if !worker_signal.is_active() {
                    return;
                }
                emit_turns(
                    node_id,
                    &session_id,
                    &path_for_worker,
                    &app,
                    &worker_signal,
                    tail.read_turns(&path_for_worker),
                );
            }
        })
    {
        stop_if_current(node_id, &signal, initial_offset.is_none());
        return Err(format!("start Muse watcher worker: {error}"));
    }
    activation_guard.disarm();
    Ok(())
}

/// Lock order matches activate/stop: activation before insertion is remembered,
/// and activation after insertion wakes this exact worker.
fn register_watcher(
    node_id: i64,
    session_id: &str,
    watcher: RecommendedWatcher,
    resume: bool,
) -> Result<Option<Arc<WorkerSignal>>, String> {
    let activated_nodes = ACTIVATED_NODES
        .lock()
        .map_err(|_| "Muse activation registry lock poisoned".to_string())?;
    let mut watchers = WATCHERS
        .lock()
        .map_err(|_| "Muse watcher registry lock poisoned".to_string())?;
    if !resume {
        if let Some(existing) = watchers.get(&node_id) {
            return if existing.session_id == session_id {
                Ok(None)
            } else {
                Err(format!(
                    "Muse node {node_id} already observes a different session"
                ))
            };
        }
    }
    let signal = Arc::new(WorkerSignal::new(activated_nodes.contains(&node_id)));
    if let Some(previous) = watchers.insert(
        node_id,
        ActiveWatcher {
            session_id: session_id.to_string(),
            _watcher: watcher,
            signal: signal.clone(),
        },
    ) {
        previous.signal.cancel();
    }
    Ok(Some(signal))
}

/// Stop a node's watcher. Dropping the watcher closes its sender, so the worker
/// exits after completing any already-received event.
pub fn stop(node_id: i64) {
    if let Ok(mut activated_nodes) = ACTIVATED_NODES.lock() {
        activated_nodes.remove(&node_id);
        if let Ok(mut watchers) = WATCHERS.lock() {
            if let Some(watcher) = watchers.remove(&node_id) {
                watcher.signal.cancel();
            }
        }
    }
}

fn clear_activation(node_id: i64) {
    if let Ok(mut activated_nodes) = ACTIVATED_NODES.lock() {
        activated_nodes.remove(&node_id);
    }
}

/// Permit a pre-spawn resume watcher to drain log records. This must follow the
/// orchestrator's `on_spawn_started` write so a detected terminal turn cannot
/// be overwritten by the initial `Spawning` status.
pub fn activate(node_id: i64) {
    if let Ok(mut activated_nodes) = ACTIVATED_NODES.lock() {
        activated_nodes.insert(node_id);
        if let Ok(watchers) = WATCHERS.lock() {
            if let Some(watcher) = watchers.get(&node_id) {
                watcher.signal.activate();
            }
        }
    }
}

fn stop_if_current(node_id: i64, signal: &Arc<WorkerSignal>, preserve_activation: bool) {
    if let Ok(mut activated_nodes) = ACTIVATED_NODES.lock() {
        if let Ok(mut watchers) = WATCHERS.lock() {
            let is_current = watchers
                .get(&node_id)
                .is_some_and(|watcher| Arc::ptr_eq(&watcher.signal, signal));
            if is_current {
                if !preserve_activation {
                    activated_nodes.remove(&node_id);
                }
                if let Some(watcher) = watchers.remove(&node_id) {
                    watcher.signal.cancel();
                }
            }
        }
    }
}

fn emit_turns(
    node_id: i64,
    session_id: &str,
    log_path: &Path,
    app: &AppHandle,
    signal: &WorkerSignal,
    turns: Result<Vec<TurnTerminal>, String>,
) {
    let turns = match turns {
        Ok(turns) => turns,
        Err(error) => {
            tracing::warn!("muse watcher: {error}");
            return;
        }
    };
    for turn in turns {
        if !signal.is_active() || !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
            return;
        }
        let detail = crate::agent::session_lifecycle::HookSignalDetail {
            provider: Some("muse".to_string()),
            provider_event: Some(format!("session-log:{}", turn.terminal)),
            provider_session_id: Some(session_id.to_string()),
            completion_reason: Some(turn.terminal.clone()),
            transcript_path: Some(log_path.to_string_lossy().to_string()),
            signal_health: crate::agent::session_lifecycle::SignalHealth::Ok,
            message: turn.reason.clone(),
            ..Default::default()
        };
        crate::node_turn::publish_ready(node_id, app, detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn run_started(run_id: &str) -> String {
        format!(
            r#"{{"payload_type":"runtime.session","payload":{{"kind":"run","run_id":"{run_id}","event":{{"kind":"started","prompt":"hi"}}}}}}"#
        )
    }

    fn run_terminal(run_id: &str, terminal: &str, reason: Option<&str>) -> String {
        let reason = match reason {
            Some(r) => format!("\"{r}\""),
            None => "null".to_string(),
        };
        format!(
            r#"{{"payload_type":"runtime.session","payload":{{"kind":"run","run_id":"{run_id}","event":{{"kind":"terminal","terminal":"{terminal}","reason":{reason},"turn_duration_ms":1027}}}}}}"#
        )
    }

    #[test]
    fn watcher_registration_is_idempotent_and_old_teardown_cannot_stop_replacement() {
        let id = 9_870_002;
        let watcher = || RecommendedWatcher::new(|_| {}, Config::default()).unwrap();
        activate(id);
        let first = register_watcher(id, "session", watcher(), false)
            .unwrap()
            .unwrap();
        assert!(first.state.lock().unwrap().activated);
        assert!(register_watcher(id, "session", watcher(), false)
            .unwrap()
            .is_none());
        assert!(first.is_active());
        assert!(register_watcher(id, "different", watcher(), false).is_err());
        let replacement = register_watcher(id, "session", watcher(), true)
            .unwrap()
            .unwrap();
        assert!(!first.is_active());
        stop_if_current(id, &first, false);
        assert!(replacement.is_active());
        stop(id);
        assert!(!replacement.is_active());
        assert!(!WATCHERS.lock().unwrap().contains_key(&id));
        assert!(!ACTIVATED_NODES.lock().unwrap().contains(&id));
    }

    #[test]
    fn classifier_emits_a_completed_terminal_once() {
        let mut tracker = MuseTurnTracker::default();
        assert_eq!(
            tracker.observe_session_log_line(&run_started("run-1")),
            None
        );
        let turn = tracker
            .observe_session_log_line(&run_terminal("run-1", "completed", None))
            .expect("terminal emits a turn");
        assert_eq!(turn.run_id, "run-1");
        assert_eq!(turn.terminal, "completed");
        assert_eq!(turn.reason, None);
        // The same durable record must not publish a second turn.
        assert_eq!(
            tracker.observe_session_log_line(&run_terminal("run-1", "completed", None)),
            None
        );
    }

    #[test]
    fn classifier_ignores_task_and_non_terminal_records() {
        let mut tracker = MuseTurnTracker::default();
        // A task-level terminal is not a run boundary.
        assert_eq!(
            tracker.observe_session_log_line(
                r#"{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"run-1","event":{"kind":"completed","task_id":"t"}}}"#
            ),
            None
        );
        // A metadata record is not a run boundary.
        assert_eq!(
            tracker.observe_session_log_line(
                r#"{"payload_type":"runtime.session.metadata","payload":{"kind":"metadata","record":{}}}"#
            ),
            None
        );
    }

    #[test]
    fn failed_terminal_is_a_turn_carrying_its_reason() {
        let mut tracker = MuseTurnTracker::default();
        tracker.observe_session_log_line(&run_started("run-9"));
        let turn = tracker
            .observe_session_log_line(&run_terminal("run-9", "failed", Some("process exited")))
            .expect("failed terminal yields the node back to the user");
        assert_eq!(turn.terminal, "failed");
        assert_eq!(turn.reason.as_deref(), Some("process exited"));
    }

    #[test]
    fn tail_defers_a_partial_suffix_until_the_record_completes() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!("{}\n", run_started("run-1")),
        )
        .unwrap();
        let mut tail = SessionLogTail::default();
        assert!(tail.read_turns(temp.path()).unwrap().is_empty());

        // An unterminated terminal record must not be consumed.
        std::fs::OpenOptions::new()
            .append(true)
            .open(temp.path())
            .unwrap()
            .write_all(r#"{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"run-1","event":{"kind":"terminal","terminal":"completed""#.as_bytes())
            .unwrap();
        assert!(tail.read_turns(temp.path()).unwrap().is_empty());

        std::fs::OpenOptions::new()
            .append(true)
            .open(temp.path())
            .unwrap()
            .write_all(b"}}}\n")
            .unwrap();
        assert_eq!(tail.read_turns(temp.path()).unwrap().len(), 1);
        // Re-reading the completed log must not re-publish.
        assert!(tail.read_turns(temp.path()).unwrap().is_empty());
    }

    #[test]
    fn late_observer_suppresses_an_earlier_turn_when_a_newer_run_is_active() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!(
                "{}\n{}\n{}\n",
                run_started("run-1"),
                run_terminal("run-1", "completed", None),
                run_started("run-2"),
            ),
        )
        .unwrap();
        let mut tail = SessionLogTail::default();
        assert!(
            tail.read_turns(temp.path()).unwrap().is_empty(),
            "run-1's completion must not mark ready while run-2 is live"
        );
    }

    #[test]
    fn tail_publishes_only_the_most_recent_terminal_when_several_are_read_at_once() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!(
                "{}\n{}\n{}\n{}\n",
                run_started("run-1"),
                run_terminal("run-1", "completed", None),
                run_started("run-2"),
                run_terminal("run-2", "completed", None),
            ),
        )
        .unwrap();
        let mut tail = SessionLogTail::default();
        let turns = tail.read_turns(temp.path()).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].run_id, "run-2");
    }

    #[test]
    fn resume_baseline_ignores_the_pre_spawn_turn_and_reads_the_new_one() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!(
                "{}\n{}\n",
                run_started("old-run"),
                run_terminal("old-run", "completed", None),
            ),
        )
        .unwrap();
        let baseline = std::fs::metadata(temp.path()).unwrap().len();
        let mut tail = SessionLogTail::from_offset(baseline);
        assert!(tail.read_turns(temp.path()).unwrap().is_empty());

        std::fs::OpenOptions::new()
            .append(true)
            .open(temp.path())
            .unwrap()
            .write_all(format!("{}\n{}\n", run_started("new-run"), run_terminal("new-run", "completed", None)).as_bytes())
            .unwrap();
        let turns = tail.read_turns(temp.path()).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].run_id, "new-run");
    }

    /// Contract guard: the checked-in recorded session log must classify to
    /// exactly one final turn completion. A Muse log-shape change turns this
    /// red instead of silently degrading to no turn signal.
    #[test]
    fn checked_in_muse_fixture_has_one_final_turn_completion() {
        let mut tail = SessionLogTail::default();
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            include_str!("../../tests/fixtures/muse_session_log.jsonl"),
        )
        .unwrap();
        let turns = tail.read_turns(temp.path()).unwrap();
        assert_eq!(turns.len(), 1, "got {turns:?}");
        assert_eq!(turns[0].terminal, "completed");
    }

    /// The log path comes from Muse's `sessions` index. A schema/column rename
    /// would otherwise silently strand every watcher, so pin the lookup: the
    /// matching row resolves, an unknown session does not.
    #[test]
    fn index_lookup_resolves_the_matching_session_log_and_rejects_unknown() {
        const SESSION: &str = "01a08844-cd5c-7f83-8cce-84f84ac52dcf";
        const GUEST_LOG: &str = "/home/u/.local/share/muse/sessions/2026/09/12/01a08844-cd5c-7f83-8cce-84f84ac52dcf/session.jsonl";
        let dir = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(dir.path().join("session-index.db")).unwrap();
        connection
            .execute_batch("CREATE TABLE sessions (session_id TEXT, session_log_path TEXT);")
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions VALUES (?1, ?2)",
                rusqlite::params![SESSION, GUEST_LOG],
            )
            .unwrap();
        drop(connection);

        let resolved = session_log_path_in(dir.path(), SESSION).expect("known session resolves");
        assert_eq!(
            resolved,
            PathBuf::from(crate::env::to_host_path(GUEST_LOG))
        );
        assert!(resolved.ends_with("session.jsonl"));
        assert!(session_log_path_in(dir.path(), "different-session").is_none());
    }

    #[test]
    fn index_lookup_returns_none_without_an_index() {
        let dir = tempfile::tempdir().unwrap();
        assert!(session_log_path_in(dir.path(), "01a0").is_none());
    }

    /// Run 183: a live session is missing from `session-index.db`, so the
    /// watcher must resolve its log from the on-disk session tree. Without this
    /// the node published no turn signal, which also left the circuit wait
    /// unobserved.
    #[test]
    fn session_log_resolves_from_the_session_tree_without_an_index() {
        const SESSION: &str = "01a0c54b-5ed4-7a61-91d7-a7a72c42fe24";
        let dir = tempfile::tempdir().unwrap();
        let log = dir
            .path()
            .join("sessions/2026/09/21")
            .join(SESSION)
            .join("session.jsonl");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, format!("{}\n", run_started("run-1"))).unwrap();

        assert_eq!(session_log_path_in(dir.path(), SESSION), Some(log));
        assert!(session_log_path_in(dir.path(), "different-session").is_none());
    }

    /// Live smoke (issue #1709). Drives the real `muse` binary in a scratch
    /// workspace — offline, because the deterministic `echo` provider makes no
    /// model call — and then asserts the watcher's *own* classifier consumes the
    /// session log that run wrote and emits exactly one completed turn. This
    /// exercises the live data path (live Muse → live `session.jsonl` →
    /// `SessionLogTail` → Node Turn) rather than a recorded fixture.
    ///
    /// The lookup is scoped to the test's own workspace through the production
    /// helpers (`workspace_candidates` + `log_path`, both bounded), and the
    /// resolved session must post-date the launch anchor — so the test cannot
    /// pass against an unrelated pre-existing session log on the same machine.
    ///
    /// `#[ignore]`d because it spawns an external CLI and appends a session log
    /// to the user's real Muse data root. Run it explicitly:
    /// `cargo test --lib muse_watcher::tests::live_muse_turn_smoke -- --ignored --nocapture`
    #[test]
    #[ignore = "spawns the real muse CLI; run explicitly for the #1709 live smoke"]
    fn live_muse_turn_smoke() {
        use std::time::SystemTime;

        let binary = ["muse", "muse.exe"]
            .into_iter()
            .find(|candidate| {
                std::process::Command::new(candidate)
                    .arg("--version")
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false)
            })
            .expect("the live smoke needs the muse CLI on PATH");

        let workspace = tempfile::tempdir().expect("scratch workspace");
        // Muse records a workspace root; initialise a repository so the metadata
        // frame names this exact directory and the lookup below is unambiguous.
        let _ = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(workspace.path())
            .output();

        let anchor_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock")
            .as_millis() as i64;
        let run = std::process::Command::new(binary)
            .current_dir(workspace.path())
            .args(["exec", "--provider", "echo", "buildmesh live smoke"])
            .output()
            .expect("spawn muse exec");
        assert!(
            run.status.success(),
            "muse exec failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );

        // `output()` returns only once the child has exited, so its log is
        // complete: resolve the session it recorded *for this workspace* rather
        // than reading whichever log happens to be newest on the host.
        let workspace_path = workspace.path().to_string_lossy().to_string();
        let root =
            crate::services::muse_sessions::data_root(&workspace_path).expect("muse data root");
        let (session_id, recorded_ms) =
            crate::services::muse_sessions::workspace_candidates(&root, &workspace_path, anchor_ms)
                .into_iter()
                .max_by_key(|(_, recorded_ms)| *recorded_ms)
                .expect("muse must record a session for the test workspace");
        assert!(
            recorded_ms >= anchor_ms,
            "resolved a session recorded before this run ({recorded_ms} < {anchor_ms})"
        );

        let log = crate::services::muse_sessions::log_path(&root, &session_id)
            .expect("session log path");
        let turns = SessionLogTail::default()
            .read_turns(&log)
            .expect("read live session log");
        assert_eq!(
            turns.len(),
            1,
            "the watcher must classify exactly one turn from the live log; got {turns:?}"
        );
        assert_eq!(turns[0].terminal, "completed");
    }
}
