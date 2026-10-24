// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;

#[test]
fn parse_minimax_response_valid() {
    // Live response shape as of 2026-06-01: wrapper is `model_remains` (not
    // `category_remains`), items carry `model_name` instead of
    // `category`/`display_name`, and the source of truth is the
    // `*_remaining_percent` fields (0-100) rather than the count pair.
    let json = r#"{
        "model_remains": [
            {
                "start_time": 1780344000000,
                "end_time": 1780358400000,
                "remains_time": 5511238,
                "current_interval_total_count": 0,
                "current_interval_usage_count": 0,
                "model_name": "general",
                "current_weekly_total_count": 0,
                "current_weekly_usage_count": 0,
                "weekly_start_time": 1780272000000,
                "weekly_end_time": 1780876800000,
                "weekly_remains_time": 523911238,
                "current_interval_status": 1,
                "current_interval_remaining_percent": 89,
                "current_weekly_status": 1,
                "current_weekly_remaining_percent": 96
            },
            {
                "start_time": 1780272000000,
                "end_time": 1780358400000,
                "remains_time": 5511238,
                "current_interval_total_count": 3,
                "current_interval_usage_count": 0,
                "model_name": "video",
                "current_weekly_total_count": 21,
                "current_weekly_usage_count": 0,
                "weekly_start_time": 1780272000000,
                "weekly_end_time": 1780876800000,
                "weekly_remains_time": 523911238,
                "current_interval_status": 1,
                "current_interval_remaining_percent": 100,
                "current_weekly_status": 1,
                "current_weekly_remaining_percent": 100
            }
        ],
        "base_resp": {
            "status_code": 0,
            "status_msg": "success"
        }
    }"#;
    let (windows, detail) = parse_minimax_response(json).unwrap();
    // Only the "general" (text-generation) model is surfaced; the "video"
    // entry must be filtered out, leaving 2 windows.
    assert_eq!(windows.len(), 2);
    // First: general 5-hour, 89% remaining → 11% used.
    assert_eq!(windows[0].label, "5-hour");
    assert_eq!(windows[0].used_percent, Some(11.0));
    assert!(windows[0].resets_at.is_some());
    // Second: general weekly, 96% remaining → 4% used.
    assert_eq!(windows[1].label, "Weekly");
    assert_eq!(windows[1].used_percent, Some(4.0));
    assert!(windows[1].resets_at.is_some());
    assert_eq!(detail, None);
}

#[test]
fn parse_minimax_response_empty_model_remains() {
    // When the response shape is current but no models are listed, we still
    // surface the user-facing "no quotas" detail rather than failing.
    let json = r#"{
        "model_remains": [],
        "base_resp": { "status_code": 0, "status_msg": "success" }
    }"#;
    let (windows, detail) = parse_minimax_response(json).unwrap();
    assert!(windows.is_empty());
    assert_eq!(detail.as_deref(), Some("No active token plan quotas found"));
}

#[test]
fn parse_minimax_response_model_without_percent_is_skipped() {
    // Forward-compat: a model entry that carries no remaining percentage is
    // skipped rather than producing a NaN window — asserted by the empty
    // `windows` below.
    let json = r#"{
        "model_remains": [
            {
                "model_name": "unknown",
                "end_time": 1780358400000,
                "weekly_end_time": 1780876800000
            }
        ],
        "base_resp": { "status_code": 0, "status_msg": "success" }
    }"#;
    let (windows, detail) = parse_minimax_response(json).unwrap();
    assert!(windows.is_empty());
    assert_eq!(detail.as_deref(), Some("No active token plan quotas found"));
}

#[test]
fn parse_minimax_balance_valid() {
    // Pay-as-you-go cash balance (issue #537): remaining + monthly spend +
    // currency, wrapped in the same base_resp envelope as the other endpoints.
    let json = r#"{
        "balance": { "remaining": 42.5, "month_spend": 7.25, "currency": "USD" },
        "base_resp": { "status_code": 0, "status_msg": "success" }
    }"#;
    let balance = parse_minimax_balance(json).unwrap();
    assert_eq!(balance.remaining, 42.5);
    assert_eq!(balance.monthly_spend, Some(7.25));
    assert_eq!(balance.currency, "USD");
}

#[test]
fn parse_minimax_balance_defaults_currency_and_optional_spend() {
    // Currency + spend are optional; currency falls back to USD, spend to None.
    let json = r#"{ "balance": { "remaining": 100.0 } }"#;
    let balance = parse_minimax_balance(json).unwrap();
    assert_eq!(balance.remaining, 100.0);
    assert_eq!(balance.monthly_spend, None);
    assert_eq!(balance.currency, "USD");
}

#[test]
fn parse_minimax_balance_rejects_percentage_only_body() {
    // A plan-percentage body (no `balance` object) must fail loudly rather
    // than masquerade as a zero balance.
    let json = r#"{
        "model_remains": [],
        "base_resp": { "status_code": 0, "status_msg": "success" }
    }"#;
    assert!(parse_minimax_balance(json).is_err());
}

#[test]
fn parse_minimax_response_rejects_legacy_category_remains() {
    // Regression guard: if Minimax ever reverts the wrapper, the parser must
    // NOT silently return zero windows — the old shape should fail loudly.
    let json = r#"{
        "category_remains": [
            {
                "category": "text_generation",
                "display_name": "Text Generation",
                "end_time": 1780153200000,
                "current_interval_total_count": 15000,
                "current_interval_usage_count": 55,
                "current_weekly_total_count": 150000,
                "current_weekly_usage_count": 732,
                "weekly_end_time": 1780272000000
            }
        ],
        "base_resp": { "status_code": 0, "status_msg": "success" }
    }"#;
    let result = parse_minimax_response(json);
    assert!(
        result.is_err(),
        "legacy category_remains shape must be rejected, not silently zero-windowed"
    );
}

#[test]
fn no_credential_returns_error() {
    // Issue #1745: minimax is the first provider migrated to the
    // outcome seam. The no-credential case is `UsageOutcome::NoCredential`,
    // which projects to `logged_in: false, error: Some(hint)`.
    let outcome = minimax_usage("");
    match outcome {
        UsageOutcome::NoCredential { hint } => {
            assert_eq!(hint, "No API key configured");
        }
        other => panic!("expected NoCredential outcome, got: {other:?}"),
    }
}

/// The rendered meter output is pinned (issue #2528).
///
/// The provider's response and the exact wire triple it must project to
/// are both fixtures: a change to the window labels, percentages or reset
/// times minimax's parse produces shows up here as a byte difference
/// rather than as a quietly different meter.
#[test]
fn minimax_rendered_output_matches_the_pinned_fixture() {
    const RESPONSE: &str = include_str!("fixtures/minimax-token-plan-remains.json");
    const RENDERED: &str = include_str!("fixtures/minimax-token-plan-remains-rendered.json");

    let (windows, detail) = parse_minimax_response(RESPONSE).unwrap();
    let usage = UsageReading {
        windows,
        balance: None,
        meters: Vec::new(),
        detail,
    }
    .into_outcome()
    .into_usage("minimax");
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        // The fixture file ends with a newline; the payload does not.
        RENDERED.trim_end(),
        "minimax's rendered meter output must stay byte-identical"
    );
}
