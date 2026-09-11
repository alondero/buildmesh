//! Account quota returned by the same key reconciliation endpoint as Muse /usage.
use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{shared_client, UsageAdapter, UsageIdentityFingerprint};
use crate::services::usage::types::{unavailable, ProviderUsage, UsageMeter, UsageWindow};
use serde::Deserialize;

pub(crate) struct MuseCodeAdapter;
const ENDPOINT: &str = "https://api.meta.ai/muse-code/key";

impl UsageAdapter for MuseCodeAdapter {
    fn id(&self) -> &'static str {
        "muse-code"
    }
    fn native_harness(&self) -> Option<&'static str> {
        Some("muse")
    }
    fn cache_identity(&self, _: &[ProviderAccount]) -> UsageIdentityFingerprint {
        let identity = credential().unwrap_or_else(|e| e);
        UsageIdentityFingerprint::new("muse-oauth", identity.as_bytes())
    }
    fn fetch(&self, _: &[ProviderAccount]) -> ProviderUsage {
        match credential() {
            Ok(token) => fetch_usage(&token, ENDPOINT),
            Err(error) => missing(error),
        }
    }
}

fn missing(error: String) -> ProviderUsage {
    let mut usage = unavailable("muse-code", error);
    usage.meters = vec![UsageMeter::Unavailable];
    usage
}

fn credential() -> Result<String, String> {
    let path = crate::env::muse_auth_path().ok_or(
        "Muse subscription credential location unavailable. Check WSL availability and unset META_API_KEY to use a Muse account login.",
    )?;
    let content = std::fs::read_to_string(path)
        .map_err(|_| "Cannot read Muse credentials. Run muse login in the harness environment.")?;
    parse_credential(&content)
}

fn parse_credential(content: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(content)
        .map_err(|_| "Invalid Muse credential file. Run muse login again.")?;
    let meta = &value["providers"]["meta"];
    if meta["mechanism"].as_str() != Some("oauth") {
        return Err("Muse subscription usage requires a Meta account login; API keys do not carry a subscription.".into());
    }
    meta["access_token"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "Muse login missing. Run muse login again.".into())
}

fn fetch_usage(token: &str, endpoint: &str) -> ProviderUsage {
    let result = (|| {
        let response = shared_client()?
            .post(endpoint)
            .bearer_auth(token)
            .json(&serde_json::json!({}))
            .send()
            .map_err(|_| "Cannot reach Muse subscription service.".to_string())?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(
                "Muse login expired or rejected. Run muse login in the harness environment.".into(),
            );
        }
        if !response.status().is_success() {
            return Err(format!(
                "Muse subscription service returned HTTP {}.",
                response.status().as_u16()
            ));
        }
        // The response also contains an API key. Deserialize only quota fields;
        // never persist or expose the minted key or personal account details.
        let snapshot: Snapshot = response
            .json()
            .map_err(|_| "Invalid Muse subscription response.".to_string())?;
        snapshot.into_usage()
    })();
    result.unwrap_or_else(missing)
}

#[derive(Deserialize)]
struct Snapshot {
    is_subs_active: bool,
    subs_tier_name: Option<String>,
    subs_usage: Option<Quota>,
}

#[derive(Deserialize)]
struct Quota {
    window: Window,
    weekly: Window,
}

#[derive(Deserialize)]
struct Window {
    used_percent: f64,
    resets_at: i64,
}

impl Window {
    fn into_window(self, label: &str) -> Result<UsageWindow, String> {
        if !self.used_percent.is_finite() || !(0.0..=100.0).contains(&self.used_percent) {
            return Err("Invalid Muse usage percentage.".into());
        }
        let reset = chrono::DateTime::from_timestamp(self.resets_at, 0)
            .ok_or("Invalid Muse reset timestamp.")?;
        Ok(UsageWindow {
            label: label.into(),
            used_percent: Some(self.used_percent),
            resets_at: Some(reset.to_rfc3339()),
        })
    }
}

impl Snapshot {
    fn into_usage(self) -> Result<ProviderUsage, String> {
        if !self.is_subs_active {
            return Err("No active Muse Code subscription.".into());
        }
        let quota = self
            .subs_usage
            .ok_or("Muse did not report subscription usage.")?;
        Ok(ProviderUsage {
            provider: "muse-code".into(),
            logged_in: true,
            windows: vec![
                quota.window.into_window("Current")?,
                quota.weekly.into_window("Weekly")?,
            ],
            balance: None,
            meters: vec![],
            detail: self.subs_tier_name,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BODY: &str = r#"{"is_subs_active":true,"subs_tier_name":"Muse Code Everyday Usage","subs_usage":{"window":{"used_percent":94,"window_duration_mins":300,"resets_at":1789161315},"weekly":{"used_percent":35,"resets_at":1789344000}}}"#;

    #[test]
    fn server_snapshot_replaces_local_request_estimates() {
        let usage = serde_json::from_str::<Snapshot>(BODY)
            .unwrap()
            .into_usage()
            .unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "Current");
        assert_eq!(usage.windows[0].used_percent, Some(94.0));
        assert_eq!(usage.windows[1].label, "Weekly");
        assert_eq!(usage.windows[1].used_percent, Some(35.0));
        assert_eq!(
            usage.windows[1].resets_at.as_deref(),
            Some("2026-09-14T00:00:00+00:00")
        );
        assert_eq!(usage.detail.as_deref(), Some("Muse Code Everyday Usage"));
        assert!(usage.meters.is_empty());
    }

    #[test]
    fn invalid_and_missing_quota_never_become_full_allowance() {
        for body in [
            BODY.replace("94", "101"),
            BODY.replace("94", "-1"),
            BODY.replace("\"used_percent\":94,", ""),
            BODY.replace("true", "false"),
            BODY.replace("1789161315", "9223372036854775807"),
        ] {
            let result = serde_json::from_str::<Snapshot>(&body)
                .map_err(|e| e.to_string())
                .and_then(Snapshot::into_usage);
            assert!(result.is_err());
        }
        for percent in [0, 100] {
            let usage = serde_json::from_str::<Snapshot>(&BODY.replace("94", &percent.to_string()))
                .unwrap()
                .into_usage()
                .unwrap();
            assert_eq!(usage.windows[0].used_percent, Some(percent as f64));
        }
    }

    #[test]
    fn credentials_require_oauth_and_never_surface_secrets_in_errors() {
        assert_eq!(
            parse_credential(
                r#"{"providers":{"meta":{"mechanism":"oauth","access_token":"test-token"}}}"#
            )
            .unwrap(),
            "test-token"
        );
        for body in [
            "secret-invalid-json",
            r#"{"providers":{"meta":{"mechanism":"api_key","api_key":"secret"}}}"#,
            "{}",
        ] {
            let error = parse_credential(body).unwrap_err();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn http_boundary_posts_oauth_and_handles_rejected_and_malformed_responses() {
        use std::io::{Read, Write};
        for (status, body, succeeds) in [
            (200, BODY, true),
            (401, "secret", false),
            (403, "secret", false),
            (500, "secret", false),
            (200, "secret", false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/muse-code/key", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                while !request.ends_with(b"\r\n\r\n{}") {
                    let n = stream.read(&mut buf).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buf[..n]);
                }
                let request = String::from_utf8(request).unwrap().to_lowercase();
                assert!(request.starts_with("post /muse-code/key http/1.1"));
                assert!(request.contains("authorization: bearer test-token\r\n"));
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let usage = fetch_usage("test-token", &endpoint);
            server.join().unwrap();
            assert_eq!(usage.error.is_none(), succeeds);
            if !succeeds {
                assert!(usage.windows.is_empty());
                assert_eq!(usage.meters, vec![UsageMeter::Unavailable]);
                assert!(!usage.error.unwrap().contains("secret"));
            }
        }
    }

    #[test]
    #[ignore = "requires authenticated Muse installation"]
    fn live_muse_subscription() {
        let usage = MuseCodeAdapter.fetch(&[]);
        assert!(usage.error.is_none(), "{:?}", usage.error);
        assert_eq!(usage.windows.len(), 2);
        println!("{}", serde_json::to_string(&usage).unwrap());
    }
}
