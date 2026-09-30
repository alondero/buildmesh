//! Regression tests for the **public** node-review start path
//! (`db::circuit::ledger::create_node_circuit_run`).
//!
//! Issue #1792 added a source-agent readiness gate to this public wrapper:
//! it refused to mint the built-in review preset until the source had captured
//! a `cli_session_id` or a readable `assistant_report` revision. That gate was
//! removed — a review starts on any source that is not already claimed and is
//! in a waitable status, whether or not the worker has observed it yet.
//!
//! The gate only ever lived in the public wrapper, never in
//! `create_node_circuit_run_locked`. The per-test in-memory `db::circuit_tests`
//! suite drives the locked helper exclusively, so it stays green even if a
//! future change re-adds an observation precondition to the wrapper. These
//! tests close that gap by driving the public entry point against the
//! process-global DB (`db::test_support::ensure_db_for_tests`) — run with
//! `--test-threads=1` like the other global-DB suites (AGENTS.md).

use super::*;
use crate::models::{EnvType, SessionStatus};

/// A source that a worker has not observed yet (no `cli_session_id`, no
/// report) must still mint a review preset run on the public path, and the
/// first-writer dedupe must still apply on a retry.
#[test]
fn review_preset_starts_on_an_unobserved_running_source() {
    crate::db::test_support::ensure_db_for_tests();
    let mesh = create_mesh(
        "review-start-unobserved",
        &format!("/tmp/review-start-unobserved-{}", std::process::id()),
    )
    .expect("create mesh");
    let source = create_agent_node(
        mesh.id,
        "Fresh source",
        &mesh.path,
        "main",
        EnvType::Windows,
        "claude",
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .expect("create source");
    update_agent_node_status(source.id, SessionStatus::Running).expect("mark the source running");
    // Deliberately leave `cli_session_id` unset: neither it nor a readable
    // report exists, so the removed readiness gate would have refused here.
    let reloaded = get_agent_node_by_id(source.id).expect("reload source");
    assert_eq!(reloaded.status, SessionStatus::Running);
    assert!(reloaded.cli_session_id.is_none());
    assert!(
        crate::coordinator::enrichment::assistant_report(&reloaded).is_none(),
        "the fixture must have no readable assistant report"
    );

    let run_id = create_node_circuit_run(source.id, None, 3, None)
        .expect("an unobserved running source must mint on the public path");
    let run = get_circuit_run(run_id).expect("read run").expect("run row");
    assert_eq!(run.source_agent_node_id, Some(source.id));
    assert_eq!(run.state, "pending");

    // First-writer-wins dedupe (issue #1660) still applies on the public path.
    assert_eq!(
        create_node_circuit_run(source.id, None, 3, None).unwrap(),
        run_id,
        "a retry returns the live run instead of minting a duplicate"
    );
}
