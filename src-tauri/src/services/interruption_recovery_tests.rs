use super::*;
use crate::agent::{process::AgentProcessRegistry, session_lifecycle::LifecycleKind};
use crate::models::SessionStatus;
use crate::services::transcript_reader::interruption::FailureKind;
use std::{
    cell::{Cell, RefCell},
    sync::{mpsc::Receiver, Arc},
};

struct Host {
    now: Cell<i64>,
    node: RefCell<AgentNode>,
    failure: RefCell<Option<Option<Failure>>>,
    revision: Cell<u64>,
    registry: Arc<AgentProcessRegistry>,
    writes: Receiver<Vec<u8>>,
    messages: RefCell<Vec<String>>,
    piloted: Cell<bool>,
    advance_during_read: Cell<bool>,
    reads_available: Cell<bool>,
    fail_reads_after_publish: Cell<bool>,
}
impl Host {
    fn new() -> Self {
        let (registry, writes) = crate::agent::process::testing::capturing_registry(-6559);
        Self {
            now: Cell::new(1000),
            node: RefCell::new(AgentNode {
                id: -6559,
                provider: "codex".into(),
                status: SessionStatus::Running,
                cli_session_id: Some("s".into()),
                ..Default::default()
            }),
            failure: RefCell::new(Some(Some(Failure {
                turn: "t".into(),
                at_ms: 100,
                code: "server_overloaded".into(),
                kind: FailureKind::Transient,
            }))),
            revision: Cell::new(1),
            registry,
            writes,
            messages: RefCell::new(vec![]),
            piloted: Cell::new(false),
            advance_during_read: Cell::new(false),
            reads_available: Cell::new(true),
            fail_reads_after_publish: Cell::new(false),
        }
    }
    fn tick(&self, state: &mut Recovery) {
        let node = self.node.borrow().clone();
        observe(self, &node, state).unwrap();
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.registry.kill_session(-6559);
    }
}
impl RecoveryHost for Host {
    fn now(&self) -> i64 {
        self.now.get()
    }
    fn node(&self, _: i64) -> Result<AgentNode, String> {
        Ok(self.node.borrow().clone())
    }
    fn observation(&self, _: i64) -> Result<db::agent_node::AgentStatusObservation, String> {
        if !self.reads_available.get() {
            return Err("reader pool busy".into());
        }
        Ok(db::agent_node::AgentStatusObservation {
            status: self.node.borrow().status,
            session_incarnation: Some("1".into()),
            session_id: Some("s".into()),
            observed_at_ms: 1,
            source_revision: self.revision.get().to_string(),
        })
    }
    fn failure(&self, _: &AgentNode) -> Option<Option<Failure>> {
        if self.advance_during_read.replace(false) {
            self.revision.set(self.revision.get() + 1);
        }
        self.failure.borrow().clone()
    }
    fn input(&self, id: i64) -> Option<String> {
        self.registry.input_stamp(id)
    }
    fn unpiloted(&self, _: i64) -> Result<bool, String> {
        Ok(!self.piloted.get())
    }
    fn publish(
        &self,
        node: &AgentNode,
        observed: &db::agent_node::AgentStatusObservation,
        _: &str,
        _: &Failure,
        message: String,
    ) -> Result<Option<db::agent_node::AgentStatusObservation>, String> {
        if observed.source_revision != self.revision.get().to_string() {
            return Ok(None);
        }
        self.messages.borrow_mut().push(message.clone());
        let mut current = self.node.borrow_mut();
        current.status = SessionStatus::AwaitingInput;
        current.lifecycle = Some(session_lifecycle::LifecycleChangedPayload::new(
            node.id,
            LifecycleKind::Error,
            SessionStatus::AwaitingInput,
            &HookSignalDetail {
                provider_event: Some(EVENT.into()),
                message: Some(message),
                ..Default::default()
            },
            "failure",
        ));
        self.revision.set(self.revision.get() + 1);
        drop(current);
        let committed = self.observation(node.id)?;
        if self.fail_reads_after_publish.get() {
            self.reads_available.set(false);
        }
        Ok(Some(committed))
    }
    fn deliver(
        &self,
        node: &AgentNode,
        pending: &Pending,
    ) -> Result<prompt_delivery::DeliveryOutcome, String> {
        deliver_continuation(&self.registry, node, pending, &|| {
            current_evidence(self, node.id, pending)
        })
    }
    fn notice(&self, _: i64, _: &Pending, message: &str) -> Result<(), String> {
        self.messages.borrow_mut().push(message.into());
        Ok(())
    }
}

#[test]
fn new_work_during_native_read_cannot_be_overwritten_by_old_failure() {
    let h = Host::new();
    h.advance_during_read.set(true);
    let mut state = Recovery::default();
    h.tick(&mut state);
    assert!(state.pending.is_none());
    assert!(h.messages.borrow().is_empty());
    assert_eq!(h.node.borrow().status, SessionStatus::Running);
}

#[test]
fn committed_revision_arms_retry_without_a_post_publication_database_read() {
    let h = Host::new();
    h.fail_reads_after_publish.set(true);
    let mut state = Recovery::default();
    h.tick(&mut state);
    assert!(state.pending.is_some());
    h.reads_available.set(true);
    h.now.set(state.pending.as_ref().unwrap().due_ms);
    h.tick(&mut state);
    assert_eq!(state.attempts, 1);
}

#[test]
fn interruption_scheduler_waits_then_delivers_one_continuation_and_enter() {
    let h = Host::new();
    let mut s = Recovery::default();
    h.tick(&mut s);
    assert_eq!(s.pending.as_ref().unwrap().due_ms, 31_000);
    h.now.set(30_999);
    h.tick(&mut s);
    assert!(h.writes.try_recv().is_err());
    h.now.set(31_000);
    h.tick(&mut s);
    assert_eq!(s.attempts, 1);
    assert!(s.pending.is_none());
    let bytes = h.writes.try_iter().flatten().collect::<Vec<_>>();
    assert!(String::from_utf8_lossy(&bytes).contains(PROMPT));
    assert_eq!(bytes.last(), Some(&b'\r'));
    h.tick(&mut s);
    assert!(h.writes.try_recv().is_err());
}

#[test]
fn unavailable_native_evidence_preserves_backoff_and_recovers_after_readability_returns() {
    let h = Host::new();
    let mut s = Recovery::default();
    h.tick(&mut s);
    let failure = h.failure.replace(None);
    h.now.set(40_000);
    h.tick(&mut s);
    assert!(s.pending.is_some());
    assert!(h.writes.try_recv().is_err());
    h.failure.replace(failure);
    h.tick(&mut s);
    assert_eq!(s.attempts, 1);
}

#[test]
fn typing_then_clearing_cancels_without_rearming_the_same_failure() {
    let h = Host::new();
    let mut s = Recovery::default();
    h.tick(&mut s);
    h.registry.write_input(-6559, b"x\x7f").unwrap();
    h.now.set(40_000);
    h.tick(&mut s);
    assert!(s.pending.is_none());
    assert_eq!(s.attempts, 0);
    h.tick(&mut s);
    assert!(s.pending.is_none());
    let bytes = h.writes.try_iter().flatten().collect::<Vec<_>>();
    assert_eq!(bytes, b"x\x7f");
    assert!(h.messages.borrow().last().unwrap().contains("cancelled"));
}

#[test]
fn newer_native_success_or_revision_cancels_retry() {
    for native in [true, false] {
        let h = Host::new();
        let mut s = Recovery::default();
        h.tick(&mut s);
        if native {
            h.failure.replace(Some(None));
        } else {
            h.revision.set(99);
        }
        h.now.set(40_000);
        h.tick(&mut s);
        assert!(s.pending.is_none());
        assert!(h.writes.try_recv().is_err());
    }
}

#[test]
fn three_new_failures_exhaust_process_budget_and_policy_never_schedules() {
    let h = Host::new();
    let mut s = Recovery::default();
    for attempt in 0..3 {
        h.failure
            .borrow_mut()
            .as_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .turn = format!("t{attempt}");
        h.tick(&mut s);
        h.now.set(s.pending.as_ref().unwrap().due_ms);
        h.tick(&mut s);
        assert_eq!(s.attempts, attempt + 1);
        let _ = h.writes.try_iter().collect::<Vec<_>>();
    }
    h.failure
        .borrow_mut()
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .turn = "exhausted".into();
    h.tick(&mut s);
    assert!(s.pending.is_none());
    assert!(h
        .messages
        .borrow()
        .last()
        .unwrap()
        .contains("limit reached"));
    let h = Host::new();
    let mut s = Recovery::default();
    h.failure
        .borrow_mut()
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .kind = FailureKind::Policy;
    h.tick(&mut s);
    assert!(s.pending.is_none());
    assert!(h.writes.try_recv().is_err());
}

#[test]
fn protected_states_and_input_requests_never_allow_recovery() {
    for status in [
        SessionStatus::Idle,
        SessionStatus::Suspended,
        SessionStatus::Archived,
        SessionStatus::Error,
        SessionStatus::Spawning,
    ] {
        assert!(!eligible(&AgentNode {
            status,
            ..Default::default()
        }));
    }
    for kind in [
        LifecycleKind::PermissionRequested,
        LifecycleKind::QuestionRequested,
        LifecycleKind::BackgroundRunning,
        LifecycleKind::InputRequired,
    ] {
        let node = AgentNode {
            status: SessionStatus::AwaitingInput,
            lifecycle: Some(session_lifecycle::LifecycleChangedPayload::new(
                1,
                kind,
                SessionStatus::AwaitingInput,
                &HookSignalDetail::default(),
                "needs input",
            )),
            ..Default::default()
        };
        assert!(!eligible(&node), "{kind:?}");
    }
    let mut node = AgentNode {
        status: SessionStatus::Ready,
        ..Default::default()
    };
    assert!(eligible(&node));
    node.lifecycle = Some(session_lifecycle::LifecycleChangedPayload::new(
        1,
        LifecycleKind::TurnCompleted,
        SessionStatus::Ready,
        &HookSignalDetail::default(),
        "turn interrupted by the user",
    ));
    assert!(!eligible(&node));
}

struct LiveProcess(i64);
impl Drop for LiveProcess {
    fn drop(&mut self) {
        PROCESS_REGISTRY.kill_session(self.0);
    }
}
fn live_node() -> LiveProcess {
    let conn = db::write_conn();
    conn.execute(
        "INSERT INTO meshes(name,path) VALUES('recovery','C:/recovery')",
        [],
    )
    .unwrap();
    let mesh = conn.last_insert_rowid();
    conn.execute("INSERT INTO agent_nodes(mesh_id,name,path,provider,status,cli_session_id,session_started_at,status_changed_at)
        VALUES(?1,'recovery','C:/recovery','codex','running','s',1,'2026-11-05T08:00:00Z')",[mesh]).unwrap();
    let id = conn.last_insert_rowid();
    drop(conn);
    crate::agent::process::testing::insert_standin(&PROCESS_REGISTRY, id);
    LiveProcess(id)
}

#[test]
fn native_failure_commit_rechecks_permission_even_when_caller_captured_its_new_revision() {
    let _db = db::test_support::isolated();
    let process = live_node();
    let id = process.0;
    let input = PROCESS_REGISTRY.input_stamp(id).unwrap();
    session_lifecycle::on_attention_with_signal(
        &session_lifecycle::DbOnlySink,
        id,
        None,
        &HookSignalDetail {
            kind: Some(LifecycleKind::PermissionRequested),
            provider_event: Some("PermissionRequest".into()),
            ..Default::default()
        },
    )
    .unwrap();
    // Reproduce the stale list + fresh revision race from the first review.
    let observed = db::agent_node::agent_status_observation(id).unwrap();
    assert!(session_lifecycle::on_native_interruption(
        &session_lifecycle::DbOnlySink,
        id,
        &observed,
        &input,
        100,
        &HookSignalDetail::default()
    )
    .unwrap()
    .is_none());
    assert_eq!(
        db::get_agent_node_by_id(id)
            .unwrap()
            .lifecycle
            .unwrap()
            .kind,
        LifecycleKind::PermissionRequested
    );
}

#[test]
fn native_failure_commit_and_notice_preserve_newer_input_and_lifecycle() {
    let _db = db::test_support::isolated();
    let process = live_node();
    let id = process.0;
    let observed = db::agent_node::agent_status_observation(id).unwrap();
    let input = PROCESS_REGISTRY.input_stamp(id).unwrap();
    let detail = HookSignalDetail {
        provider_event: Some(EVENT.into()),
        message: Some("retry pending".into()),
        ..Default::default()
    };
    assert!(session_lifecycle::on_native_interruption(
        &session_lifecycle::DbOnlySink,
        id,
        &observed,
        &input,
        100,
        &detail
    )
    .unwrap()
    .is_some());
    assert_eq!(
        db::get_agent_node_by_id(id).unwrap().status,
        SessionStatus::AwaitingInput
    );
    let revision = db::agent_node::agent_status_observation(id)
        .unwrap()
        .source_revision;
    session_lifecycle::native_interruption_notice(
        &session_lifecycle::DbOnlySink,
        id,
        &revision,
        "retry cancelled",
    )
    .unwrap();
    assert_eq!(
        db::get_agent_node_by_id(id)
            .unwrap()
            .lifecycle
            .unwrap()
            .message
            .as_deref(),
        Some("retry cancelled")
    );
    let observed = db::agent_node::agent_status_observation(id).unwrap();
    PROCESS_REGISTRY.write_input(id, b"draft").unwrap();
    assert!(session_lifecycle::on_native_interruption(
        &session_lifecycle::DbOnlySink,
        id,
        &observed,
        &input,
        100,
        &detail
    )
    .unwrap()
    .is_none());
    session_lifecycle::on_hook_running(&session_lifecycle::DbOnlySink, id).unwrap();
    session_lifecycle::native_interruption_notice(
        &session_lifecycle::DbOnlySink,
        id,
        &revision,
        "stale notice",
    )
    .unwrap();
    assert_eq!(
        db::get_agent_node_by_id(id).unwrap().status,
        SessionStatus::Running
    );
}
