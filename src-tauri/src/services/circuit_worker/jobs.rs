//! Bounded external work; only the Circuit worker consumes results and commits transitions.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use super::turn_classify::ClassifiedTurn;
use super::{db, CircuitNodeKind, RunState, RunView, StepStatus, StepView};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Key {
    run: i64,
    step: String,
    attempt: i32,
    verification: bool,
    argument: String,
}

type Task<T> = Box<dyn FnOnce(Arc<AtomicBool>) -> T + Send>;
struct Entry<T> {
    cancelled: Arc<AtomicBool>,
    resource: Option<String>,
    running: bool,
    task: Option<Task<T>>,
    result: Option<T>,
}
struct State<T> {
    entries: HashMap<Key, Entry<T>>,
    queue: VecDeque<Key>,
    limit: usize,
    admission_deferrals: u64,
}
struct Pool<T>(Arc<Mutex<State<T>>>);

impl<T: Send + 'static> Pool<T> {
    fn new(limit: usize) -> Self {
        Self(Arc::new(Mutex::new(State { entries: HashMap::new(), queue: VecDeque::new(), limit, admission_deferrals: 0 })))
    }

    fn poll(&self, key: Key, resource: Option<String>, task: impl FnOnce(Arc<AtomicBool>) -> T + Send + 'static) -> Option<T> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = state.entries.get_mut(&key) {
            if entry.cancelled.load(Ordering::Acquire) { return None; }
            if let Some(result) = entry.result.take() {
                state.entries.remove(&key);
                return Some(result);
            }
            return None;
        }
        if state.queue.len() >= 128 {
            state.admission_deferrals = state.admission_deferrals.saturating_add(1);
            let admission_deferrals = state.admission_deferrals;
            drop(state);
            // Count refused admission attempts, not pending polls or distinct jobs.
            // Exponential sampling keeps sustained pressure visible without per-tick spam.
            if admission_deferrals.is_power_of_two() {
                tracing::warn!(admission_deferrals, queue_limit = 128, run_id = key.run,
                    step_id = %key.step, attempt = key.attempt,
                    "Circuit job admission deferred: queue full; scheduler will retry");
            }
            return None;
        }
        state.entries.insert(key.clone(), Entry {
            cancelled: Arc::new(AtomicBool::new(false)), resource, running: false,
            task: Some(Box::new(task)), result: None,
        });
        state.queue.push_back(key);
        drop(state);
        Self::dispatch(self.0.clone());
        None
    }

    fn retain(&self, keep: impl Fn(&Key) -> bool) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.entries.retain(|key, entry| {
            if keep(key) { return true; }
            entry.cancelled.store(true, Ordering::Release);
            // A cancelled external call still occupies its slot/resource until it exits.
            entry.running
        });
        let retained: std::collections::HashSet<_> = state.entries.keys().cloned().collect();
        state.queue.retain(|key| retained.contains(key));
        drop(state);
        Self::dispatch(self.0.clone());
    }

    fn dispatch(shared: Arc<Mutex<State<T>>>) {
        loop {
            let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
            if state.entries.values().filter(|entry| entry.running).count() >= state.limit { return; }
            let index = state.queue.iter().position(|key| {
                let candidate = &state.entries[key];
                candidate.resource.as_ref().is_none_or(|resource|
                    !state.entries.values().any(|entry| entry.running && entry.resource.as_ref() == Some(resource)))
            });
            let Some(index) = index else { return; };
            let key = state.queue.remove(index).expect("queued job");
            let entry = state.entries.get_mut(&key).expect("registered job");
            entry.running = true;
            let task = entry.task.take().expect("queued task");
            let cancelled = entry.cancelled.clone();
            drop(state);
            let worker = shared.clone();
            let worker_key = key.clone();
            let spawn = std::thread::Builder::new().name("circuit-job".into()).spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (!cancelled.load(Ordering::Acquire)).then(|| task(cancelled.clone()))
                }));
                let mut state = worker.lock().unwrap_or_else(|e| e.into_inner());
                match result {
                    Ok(Some(result)) if !cancelled.load(Ordering::Acquire) => {
                        if let Some(entry) = state.entries.get_mut(&worker_key) {
                            entry.running = false;
                            entry.result = Some(result);
                        }
                    }
                    _ => { state.entries.remove(&worker_key); }
                }
                drop(state);
                super::wake_circuit_worker();
                Self::dispatch(worker);
            });
            if let Err(error) = spawn {
                shared.lock().unwrap_or_else(|e| e.into_inner()).entries.remove(&key);
                tracing::warn!("Circuit job could not start: {error}");
            }
        }
    }
}

enum ResultValue { Classification(Option<Box<ClassifiedTurn>>, Option<AgentFence>), Verification(Option<bool>), Watchdog(Vec<super::turn_classify::QuietClassifierFailure>) }
static JOBS: once_cell::sync::Lazy<Pool<ResultValue>> = once_cell::sync::Lazy::new(|| Pool::new(4));

#[derive(PartialEq, Eq)]
struct AgentFence { agent: i64, lifecycle: Option<String>, input: Option<String> }
impl AgentFence {
    fn read(agent: i64) -> Self {
        Self { agent, lifecycle: db::agent_turn_stamp(agent).ok().flatten(),
            input: crate::agent::process::PROCESS_REGISTRY.input_stamp(agent) }
    }
    fn current(&self) -> bool { *self == Self::read(self.agent) }
}

fn key(view: &RunView, step: &StepView, verification: bool, argument: String) -> Key {
    Key { run: view.run_id, step: step.node_id.clone(), attempt: step.attempt, verification, argument }
}

fn current(key: &Key) -> bool {
    db::get_circuit_run(key.run).ok().flatten().is_some_and(|run| run.state == "running")
        && super::load_steps(key.run).is_ok_and(|steps| steps.iter().any(|step|
            step.node_id == key.step && step.attempt == key.attempt
                && matches!(step.status, StepStatus::Running | StepStatus::Unverified)))
}

pub(super) fn retain_runs(runs: &[db::ActiveCircuitRun]) {
    JOBS.retain(|key| key.run == -1 || runs.iter().any(|run| run.run.id == key.run && run.run.state == "running"));
}

pub(super) fn cancel_run(run_id: i64) {
    JOBS.retain(|key| key.run != run_id);
}

pub(super) fn watchdog(app: tauri::AppHandle) -> Vec<super::turn_classify::QuietClassifierFailure> {
    let key = Key { run: -1, step: "watchdog".into(), attempt: 0, verification: false, argument: String::new() };
    match JOBS.poll(key, None, move |_| ResultValue::Watchdog(super::turn_classify::lost_turn_watchdog_pass(&app))) {
        Some(ResultValue::Watchdog(failures)) => failures,
        _ => Vec::new(),
    }
}

/// A watchdog observes a borrowed session but may publish only while its
/// original run, gate attempt and target still own that recovery request.
pub(super) struct RecoveryTarget {
    run: i64,
    step: String,
    attempt: i32,
    agent: i64,
}

impl RecoveryTarget {
    pub(super) fn new(view: &RunView, step: &StepView, agent: i64) -> Self {
        Self { run: view.run_id, step: step.node_id.clone(), attempt: step.attempt, agent }
    }

    pub(super) fn database_fence(&self, graph: &super::CircuitGraph) -> db::agent_node::CircuitRecoveryFence {
        db::agent_node::CircuitRecoveryFence {
            run_id: self.run, step_id: self.step.clone(), attempt: self.attempt,
            agent_node_id: self.agent, graph: graph.clone(),
        }
    }

    pub(super) fn matches(&self, view: &RunView) -> bool {
        view.run_id == self.run && view.state == RunState::Running
            && view.step(&self.step).is_some_and(|step| step.attempt == self.attempt
                && matches!(step.status, StepStatus::Running | StepStatus::Unverified)
                && super::observation::observed_agent_for_step(step, &view.graph, &view.steps, view.context.source_agent_id()) == Some(self.agent))
    }

    pub(super) fn publish(
        &self, permit: &super::CircuitEffectBatchPermit, active: &db::ActiveCircuitRun,
        publish: impl FnOnce(),
    ) -> bool {
        self.publish_with(permit, || {
            let run = db::get_circuit_run(self.run).ok().flatten()?;
            Some(RunView {
                run_id: run.id, state: RunState::from_db_str(&run.state),
                graph: super::CircuitGraph::from_json(&active.circuit_graph_json).ok()?,
                context: super::CircuitContext::from_json(&run.context_json).ok()?,
                steps: super::load_steps(self.run).ok()?,
            })
        }, publish)
    }

    fn publish_with(
        &self, permit: &super::CircuitEffectBatchPermit,
        current: impl FnOnce() -> Option<RunView>, publish: impl FnOnce(),
    ) -> bool {
        if permit.is_cancelled() { return false; }
        let Some(view) = current() else { return false; };
        // The cancellation marker is checked again after potentially contended
        // DB reads, immediately before entering the lifecycle/input write seam.
        if !self.matches(&view) || permit.is_cancelled() { return false; }
        publish();
        true
    }
}

pub(super) fn reconcile(view: &RunView) {
    JOBS.retain(|key| key.run != view.run_id || (view.state == RunState::Running
        && view.step(&key.step).is_some_and(|step| step.attempt == key.attempt
            && matches!(step.status, StepStatus::Running | StepStatus::Unverified)
            && (!key.verification || matches!(view.graph.node(&step.node_id).map(|node| &node.kind),
                Some(CircuitNodeKind::DeterministicVerification { command }) if view.context.resolve(command) == key.argument)))));
}

pub(super) fn classify(active: &db::ActiveCircuitRun, view: &RunView, step: &StepView) -> Option<ClassifiedTurn> {
    let key = key(view, step, false, String::new());
    let job_key = key.clone();
    let active = active.clone();
    let snapshot = view.clone();
    let agent = step.agent_node_id.or_else(|| view.resolve_target_agent(&step.node_id));
    match JOBS.poll(key, agent.map(|id| format!("agent:{id}")), move |cancelled| {
        let permit = super::begin_circuit_effect_batch(job_key.run);
        if permit.is_cancelled() || cancelled.load(Ordering::Acquire) || !current(&job_key) {
            return ResultValue::Classification(None, None);
        }
        let fence = agent.map(AgentFence::read);
        let result = super::turn_classify::classify_step_turn(&active, &snapshot, &job_key.step);
        ResultValue::Classification(result.filter(|_| !permit.is_cancelled()).map(Box::new), fence)
    }) {
        Some(ResultValue::Classification(result, fence)) if fence.as_ref().is_none_or(AgentFence::current) => {
            result.filter(|value| value.binding.as_ref().is_none_or(|binding|
                binding.input_guard.report_guard.as_ref().is_none_or(|report| report.is_current())
                    && binding.input_guard.transcript_guard.as_ref().is_none_or(|transcript| transcript.is_current())))
                .map(|value| *value)
        }
        _ => None,
    }
}

pub(super) fn verify(active: &db::ActiveCircuitRun, view: &RunView, step: &StepView, command: &str) -> Option<bool> {
    let resolved = view.context.resolve(command);
    let key = key(view, step, true, resolved.clone());
    let job_key = key.clone();
    let mesh = db::get_mesh_by_id(active.run.mesh_id).ok()?;
    // Different mesh records can refer to the same directory. Serialize by path.
    let path_key = mesh.path.replace('\\', "/").trim_end_matches('/').to_owned();
    let path_key = if cfg!(windows) { path_key.to_lowercase() } else { path_key };
    match JOBS.poll(key, Some(format!("verification:{path_key}")), move |cancelled| {
        let permit = super::begin_circuit_effect_batch(job_key.run);
        if permit.is_cancelled() || cancelled.load(Ordering::Acquire) || !current(&job_key) { return ResultValue::Verification(None); }
        let green = super::run_verification_command(&mesh.path, &resolved, &cancelled, &permit.cancelled);
        ResultValue::Verification((!permit.is_cancelled() && !cancelled.load(Ordering::Acquire) && current(&job_key)).then_some(green))
    }) {
        Some(ResultValue::Verification(green)) => green,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn test_key(run: i64) -> Key {
        Key { run, step: "gate".into(), attempt: 1, verification: false, argument: String::new() }
    }

    fn borrowed_recovery_view(run_id: i64) -> RunView {
        let mut context = super::super::CircuitContext::new();
        context.set("source.agent_id", "77");
        RunView {
            run_id, state: RunState::Running, context,
            graph: super::super::CircuitGraph {
                version: 3, blueprint: None, edges: vec![],
                nodes: vec![crate::circuit::model::CircuitNode {
                    id: "gate".into(), kind: CircuitNodeKind::AwaitAgentTurn { target_node_id: Some("$source".into()) },
                }],
            },
            steps: vec![StepView { node_id: "gate".into(), status: StepStatus::Unverified,
                outcome: None, error: None, agent_node_id: None, attempt: 1 }],
        }
    }

    #[test]
    fn cancelled_borrowed_run_cannot_publish_delayed_recovery() {
        let run_id = -239_031;
        let view = borrowed_recovery_view(run_id);
        let target = RecoveryTarget::new(&view, &view.steps[0], 77);
        let permit = super::super::begin_circuit_effect_batch(run_id);
        let published = Arc::new(AtomicBool::new(false));
        let observed = published.clone();
        let (reading_tx, reading_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || target.publish_with(&permit, || {
            reading_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Some(view)
        }, || { observed.store(true, Ordering::Release); }));
        reading_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        super::super::mark_circuit_run_cancelled(run_id);
        release_tx.send(()).unwrap();
        assert!(!worker.join().unwrap());
        assert!(!published.load(Ordering::Acquire), "borrowed source must receive no Ready/attention publication after cancellation");
        super::super::finish_circuit_run_cancellation(run_id);
    }

    #[test]
    fn recovery_publication_requires_current_run_attempt_and_borrowed_target() {
        let view = borrowed_recovery_view(-239_032);
        let target = RecoveryTarget::new(&view, &view.steps[0], 77);
        let permit = super::super::begin_circuit_effect_batch(view.run_id);
        assert!(target.publish_with(&permit, || Some(view.clone()), || {}));
        let mut paused = view.clone();
        paused.state = RunState::Paused;
        let mut attempt = view.clone();
        attempt.steps[0].attempt += 1;
        let mut rebound = view.clone();
        rebound.context.set("source.agent_id", "88");
        let mut ended = view.clone();
        ended.steps[0].status = StepStatus::Completed;
        for current in [paused, attempt, rebound, ended] {
            assert!(!target.publish_with(&permit, || Some(current), || panic!("obsolete recovery must not publish")));
        }
    }

    fn completed(pool: &Pool<usize>, key: Key) -> usize {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = pool.poll(key.clone(), None, |_| panic!("existing job must not be rescheduled")) { return value; }
            assert!(Instant::now() < deadline, "job did not finish");
            std::thread::yield_now();
        }
    }

    #[test]
    fn pending_external_job_does_not_block_unrelated_observation() {
        let pool = Pool::new(2);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        assert_eq!(pool.poll(test_key(1), None, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            11
        }), None);
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(pool.poll(test_key(2), None, |_| 22), None);
        assert_eq!(completed(&pool, test_key(2)), 22);
        assert!(pool.0.lock().unwrap().entries[&test_key(1)].running);
        release_tx.send(()).unwrap();
        assert_eq!(completed(&pool, test_key(1)), 11);
    }

    #[test]
    fn concurrency_bound_and_cancelled_attempt_results_are_enforced() {
        let pool = Pool::new(1);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.poll(test_key(1), None, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            11
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (new_tx, new_rx) = mpsc::channel();
        let mut next_attempt = test_key(1);
        next_attempt.attempt = 2;
        pool.poll(next_attempt.clone(), None, move |_| { new_tx.send(()).unwrap(); 22 });
        assert!(matches!(new_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        pool.retain(|key| key.attempt == 2);
        // Cancellation must not release capacity before the old external call exits.
        assert_eq!(pool.0.lock().unwrap().entries.values().filter(|entry| entry.running).count(), 1);
        assert!(matches!(new_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release_tx.send(()).unwrap();
        new_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(completed(&pool, next_attempt), 22);
        assert!(!pool.0.lock().unwrap().entries.contains_key(&test_key(1)));
    }

    #[test]
    fn verification_resource_is_serialized_while_other_directories_progress() {
        let pool = Pool::new(4);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.poll(test_key(1), Some("directory-a".into()), move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            11
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (second_tx, second_rx) = mpsc::channel();
        pool.poll(test_key(2), Some("directory-a".into()), move |_| { second_tx.send(()).unwrap(); 22 });
        pool.poll(test_key(3), Some("directory-b".into()), |_| 33);
        assert_eq!(completed(&pool, test_key(3)), 33);
        assert!(matches!(second_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release_tx.send(()).unwrap();
        second_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(completed(&pool, test_key(1)), 11);
        assert_eq!(completed(&pool, test_key(2)), 22);
    }

    #[test]
    fn cancelling_queued_work_prevents_execution() {
        let pool = Pool::new(1);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.poll(test_key(1), None, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            11
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let executed = Arc::new(AtomicBool::new(false));
        let observed = executed.clone();
        pool.poll(test_key(2), None, move |_| { observed.store(true, Ordering::Release); 22 });
        pool.retain(|key| key.run == 1);
        release_tx.send(()).unwrap();
        assert_eq!(completed(&pool, test_key(1)), 11);
        assert!(!executed.load(Ordering::Acquire));
        assert!(!pool.0.lock().unwrap().entries.contains_key(&test_key(2)));
    }

    #[test]
    fn full_queue_counts_refused_admission_but_not_pending_and_allows_retry() {
        let pool = Pool::new(1);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.poll(test_key(0), None, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            0
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for id in 1..=128 { pool.poll(test_key(id), None, |_| panic!("queued job must be cancelled")); }
        assert!(pool.poll(test_key(129), None, |_| panic!("refused job must not execute")).is_none());
        assert!(pool.poll(test_key(1), None, |_| panic!("pending job must not be replaced")).is_none());
        {
            let state = pool.0.lock().unwrap();
            assert_eq!(state.admission_deferrals, 1);
            assert_eq!(state.queue.len(), 128);
            assert!(!state.entries.contains_key(&test_key(129)));
        }
        pool.retain(|key| key.run == 0);
        assert!(pool.poll(test_key(129), None, |_| 129).is_none());
        release_tx.send(()).unwrap();
        assert_eq!(completed(&pool, test_key(129)), 129);
        assert_eq!(pool.0.lock().unwrap().admission_deferrals, 1);
    }

    #[test]
    fn thirty_sessions_each_progress_with_four_external_slots() {
        let pool = Pool::new(4);
        let (started_tx, started_rx) = mpsc::channel();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut releases = Vec::new();
        for id in 0..30 {
            let started = started_tx.clone();
            let active = active.clone();
            let maximum = maximum.clone();
            let release = if id < 4 {
                let (tx, rx) = mpsc::channel();
                releases.push(tx);
                Some(rx)
            } else { None };
            pool.poll(test_key(id), None, move |_| {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(count, Ordering::SeqCst);
                started.send(id).unwrap();
                if let Some(release) = release { release.recv().unwrap(); }
                active.fetch_sub(1, Ordering::SeqCst);
                id as usize
            });
        }
        let mut observed = Vec::new();
        for _ in 0..4 { observed.push(started_rx.recv_timeout(Duration::from_secs(5)).unwrap()); }
        assert_eq!(pool.0.lock().unwrap().queue.len(), 26);
        assert!(matches!(started_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        for release in releases { release.send(()).unwrap(); }
        for _ in 4..30 { observed.push(started_rx.recv_timeout(Duration::from_secs(5)).unwrap()); }
        observed.sort_unstable();
        assert_eq!(observed, (0..30).collect::<Vec<_>>());
        for id in 0..30 { assert_eq!(completed(&pool, test_key(id)), id as usize); }
        assert_eq!(maximum.load(Ordering::SeqCst), 4);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn real_verification_process_stops_on_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_string_lossy().into_owned();
        let command = if cfg!(windows) {
            "powershell.exe -NoProfile -Command \"Set-Content ready started; while ($true) { Start-Sleep -Seconds 1 }\""
        } else {
            "printf started > ready; while :; do sleep 1; done"
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = cancelled.clone();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            result_tx.send(super::super::run_verification_command(&path, command, &token, &AtomicBool::new(false))).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.path().join("ready").exists() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let started = directory.path().join("ready").exists();
        cancelled.store(true, Ordering::Release);
        assert!(!result_rx.recv_timeout(Duration::from_secs(10)).expect("cancelled verification must exit"));
        worker.join().unwrap();
        assert!(started, "real shell command must reach its running state before cancellation");
    }

    #[test]
    fn quoted_verification_executes_script_and_preserves_nonzero_exit() {
        for exit_code in [0, 7] {
            let directory = tempfile::tempdir().unwrap();
            let command = if cfg!(windows) {
                format!("powershell.exe -NoProfile -Command \"Set-Content -LiteralPath 'quoted marker.txt' executed; exit {exit_code}\"")
            } else {
                format!("sh -c 'printf executed > \"quoted marker.txt\"; exit {exit_code}'")
            };
            let green = super::super::run_verification_command(&directory.path().to_string_lossy(), &command,
                &AtomicBool::new(false), &AtomicBool::new(false));
            assert_eq!(green, exit_code == 0, "the shell must return the quoted script's actual exit status");
            assert_eq!(std::fs::read_to_string(directory.path().join("quoted marker.txt")).unwrap().trim(), "executed",
                "exit zero without executing the quoted script is not successful verification");
        }
    }
}
