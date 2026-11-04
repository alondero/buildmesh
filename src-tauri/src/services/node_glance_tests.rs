//! Node Glance layering over a node row.
use super::*;
use crate::models::{AgentNode, Provider};
use chrono::TimeZone;

fn entry(provider: Provider) -> AgentHistoryEntry {
    AgentHistoryEntry {
        node: AgentNode {
            id: 9,
            provider: provider.to_string(),
            status: SessionStatus::Running,
            created_at: chrono::Utc.timestamp_millis_opt(1_000).unwrap(),
            ..Default::default()
        },
        status_changed_at: Some(chrono::Utc.timestamp_millis_opt(5_000).unwrap()),
        session_started_at: Some(4_000),
    }
}

fn transcript() -> TranscriptGlance {
    TranscriptGlance {
        last_prompt: Some("Fix the build".to_string()),
        last_message: None,
        message_after_prompt: true,
        activity: Vec::new(),
        tool_calls_since_prompt: 0,
        updated_at_ms: Some(6_000),
    }
}

#[test]
fn a_readable_transcript_is_layered_over_the_node_timings() {
    let glance = compose(&entry(Provider::Anthropic), Ok(transcript()), 7_000);
    assert_eq!(glance.node_id, 9);
    assert_eq!(glance.status, SessionStatus::Running);
    assert_eq!(glance.observed_at_ms, 7_000);
    assert_eq!(glance.status_since_ms, Some(5_000));
    assert_eq!(glance.session_started_at_ms, Some(4_000));
    assert_eq!(glance.created_at_ms, 1_000);
    assert_eq!(glance.transcript, Some(transcript()));
    assert_eq!(glance.transcript_unavailable, None);
}

#[test]
fn an_unreadable_transcript_keeps_the_timings_and_names_the_reason() {
    let glance = compose(
        &entry(Provider::Anthropic),
        Err(UnavailableReason::NoTranscript),
        7_000,
    );
    assert_eq!(glance.transcript, None);
    assert_eq!(
        glance.transcript_unavailable,
        Some(UnavailableReason::NoTranscript)
    );
    assert_eq!(glance.status_since_ms, Some(5_000));
}

#[test]
fn a_harness_without_a_transcript_is_unsupported_without_reading_disk() {
    let node = AgentNode {
        provider: Provider::Terminal.to_string(),
        cli_session_id: Some("anything".to_string()),
        ..Default::default()
    };
    assert_eq!(
        crate::coordinator::enrichment::transcript_glance(&node),
        Err(UnavailableReason::Unsupported)
    );
}

#[test]
fn a_node_without_a_session_reports_no_session() {
    let node = AgentNode {
        provider: Provider::Anthropic.to_string(),
        path: "X:\\src\\proj".to_string(),
        cli_session_id: None,
        ..Default::default()
    };
    assert_eq!(
        crate::coordinator::enrichment::transcript_glance(&node),
        Err(UnavailableReason::NoSession)
    );
}

#[test]
fn the_wire_shape_is_flat_and_snake_case() {
    let glance = compose(
        &entry(Provider::Anthropic),
        Err(UnavailableReason::ShapeChanged),
        7_000,
    );
    let json = serde_json::to_value(&glance).unwrap();
    assert_eq!(json["status"], "running");
    assert_eq!(json["transcript"], serde_json::Value::Null);
    assert_eq!(json["transcript_unavailable"], "shape_changed");
    assert_eq!(json["status_since_ms"], 5_000);
}
