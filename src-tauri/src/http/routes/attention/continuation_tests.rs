//! Issue #2263 at the identity seam: a *proven* native session continuation,
//! replayed through the pieces the webhook runs — the transcript locator and
//! `continued-in` scan, the conditional identity write, the lifecycle
//! projection and the Circuit's report read.
//!
//! Identity and observation are asserted together: what the run observes is the
//! successor's report, read under the identity the node stores, so either half
//! alone would miss a write published against a retired session.

use super::continuation::test_support::{with_claude_home, Fixture};
use super::*;
use crate::agent::session_lifecycle::{DbOnlySink, HookSignalDetail};
use serde_json::json;

// The session ids recorded in run 455 (docs/archive/2026-10/claude-review-runs-455-457.md).
const OLD: &str = "d1b48df8-eb97-4867-b365-a34cdbdc5900";
const NEW: &str = "a5acfe57-a409-4aaa-a644-4f7621934663";
const UNRELATED: &str = "3f2b8c14-77d5-4a0e-9c31-6b0e2f7a1d55";
const CAPTURED: &str = "6d9b1f42-0a37-4e88-9b21-77c50e6ad3f1";
const REPORT: &str = "BUILDMESH_HANDOFF_V1: READY\n\nThe successor finished the review.";

/// The run-455 transition: the predecessor's transcript carries the record,
/// the successor's own transcript carries the report it went on to publish.
fn continued_run() -> Fixture {
    Fixture::new(&[
        (
            OLD,
            vec![
                json!({"type": "user", "timestamp": "2026-10-12T18:57:40.000Z",
                       "message": {"role": "user", "content": "keep going"}}),
                json!({
                    "type": "continued-in",
                    "sessionId": OLD,
                    "continuedInSessionId": NEW,
                    "timestamp": "2026-10-12T18:57:48.345Z",
                }),
            ],
        ),
        (
            NEW,
            vec![
                json!({"type": "user", "timestamp": "2026-10-12T18:57:49.000Z",
                       "message": {"role": "user", "content": "keep going"}}),
                json!({
                    "type": "assistant",
                    "timestamp": "2026-10-12T18:58:03.000Z",
                    "message": {"id": "msg-2263", "role": "assistant",
                                "content": [{"type": "text", "text": REPORT}]},
                }),
            ],
        ),
    ])
}

/// What the Circuit reads for `session_id` through its own report seam.
fn circuit_report(fixture: &Fixture, session_id: &str) -> Result<String, ReportReadError> {
    with_claude_home(fixture.home_path(), || {
        crate::coordinator::enrichment::circuit_report_snapshot(&crate::models::AgentNode {
            provider: "anthropic".to_string(),
            path: fixture.node_path().to_string(),
            cli_session_id: Some(session_id.to_string()),
            ..Default::default()
        })
        .map(|snapshot| snapshot.text)
    })
}

type ReportReadError = crate::services::transcript_reader::report_snapshot::ReportReadError;

fn seed_node(path: &str, session_id: Option<&str>) -> i64 {
    let unique = format!(
        "issue2263-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let conn = crate::db::write_conn();
    // Mesh paths are unique, so two nodes in one fixture home need two meshes;
    // the node path is what the transcript locator resolves, so it stays shared.
    conn.execute(
        "INSERT INTO meshes (name, path) VALUES (?1, ?2)",
        rusqlite::params![unique, format!("{path}#{unique}")],
    )
    .unwrap();
    let mesh_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO agent_nodes
            (mesh_id, name, path, provider, status, cli_session_id, session_started_at)
         VALUES (?1, ?2, ?3, 'anthropic', 'running', ?4, ?5)",
        rusqlite::params![
            mesh_id,
            format!("node-{unique}"),
            path,
            session_id,
            GENERATION
        ],
    )
    .unwrap();
    conn.last_insert_rowid()
}

const GENERATION: i64 = 1_757_700_000_000;

fn generation(node_id: i64) -> Option<i64> {
    crate::db::session_started_at_ms(node_id).unwrap()
}

fn node_row(node_id: i64) -> crate::models::AgentNode {
    crate::db::get_agent_node_by_id(node_id).unwrap()
}

/// The lifecycle projection the route publishes for a clean `Stop`.
fn publish_turn_completed(node_id: i64, session_id: &str) -> bool {
    let detail = HookSignalDetail {
        provider: Some("anthropic".to_string()),
        provider_event: Some("Stop".to_string()),
        provider_session_id: Some(session_id.to_string()),
        ..Default::default()
    };
    crate::agent::session_lifecycle::on_turn_completed(&DbOnlySink, node_id, &detail).unwrap()
}

#[test]
fn a_proven_continuation_follows_the_successor_to_its_identity_and_its_report() {
    let _db = crate::db::test_support::isolated();
    let fixture = continued_run();
    let node_id = seed_node(fixture.node_path(), Some(OLD));
    // The predecessor's own transcript never carries that report, so every
    // observation below can only come from the adopted successor.
    assert_ne!(
        circuit_report(&fixture, OLD).ok(),
        Some(REPORT.to_string()),
        "the predecessor must not already expose the successor's report"
    );

    let successor = fixture.prove("anthropic", OLD, NEW);
    assert_eq!(successor.as_deref(), Some(NEW));
    assert!(
        continuation::admit(
            node_id,
            generation(node_id),
            OLD,
            Some(NEW),
            successor.as_deref()
        )
        .unwrap(),
        "a proven continuation must be admitted"
    );

    // Durable identity and lifecycle observation move together: a successor
    // `Stop` publishes, and the Circuit then reads the successor's report.
    assert_eq!(node_row(node_id).cli_session_id.as_deref(), Some(NEW));
    assert!(publish_turn_completed(node_id, NEW));
    let node = node_row(node_id);
    assert_eq!(node.status, crate::models::SessionStatus::Ready);
    assert_eq!(node.cli_session_id.as_deref(), Some(NEW));
    assert_eq!(
        node.lifecycle.as_ref().map(|event| event.kind),
        Some(crate::agent::session_lifecycle::LifecycleKind::TurnCompleted)
    );
    assert_eq!(circuit_report(&fixture, NEW), Ok(REPORT.to_string()));
}

#[test]
fn a_successor_prompt_reconciles_the_identity_before_its_stop() {
    // Ordering: the first successor callback can be a prompt, not the turn
    // fence. The identity must already be reconciled when the `Stop` lands,
    // or the turn would be published under the retired session.
    let _db = crate::db::test_support::isolated();
    let fixture = continued_run();
    let node_id = seed_node(fixture.node_path(), Some(OLD));

    let successor = fixture.prove("anthropic", OLD, NEW);
    assert!(continuation::admit(
        node_id,
        generation(node_id),
        OLD,
        Some(NEW),
        successor.as_deref()
    )
    .unwrap());
    assert_eq!(node_row(node_id).cli_session_id.as_deref(), Some(NEW));

    // The successor's own callbacks now match the stored identity: the fence
    // admits them with no further proof, and nothing is adopted twice.
    let repeat = fixture.prove("anthropic", NEW, NEW);
    assert_eq!(repeat, None);
    assert!(continuation::admit(
        node_id,
        generation(node_id),
        NEW,
        Some(NEW),
        repeat.as_deref()
    )
    .unwrap());
    assert_eq!(node_row(node_id).cli_session_id.as_deref(), Some(NEW));
    assert!(publish_turn_completed(node_id, NEW));
    assert_eq!(
        node_row(node_id).status,
        crate::models::SessionStatus::Ready
    );
}

#[test]
fn an_unproven_or_superseded_callback_keeps_the_stored_identity() {
    let _db = crate::db::test_support::isolated();
    let fixture = continued_run();
    let node_id = seed_node(fixture.node_path(), Some(OLD));

    let unrelated = fixture.prove("anthropic", OLD, UNRELATED);
    assert_eq!(unrelated, None);
    assert!(!continuation::admit(
        node_id,
        generation(node_id),
        OLD,
        Some(UNRELATED),
        unrelated.as_deref(),
    )
    .unwrap());
    assert_eq!(node_row(node_id).cli_session_id.as_deref(), Some(OLD));

    // Reversed: the node already stores the successor, so the predecessor's
    // late callback is a retired process, never a lineage.
    crate::db::update_cli_session_id(node_id, NEW).unwrap();
    let reversed = fixture.prove("anthropic", NEW, OLD);
    assert_eq!(reversed, None);
    assert!(!continuation::admit(
        node_id,
        generation(node_id),
        NEW,
        Some(OLD),
        reversed.as_deref()
    )
    .unwrap());

    // No adoption, so nothing else moved either.
    let node = node_row(node_id);
    assert_eq!(node.cli_session_id.as_deref(), Some(NEW));
    assert_eq!(node.status, crate::models::SessionStatus::Running);
    assert!(
        node.lifecycle.is_none(),
        "a dropped callback publishes nothing"
    );
}

#[test]
fn a_replaced_process_loses_the_race_and_its_successor_is_dropped() {
    // The proof was made against identity `OLD`; the node moved on before the
    // write. A retired process must not resurrect its conversation.
    let _db = crate::db::test_support::isolated();
    let fixture = continued_run();
    let node_id = seed_node(fixture.node_path(), Some(OLD));
    let successor = fixture.prove("anthropic", OLD, NEW);
    assert_eq!(successor.as_deref(), Some(NEW));

    // Another capture wins the race: the node now owns a different session.
    crate::db::update_cli_session_id(node_id, CAPTURED).unwrap();
    assert!(!continuation::admit(
        node_id,
        generation(node_id),
        OLD,
        Some(NEW),
        successor.as_deref()
    )
    .unwrap());
    assert_eq!(node_row(node_id).cli_session_id.as_deref(), Some(CAPTURED));

    // A restart clears the identity and bumps the process generation, so even
    // a node that stores `OLD` again cannot adopt on the retired generation.
    crate::db::clear_cli_session_id(node_id).unwrap();
    crate::db::update_cli_session_id(node_id, OLD).unwrap();
    assert_ne!(
        generation(node_id),
        Some(GENERATION),
        "a restart must move the process generation"
    );
    assert!(!continuation::admit(
        node_id,
        Some(GENERATION),
        OLD,
        Some(NEW),
        successor.as_deref()
    )
    .unwrap());
    assert_eq!(node_row(node_id).cli_session_id.as_deref(), Some(OLD));
}

#[test]
fn a_live_node_that_already_owns_the_successor_keeps_it() {
    // One live owner per session id: two nodes must not converge on one
    // transcript and fight over report binding.
    let _db = crate::db::test_support::isolated();
    let fixture = continued_run();
    let first = seed_node(fixture.node_path(), Some(OLD));
    let second = seed_node(fixture.node_path(), Some(NEW));

    let successor = fixture.prove("anthropic", OLD, NEW);
    assert!(!continuation::admit(
        first,
        generation(first),
        OLD,
        Some(NEW),
        successor.as_deref()
    )
    .unwrap());
    assert_eq!(node_row(first).cli_session_id.as_deref(), Some(OLD));
    assert_eq!(node_row(second).cli_session_id.as_deref(), Some(NEW));

    // An archived owner does not block adoption.
    crate::db::update_agent_node_status(second, crate::models::SessionStatus::Archived).unwrap();
    assert!(continuation::admit(
        first,
        generation(first),
        OLD,
        Some(NEW),
        successor.as_deref()
    )
    .unwrap());
    assert_eq!(node_row(first).cli_session_id.as_deref(), Some(NEW));
}
