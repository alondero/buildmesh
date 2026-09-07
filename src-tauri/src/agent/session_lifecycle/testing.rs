//! Shared [`SessionLifecycleSink`] fixture for backend unit tests.
//!
//! Three test files previously rolled their own sink stub (each near
//! identical to the others with different field names):
//!
//! - `agent::session_lifecycle::tests::FakeSink`
//! - `commands::attention::tests::FakeLifecycleSink`
//! - `http::ws::tests::MockSink`
//!
//! All three implement the same trait, all three record `write_status`
//! calls and event emits, none touch the global DB or `AppHandle`.
//! They belong in one place so a regression that adds a new
//! `SessionLifecycleSink` method surfaces once (a compile error in
//! this file's `impl`) instead of three times.
//!
//! The fields are public via accessors that return owned `Vec`s so a
//! test that calls `sink.writes()` twice gets two snapshots — `RefCell`
//! borrow semantics don't leak into the test code.

use std::cell::RefCell;

use crate::agent::session_lifecycle::{
    LifecycleChangedPayload, SessionLifecycleSink, SessionStatus,
};

/// Records every status write and event emit. Single shared fixture
/// for any test that needs to observe a `SessionLifecycleSink` without
/// touching the global DB or `AppHandle`.
#[derive(Default)]
pub struct RecordingSink {
    writes: RefCell<Vec<(i64, SessionStatus)>>,
    writes_if: RefCell<Vec<(i64, SessionStatus, SessionStatus)>>,
    writes_unless: RefCell<Vec<(i64, SessionStatus, Vec<SessionStatus>)>>,
    attention_needed: RefCell<Vec<i64>>,
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
