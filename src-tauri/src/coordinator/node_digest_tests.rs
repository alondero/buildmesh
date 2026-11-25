use super::*;
use crate::models::Provider;

fn node(status: SessionStatus, provider: Provider) -> AgentNode {
    AgentNode {
        id: 7,
        name: "fix-login".to_string(),
        provider: provider.to_string(),
        status,
        ..Default::default()
    }
}

#[test]
fn awaiting_input_node_needs_feedback_with_waiting_since() {
    let changed = Utc::now();
    let digest = spine(
        &node(SessionStatus::AwaitingInput, Provider::Anthropic),
        "core",
        changed,
    );

    assert_eq!(digest.id, 7);
    assert_eq!(digest.name, "fix-login");
    assert_eq!(digest.mesh, "core");
    assert_eq!(digest.provider, "anthropic");
    assert_eq!(digest.status, "awaiting_input");
    assert!(
        digest.needs_feedback,
        "awaiting_input must set needs_feedback"
    );
    assert_eq!(
        digest.waiting_since,
        Some(changed),
        "waiting_since is present and equals the status-change time"
    );
    assert_eq!(digest.last_activity, changed);
}

#[test]
fn running_node_is_not_waiting() {
    let changed = Utc::now();
    let digest = spine(
        &node(SessionStatus::Running, Provider::Codex),
        "core",
        changed,
    );

    assert_eq!(digest.status, "running");
    assert_eq!(digest.provider, "codex");
    assert!(!digest.needs_feedback);
    assert_eq!(
        digest.waiting_since, None,
        "a running node has no waiting_since"
    );
    assert_eq!(digest.last_activity, changed);
}

#[test]
fn idle_node_reports_spine_without_feedback() {
    let digest = spine(
        &node(SessionStatus::Idle, Provider::Anthropic),
        "core",
        Utc::now(),
    );
    assert_eq!(digest.status, "idle");
    assert!(!digest.needs_feedback);
    assert!(digest.waiting_since.is_none());
}

#[test]
fn digest_serializes_to_expected_json_shape() {
    let changed = "2026-06-14T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let digest = spine(
        &node(SessionStatus::AwaitingInput, Provider::Anthropic),
        "core",
        changed,
    );
    let json: serde_json::Value = serde_json::to_value(&digest).unwrap();

    for key in [
        "id",
        "name",
        "mesh",
        "provider",
        "status",
        "needs_feedback",
        "waiting_since",
        "last_activity",
        "enrichment",
    ] {
        assert!(json.get(key).is_some(), "digest JSON must carry `{key}`");
    }
    assert_eq!(json["needs_feedback"], serde_json::json!(true));
    assert_eq!(
        json["waiting_since"],
        serde_json::json!("2026-06-14T10:00:00Z")
    );
    assert!(
        json.get("observed_session_telemetry").is_none(),
        "absent Muse observations must not serialize a telemetry object"
    );
}

fn available(last: Option<&str>) -> TranscriptTail {
    TranscriptTail::Available {
        turns: vec![],
        last_assistant_message: last.map(str::to_string),
    }
}

#[test]
fn supported_provider_with_good_transcript_is_enriched() {
    let tail = available(Some("Found it — shall I apply the fix?"));
    let digest = layered(
        &node(SessionStatus::Running, Provider::Anthropic),
        "core",
        Utc::now(),
        Some(&tail),
    );
    assert_eq!(
        digest.enrichment,
        Enrichment::Available {
            last_assistant_message: Some("Found it — shall I apply the fix?".to_string()),
        }
    );
}

#[test]
fn unsupported_provider_degrades_to_unsupported_flag() {
    let digest = layered(
        &node(SessionStatus::Running, Provider::OpenCode2),
        "core",
        Utc::now(),
        None,
    );
    assert_eq!(
        digest.enrichment,
        Enrichment::Unavailable {
            reason: EnrichmentUnavailable::Unsupported,
        },
        "an unsupported provider is flagged, never silently omitted"
    );
}

#[test]
fn parse_failure_degrades_to_shape_changed_flag() {
    let tail = TranscriptTail::Unavailable {
        reason: UnavailableReason::ShapeChanged,
    };
    let digest = layered(
        &node(SessionStatus::Running, Provider::Anthropic),
        "core",
        Utc::now(),
        Some(&tail),
    );
    assert_eq!(
        digest.enrichment,
        Enrichment::Unavailable {
            reason: EnrichmentUnavailable::ShapeChanged,
        }
    );
}

#[test]
fn awaiting_input_node_surfaces_the_blocking_question_as_its_message() {
    let question = "Found it — the redirect drops the query string. Shall I apply the fix?";
    let tail = available(Some(question));
    let digest = layered(
        &node(SessionStatus::AwaitingInput, Provider::Anthropic),
        "core",
        Utc::now(),
        Some(&tail),
    );

    assert!(digest.needs_feedback, "spine still flags it as blocked");
    assert!(digest.waiting_since.is_some());
    assert_eq!(
        digest.enrichment,
        Enrichment::Available {
            last_assistant_message: Some(question.to_string()),
        },
        "the blocking question is surfaced as the last assistant message"
    );
}

#[test]
fn every_reader_reason_maps_to_a_distinct_unavailable_flag() {
    use EnrichmentUnavailable as E;
    let cases = [
        (UnavailableReason::Unsupported, E::Unsupported),
        (UnavailableReason::NoSession, E::NoSession),
        (UnavailableReason::NoTranscript, E::NoTranscript),
        (UnavailableReason::Unreadable, E::Unreadable),
        (UnavailableReason::Empty, E::Empty),
        (UnavailableReason::ShapeChanged, E::ShapeChanged),
    ];
    for (reader, expected) in cases {
        assert_eq!(EnrichmentUnavailable::from(reader), expected);
    }
}

#[test]
fn enrichment_serializes_to_status_envelope() {
    let enriched: serde_json::Value = serde_json::to_value(Enrichment::Available {
        last_assistant_message: Some("hi".to_string()),
    })
    .unwrap();
    assert_eq!(enriched["status"], "available");
    assert_eq!(enriched["last_assistant_message"], "hi");

    let degraded: serde_json::Value = serde_json::to_value(Enrichment::Unavailable {
        reason: EnrichmentUnavailable::Unsupported,
    })
    .unwrap();
    assert_eq!(degraded["status"], "unavailable");
    assert_eq!(degraded["reason"], "unsupported");
}

#[test]
fn muse_observed_telemetry_is_a_digest_layer_not_quota() {
    use crate::agent::provider::muse::telemetry::{
        ObservedCumulativeUsage, ObservedMuseSessionTelemetry, ObservedTelemetryKind,
    };

    let mut digest = layered(
        &node(SessionStatus::Running, Provider::Muse),
        "core",
        Utc::now(),
        None,
    );
    assert!(digest.observed_session_telemetry.is_none());

    digest.observed_session_telemetry = Some(ObservedMuseSessionTelemetry {
        kind: ObservedTelemetryKind::ObservedSessionTelemetry,
        node_id: 7,
        session_id: "sess-aaaa-1111".into(),
        model_id: Some("muse-spark-1.3-contributor".into()),
        last_turn: None,
        cumulative: ObservedCumulativeUsage {
            prompt_tokens: 45,
            output_tokens: 25,
            total_tokens: 70,
        },
        context: None,
    });

    let json: serde_json::Value = serde_json::to_value(&digest).unwrap();
    assert_eq!(
        json["observed_session_telemetry"]["kind"],
        "observed_session_telemetry"
    );
    assert_eq!(
        json["observed_session_telemetry"]["cumulative"]["total_tokens"],
        70
    );
    assert!(json["observed_session_telemetry"]
        .get("remaining")
        .is_none());
    assert!(json["observed_session_telemetry"]
        .get("resets_at")
        .is_none());
    assert!(json["windows"].is_null() || json.get("windows").is_none());
    assert_eq!(json["enrichment"]["status"], "unavailable");
    assert_eq!(json["enrichment"]["reason"], "unsupported");
}
