use super::super::adapter::{shared_client, UsageAdapter, UsageIdentityFingerprint};
use super::super::outcome::UsageOutcome;
use super::super::types::{BillingBalance, UsageAmount, UsageMeter, UsagePage, UsageWindow};
use crate::preferences::ProviderAccount;
use std::path::PathBuf;

pub(crate) struct DevinAdapter;

const DEFAULT_SERVER: &str = "https://server.codeium.com";
const USER_STATUS_PATH: &str = "/exa.seat_management_pb.SeatManagementService/GetUserStatus";
const CLI_VERSION: &str = "3000.14.30821";

impl UsageAdapter for DevinAdapter {
    fn id(&self) -> &'static str {
        "devin"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("devin")
    }

    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new(
            "https://app.devin.ai/settings/usage",
            "View usage",
        ))
    }

    fn cache_identity(&self, _: &[ProviderAccount]) -> UsageIdentityFingerprint {
        fingerprint_for(resolve(&production_inputs()).as_ref())
    }

    fn fetch(&self, _: &[ProviderAccount]) -> UsageOutcome {
        fetch_with(resolve(&production_inputs()))
    }
}

struct Inputs {
    credentials_file: Option<PathBuf>,
    env_api_key: Option<String>,
    env_server_url: Option<String>,
}

struct Credentials {
    api_key: String,
    server_url: String,
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn credentials_path() -> Option<PathBuf> {
    if cfg!(windows) {
        return non_empty_env("APPDATA")
            .map(|dir| PathBuf::from(dir).join("devin").join("credentials.toml"));
    }
    let base = non_empty_env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            Some(
                crate::services::usage::types::home_dir()
                    .join(".local")
                    .join("share"),
            )
        })?;
    Some(base.join("devin").join("credentials.toml"))
}

fn production_inputs() -> Inputs {
    Inputs {
        credentials_file: credentials_path(),
        env_api_key: non_empty_env("WINDSURF_API_KEY"),
        env_server_url: non_empty_env("WINDSURF_API_SERVER_URL"),
    }
}

fn toml_string(document: &toml_edit::DocumentMut, key: &str) -> Option<String> {
    document
        .get(key)
        .and_then(|item| item.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve(inputs: &Inputs) -> Option<Credentials> {
    let file = inputs
        .credentials_file
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| raw.parse::<toml_edit::DocumentMut>().ok());
    let (api_key, file_server) = match &file {
        Some(document) => (
            toml_string(document, "windsurf_api_key")?,
            toml_string(document, "api_server_url"),
        ),
        None => (inputs.env_api_key.clone()?, None),
    };
    let server_url = inputs
        .env_server_url
        .clone()
        .or(file_server)
        .unwrap_or_else(|| DEFAULT_SERVER.to_string());
    Some(Credentials {
        api_key,
        server_url: server_url.trim().trim_end_matches('/').to_string(),
    })
}

fn fingerprint_for(credentials: Option<&Credentials>) -> UsageIdentityFingerprint {
    match credentials {
        Some(credentials) => {
            UsageIdentityFingerprint::new("devin-cli", credentials.api_key.as_bytes())
        }
        None => UsageIdentityFingerprint::new("devin-cli", b"signed-out"),
    }
}

fn not_signed_in() -> UsageOutcome {
    UsageOutcome::NoCredential {
        hint: "Devin is not signed in. Run devin auth login.".into(),
    }
}

fn login_rejected() -> UsageOutcome {
    UsageOutcome::Rejected {
        hint: "Devin login expired. Run devin auth login.".into(),
    }
}

fn supported_http(url: &str) -> bool {
    reqwest::Url::parse(url)
        .map(|parsed| matches!(parsed.scheme(), "http" | "https"))
        .unwrap_or(false)
}

fn fetch_with(credentials: Option<Credentials>) -> UsageOutcome {
    let Some(credentials) = credentials else {
        return not_signed_in();
    };
    if !supported_http(&credentials.server_url) {
        return UsageOutcome::Unavailable {
            reason: "Devin sign-in points at an unsupported address.".into(),
        };
    }
    let client = match shared_client() {
        Ok(client) => client,
        Err(reason) => return UsageOutcome::Unavailable { reason },
    };
    let body = serde_json::json!({
        "metadata": {
            "api_key": credentials.api_key,
            "ide_name": "devin-cli",
            "ide_version": CLI_VERSION,
            "extension_name": "chisel",
            "extension_version": CLI_VERSION,
        }
    })
    .to_string();
    let response = client
        .post(format!("{}{}", credentials.server_url, USER_STATUS_PATH))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .header("Accept", "application/json")
        .header("User-Agent", "Buildmesh-Usage-Probe/1.0")
        .body(body)
        .send();
    let response = match response {
        Ok(response) => response,
        Err(_) => {
            return UsageOutcome::Unavailable {
                reason: "Cannot reach Devin to read usage.".into(),
            }
        }
    };
    let status = response.status().as_u16();
    let text = response.text().unwrap_or_default();
    match status {
        200..=299 => outcome_from_body(&text),
        401 | 403 => login_rejected(),
        429 => UsageOutcome::RateLimited {
            reason: "Rate limited — usage data temporarily unavailable".into(),
        },
        400 if connect_code(&text)
            .is_some_and(|code| code == "invalid_argument" || code == "unauthenticated") =>
        {
            login_rejected()
        }
        _ => UsageOutcome::Unavailable {
            reason: format!("Devin usage service returned HTTP {status}."),
        },
    }
}

fn connect_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("code")?
        .as_str()
        .map(str::to_ascii_lowercase)
}

fn camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = false;
    for ch in snake.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn lookup<'a>(scopes: &[&'a serde_json::Value], snake: &str) -> Option<&'a serde_json::Value> {
    let camel = camel(snake);
    scopes.iter().find_map(|scope| {
        scope
            .get(snake)
            .or_else(|| scope.get(camel.as_str()))
            .filter(|value| !value.is_null())
    })
}

fn number(scopes: &[&serde_json::Value], snake: &str) -> Option<f64> {
    let value = lookup(scopes, snake)?;
    value
        .as_f64()
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<f64>().ok())
        })
        .filter(|number| number.is_finite())
}

fn flag(scopes: &[&serde_json::Value], snake: &str) -> bool {
    lookup(scopes, snake)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn unix_to_rfc3339(seconds: f64) -> Option<String> {
    if seconds <= 0.0 {
        return None;
    }
    chrono::DateTime::from_timestamp(seconds as i64, 0).map(|instant| instant.to_rfc3339())
}

fn plan_end(scopes: &[&serde_json::Value]) -> Option<String> {
    let value = lookup(scopes, "plan_end")?;
    if let Some(text) = value.as_str() {
        let text = text.trim();
        if chrono::DateTime::parse_from_rfc3339(text).is_ok() {
            return Some(text.to_string());
        }
        return text.parse::<f64>().ok().and_then(unix_to_rfc3339);
    }
    if let Some(seconds) = value.get("seconds") {
        let seconds = seconds
            .as_f64()
            .or_else(|| seconds.as_str().and_then(|text| text.parse::<f64>().ok()))?;
        return unix_to_rfc3339(seconds);
    }
    value.as_f64().and_then(unix_to_rfc3339)
}

fn quota_window(
    scopes: &[&serde_json::Value],
    label: &str,
    remaining: &str,
    reset: &str,
) -> Option<UsageWindow> {
    let remaining = number(scopes, remaining)?;
    Some(UsageWindow {
        label: label.into(),
        used_percent: Some((100.0 - remaining).clamp(0.0, 100.0)),
        resets_at: number(scopes, reset).and_then(unix_to_rfc3339),
    })
}

fn outcome_from_body(body: &str) -> UsageOutcome {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(body) else {
        return UsageOutcome::Unavailable {
            reason: "Devin returned an unreadable usage response.".into(),
        };
    };
    let user = root
        .get("userStatus")
        .or_else(|| root.get("user_status"))
        .unwrap_or(&root);
    let plan_status = user.get("planStatus").or_else(|| user.get("plan_status"));
    let plan_info = user
        .get("planInfo")
        .or_else(|| user.get("plan_info"))
        .or_else(|| {
            plan_status
                .and_then(|status| status.get("planInfo").or_else(|| status.get("plan_info")))
        });
    let mut scopes: Vec<&serde_json::Value> = Vec::new();
    scopes.extend(plan_status);
    scopes.extend(plan_info);
    scopes.push(user);

    let mut windows = Vec::new();
    let mut meters = Vec::new();
    let mut balance = None;
    let mut detail = None;

    if !flag(&scopes, "hide_daily_quota") {
        windows.extend(quota_window(
            &scopes,
            "Daily",
            "daily_quota_remaining_percent",
            "daily_quota_reset_at_unix",
        ));
    }
    if !flag(&scopes, "hide_weekly_quota") {
        windows.extend(quota_window(
            &scopes,
            "Weekly",
            "weekly_quota_remaining_percent",
            "weekly_quota_reset_at_unix",
        ));
    }
    if let Some(micros) = number(&scopes, "overage_balance_micros").filter(|micros| *micros > 0.0) {
        balance = Some(BillingBalance {
            remaining: micros / 1_000_000.0,
            monthly_spend: None,
            currency: "USD".into(),
        });
    }

    if windows.is_empty() {
        let cycle_end = plan_end(&scopes);
        let (consumed, limit, unit, dollars) = if number(&scopes, "acu_consumed").is_some()
            || number(&scopes, "acu_limit").is_some()
        {
            (
                number(&scopes, "acu_consumed"),
                number(&scopes, "acu_limit"),
                "ACUs",
                false,
            )
        } else {
            (
                number(&scopes, "dollars_consumed"),
                number(&scopes, "dollars_limit"),
                "USD",
                true,
            )
        };
        let positive_limit = limit.filter(|limit| *limit > 0.0);
        match (consumed, positive_limit) {
            (Some(consumed), Some(limit)) => {
                windows.push(UsageWindow {
                    label: "Billing cycle".into(),
                    used_percent: Some((consumed / limit * 100.0).clamp(0.0, 100.0)),
                    resets_at: cycle_end,
                });
                detail = Some(if dollars {
                    format!("${consumed:.2} / ${limit:.2}")
                } else {
                    format!("{consumed:.2} / {limit:.2} {unit}")
                });
            }
            (Some(consumed), None) => {
                meters.push(UsageMeter::NoIndividualLimit {
                    amount: UsageAmount {
                        used: consumed,
                        limit: None,
                        remaining: None,
                        unit: unit.into(),
                        used_percent: None,
                        resets_at: cycle_end,
                    },
                });
            }
            (None, Some(limit)) => {
                detail = Some(if dollars {
                    format!("Limit ${limit:.2}")
                } else {
                    format!("{unit} limit {limit:.2}")
                });
            }
            (None, None) => {
                let available = number(&scopes, "available_prompt_credits").filter(|v| *v > 0.0);
                let used = number(&scopes, "used_prompt_credits");
                if let (Some(available), Some(used)) = (available, used) {
                    windows.push(UsageWindow {
                        label: "Credits".into(),
                        used_percent: Some((used / available * 100.0).clamp(0.0, 100.0)),
                        resets_at: cycle_end,
                    });
                }
            }
        }
    }

    if windows.is_empty() && meters.is_empty() && balance.is_none() && detail.is_none() {
        detail = Some("No quota data available".into());
    }
    UsageOutcome::Reading {
        windows,
        balance,
        meters,
        detail,
    }
}

#[cfg(test)]
#[path = "devin_tests.rs"]
mod tests;
