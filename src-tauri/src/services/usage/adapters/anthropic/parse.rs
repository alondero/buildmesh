//! Parse Claude OAuth `/api/oauth/usage` bodies.
//!
//! Consumer plans report rolling `five_hour` / `seven_day` windows. Enterprise
//! reports monthly spend: prefer the current `spend` object, fall back to the
//! older `extra_usage` credits shape. Partial bodies are valid — a missing
//! window is omitted, not treated as an error.
//!
//! Note: the `subscription_type` / `rate_limit_tier` plan fields on the
//! response body are no longer surfaced after #1689 cut
//! `ProviderUsage.plan`; serde ignores unknown fields so a stale
//! response shape still parses cleanly.

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
    #[serde(default)]
    resets_at: Option<Value>,
}

#[derive(Deserialize, Debug)]
struct Spend {
    used: Option<Value>,
    limit: Option<Value>,
    #[serde(default)]
    resets_at: Option<Value>,
}

#[derive(Deserialize, Debug)]
struct LimitEntry {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    resets_at: Option<Value>,
    #[serde(default)]
    is_active: Option<bool>,
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
    #[serde(default)]
    limits: Option<Vec<LimitEntry>>,
}

pub(crate) fn parse_anthropic_usage(
    body: &str,
    fallback_reset: Option<String>,
) -> Result<ParsedAnthropicUsage, UsageError> {
    let resp: Resp = serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = Vec::new();
    push_window(&mut windows, "5-hour", resp.five_hour);
    push_window(&mut windows, "7-day", resp.seven_day);
    push_window(&mut windows, "7-day Sonnet", resp.seven_day_sonnet);

    let resets_at = spend_reset(resp.spend.as_ref())
        .or_else(|| active_monthly_limit_reset(resp.limits.as_deref().unwrap_or_default()))
        .or_else(|| extra_usage_reset(resp.extra_usage.as_ref()))
        .or(fallback_reset);

    let mut meters = Vec::new();
    if let Some(meter) = spend_meter(resp.spend.as_ref(), resets_at.clone()) {
        meters.push(meter);
    } else if let Some(meter) = extra_usage_meter(resp.extra_usage.as_ref(), resets_at) {
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

fn spend_meter(spend: Option<&Spend>, resets_at: Option<String>) -> Option<UsageMeter> {
    let spend = spend?;
    let (used, unit) = parse_money(spend.used.as_ref()?)?;
    let limit = spend.limit.as_ref().and_then(parse_money);
    Some(amount_meter(
        used,
        limit.map(|(value, _)| value),
        unit,
        None,
        resets_at,
    ))
}

fn extra_usage_meter(extra: Option<&ExtraUsage>, resets_at: Option<String>) -> Option<UsageMeter> {
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
    Some(amount_meter(
        used,
        limit,
        unit,
        extra.utilization,
        resets_at,
    ))
}

fn spend_reset(spend: Option<&Spend>) -> Option<String> {
    reset_value(spend?.resets_at.as_ref())
}

fn extra_usage_reset(extra: Option<&ExtraUsage>) -> Option<String> {
    reset_value(extra?.resets_at.as_ref())
}

fn active_monthly_limit_reset(limits: &[LimitEntry]) -> Option<String> {
    limits
        .iter()
        .filter(|entry| entry.is_active.unwrap_or(true))
        .find(|entry| {
            entry.group.as_deref() == Some("monthly")
                || (entry.group.is_none() && entry.kind.as_deref() == Some("spend"))
        })
        .and_then(|entry| reset_value(entry.resets_at.as_ref()))
}

fn reset_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Number(number) => number
            .as_i64()
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
            .map(|instant| instant.to_rfc3339()),
        _ => None,
    }
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
            let exponent = object.get("exponent").and_then(Value::as_i64).unwrap_or(2) as i32;
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

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
