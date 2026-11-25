use super::*;

#[test]
fn consumer_windows_keep_five_hour_and_seven_day() {
    let json = r#"{
        "five_hour":{"utilization":41.0,"resets_at":"2026-05-30T21:30:00.379395+00:00"},
        "seven_day":{"utilization":33.0,"resets_at":"2026-06-05T04:00:00.379418+00:00"}
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    assert_eq!(parsed.windows.len(), 2);
    assert_eq!(parsed.windows[0].label, "5-hour");
    assert_eq!(parsed.windows[0].used_percent, Some(41.0));
    assert_eq!(parsed.windows[1].label, "7-day");
    assert_eq!(parsed.windows[1].used_percent, Some(33.0));
    assert!(parsed.meters.is_empty());
}

#[test]
fn consumer_zero_utilization_stays_zero() {
    let json = r#"{"five_hour":{"utilization":0.0},"seven_day":{"utilization":0.0}}"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    assert_eq!(parsed.windows[0].used_percent, Some(0.0));
    assert_eq!(parsed.windows[1].used_percent, Some(0.0));
}

#[test]
fn seven_day_sonnet_window_is_kept() {
    let json = r#"{"seven_day_sonnet":{"utilization":12.5,"resets_at":"2026-06-01T00:00:00Z"}}"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    assert_eq!(parsed.windows[0].label, "7-day Sonnet");
    assert_eq!(parsed.windows[0].used_percent, Some(12.5));
}

#[test]
fn malformed_json_is_a_shape_error() {
    assert!(parse_anthropic_usage("{not json", None).is_err());
}

#[test]
fn empty_object_is_explicitly_unavailable() {
    let parsed = parse_anthropic_usage("{}", None).unwrap();
    assert!(parsed.windows.is_empty());
    assert_eq!(parsed.meters, vec![UsageMeter::Unavailable]);
}

#[test]
fn window_without_utilization_is_omitted() {
    let json =
        r#"{"five_hour":{"resets_at":"2026-05-30T21:30:00Z"},"seven_day":{"utilization":10.0}}"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    assert_eq!(parsed.windows.len(), 1);
    assert_eq!(parsed.windows[0].label, "7-day");
}

#[test]
fn enterprise_spend_with_limit_is_metered() {
    let json = r#"{
        "spend": {
            "used": {"amount_minor": 2500, "currency": "USD", "exponent": 2},
            "limit": {"amount_minor": 10000, "currency": "USD", "exponent": 2},
            "enabled": true
        }
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    assert!(parsed.windows.is_empty());
    match &parsed.meters[..] {
        [UsageMeter::Metered { amount }] => {
            assert_eq!(amount.used, 25.0);
            assert_eq!(amount.limit, Some(100.0));
            assert_eq!(amount.remaining, Some(75.0));
            assert_eq!(amount.unit, "USD");
            assert_eq!(amount.used_percent, Some(25.0));
        }
        other => panic!("expected metered spend, got {other:?}"),
    }
}

#[test]
fn enterprise_spend_without_limit_is_uncapped() {
    let json = r#"{
        "spend": {
            "used": {"amount_minor": 325, "currency": "USD", "exponent": 2},
            "limit": null,
            "enabled": false
        }
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::NoIndividualLimit { amount }] => {
            assert_eq!(amount.used, 3.25);
            assert_eq!(amount.limit, None);
            assert_eq!(amount.remaining, None);
            assert_eq!(amount.unit, "USD");
            assert!(amount.used_percent.is_none());
        }
        other => panic!("expected uncapped spend, got {other:?}"),
    }
}

#[test]
fn enterprise_extra_usage_carries_its_monthly_reset() {
    let json = r#"{
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 2050,
            "used_credits": 325,
            "currency": "USD",
            "resets_at": "2026-12-01T00:00:00Z"
        }
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::Metered { amount }] => {
            assert_eq!(amount.resets_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        }
        other => panic!("expected metered extra_usage, got {other:?}"),
    }
}

#[test]
fn active_monthly_spend_limit_supplies_the_reset_when_spend_omits_it() {
    let json = r#"{
        "spend": {
            "used": {"amount_minor": 2500, "currency": "USD", "exponent": 2},
            "limit": {"amount_minor": 10000, "currency": "USD", "exponent": 2}
        },
        "limits": [
            {"kind":"session","group":"session","percent":2,"resets_at":"2026-11-25T12:00:00Z","is_active":false},
            {"kind":"spend","group":"monthly","percent":25,"resets_at":"2026-12-01T00:00:00Z","is_active":true}
        ]
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::Metered { amount }] => {
            assert_eq!(amount.resets_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        }
        other => panic!("expected metered spend, got {other:?}"),
    }
}

#[test]
fn extra_usage_reset_fills_in_when_spend_has_no_reset() {
    let json = r#"{
        "spend": {
            "used": {"amount_minor": 2500, "currency": "USD", "exponent": 2},
            "limit": {"amount_minor": 10000, "currency": "USD", "exponent": 2}
        },
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 10000,
            "used_credits": 2500,
            "resets_at": "2026-12-01T00:00:00Z"
        }
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::Metered { amount }] => {
            assert_eq!(amount.resets_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        }
        other => panic!("expected metered spend, got {other:?}"),
    }
}

#[test]
fn response_header_reset_fills_in_when_the_body_omits_it() {
    let json = r#"{
        "spend": {
            "used": {"amount_minor": 2500, "currency": "USD", "exponent": 2},
            "limit": {"amount_minor": 10000, "currency": "USD", "exponent": 2}
        }
    }"#;
    let parsed = parse_anthropic_usage(json, Some("2026-12-01T00:00:00Z".to_string())).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::Metered { amount }] => {
            assert_eq!(amount.resets_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        }
        other => panic!("expected metered spend, got {other:?}"),
    }
}

#[test]
fn body_reset_wins_over_the_response_header() {
    let json = r#"{
        "spend": {
            "used": {"amount_minor": 2500, "currency": "USD", "exponent": 2},
            "limit": {"amount_minor": 10000, "currency": "USD", "exponent": 2},
            "resets_at": "2026-12-01T00:00:00Z"
        }
    }"#;
    let parsed = parse_anthropic_usage(json, Some("2026-11-15T00:00:00Z".to_string())).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::Metered { amount }] => {
            assert_eq!(amount.resets_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        }
        other => panic!("expected metered spend, got {other:?}"),
    }
}

#[test]
fn extra_usage_is_used_when_spend_is_absent() {
    let json = r#"{
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 2050,
            "used_credits": 325,
            "currency": "USD"
        }
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::Metered { amount }] => {
            assert_eq!(amount.used, 3.25);
            assert_eq!(amount.limit, Some(20.5));
            assert_eq!(amount.remaining, Some(17.25));
            assert_eq!(amount.unit, "USD");
        }
        other => panic!("expected extra_usage meter, got {other:?}"),
    }
}

#[test]
fn extra_usage_without_limit_is_uncapped() {
    let json = r#"{
        "extra_usage": {
            "is_enabled": true,
            "used_credits": 0,
            "utilization": 0.0
        }
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::NoIndividualLimit { amount }] => {
            assert_eq!(amount.used, 0.0);
            assert_eq!(amount.used_percent, Some(0.0));
        }
        other => panic!("expected uncapped extra usage, got {other:?}"),
    }
}

#[test]
fn disabled_extra_usage_is_ignored() {
    let json = r#"{
        "five_hour":{"utilization":1.0},
        "extra_usage":{"is_enabled":false,"monthly_limit":1000,"used_credits":10}
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    assert_eq!(parsed.windows.len(), 1);
    assert!(parsed.meters.is_empty());
}

#[test]
fn spend_is_preferred_over_extra_usage() {
    let json = r#"{
        "spend": {
            "used": {"amount_minor": 400, "currency": "USD", "exponent": 2},
            "limit": null
        },
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 9999,
            "used_credits": 1
        }
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    match &parsed.meters[..] {
        [UsageMeter::NoIndividualLimit { amount }] => {
            assert_eq!(amount.used, 4.0);
        }
        other => panic!("spend should win over extra_usage, got {other:?}"),
    }
}

#[test]
fn mixed_consumer_windows_and_spend_are_both_kept() {
    let json = r#"{
        "five_hour":{"utilization":10.0},
        "seven_day":{"utilization":20.0},
        "spend": {
            "used": {"amount_minor": 100, "currency": "USD", "exponent": 2},
            "limit": {"amount_minor": 500, "currency": "USD", "exponent": 2}
        }
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    assert_eq!(parsed.windows.len(), 2);
    assert!(matches!(parsed.meters[0], UsageMeter::Metered { .. }));
}

#[test]
fn stale_plan_fields_are_ignored() {
    // subscription_type / rate_limit_tier / their camelCase variants
    // are no longer in the parsed shape after #1689. Serde defaults
    // make them silently dropped, but a regression that re-introduces
    // a strict parser would fail this test. Lock the lenient behavior.
    let json = r#"{
        "five_hour":{"utilization":1.0},
        "subscription_type":"enterprise",
        "subscriptionType":"pro",
        "rate_limit_tier":"default",
        "rateLimitTier":"max"
    }"#;
    let parsed = parse_anthropic_usage(json, None).unwrap();
    assert_eq!(parsed.windows.len(), 1);
    assert!(parsed.meters.is_empty());
}
