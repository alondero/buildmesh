//! Passive lifecycle detection for Command Code transcripts.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tauri::AppHandle;

use crate::models::EnvType;

struct ActiveWatcher {
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
    fn new(node_id: i64) -> Self {
        Self {
            node_id,
            armed: true,
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

/// Nodes whose initial `Spawning` lifecycle write has completed. This is
/// intentionally kept independently of a watcher: fresh Command Code session
/// discovery can happen after the orchestrator reaches that milestone.
static ACTIVATED_NODES: once_cell::sync::Lazy<Mutex<HashSet<i64>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashSet::new()));

/// A terminal lifecycle transition observed in a Command Code transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTransition {
    TurnCompleted,
    AwaitingInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptActivity {
    UserTurn,
    ToolUse,
    ToolResult,
    AssistantResponse,
    TurnCompleted,
    AwaitingInput,
}

/// Stateful JSONL classifier which emits each terminal transition once.
///
/// Command Code's durable transcript records a user prompt, any tool-use
/// exchange, then the final assistant reply. The final reply is the durable
/// completion boundary; user-shaped tool results are not a second prompt.
#[derive(Default)]
pub struct TurnTracker {
    state: Option<TranscriptActivity>,
    pending_tool_calls: bool,
}

/// Incremental reader for an append-only Command Code JSONL transcript.
///
/// It leaves an unterminated final line in place for a later retry: `notify`
/// can fire while a process is still writing a record, and consuming that
/// fragment would make a terminal transition disappear permanently.
#[derive(Default)]
struct TranscriptTail {
    offset: u64,
    tracker: TurnTracker,
}

impl TranscriptTail {
    fn from_offset(offset: u64) -> Self {
        Self {
            offset,
            tracker: TurnTracker::default(),
        }
    }
    fn read_transitions(&mut self, path: &Path) -> Result<Vec<TerminalTransition>, String> {
        let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let file_len = file
            .metadata()
            .map_err(|e| format!("stat {}: {e}", path.display()))?
            .len();
        if file_len < self.offset {
            self.offset = 0;
            self.tracker = TurnTracker::default();
        }

        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(self.offset))
            .map_err(|e| format!("seek {}: {e}", path.display()))?;
        let mut transitions = Vec::new();
        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            if bytes == 0 {
                break;
            }
            if !line.ends_with('\n') {
                break;
            }
            self.offset += bytes as u64;
            if let Some(transition) = self.tracker.observe_transcript_line(&line) {
                transitions.push(transition);
            }
        }
        Ok(transitions)
    }
}

/// Start observing a fresh Command Code session. Its initial transcript is
/// replayed so a fast first turn cannot outrun session-ID capture.
pub fn start_for_session(
    node_id: i64,
    session_id: &str,
    spawn_path: &str,
    env_type: EnvType,
    app: &AppHandle,
) -> Result<(), String> {
    let sessions_dir = crate::env::commandcode_sessions_dir(env_type, spawn_path)
        .ok_or_else(|| format!("no Command Code sessions directory for {env_type:?}"))?;
    let transcript_path = sessions_dir.join(format!("{session_id}.jsonl"));
    start(node_id, session_id, transcript_path, app, None)
}

/// Start observing a resumed session from its exact pre-spawn EOF. The offset
/// is captured before the child starts, then the watcher reads every complete
/// record appended after it. This avoids both replaying an old completed turn
/// and losing a fast first resumed turn.
pub fn start_for_resumed_session(
    node_id: i64,
    session_id: &str,
    spawn_path: &str,
    env_type: EnvType,
    app: &AppHandle,
) -> Result<(), String> {
    let sessions_dir = crate::env::commandcode_sessions_dir(env_type, spawn_path)
        .ok_or_else(|| format!("no Command Code sessions directory for {env_type:?}"))?;
    let transcript_path = sessions_dir.join(format!("{session_id}.jsonl"));
    let offset = std::fs::metadata(&transcript_path)
        .map_err(|e| format!("stat {}: {e}", transcript_path.display()))?
        .len();
    start(node_id, session_id, transcript_path, app, Some(offset))
}

/// Async boundary for resume setup. The exact pre-spawn snapshot and watcher
/// registration remain synchronous so they are executed atomically on the
/// blocking pool before the child is launched.
pub async fn start_for_resumed_session_async(
    node_id: i64,
    session_id: &str,
    spawn_path: &str,
    env_type: EnvType,
    app: AppHandle,
) -> Result<(), String> {
    let session_id = session_id.to_string();
    let spawn_path = spawn_path.to_string();
    crate::blocking::run_blocking("commandcode watcher resume", move || {
        start_for_resumed_session(node_id, &session_id, &spawn_path, env_type, &app)
    })
    .await
}

fn start(
    node_id: i64,
    session_id: &str,
    transcript_path: PathBuf,
    app: &AppHandle,
    initial_offset: Option<u64>,
) -> Result<(), String> {
    let mut activation_guard = ActivationGuard::new(node_id);
    if !transcript_path.is_file() {
        return Err(format!(
            "Command Code transcript does not exist: {}",
            transcript_path.display()
        ));
    }
    // Fresh-session capture happens after the PTY reader has registered. Do
    // not attach a replaying watcher to a process that already exited while
    // the capture poll was in flight. Resume watchers deliberately start
    // before registration and are handled by the worker's wait below.
    if initial_offset.is_none() && !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        return Err(format!("Command Code node {node_id} is no longer running"));
    }

    let (tx, rx) = mpsc::sync_channel(1);
    let path_for_callback = transcript_path.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result| match result {
            Ok(_) => {
                let _ = tx.try_send(());
            }
            Err(error) => tracing::warn!(
                "commandcode watcher: notify error for {}: {error}",
                path_for_callback.display()
            ),
        },
        Config::default().with_poll_interval(std::time::Duration::from_secs(2)),
    )
    .map_err(|e| format!("create Command Code watcher: {e}"))?;
    watcher
        .watch(&transcript_path, RecursiveMode::NonRecursive)
        .map_err(|e| format!("watch {}: {e}", transcript_path.display()))?;

    // Lock ordering matches `activate`/`stop`, so an activation that races
    // fresh-session discovery is either remembered before we insert or opens
    // this exact gate afterwards — never lost between the two maps.
    let activated_nodes = ACTIVATED_NODES
        .lock()
        .map_err(|_| "Command Code activation registry lock poisoned".to_string())?;
    let signal = Arc::new(WorkerSignal::new(activated_nodes.contains(&node_id)));
    WATCHERS
        .lock()
        .map_err(|_| "Command Code watcher registry lock poisoned".to_string())?
        .insert(
            node_id,
            ActiveWatcher {
                _watcher: watcher,
                signal: signal.clone(),
            },
        );
    drop(activated_nodes);

    // Close the registration/reader-exit race: if the reader died between
    // the first liveness check and map insertion, cancel this exact watcher
    // before its worker can replay the terminal transcript.
    if initial_offset.is_none() && !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
        stop_if_current(node_id, &signal);
        return Err(format!("Command Code node {node_id} is no longer running"));
    }

    let app = app.clone();
    let path_for_worker = transcript_path.clone();
    let session_id = session_id.to_string();
    let worker_signal = signal.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("commandcode-watcher-{node_id}"))
        .spawn(move || {
            // A resumed watcher is installed immediately before child spawn,
            // so wait until the process is registered before consuming its
            // post-baseline records. Otherwise a fast first turn could be
            // consumed while the lifecycle sink is not yet available.
            if !worker_signal.wait_until_ready(node_id) {
                return;
            }

            let mut tail = initial_offset
                .map(TranscriptTail::from_offset)
                .unwrap_or_default();
            // Read once after registration as well as on notify events. For
            // a resume this closes the narrow window between capturing the
            // old EOF and arming the OS watcher; records written there are
            // post-baseline and must not be lost.
            emit_transitions(
                node_id,
                &session_id,
                &path_for_worker,
                &app,
                &worker_signal,
                tail.read_transitions(&path_for_worker),
            );

            while rx.recv().is_ok() {
                while rx.try_recv().is_ok() {}
                if !worker_signal.is_active() {
                    return;
                }
                emit_transitions(
                    node_id,
                    &session_id,
                    &path_for_worker,
                    &app,
                    &worker_signal,
                    tail.read_transitions(&path_for_worker),
                );
            }
        })
    {
        stop_if_current(node_id, &signal);
        return Err(format!("start Command Code watcher worker: {error}"));
    }
    activation_guard.disarm();
    Ok(())
}

/// Stop a node's watcher. Dropping the watcher closes its sender, so the
/// worker exits after completing any already-received event.
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

/// Permit a pre-spawn resume watcher to drain transcript records. This must
/// follow the orchestrator's `on_spawn_started` write so a detected terminal
/// transition cannot be overwritten by the initial `Spawning` status.
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

fn stop_if_current(node_id: i64, signal: &Arc<WorkerSignal>) {
    if let Ok(mut activated_nodes) = ACTIVATED_NODES.lock() {
        if let Ok(mut watchers) = WATCHERS.lock() {
            let is_current = watchers
                .get(&node_id)
                .is_some_and(|watcher| Arc::ptr_eq(&watcher.signal, signal));
            if is_current {
                activated_nodes.remove(&node_id);
                if let Some(watcher) = watchers.remove(&node_id) {
                    watcher.signal.cancel();
                }
            }
        }
    }
}

fn emit_transitions(
    node_id: i64,
    session_id: &str,
    transcript_path: &Path,
    app: &AppHandle,
    signal: &WorkerSignal,
    transitions: Result<Vec<TerminalTransition>, String>,
) {
    let transitions = match transitions {
        Ok(transitions) => transitions,
        Err(error) => {
            tracing::warn!("commandcode watcher: {error}");
            return;
        }
    };
    for transition in transitions {
        if !signal.is_active() || !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
            return;
        }
        let detail = crate::agent::session_lifecycle::HookSignalDetail {
            provider: Some("commandcode".to_string()),
            provider_event: Some(
                match transition {
                    TerminalTransition::TurnCompleted => "transcript:turn_complete",
                    TerminalTransition::AwaitingInput => "transcript:awaiting_input",
                }
                .to_string(),
            ),
            provider_session_id: Some(session_id.to_string()),
            transcript_path: Some(transcript_path.to_string_lossy().to_string()),
            signal_health: crate::agent::session_lifecycle::SignalHealth::Ok,
            ..Default::default()
        };
        match transition {
            TerminalTransition::TurnCompleted => {
                crate::node_turn::publish_ready(node_id, app, detail);
            }
            TerminalTransition::AwaitingInput => {
                crate::node_turn::publish_with_signal(node_id, app, None, detail);
            }
        }
    }
}

impl TurnTracker {
    pub fn observe_transcript_line(&mut self, line: &str) -> Option<TerminalTransition> {
        let activity = transcript_activity(line)?;
        if activity == TranscriptActivity::UserTurn {
            self.pending_tool_calls = false;
        }
        if activity == TranscriptActivity::ToolUse {
            self.pending_tool_calls = true;
        }
        if activity == TranscriptActivity::ToolResult {
            self.pending_tool_calls = false;
        }
        let transition = match activity {
            TranscriptActivity::AssistantResponse if !self.pending_tool_calls => {
                Some(TerminalTransition::TurnCompleted)
            }
            TranscriptActivity::TurnCompleted
                if !matches!(
                    self.state,
                    Some(TranscriptActivity::TurnCompleted | TranscriptActivity::AssistantResponse)
                ) =>
            {
                Some(TerminalTransition::TurnCompleted)
            }
            TranscriptActivity::AwaitingInput
                if self.state != Some(TranscriptActivity::AwaitingInput) =>
            {
                Some(TerminalTransition::AwaitingInput)
            }
            _ => None,
        };
        self.state = Some(activity);
        if transition.is_some() {
            self.pending_tool_calls = false;
        }
        transition
    }
}

fn transcript_activity(line: &str) -> Option<TranscriptActivity> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = value
        .get("type")
        .or_else(|| value.get("event"))
        .or_else(|| value.get("kind"))
        .and_then(serde_json::Value::as_str)?;

    match kind {
        "turn_complete" | "turn_completed" => Some(TranscriptActivity::TurnCompleted),
        "awaiting_input" | "input_required" | "permission_requested" | "question_requested" => {
            Some(TranscriptActivity::AwaitingInput)
        }
        "user_turn" | "user_input" => Some(TranscriptActivity::UserTurn),
        "tool_use" | "tool_call" => Some(TranscriptActivity::ToolUse),
        "message" => message_activity(value.get("message")?),
        _ => None,
    }
}

fn message_activity(message: &serde_json::Value) -> Option<TranscriptActivity> {
    match crate::services::transcript_reader::commandcode_message_activity(message)? {
        crate::services::transcript_reader::CommandCodeMessageActivity::UserTurn => {
            Some(TranscriptActivity::UserTurn)
        }
        crate::services::transcript_reader::CommandCodeMessageActivity::ToolUse => {
            Some(TranscriptActivity::ToolUse)
        }
        crate::services::transcript_reader::CommandCodeMessageActivity::ToolResult => {
            Some(TranscriptActivity::ToolResult)
        }
        crate::services::transcript_reader::CommandCodeMessageActivity::AssistantResponse => {
            Some(TranscriptActivity::AssistantResponse)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn transcript_tool_turn_completes_only_after_the_final_assistant_message() {
        let mut tracker = TurnTracker::default();

        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","message":{"role":"user","content":"Implement the watcher."}}"#,
            ),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","message":{"role":"assistant","content":[{"type":"tool_use","name":"write_file"}]}}"#,
            ),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","message":{"role":"user","content":[{"type":"tool_result","content":"done"}]}}"#,
            ),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","message":{"role":"assistant","content":"Watcher implemented."}}"#,
            ),
            Some(TerminalTransition::TurnCompleted)
        );
    }

    #[test]
    fn transcript_without_tool_use_completes_a_turn() {
        let mut tracker = TurnTracker::default();

        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","message":{"role":"user","content":"Inspect the watcher."}}"#,
            ),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Still inspecting."}]}}"#,
            ),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","message":{"role":"assistant","content":"Inspection complete."}}"#,
            ),
            Some(TerminalTransition::TurnCompleted)
        );
    }

    #[test]
    fn a_new_direct_turn_does_not_inherit_pending_tool_state() {
        let mut tracker = TurnTracker::default();

        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"user_turn"}"#),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"tool_use"}"#),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"tool_result"}"#),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"turn_complete"}"#),
            Some(TerminalTransition::TurnCompleted)
        );
        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"user_turn"}"#),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","message":{"role":"assistant","content":"Direct answer."}}"#,
            ),
            Some(TerminalTransition::TurnCompleted)
        );
    }

    #[test]
    fn assistant_message_with_tool_use_waits_for_tool_result_before_completion() {
        let mut tracker = TurnTracker::default();

        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","id":"assistant-1","message":{"role":"user","content":"Implement the watcher."}}"#,
            ),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","id":"assistant-1","message":{"role":"assistant","content":[{"type":"text","text":"I will inspect the code first."},{"type":"tool_use","name":"read_file"}]}}"#,
            ),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","id":"assistant-1","message":{"role":"user","content":[{"type":"tool_result","content":"source"}]}}"#,
            ),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(
                r#"{"type":"message","id":"assistant-1","message":{"role":"assistant","content":"The inspection is complete."}}"#,
            ),
            Some(TerminalTransition::TurnCompleted)
        );
    }

    #[test]
    fn checked_in_commandcode_fixture_has_one_final_turn_completion() {
        let mut tracker = TurnTracker::default();
        let transitions: Vec<_> = include_str!("../../tests/fixtures/commandcode_transcript.jsonl")
            .lines()
            .filter_map(|line| tracker.observe_transcript_line(line))
            .collect();

        assert_eq!(transitions, vec![TerminalTransition::TurnCompleted]);
    }

    #[test]
    fn transcript_explicit_awaiting_input_is_a_terminal_transition_once() {
        let mut tracker = TurnTracker::default();

        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"user_turn"}"#),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"tool_use"}"#),
            None
        );
        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"awaiting_input"}"#),
            Some(TerminalTransition::AwaitingInput)
        );
        assert_eq!(
            tracker.observe_transcript_line(r#"{"type":"awaiting_input"}"#),
            None
        );
    }

    #[test]
    fn tail_retries_an_incomplete_jsonl_line_without_replaying_completed_records() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"Implement it.\"}}\n",
        )
        .unwrap();
        let mut tail = TranscriptTail::default();

        assert!(tail.read_transitions(temp.path()).unwrap().is_empty());

        std::fs::OpenOptions::new()
            .append(true)
            .open(temp.path())
            .unwrap()
            .write_all(b"{\"type\":\"turn_complete\"")
            .unwrap();
        assert!(tail.read_transitions(temp.path()).unwrap().is_empty());

        std::fs::OpenOptions::new()
            .append(true)
            .open(temp.path())
            .unwrap()
            .write_all(b"}\n")
            .unwrap();
        assert_eq!(
            tail.read_transitions(temp.path()).unwrap(),
            vec![TerminalTransition::TurnCompleted]
        );
        assert!(tail.read_transitions(temp.path()).unwrap().is_empty());
    }

    #[test]
    fn tail_started_at_resume_baseline_reads_the_fast_first_resumed_turn() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), "{\"type\":\"turn_complete\"}\n").unwrap();
        let baseline = std::fs::metadata(temp.path()).unwrap().len();
        let mut tail = TranscriptTail::from_offset(baseline);

        std::fs::OpenOptions::new()
            .append(true)
            .open(temp.path())
            .unwrap()
            .write_all(
                b"{\"type\":\"user_turn\"}\n{\"type\":\"tool_use\"}\n{\"type\":\"turn_complete\"}\n",
            )
            .unwrap();

        assert_eq!(
            tail.read_transitions(temp.path()).unwrap(),
            vec![TerminalTransition::TurnCompleted]
        );
    }
}
