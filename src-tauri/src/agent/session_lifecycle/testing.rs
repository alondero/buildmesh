//! Shared [`SessionLifecycleSink`] fixture for backend unit tests.
//!
//! Single source of truth for the `SessionLifecycleSink` test double.
//! Any test that needs to observe lifecycle writes or event emits
//! without touching the global DB or `AppHandle` constructs one of
//! these.
//!
//! Accessors return owned `Vec`s so a test that calls `sink.writes()`
//! twice gets two snapshots — `RefCell` borrow semantics don't leak
//! into the test code.

use std::cell::RefCell;

use crate::agent::session_lifecycle::{
    LifecycleChangedPayload, SemanticTurnPayload, SessionLifecycleSink, SessionStatus,
};

/// Records every status write and event emit.
#[derive(Default)]
pub struct RecordingSink {
    writes: RefCell<Vec<(i64, SessionStatus)>>,
    writes_if: RefCell<Vec<(i64, SessionStatus, SessionStatus)>>,
    writes_unless: RefCell<Vec<(i64, SessionStatus, Vec<SessionStatus>)>>,
    attention_needed: RefCell<Vec<i64>>,
    /// Pairs of `(node_id, semantic_turn)` for every
    /// `emit_attention_needed_with_payload` call. The default
    /// `SessionLifecycleSink::emit_attention_needed_with_payload`
    /// implementation silently drops the payload — overriding it here
    /// so a test that asserts on the payload (issue #1364
    /// classification) gets a real observation seam.
    attention_needed_with_payload: RefCell<Vec<(i64, Option<SemanticTurnPayload>)>>,
    attention_cleared: RefCell<Vec<i64>>,
    resume_failed: RefCell<Vec<(i64, String)>>,
    lifecycle_changed: RefCell<Vec<LifecycleChangedPayload>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn writes(&self) -> Vec<(i64, SessionStatus)> {
        self.writes.borrow().clone()
    }

    pub fn writes_if(&self) -> Vec<(i64, SessionStatus, SessionStatus)> {
        self.writes_if.borrow().clone()
    }

    pub fn writes_unless(&self) -> Vec<(i64, SessionStatus, Vec<SessionStatus>)> {
        self.writes_unless.borrow().clone()
    }

    pub fn attention_needed(&self) -> Vec<i64> {
        self.attention_needed.borrow().clone()
    }

    pub fn attention_needed_with_payload(
        &self,
    ) -> Vec<(i64, Option<SemanticTurnPayload>)> {
        self.attention_needed_with_payload.borrow().clone()
    }

    pub fn attention_cleared(&self) -> Vec<i64> {
        self.attention_cleared.borrow().clone()
    }

    pub fn resume_failed(&self) -> Vec<(i64, String)> {
        self.resume_failed.borrow().clone()
    }

    pub fn lifecycle_changed(&self) -> Vec<LifecycleChangedPayload> {
        self.lifecycle_changed.borrow().clone()
    }
}

impl SessionLifecycleSink for RecordingSink {
    fn write_status(&self, node_id: i64, new: SessionStatus) -> Result<(), String> {
        self.writes.borrow_mut().push((node_id, new));
        Ok(())
    }

    fn write_status_if(
        &self,
        node_id: i64,
        new: SessionStatus,
        expected: SessionStatus,
    ) -> Result<bool, String> {
        self.writes_if
            .borrow_mut()
            .push((node_id, new, expected));
        Ok(true)
    }

    fn write_status_unless_in(
        &self,
        node_id: i64,
        new: SessionStatus,
        forbidden: &[SessionStatus],
    ) -> Result<bool, String> {
        self.writes_unless
            .borrow_mut()
            .push((node_id, new, forbidden.to_vec()));
        Ok(true)
    }

    fn emit_attention_needed(&self, node_id: i64) {
        self.attention_needed.borrow_mut().push(node_id);
    }

    fn emit_attention_needed_with_payload(
        &self,
        node_id: i64,
        semantic_turn: Option<SemanticTurnPayload>,
    ) {
        // Mirror the trait default's call to `emit_attention_needed`
        // so callers that go through the payload variant still
        // surface in `attention_needed()` (tests using the simpler
        // accessor keep working). The payload itself goes to the
        // dedicated `attention_needed_with_payload` list.
        SessionLifecycleSink::emit_attention_needed(self, node_id);
        self.attention_needed_with_payload
            .borrow_mut()
            .push((node_id, semantic_turn));
    }

    fn emit_attention_cleared(&self, node_id: i64) {
        self.attention_cleared.borrow_mut().push(node_id);
    }

    fn emit_resume_failed(&self, node_id: i64, reason: &str) {
        self.resume_failed
            .borrow_mut()
            .push((node_id, reason.to_string()));
    }

    fn emit_lifecycle_changed(&self, payload: LifecycleChangedPayload) {
        self.lifecycle_changed.borrow_mut().push(payload);
    }
}
