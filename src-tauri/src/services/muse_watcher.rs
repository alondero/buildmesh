//! Passive turn detection for Meta Muse's durable session log (issue #1709).
//!
//! Muse's interactive TUI has no attention-hook *flag* — `muse --help` exposes
//! none — but Muse 1.3.0 **does** ship a claude-compatible plugin hook surface
//! (`Stop`/`Notification`/… in a `.claude-plugin/plugin.json` bundle). It is
//! deliberately not provisioned: installing it writes a global plugin cache and
//! sits at `review_needed` until an explicit `muse plugins approve`, which
//! Buildmesh must not do unattended. See `docs/research/muse-attention-signals.md`
//! §(d) for that probe, including the supported/rejected hook-event vocabulary.
//! (The 1.1.1-era claim that "no hook files exist" was superseded and is recorded
//! there, not here.)
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
//! The same log also records the durable `userInput/*` facts, which arrive as
//! run-scoped events and pair strictly by `prompt_id`:
//!
//! ```json
//! {"payload_type":"runtime.session","payload":{"kind":"run","run_id":"<uuid>",
//!   "event":{"kind":"user_input_prompt_requested","prompt_id":"<uuid>","tool_name":"request_user_input",…}}}
//! {"payload_type":"runtime.session","payload":{"kind":"run","run_id":"<uuid>",
//!   "event":{"kind":"user_input_prompt_settled","prompt_id":"<uuid>","outcome":"answered",…}}}
//! ```
//!
//! `requested` is the agent blocked on a **question**, so it publishes
//! `QuestionRequested` (the node lands in `AwaitingInput`); `settled` publishes
//! `WorkResumed`, which is what actually clears it. Both halves are needed: a
//! watcher that opens the wait but never closes it strands the node in
//! `AwaitingInput` whenever the answer does not clear it by some other path.
//! These are the MSP `userInput/requested` and `userInput/settled`
//! notifications folded to disk.
//!
//! **Launch mode is `SkipPermissions`, but a question is not an approval.**
//! Buildmesh launches with `--disable-approval` (the sandbox flag is issue
//! #1788's separate concern); every observed `approval_disabled` session log
//! carries zero `approval/requested` records, so a tool-approval
//! `PermissionRequested` signal is impossible by construction and is not
//! classified here. `request_user_input` is not a tool approval, so it still
//! fires under `--disable-approval` — observed live in retained 1.3.0 logs —
//! and is the *only* way a Muse node reaches `AwaitingInput`.

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

/// One yielded-control fact read from the durable session log.
///
/// Three distinct transitions share one log and must not collapse into each
/// other: `Completed` is a finished turn (`Ready`), `AwaitingInput` is a run
/// still going with the agent blocked on an unanswered question
/// (`AwaitingInput`), and `Resumed` is the other half of that contract — the
/// question was answered and the agent is working again (`Running`). Emitting
/// the first without the third strands the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnSignal {
    Completed(TurnTerminal),
    AwaitingInput { run_id: String, prompt_id: String },
    Resumed { run_id: String, prompt_id: String },
}

/// Where the node stands with respect to the run in the log.
///
/// "Blocked on a question" is a state, not a sidecar flag: folding the open
/// prompt in here is what lets a reader tell at a glance that the agent is
/// waiting, and it makes "terminal while a question is still open"
/// unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RunState {
    Active,
    /// The agent is blocked on an unanswered `request_user_input` question.
    AwaitingInput {
        prompt_id: String,
    },
    Terminal,
}

/// Stateful session-log classifier which emits each yield once.
///
/// A fact is only meaningful while its run is the current one: if a newer run
/// has already `started`, an older run's terminal or question belongs to a turn
/// the node moved past and must not be published. A run this tracker has never
/// seen is *not* stale — a resumed watcher legitimately starts mid-run.
#[derive(Default)]
pub struct MuseTurnTracker {
    state: Option<RunState>,
    current_run: Option<String>,
    emitted_run: Option<String>,
}

impl MuseTurnTracker {
    /// A record from a run this tracker has already moved past.
    fn is_stale_run(&self, run_id: &str) -> bool {
        self.current_run
            .as_deref()
            .is_some_and(|current| current != run_id)
    }

    fn awaiting(&self) -> Option<&str> {
        match &self.state {
            Some(RunState::AwaitingInput { prompt_id }) => Some(prompt_id),
            _ => None,
        }
    }

    pub fn observe_session_log_line(&mut self, line: &str) -> Option<TurnSignal> {
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
                self.current_run = Some(run_id);
                None
            }
            "terminal" => {
                if self.is_stale_run(&run_id)
                    || self.emitted_run.as_deref() == Some(run_id.as_str())
                {
                    return None;
                }
                self.state = Some(RunState::Terminal);
                self.current_run = Some(run_id.clone());
                self.emitted_run = Some(run_id.clone());
                Some(TurnSignal::Completed(TurnTerminal {
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
                }))
            }
            "user_input_prompt_requested" => {
                let prompt_id = event.get("prompt_id")?.as_str()?.to_string();
                // Same run-scoping the terminal arm applies: a question from a
                // superseded run is not this node's current business.
                if self.is_stale_run(&run_id) || self.awaiting() == Some(prompt_id.as_str()) {
                    return None;
                }
                self.current_run = Some(run_id.clone());
                self.state = Some(RunState::AwaitingInput {
                    prompt_id: prompt_id.clone(),
                });
                Some(TurnSignal::AwaitingInput { run_id, prompt_id })
            }
            "user_input_prompt_settled" => {
                let prompt_id = event.get("prompt_id")?.as_str()?.to_string();
                // Only the question actually outstanding can be answered; a
                // settle for a superseded prompt is not a resumption.
                if self.is_stale_run(&run_id) || self.awaiting() != Some(prompt_id.as_str()) {
                    return None;
                }
                self.state = Some(RunState::Active);
                Some(TurnSignal::Resumed { run_id, prompt_id })
            }
            _ => None,
        }
    }
}

pub(crate) fn report_turn_finished(lines: &[String]) -> bool {
    let mut tracker = MuseTurnTracker::default();
    let mut completed = false;
    for line in lines {
        if let Some(TurnSignal::Completed(turn)) = tracker.observe_session_log_line(line) {
            completed = turn.terminal == "completed";
        }
    }
    // The *final* state decides. A run blocked on an unanswered question has
    // produced no finished turn to report, however many completions precede it
    // — the node is waiting on the user, not finished.
    matches!(tracker.state, Some(RunState::Terminal)) && completed
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
    pending: Option<TurnSignal>,
}

impl SessionLogTail {
    fn from_offset(offset: u64) -> Self {
        Self {
            offset,
            ..Default::default()
        }
    }

    /// Read whatever the log has appended and return the *one* transition that
    /// is current, if any.
    ///
    /// Deliberately a single signal, not a batch: a late observer can see many
    /// turns at once, but the node has one present state. Publishing a stale
    /// completion just before the current question would flash `Ready` and fire
    /// a rename for work that is not done.
    fn read_turns(&mut self, path: &Path) -> Result<Option<TurnSignal>, String> {
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
                // The suffix could carry a run boundary or a question. Keep the
                // candidate pending until we can inspect the complete record.
                return Ok(None);
            }
            self.offset += bytes as u64;
            let Some(signal) = self.tracker.observe_session_log_line(&line) else {
                continue;
            };
            // A question opened and answered inside this same batch was never
            // shown to the user, so neither half of it is news.
            if matches!(signal, TurnSignal::Resumed { .. })
                && matches!(self.pending, Some(TurnSignal::AwaitingInput { .. }))
            {
                self.pending = None;
                continue;
            }
            self.pending = Some(signal);
        }
        // Only a still-current fact may be published: an earlier completion must
        // not mark a node ready while a newer turn is already running, and a
        // question answered later in the batch must not leave the node waiting.
        let stale = match &self.pending {
            Some(TurnSignal::Completed(_)) => {
                !matches!(self.tracker.state, Some(RunState::Terminal))
            }
            Some(TurnSignal::AwaitingInput { prompt_id, .. }) => {
                self.tracker.awaiting() != Some(prompt_id.as_str())
            }
            Some(TurnSignal::Resumed { .. }) => {
                // A resumption only means something if the node was actually
                // waiting; a question reopened since makes it moot.
                matches!(self.tracker.state, Some(RunState::AwaitingInput { .. }))
            }
            None => false,
        };
        if stale {
            self.pending = None;
        }
        Ok(self.pending.take())
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

/// A follow-up is accepted only when this session records its matching run.
/// TUI redraws (including the pasted-content box) are not submission receipts.
#[derive(Debug)]
pub(crate) struct PromptReceipt {
    path: PathBuf,
    offset: u64,
    prompt: String,
}

impl PromptReceipt {
    pub(crate) fn capture(node: &crate::models::AgentNode, prompt: &str) -> Result<Option<Self>, String> {
        let Some(session) = node.cli_session_id.as_deref().filter(|id| !id.is_empty()) else {
            // First-turn delivery precedes session discovery; the initial-turn
            // watcher owns that path. This receipt fences established sessions.
            return Ok(None);
        };
        let directory = crate::env::node_working_path(node).spawn_path;
        let path = session_log_path(session, &directory)
            .ok_or("Muse session log is unavailable; follow-up was not staged")?;
        Self::from_log(path, prompt).map(Some)
    }

    pub(crate) fn from_log(path: PathBuf, prompt: &str) -> Result<Self, String> {
        let offset = std::fs::metadata(&path).map_err(|error| error.to_string())?.len();
        Ok(Self { path, offset, prompt: prompt.replace("\r\n", "\n") })
    }

    pub(crate) fn accepted(&self) -> bool {
        let Ok(mut file) = File::open(&self.path) else { return false; };
        if file.seek(SeekFrom::Start(self.offset)).is_err() { return false; }
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) || !line.ends_with('\n') { return false; }
            let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else { continue; };
            if record["payload_type"] == "runtime.session"
                && record["payload"]["kind"] == "run"
                && record["payload"]["event"]["kind"] == "started"
                && record["payload"]["event"]["prompt"].as_str()
                    .is_some_and(|prompt| prompt.replace("\r\n", "\n") == self.prompt)
            {
                return true;
            }
        }
    }
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
            emit_signal(
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
                emit_signal(
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

fn emit_signal(
    node_id: i64,
    session_id: &str,
    log_path: &Path,
    app: &AppHandle,
    signal: &WorkerSignal,
    turn: Result<Option<TurnSignal>, String>,
) {
    let turn = match turn {
        Ok(turn) => turn,
        Err(error) => {
            tracing::warn!("muse watcher: {error}");
            return;
        }
    };
    let Some(turn) = turn else { return };
    if !signal.is_active() || !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        return;
    }
    let base = crate::agent::session_lifecycle::HookSignalDetail {
        provider: Some("muse".to_string()),
        provider_session_id: Some(session_id.to_string()),
        transcript_path: Some(log_path.to_string_lossy().to_string()),
        signal_health: crate::agent::session_lifecycle::SignalHealth::Ok,
        ..Default::default()
    };
    match turn {
        TurnSignal::Completed(turn) => {
            let detail = crate::agent::session_lifecycle::HookSignalDetail {
                provider_event: Some(format!("session-log:{}", turn.terminal)),
                completion_reason: Some(turn.terminal),
                message: turn.reason,
                ..base
            };
            crate::node_turn::publish_ready(node_id, app, detail);
        }
        TurnSignal::AwaitingInput { prompt_id, .. } => {
            // `QuestionRequested`, not `InputRequired`: the log says exactly
            // what this is. It also *disarms* the output-based autoclear safety
            // net — background output and terminal redraws cannot answer a
            // question, so they must not clear the attention behind the user's
            // back.
            let detail = crate::agent::session_lifecycle::HookSignalDetail {
                kind: Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested),
                provider_event: Some("session-log:user_input_prompt_requested".to_string()),
                message: Some(format!("Muse is waiting on question {prompt_id}")),
                ..base
            };
            // No semantic turn: `request_user_input` is a question, not a
            // tool-approval request, so it must land on the question lifecycle
            // rather than `PermissionRequested`.
            crate::node_turn::publish_with_signal(node_id, app, None, detail);
        }
        TurnSignal::Resumed { prompt_id, .. } => {
            // The other half of the contract. Without this, a node put into
            // `AwaitingInput` stays there whenever the answer does not come
            // through a path that clears attention on its own (the PTY submit
            // event, the autoclear net), stranding it forever. `WorkResumed`
            // writes `Running` and clears the flag, and deliberately skips both
            // the attention-mark and the AI rename.
            let detail = crate::agent::session_lifecycle::HookSignalDetail {
                kind: Some(crate::agent::session_lifecycle::LifecycleKind::WorkResumed),
                provider_event: Some("session-log:user_input_prompt_settled".to_string()),
                message: Some(format!("Muse resumed after question {prompt_id}")),
                ..base
            };
            crate::node_turn::publish_with_signal(node_id, app, None, detail);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_receipt_requires_a_new_complete_matching_run_start() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let record = |kind: &str, prompt: &str| serde_json::json!({
            "payload_type":"runtime.session", "payload":{"kind":"run", "event":{"kind":kind,"prompt":prompt}}
        }).to_string();
        std::fs::write(&path, format!("{}\n", record("started", "fix\nreview"))).unwrap();
        let receipt = PromptReceipt::from_log(path.clone(), "fix\r\nreview").unwrap();
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        for line in ["malformed".into(), record("terminal", "fix\nreview"), record("started", "other task")] {
            writeln!(file, "{line}").unwrap();
            assert!(!receipt.accepted());
        }
        file.write_all(record("started", "fix\nreview").as_bytes()).unwrap();
        assert!(!receipt.accepted(), "partial publication is not an acknowledgement");
        file.write_all(b"\n").unwrap();
        assert!(receipt.accepted());
        drop(file);
        std::fs::remove_file(path).unwrap();
        assert!(!receipt.accepted(), "missing native evidence never falls back to redraw");
    }

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

    /// A `request_user_input` question opening, as Muse 1.3.0 records it.
    fn prompt_requested(run_id: &str, prompt_id: &str) -> String {
        format!(
            r#"{{"payload_type":"runtime.session","payload":{{"kind":"run","run_id":"{run_id}","event":{{"kind":"user_input_prompt_requested","prompt_id":"{prompt_id}","tool_name":"request_user_input","questions":[{{"id":"scope","header":"Scope","question":"How far?","options":[{{"label":"Parity first"}}]}}]}}}}}}"#
        )
    }

    /// The user answering that question; the run resumes.
    fn prompt_settled(run_id: &str, prompt_id: &str) -> String {
        format!(
            r#"{{"payload_type":"runtime.session","payload":{{"kind":"run","run_id":"{run_id}","event":{{"kind":"user_input_prompt_settled","prompt_id":"{prompt_id}","outcome":"answered","answers":[{{"id":"scope","selected_label":"Parity first"}}]}}}}}}"#
        )
    }

    fn completed(signal: &TurnSignal) -> &TurnTerminal {
        match signal {
            TurnSignal::Completed(turn) => turn,
            other => panic!("expected a completed turn, got {other:?}"),
        }
    }

    fn awaiting(signal: &TurnSignal) -> (&str, &str) {
        match signal {
            TurnSignal::AwaitingInput { run_id, prompt_id } => (run_id, prompt_id),
            other => panic!("expected an awaiting-input yield, got {other:?}"),
        }
    }

    fn resumed(signal: &TurnSignal) -> (&str, &str) {
        match signal {
            TurnSignal::Resumed { run_id, prompt_id } => (run_id, prompt_id),
            other => panic!("expected a resumption, got {other:?}"),
        }
    }

    /// Append records to a log the watcher is mid-way through tailing.
    fn append(path: &std::path::Path, text: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("append open");
        file.write_all(text.as_bytes()).expect("append write");
        file.flush().expect("append flush");
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
        let turn = completed(&turn);
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
        let turn = completed(&turn);
        assert_eq!(turn.terminal, "failed");
        assert_eq!(turn.reason.as_deref(), Some("process exited"));
    }

    /// Issue #1709: the durable `userInput/requested` fold. Muse records a
    /// `request_user_input` question as a run-scoped event, and it survives
    /// `--disable-approval` because it is a question, not a tool approval. This
    /// is the only way a Muse node reaches `AwaitingInput`.
    #[test]
    fn an_open_user_input_question_is_an_awaiting_input_yield() {
        let mut tracker = MuseTurnTracker::default();
        tracker.observe_session_log_line(&run_started("run-1"));
        let signal = tracker
            .observe_session_log_line(&prompt_requested("run-1", "p-1"))
            .expect("a question must reach the user");
        assert_eq!(awaiting(&signal), ("run-1", "p-1"));
        // Idempotent: the same durable record must not publish twice.
        assert_eq!(
            tracker.observe_session_log_line(&prompt_requested("run-1", "p-1")),
            None
        );
        // Answering is a transition in its own right — the clear half of the
        // contract. Emitting nothing here would strand the node in
        // `AwaitingInput` whenever the answer does not clear it by other means.
        let resumed_signal = tracker
            .observe_session_log_line(&prompt_settled("run-1", "p-1"))
            .expect("answering a question must resume the node");
        assert_eq!(resumed(&resumed_signal), ("run-1", "p-1"));
        // A later question in the same run is its own yield.
        let second = tracker
            .observe_session_log_line(&prompt_requested("run-1", "p-2"))
            .expect("a second question is a fresh yield");
        assert_eq!(awaiting(&second), ("run-1", "p-2"));
    }

    /// F2: a question from a run the node has already moved past is stale, and
    /// must be rejected exactly as a stale terminal is. Without this an
    /// out-of-order record from an old run would drag a working node back into
    /// `AwaitingInput`.
    #[test]
    fn a_question_from_a_superseded_run_is_rejected() {
        let mut tracker = MuseTurnTracker::default();
        tracker.observe_session_log_line(&run_started("run-2"));
        assert_eq!(
            tracker.observe_session_log_line(&prompt_requested("run-1", "p-old")),
            None,
            "a question from a run the tracker has moved past must not publish"
        );
        // The current run's question still works.
        let signal = tracker
            .observe_session_log_line(&prompt_requested("run-2", "p-new"))
            .expect("the current run's question must publish");
        assert_eq!(awaiting(&signal), ("run-2", "p-new"));
        // ...and a settle for the rejected prompt is not a resumption.
        assert_eq!(
            tracker.observe_session_log_line(&prompt_settled("run-1", "p-old")),
            None
        );
    }

    /// A settled question is not a completion: the run is still in flight, so
    /// the report snapshot must keep refusing to call it a finished turn.
    #[test]
    fn an_open_question_is_not_a_turn_completion() {
        let mut lines = vec![run_started("run-1"), prompt_requested("run-1", "p-1")];
        assert!(
            !report_turn_finished(&lines),
            "a run blocked on a question has produced no finished turn"
        );
        lines.push(prompt_settled("run-1", "p-1"));
        lines.push(run_terminal("run-1", "completed", None));
        assert!(report_turn_finished(&lines));
    }

    /// F1: a completed turn followed by a question in a *later* run. The
    /// completion is real, but it is no longer the current state — the node is
    /// waiting on the user, so reporting "turn finished" here would let a
    /// consumer act on stale evidence.
    #[test]
    fn a_question_after_a_completed_turn_is_not_reported_as_a_finished_turn() {
        let lines = vec![
            run_started("run-1"),
            run_terminal("run-1", "completed", None),
            run_started("run-2"),
            prompt_requested("run-2", "p-1"),
        ];
        assert!(
            !report_turn_finished(&lines),
            "an outstanding question must override an earlier completion"
        );
        // Once answered and that run completes, it is reportable again.
        let mut settled = lines;
        settled.push(prompt_settled("run-2", "p-1"));
        settled.push(run_terminal("run-2", "completed", None));
        assert!(report_turn_finished(&settled));
    }

    /// A question opened *and* answered before the watcher looked is not a
    /// yield: the user is already past it, so the node must not light up — and
    /// must not be told to resume from a wait it never entered.
    #[test]
    fn a_question_answered_within_one_read_batch_publishes_nothing() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!(
                "{}\n{}\n{}\n",
                run_started("run-1"),
                prompt_requested("run-1", "p-1"),
                prompt_settled("run-1", "p-1"),
            ),
        )
        .unwrap();
        let mut tail = SessionLogTail::default();
        assert!(
            tail.read_turns(temp.path()).unwrap().is_none(),
            "a prompt opened and answered in the same batch is not news either way"
        );
    }

    /// The resumption is published when the answer arrives in a later batch,
    /// which is the case that actually strands a node when it is dropped.
    #[test]
    fn answering_a_surfaced_question_publishes_the_resumption() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!(
                "{}\n{}\n",
                run_started("run-1"),
                prompt_requested("run-1", "p-1")
            ),
        )
        .unwrap();
        let mut tail = SessionLogTail::default();
        let surfaced = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("the question is news");
        assert_eq!(awaiting(&surfaced), ("run-1", "p-1"));
        // No new records: nothing re-published.
        assert!(tail.read_turns(temp.path()).unwrap().is_none());

        append(
            temp.path(),
            &format!("{}\n", prompt_settled("run-1", "p-1")),
        );
        let resumed_signal = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("answering a question the user actually saw must resume the node");
        assert_eq!(resumed(&resumed_signal), ("run-1", "p-1"));
    }

    /// A question asked and then the turn ends in one batch: the node is done,
    /// so the completion is the current state and must win.
    #[test]
    fn a_question_followed_by_a_terminal_in_one_batch_publishes_the_completion() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!(
                "{}\n{}\n{}\n",
                run_started("run-1"),
                prompt_requested("run-1", "p-1"),
                run_terminal("run-1", "completed", None),
            ),
        )
        .unwrap();
        let mut tail = SessionLogTail::default();
        let turn = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("the terminal is current");
        assert_eq!(completed(&turn).terminal, "completed");
    }

    /// The open question publishes once, and the run's terminal still reports
    /// normally afterwards — the two transitions are independent.
    #[test]
    fn an_open_question_publishes_and_the_later_terminal_still_completes() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!(
                "{}\n{}\n",
                run_started("run-1"),
                prompt_requested("run-1", "p-1")
            ),
        )
        .unwrap();
        let mut tail = SessionLogTail::default();
        let question = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("the question is news");
        assert_eq!(awaiting(&question), ("run-1", "p-1"));
        // Re-reading the same records must not republish the question.
        assert!(tail.read_turns(temp.path()).unwrap().is_none());

        append(
            temp.path(),
            &format!(
                "{}\n{}\n",
                prompt_settled("run-1", "p-1"),
                run_terminal("run-1", "completed", None)
            ),
        );
        let turn = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("the completion is the current state");
        assert_eq!(completed(&turn).terminal, "completed");
    }

    /// A partial `user_input_prompt_requested` record must not be consumed: it
    /// could be the question the user is staring at.
    #[test]
    fn a_partially_written_question_is_not_published() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), format!("{}\n", run_started("run-1"))).unwrap();
        let mut tail = SessionLogTail::default();
        assert!(tail.read_turns(temp.path()).unwrap().is_none());

        append(temp.path(), prompt_requested("run-1", "p-1").trim_end());
        assert!(
            tail.read_turns(temp.path()).unwrap().is_none(),
            "an unterminated record is not yet a question"
        );

        append(temp.path(), "\n");
        let question = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("the completed record is now a question");
        assert_eq!(awaiting(&question), ("run-1", "p-1"));
    }

    /// Muse nests each subagent's log under
    /// `<session>/subagent/<child-uuid>/session.jsonl`. A node's turn signal must
    /// come from its own top-level log only: a child's turn is not the node's,
    /// and treating it as one would mark the node ready mid-flight. The lookup
    /// reads `…/<day>/<session-id>/session.jsonl` at a fixed depth and does not
    /// recurse, so a nested log is unreachable by *any* id — the strongest form
    /// of the guarantee. If the lookup ever grows a recursive fallback, this test
    /// is what must stop a subagent from hijacking its parent's turn signal.
    #[test]
    fn a_subagent_log_is_unreachable_so_it_can_never_signal_for_its_parent() {
        const PARENT: &str = "01a0c000-0000-7000-8000-000000000000";
        const CHILD: &str = "01a0d000-0000-7000-8000-000000000000";
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("sessions/2026/10/06");
        let parent_log = day.join(PARENT).join("session.jsonl");
        let child_log = day
            .join(PARENT)
            .join("subagent")
            .join(CHILD)
            .join("session.jsonl");
        std::fs::create_dir_all(child_log.parent().unwrap()).unwrap();
        std::fs::write(&parent_log, format!("{}\n", run_started("run-1"))).unwrap();
        std::fs::write(
            &child_log,
            format!("{}\n", run_terminal("child-run", "completed", None)),
        )
        .unwrap();

        // The node's own session resolves to its own log...
        assert_eq!(session_log_path_in(dir.path(), PARENT), Some(parent_log));
        // ...and a subagent id resolves to nothing at all, so a child's
        // completion can never be published as this node's turn.
        assert_eq!(session_log_path_in(dir.path(), CHILD), None);
        assert!(session_log_path_in(dir.path(), "unknown-session").is_none());
    }

    #[test]
    fn tail_defers_a_partial_suffix_until_the_record_completes() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), format!("{}\n", run_started("run-1"))).unwrap();
        let mut tail = SessionLogTail::default();
        assert!(tail.read_turns(temp.path()).unwrap().is_none());

        // An unterminated terminal record must not be consumed.
        append(
            temp.path(),
            r#"{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"run-1","event":{"kind":"terminal","terminal":"completed""#,
        );
        assert!(tail.read_turns(temp.path()).unwrap().is_none());

        append(temp.path(), "}}}\n");
        assert!(tail.read_turns(temp.path()).unwrap().is_some());
        // Re-reading the completed log must not re-publish.
        assert!(tail.read_turns(temp.path()).unwrap().is_none());
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
            tail.read_turns(temp.path()).unwrap().is_none(),
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
        let turn = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("the current completion is published");
        assert_eq!(completed(&turn).run_id, "run-2");
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
        assert!(tail.read_turns(temp.path()).unwrap().is_none());

        append(
            temp.path(),
            &format!(
                "{}\n{}\n",
                run_started("new-run"),
                run_terminal("new-run", "completed", None)
            ),
        );
        let turn = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("the post-spawn turn is the only one published");
        assert_eq!(completed(&turn).run_id, "new-run");
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
        let turn = tail
            .read_turns(temp.path())
            .unwrap()
            .expect("the recorded log's final completion must classify");
        assert_eq!(completed(&turn).terminal, "completed");
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
        assert!(PromptReceipt { path: log.clone(), offset: 0, prompt: "buildmesh live smoke".into() }.accepted(),
            "the installed Muse must record the exact accepted prompt at its native run boundary");
        assert!(!PromptReceipt::from_log(log.clone(), "buildmesh live smoke").unwrap().accepted(),
            "a captured receipt must ignore the already-finished native run");
        let turn = SessionLogTail::default()
            .read_turns(&log)
            .expect("read live session log")
            .expect("the live log must classify exactly one current turn");
        assert_eq!(completed(&turn).terminal, "completed");
    }
}
