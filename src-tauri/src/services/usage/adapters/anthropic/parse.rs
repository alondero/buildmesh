//! Parse Claude OAuth `/api/oauth/usage` bodies.
//!
//! Consumer plans report rolling `five_hour` / `seven_day` windows. Enterprise
//! reports monthly spend: prefer the current `spend` object, fall back to the
//! older `extra_usage` credits shape. Partial bodies are valid — a missing
//! window is omitted, not treated as an error.

use crate::services::usage::types::{UsageAmount, UsageError, UsageMeter, UsageWindow};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct ParsedAnthropicUsage {
    pub windows: Vec<UsageWindow>,
    pub meters: Vec<UsageMeter>,
}

#[derive(Deserialize, Debug)]
struct UsageBucket {
    utilization: Option<f64>,
    #[serde(rename = "resets_at")]
    resets_at: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ExtraUsage {
    #[serde(rename = "is_enabled")]
    is_enabled: Option<bool>,
    #[serde(rename = "monthly_limit")]
    monthly_limit: Option<f64>,
    #[serde(rename = "used_credits")]
    used_credits: Option<f64>,
    utilization: Option<f64>,
    currency: Option<String>,
}

#[derive(Deserialize, Debug)]
struct Spend {
    used: Option<Value>,
    limit: Option<Value>,
}

#[derive(Deserialize, Debug)]
struct Resp {
    #[serde(default)]
    five_hour: Option<UsageBucket>,
    #[serde(default)]
    seven_day: Option<UsageBucket>,
    #[serde(default)]
    seven_day_sonnet: Option<UsageBucket>,
    #[serde(default)]
    spend: Option<Spend>,
    #[serde(default)]
    extra_usage: Option<ExtraUsage>,
}

pub(crate) fn parse_anthropic_usage(body: &str) -> Result<ParsedAnthropicUsage, UsageError> {
    let resp: Resp = serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = Vec::new();
    push_window(&mut windows, "5-hour", resp.five_hour);
    push_window(&mut windows, "7-day", resp.seven_day);
    push_window(&mut windows, "7-day Sonnet", resp.seven_day_sonnet);

    let mut meters = Vec::new();
    if let Some(meter) = spend_meter(resp.spend.as_ref()) {
        meters.push(meter);
    } else if let Some(meter) = extra_usage_meter(resp.extra_usage.as_ref()) {
        meters.push(meter);
    }

    if windows.is_empty() && meters.is_empty() {
        meters.push(UsageMeter::Unavailable);
    }

    Ok(ParsedAnthropicUsage { windows, meters })
}

fn push_window(windows: &mut Vec<UsageWindow>, label: &str, bucket: Option<UsageBucket>) {
    let Some(bucket) = bucket else {
        return;
    };
    let Some(util) = bucket.utilization else {
        return;
    };
    windows.push(UsageWindow {
        label: label.to_string(),
        used_percent: Some(util),
        resets_at: bucket.resets_at,
    });
}

fn spend_meter(spend: Option<&Spend>) -> Option<UsageMeter> {
    let spend = spend?;
    let (used, unit) = parse_money(spend.used.as_ref()?)?;
    let limit = spend.limit.as_ref().and_then(parse_money);
    Some(amount_meter(
        used,
        limit.map(|(value, _)| value),
        unit,
        None,
        None,
    ))
}

fn extra_usage_meter(extra: Option<&ExtraUsage>) -> Option<UsageMeter> {
    let extra = extra?;
    if extra.is_enabled != Some(true) {
        return None;
    }
    let used_credits = extra.used_credits?;
    let used = used_credits / 100.0;
    let limit = extra.monthly_limit.map(|limit| limit / 100.0);
    let unit = extra
        .currency
        .as_deref()
        .filter(|currency| !currency.is_empty())
        .unwrap_or("USD")
        .to_string();
    Some(amount_meter(used, limit, unit, extra.utilization, None))
}

fn amount_meter(
    used: f64,
    limit: Option<f64>,
    unit: String,
    used_percent: Option<f64>,
    resets_at: Option<String>,
) -> UsageMeter {
    let remaining = limit.map(|limit| limit - used);
    let used_percent = used_percent.or_else(|| {
        limit.and_then(|limit| {
            if limit > 0.0 {
                Some((used / limit) * 100.0)
            } else {
                None
            }
        })
    });
    let amount = UsageAmount {
        used,
        limit,
        remaining,
        unit,
        used_percent,
        resets_at,
    };
    if limit.is_some() {
        UsageMeter::Metered { amount }
    } else {
        UsageMeter::NoIndividualLimit { amount }
    }
}

fn parse_money(value: &Value) -> Option<(f64, String)> {
    match value {
        Value::Null => None,
        Value::Number(number) => Some((number.as_f64()?, "USD".to_string())),
        Value::Object(object) => {
            let minor = object.get("amount_minor")?.as_f64()?;
            let exponent = object
                .get("exponent")
                .and_then(Value::as_i64)
                .unwrap_or(2) as i32;
            let currency = object
                .get("currency")
                .and_then(Value::as_str)
                .filter(|currency| !currency.is_empty())
                .unwrap_or("USD")
                .to_string();
            Some((minor / 10f64.powi(exponent), currency))
        }
        _ => None,
    }
}

/// Provider-reported plan label. Unknown names pass through unchanged so we
/// never collapse distinct plans into one another.
pub(crate) fn plan_label(
    subscription_type: Option<&str>,
    rate_limit_tier: Option<&str>,
) -> Option<String> {
    let raw = subscription_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            rate_limit_tier
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })?;
    Some(match raw.to_ascii_lowercase().as_str() {
        "pro" => "Pro".to_string(),
        "max" => "Max".to_string(),
        "team" => "Team".to_string(),
        "enterprise" => "Enterprise".to_string(),
        _ => raw.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_windows_keep_five_hour_and_seven_day() {
        let json = r#"{
            "five_hour":{"utilization":41.0,"resets_at":"2026-05-30T21:30:00.379395+00:00"},
            "seven_day":{"utilization":33.0,"resets_at":"2026-06-05T04:00:00.379418+00:00"}
        }"#;
        let parsed = parse_anthropic_usage(json).unwrap();
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
        let parsed = parse_anthropic_usage(json).unwrap();
        assert_eq!(parsed.windows[0].used_percent, Some(0.0));
        assert_eq!(parsed.windows[1].used_percent, Some(0.0));
    }

    #[test]
    fn seven_day_sonnet_window_is_kept() {
        let json = r#"{"seven_day_sonnet":{"utilization":12.5,"resets_at":"2026-06-01T00:00:00Z"}}"#;
        let parsed = parse_anthropic_usage(json).unwrap();
        assert_eq!(parsed.windows[0].label, "7-day Sonnet");
        assert_eq!(parsed.windows[0].used_percent, Some(12.5));
    }

    #[test]
    fn malformed_json_is_a_shape_error() {
        assert!(parse_anthropic_usage("{not json").is_err());
    }

    #[test]
    fn empty_object_is_explicitly_unavailable() {
        let parsed = parse_anthropic_usage("{}").unwrap();
        assert!(parsed.windows.is_empty());
        assert_eq!(parsed.meters, vec![UsageMeter::Unavailable]);
    }

    #[test]
    fn window_without_utilization_is_omitted() {
        let json = r#"{"five_hour":{"resets_at":"2026-05-30T21:30:00Z"},"seven_day":{"utilization":10.0}}"#;
        let parsed = parse_anthropic_usage(json).unwrap();
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
        let parsed = parse_anthropic_usage(json).unwrap();
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
        let parsed = parse_anthropic_usage(json).unwrap();
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
    fn extra_usage_is_used_when_spend_is_absent() {
        let json = r#"{
            "extra_usage": {
                "is_enabled": true,
                "monthly_limit": 2050,
                "used_credits": 325,
                "currency": "USD"
            }
        }"#;
        let parsed = parse_anthropic_usage(json).unwrap();
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
        let parsed = parse_anthropic_usage(json).unwrap();
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
        let parsed = parse_anthropic_usage(json).unwrap();
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
        let parsed = parse_anthropic_usage(json).unwrap();
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
        let parsed = parse_anthropic_usage(json).unwrap();
        assert_eq!(parsed.windows.len(), 2);
        assert!(matches!(parsed.meters[0], UsageMeter::Metered { .. }));
    }

    #[test]
    fn unknown_plan_names_pass_through() {
        assert_eq!(plan_label(Some("enterprise"), None).as_deref(), Some("Enterprise"));
        assert_eq!(plan_label(Some("pro"), None).as_deref(), Some("Pro"));
        assert_eq!(
            plan_label(Some("Business Plus"), None).as_deref(),
            Some("Business Plus")
        );
        assert_eq!(plan_label(None, Some("default_claude_max_20x")).as_deref(), Some("default_claude_max_20x"));
        assert_eq!(plan_label(Some(""), Some("")).as_deref(), None);
    }
}
