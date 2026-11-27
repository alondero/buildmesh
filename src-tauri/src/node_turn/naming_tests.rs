use super::publish_hook_with_sink;
use crate::agent::session_lifecycle::{testing::RecordingSink, HookSignalDetail, LifecycleKind};
use crate::models::SessionStatus;
use std::cell::Cell;

#[test]
fn accepted_work_resumption_schedules_naming_before_the_first_yield() {
    let sink = RecordingSink::with_status(SessionStatus::Ready);
    let scheduled = Cell::new(false);
    publish_hook_with_sink(
        &sink,
        42,
        LifecycleKind::WorkResumed,
        &HookSignalDetail::default(),
        || {
            assert_eq!(sink.status(), Some(SessionStatus::Running));
            scheduled.set(true);
        },
    );
    assert!(
        scheduled.get(),
        "running work left the node on its generated name"
    );
}
