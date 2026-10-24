//! Perch subscription usage.
//!
//! Perch (`perchai-cli`) signs in on its own. The session file is
//! `~/.perch/cli-auth-session.json`, or `$PERCH_CLI_AUTH_DIR/cli-auth-session.json`.
//! On macOS the session itself lives in the login keychain (service
//! `app.perchai.cli-auth`, account `default`) unless that directory is set.
//! The refresh lock and the invalid-grant marker always live in the file
//! directory, including when the session is in the keychain.
//!
//! An expired access token is refreshed against Supabase and the rotated
//! refresh token is written back. A failed refresh does not delete the
//! session. Confirmed against perchai-cli 2.4.116 (`dist/perch.mjs`), not a
//! captured live account snapshot. 1 PT = $0.001.

use super::super::adapter::{shared_client, UsageAdapter, UsageIdentityFingerprint};
use super::super::outcome::UsageOutcome;
use super::super::types::{UsageAmount, UsageMeter, UsagePage, UsageWindow};
use crate::preferences::ProviderAccount;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) struct PerchAdapter;

const SESSION_FILE: &str = "cli-auth-session.json";
const LOCK_FILE: &str = "cli-auth-refresh.lock";
const INVALID_FILE: &str = "cli-auth-refresh.invalid.json";
const SKEW_SECS: i64 = 90;
const LOCK_STALE: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_secs(15);
const INVALID_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const STARTER_MONTHLY_PT: f64 = 20_000.0;
const PRO_MANUAL_MONTHLY_PT: f64 = 75_000.0;

impl UsageAdapter for PerchAdapter {
    fn id(&self) -> &'static str {
        "perch"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("perch")
    }

    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new(
            "https://www.perchai.app/docs/plans/pro",
            "Usage guide",
        ))
    }

    fn cache_identity(&self, _: &[ProviderAccount]) -> UsageIdentityFingerprint {
        fingerprint_at(&production_location())
    }

    fn fetch(&self, _: &[ProviderAccount]) -> UsageOutcome {
        fetch_at(&production_location())
    }
}

struct AuthLocation {
    dir: PathBuf,
    keychain: bool,
}

#[derive(Clone)]
struct Session {
    app_url: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
    user_id: Option<String>,
    email: Option<String>,
    updated_at: String,
}

fn production_location() -> AuthLocation {
    if let Some(dir) = env_auth_dir() {
        return AuthLocation {
            dir,
            keychain: false,
        };
    }
    AuthLocation {
        dir: crate::services::usage::types::home_dir().join(".perch"),
        keychain: cfg!(target_os = "macos"),
    }
}

fn env_auth_dir() -> Option<PathBuf> {
    std::env::var("PERCH_CLI_AUTH_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn fingerprint_at(loc: &AuthLocation) -> UsageIdentityFingerprint {
    match read_session(loc) {
        Some(session) => {
            let identity = session
                .user_id
                .filter(|id| !id.trim().is_empty())
                .unwrap_or(session.access_token);
            UsageIdentityFingerprint::new("perch-cli", identity.as_bytes())
        }
        None => UsageIdentityFingerprint::new("perch-cli", b"signed-out"),
    }
}

fn fetch_at(loc: &AuthLocation) -> UsageOutcome {
    let Some(session) = read_session(loc) else {
        return not_signed_in();
    };
    let now = chrono::Utc::now().timestamp();
    let session = if access_token_usable(&session, now) {
        session
    } else if refresh_token_of(&session).is_some() {
        match ensure_token(loc, &session) {
            Ok(refreshed) => refreshed,
            Err(outcome) => return outcome,
        }
    } else {
        return login_expired();
    };
    match account_request(&session) {
        AccountHit::Body(body) => outcome_from_account(&body),
        AccountHit::Auth => retry_after_auth(loc, &session),
        AccountHit::RateLimited => rate_limited(),
        AccountHit::Failed(reason) => UsageOutcome::Unavailable { reason },
    }
}

fn retry_after_auth(loc: &AuthLocation, session: &Session) -> UsageOutcome {
    if refresh_token_of(session).is_none() {
        return login_expired();
    }
    let refreshed = match ensure_token(loc, session) {
        Ok(session) => session,
        Err(outcome) => return outcome,
    };
    match account_request(&refreshed) {
        AccountHit::Body(body) => outcome_from_account(&body),
        AccountHit::Auth => login_expired(),
        AccountHit::RateLimited => rate_limited(),
        AccountHit::Failed(reason) => UsageOutcome::Unavailable { reason },
    }
}

fn not_signed_in() -> UsageOutcome {
    UsageOutcome::NoCredential {
        hint: "Perch is not signed in. Run perch login.".into(),
    }
}

fn login_expired() -> UsageOutcome {
    UsageOutcome::NoCredential {
        hint: "Perch login expired. Run perch login.".into(),
    }
}

fn rate_limited() -> UsageOutcome {
    UsageOutcome::RateLimited {
        reason: "Rate limited — usage data temporarily unavailable".into(),
    }
}

fn cannot_reach() -> UsageOutcome {
    UsageOutcome::Unavailable {
        reason: "Cannot reach Perch to refresh the sign-in.".into(),
    }
}

fn unsupported_address() -> UsageOutcome {
    UsageOutcome::Unavailable {
        reason: "Perch sign-in points at an unsupported address.".into(),
    }
}

fn refresh_http(status: u16) -> UsageOutcome {
    UsageOutcome::Unavailable {
        reason: format!("Perch sign-in refresh returned HTTP {status}."),
    }
}

/// `expires_at > now + 90`. Equality at the skew boundary is already expired.
/// A missing expiry is treated as usable until the account endpoint rejects it.
fn access_token_usable(session: &Session, now_unix: i64) -> bool {
    if session.access_token.trim().is_empty() || session.app_url.trim().is_empty() {
        return false;
    }
    match session.expires_at {
        Some(expires_at) => expires_at > now_unix + SKEW_SECS,
        None => true,
    }
}

fn read_session(loc: &AuthLocation) -> Option<Session> {
    let raw = if loc.keychain {
        #[cfg(target_os = "macos")]
        {
            read_keychain()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    } else {
        std::fs::read_to_string(loc.dir.join(SESSION_FILE)).ok()
    }?;
    parse_session(&raw)
}

fn parse_session(raw: &str) -> Option<Session> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if value.get("version").and_then(|v| v.as_i64()) != Some(1) {
        return None;
    }
    let app_url = required_string(&value, "appUrl")?;
    let access_token = required_string(&value, "accessToken")?;
    let updated_at = required_string(&value, "updatedAt")?;
    Some(Session {
        app_url,
        access_token,
        refresh_token: optional_string(&value, "refreshToken"),
        expires_at: value.get("expiresAt").and_then(|v| v.as_i64()),
        user_id: optional_string(&value, "userId"),
        email: optional_string(&value, "email"),
        updated_at,
    })
}

fn required_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn optional_string(value: &serde_json::Value, key: &str) -> Option<String> {
    required_string(value, key)
}

fn refresh_token_of(session: &Session) -> Option<&str> {
    session
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_lock(dir: &Path) -> Result<LockGuard, UsageOutcome> {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(LOCK_FILE);
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let body = serde_json::json!({
                    "pid": std::process::id(),
                    "createdAt": chrono::Utc::now().timestamp_millis(),
                });
                let _ = file.write_all(body.to_string().as_bytes());
                return Ok(LockGuard { path });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(&path) {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(UsageOutcome::Unavailable {
                        reason: "Perch sign-in refresh is busy. Try again in a moment.".into(),
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                return Err(UsageOutcome::Unavailable {
                    reason: "Cannot refresh the Perch sign-in.".into(),
                });
            }
        }
    }
}

fn lock_is_stale(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    match modified.elapsed() {
        Ok(age) => age >= LOCK_STALE,
        Err(_) => true,
    }
}

/// Refresh under the CLI's exclusive lock. If another writer already stored a
/// usable session, use that instead of rotating the refresh token again.
fn ensure_token(loc: &AuthLocation, expected: &Session) -> Result<Session, UsageOutcome> {
    let Some(refresh) = refresh_token_of(expected) else {
        return Err(login_expired());
    };
    if invalid_grant_blocks(loc, refresh) {
        return Err(login_expired());
    }
    let _guard = acquire_lock(&loc.dir)?;
    let current = read_session(loc);
    let candidate = match &current {
        Some(current) => {
            let now = chrono::Utc::now().timestamp();
            if session_changed(current, expected) && access_token_usable(current, now) {
                return Ok(current.clone());
            }
            if refresh_token_of(current).is_some() {
                current.clone()
            } else {
                expected.clone()
            }
        }
        None => expected.clone(),
    };
    let Some(refresh) = refresh_token_of(&candidate).map(str::to_string) else {
        return Err(login_expired());
    };
    if invalid_grant_blocks(loc, &refresh) {
        return Err(login_expired());
    }
    refresh_and_persist(loc, &candidate, &refresh)
}

fn session_changed(current: &Session, expected: &Session) -> bool {
    current.updated_at != expected.updated_at
        || current.access_token != expected.access_token
        || current.refresh_token != expected.refresh_token
}

fn invalid_path(loc: &AuthLocation) -> PathBuf {
    loc.dir.join(INVALID_FILE)
}

fn invalid_grant_blocks(loc: &AuthLocation, refresh: &str) -> bool {
    let path = invalid_path(loc);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return false,
    };
    let Some(parsed) = serde_json::from_str::<serde_json::Value>(&raw).ok() else {
        let _ = std::fs::remove_file(&path);
        return false;
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    match parsed.get("expiresAt").and_then(|value| value.as_i64()) {
        Some(expires_at) if expires_at > now_ms => {
            parsed.get("tokenHash").and_then(|value| value.as_str())
                == Some(token_hash(refresh).as_str())
        }
        _ => {
            let _ = std::fs::remove_file(&path);
            false
        }
    }
}

fn write_invalid_grant(loc: &AuthLocation, refresh: &str) {
    let _ = std::fs::create_dir_all(&loc.dir);
    let body = serde_json::json!({
        "tokenHash": token_hash(refresh),
        "expiresAt": chrono::Utc::now().timestamp_millis() + INVALID_TTL_MS,
    });
    let _ = std::fs::write(invalid_path(loc), body.to_string());
}

fn clear_invalid_grant(loc: &AuthLocation) {
    let _ = std::fs::remove_file(invalid_path(loc));
}

fn token_hash(refresh: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(refresh.as_bytes());
    hex::encode(hasher.finalize())
}

fn refresh_and_persist(
    loc: &AuthLocation,
    session: &Session,
    refresh: &str,
) -> Result<Session, UsageOutcome> {
    let app_url = trim_slashes(&session.app_url);
    if !supported_http(&app_url) {
        return Err(unsupported_address());
    }
    let config_url = format!("{app_url}/api/perch-terminal/cli-auth/config");
    let config = http_get(&config_url, None).map_err(|_| cannot_reach())?;
    if config.status == 429 {
        return Err(rate_limited());
    }
    if !(200..300).contains(&config.status) {
        return Err(refresh_http(config.status));
    }
    let cfg: serde_json::Value =
        serde_json::from_str(&config.body).unwrap_or(serde_json::Value::Null);
    if cfg.get("ok").and_then(|value| value.as_bool()) != Some(true) {
        return Err(unsupported_address());
    }
    let Some(supabase) = json_string(&cfg, &["supabaseUrl", "supabase_url"]) else {
        return Err(unsupported_address());
    };
    let Some(anon) = json_string(&cfg, &["supabaseAnonKey", "supabase_anon_key"]) else {
        return Err(unsupported_address());
    };
    let supabase = trim_slashes(&supabase);
    if !supported_http(&supabase) {
        return Err(unsupported_address());
    }
    let token_url = format!("{supabase}/auth/v1/token?grant_type=refresh_token");
    let body = serde_json::json!({ "refresh_token": refresh }).to_string();
    let token = http_post(&token_url, &anon, &body).map_err(|_| cannot_reach())?;
    if token.status == 429 {
        return Err(rate_limited());
    }
    if token.status == 400 && invalid_grant_body(&token.body) {
        write_invalid_grant(loc, refresh);
        return Err(login_expired());
    }
    if !(200..300).contains(&token.status) {
        return Err(refresh_http(token.status));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&token.body).map_err(|_| UsageOutcome::Unavailable {
            reason: "Perch sign-in refresh returned an unreadable response.".into(),
        })?;
    let Some(access_token) = json_string(&parsed, &["access_token", "accessToken"]) else {
        return Err(UsageOutcome::Unavailable {
            reason: "Perch sign-in refresh returned an unreadable response.".into(),
        });
    };
    let new_refresh = json_string(&parsed, &["refresh_token", "refreshToken"])
        .unwrap_or_else(|| refresh.to_string());
    let expires_at = parsed
        .get("expires_at")
        .and_then(|value| value.as_i64())
        .or_else(|| parsed.get("expiresAt").and_then(|value| value.as_i64()));
    let user = parsed.get("user");
    let user_id = user
        .and_then(|value| json_string(value, &["id"]))
        .or_else(|| session.user_id.clone());
    let email = user
        .and_then(|value| json_string(value, &["email"]))
        .or_else(|| session.email.clone());
    let updated = Session {
        app_url: session.app_url.clone(),
        access_token,
        refresh_token: Some(new_refresh),
        expires_at,
        user_id,
        email,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    persist_session(loc, &updated)?;
    clear_invalid_grant(loc);
    Ok(updated)
}

fn invalid_grant_body(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let code = json_string(&value, &["code", "error", "error_code"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = json_string(&value, &["message", "error_description", "msg"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    code == "refresh_token_not_found"
        || code == "invalid_grant"
        || message.contains("refresh token")
        || message.contains("invalid grant")
}

fn persist_session(loc: &AuthLocation, session: &Session) -> Result<(), UsageOutcome> {
    let json = session_json(session);
    if loc.keychain {
        #[cfg(target_os = "macos")]
        {
            return write_keychain(&json);
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Err(UsageOutcome::Unavailable {
                reason: "Cannot save the Perch sign-in.".into(),
            });
        }
    }
    write_session_file(&loc.dir, &json)
}

fn session_json(session: &Session) -> String {
    serde_json::json!({
        "version": 1,
        "appUrl": session.app_url,
        "accessToken": session.access_token,
        "refreshToken": session.refresh_token,
        "expiresAt": session.expires_at,
        "userId": session.user_id,
        "email": session.email,
        "updatedAt": session.updated_at,
    })
    .to_string()
}

fn write_session_file(dir: &Path, json: &str) -> Result<(), UsageOutcome> {
    std::fs::create_dir_all(dir).map_err(|_| UsageOutcome::Unavailable {
        reason: "Cannot save the Perch sign-in.".into(),
    })?;
    let final_path = dir.join(SESSION_FILE);
    let temporary = dir.join("cli-auth-session.json.writing");
    std::fs::write(&temporary, json).map_err(|_| UsageOutcome::Unavailable {
        reason: "Cannot save the Perch sign-in.".into(),
    })?;
    restrict_owner(&temporary);
    if std::fs::rename(&temporary, &final_path).is_err() {
        std::fs::write(&final_path, json).map_err(|_| UsageOutcome::Unavailable {
            reason: "Cannot save the Perch sign-in.".into(),
        })?;
        let _ = std::fs::remove_file(&temporary);
    }
    restrict_owner(&final_path);
    Ok(())
}

fn restrict_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(target_os = "macos")]
fn read_keychain() -> Option<String> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "app.perchai.cli-auth",
            "-a",
            "default",
            "-w",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(target_os = "macos")]
fn write_keychain(json: &str) -> Result<(), UsageOutcome> {
    let status = std::process::Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            "app.perchai.cli-auth",
            "-a",
            "default",
            "-w",
            json,
        ])
        .status()
        .map_err(|_| UsageOutcome::Unavailable {
            reason: "Cannot save the Perch sign-in.".into(),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(UsageOutcome::Unavailable {
            reason: "Cannot save the Perch sign-in.".into(),
        })
    }
}

struct HttpText {
    status: u16,
    body: String,
}

fn http_get(url: &str, bearer: Option<&str>) -> Result<HttpText, ()> {
    let client = shared_client().map_err(|_| ())?;
    let mut request = client.get(url).header("Accept", "application/json");
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(|_| ())?;
    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();
    Ok(HttpText { status, body })
}

fn http_post(url: &str, anon_key: &str, body: &str) -> Result<HttpText, ()> {
    let client = shared_client().map_err(|_| ())?;
    let response = client
        .post(url)
        .header("Accept", "application/json")
        .header("apikey", anon_key)
        .bearer_auth(anon_key)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .map_err(|_| ())?;
    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();
    Ok(HttpText { status, body })
}

enum AccountHit {
    Body(String),
    Auth,
    RateLimited,
    Failed(String),
}

fn account_request(session: &Session) -> AccountHit {
    let app_url = trim_slashes(&session.app_url);
    if !supported_http(&app_url) {
        return AccountHit::Failed("Perch sign-in points at an unsupported address.".into());
    }
    let url = format!("{app_url}/api/perchai/account");
    let response = match http_get(&url, Some(&session.access_token)) {
        Ok(response) => response,
        Err(()) => {
            return AccountHit::Failed("Cannot reach Perch to read usage.".into());
        }
    };
    if response.status == 429 {
        return AccountHit::RateLimited;
    }
    if response.status == 401 || response.status == 403 || not_authenticated(&response.body) {
        return AccountHit::Auth;
    }
    if !(200..300).contains(&response.status) {
        return AccountHit::Failed(format!(
            "Perch account service returned HTTP {}.",
            response.status
        ));
    }
    AccountHit::Body(response.body)
}

fn not_authenticated(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| json_string(&value, &["error"]))
        .is_some_and(|error| error == "not_authenticated")
}

fn trim_slashes(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn supported_http(url: &str) -> bool {
    reqwest::Url::parse(url)
        .map(|parsed| matches!(parsed.scheme(), "http" | "https"))
        .unwrap_or(false)
}

fn json_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|item| item.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

/// `ceil(usd / 0.001)` when the amount is finite and positive. Matches CLI `Xle`.
fn ceil_pt(usd: f64) -> i64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    (usd / 0.001).ceil() as i64
}

/// `round(usd * 1000)`. Matches CLI `TPe` for rolling windows.
fn round_pt(usd: f64) -> i64 {
    if !usd.is_finite() {
        return 0;
    }
    (usd * 1000.0).round() as i64
}

/// `ceil(usd * 1000)` when positive. Matches CLI `yGo` for a manual USD fallback.
fn manual_pt_from_usd(usd: f64) -> Option<i64> {
    if !usd.is_finite() || usd <= 0.0 {
        return None;
    }
    Some((usd * 1000.0).ceil() as i64)
}

fn finite_number(value: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|item| item.as_f64())
            .filter(|number| number.is_finite())
    })
}

fn outcome_from_account(body: &str) -> UsageOutcome {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return UsageOutcome::Unavailable {
            reason: "Perch returned an unreadable account.".into(),
        };
    };
    if not_authenticated(body) {
        return login_expired();
    }
    if value.get("ok").and_then(|flag| flag.as_bool()) != Some(true) {
        return UsageOutcome::Unavailable {
            reason: "Perch did not return an account.".into(),
        };
    }
    let Some(session) = value.get("session").filter(|item| item.is_object()) else {
        return UsageOutcome::Unavailable {
            reason: "Perch did not return an account.".into(),
        };
    };
    if json_string(session, &["billingLane", "billing_lane"]).as_deref() == Some("byo") {
        return UsageOutcome::ManagedExternally {
            platform: "your own API key".into(),
        };
    }
    let meter = value
        .get("usageMeter")
        .or_else(|| value.get("usage_meter"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let credits = finite_number(&value, &["creditBalancePt", "credit_balance_pt"])
        .filter(|amount| *amount > 0.0)
        .map(|amount| amount.floor() as i64)
        .unwrap_or(0);
    reading_from(session, &meter, credits)
}

enum EntitlementLimit {
    Absent,
    Present(Option<f64>),
}

fn entitlement_limit(session: &serde_json::Value, key: &str) -> EntitlementLimit {
    let Some(list) = session
        .get("entitlements")
        .and_then(|value| value.as_array())
    else {
        return EntitlementLimit::Absent;
    };
    let Some(item) = list
        .iter()
        .find(|entry| entry.get("key").and_then(|value| value.as_str()) == Some(key))
    else {
        return EntitlementLimit::Absent;
    };
    let value_json = item.get("value_json").or_else(|| item.get("valueJson"));
    let Some(object) = value_json.and_then(|value| value.as_object()) else {
        return EntitlementLimit::Present(None);
    };
    match object.get("limit").and_then(|value| value.as_f64()) {
        Some(limit) if limit.is_finite() && limit > 0.0 => EntitlementLimit::Present(Some(limit)),
        _ => EntitlementLimit::Present(None),
    }
}

fn plan_code(session: &serde_json::Value) -> String {
    json_string(session, &["planCode", "plan_code"]).unwrap_or_default()
}

fn plan_name(session: &serde_json::Value) -> Option<String> {
    json_string(session, &["planName", "plan_name"])
}

/// CLI `vme`: a trimmed plan name wins, `pilot` becomes Starter, otherwise
/// the plan code is capitalised. The fallback is Starter.
fn plan_label(name: Option<&str>, code: &str) -> String {
    if let Some(trimmed) = name.map(str::trim).filter(|text| !text.is_empty()) {
        if trimmed.eq_ignore_ascii_case("pilot") {
            return "Starter".into();
        }
        return trimmed.to_string();
    }
    if code == "pilot" {
        return "Starter".into();
    }
    let code = code.trim();
    let mut chars = code.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Starter".into(),
    }
}

fn starter_plan(code: &str) -> bool {
    code == "pilot" || code == "demo"
}

/// CLI `EDt`. A positive entitlement wins. A present-but-empty entitlement,
/// or no entitlement, falls back to 20,000 PT for pilot, demo, and any plan
/// whose label is Starter.
fn monthly_limit(session: &serde_json::Value, code: &str, label: &str) -> Option<f64> {
    let from_entitlement = match entitlement_limit(session, "usage.monthly_pt") {
        EntitlementLimit::Absent => None,
        EntitlementLimit::Present(limit) => Some(limit),
    };
    if let Some(Some(limit)) = from_entitlement {
        return Some(limit);
    }
    if starter_plan(code) || label == "Starter" {
        Some(STARTER_MONTHLY_PT)
    } else {
        None
    }
}

/// CLI `CDt`. A positive entitlement wins. Pro falls back to 75,000 PT even
/// when the entitlement is present with a null limit.
fn manual_limit(session: &serde_json::Value, code: &str) -> Option<f64> {
    if let EntitlementLimit::Present(Some(limit)) =
        entitlement_limit(session, "usage.manual_monthly_pt")
    {
        return Some(limit);
    }
    if code == "pro" {
        Some(PRO_MANUAL_MONTHLY_PT)
    } else {
        None
    }
}

fn unlimited_account(session: &serde_json::Value, code: &str) -> bool {
    let role = json_string(session, &["membershipRole", "membership_role"]);
    matches!(role.as_deref(), Some("admin") | Some("internal")) || code == "internal"
}

struct Row {
    label: &'static str,
    used: f64,
    limit: Option<f64>,
    resets_at: Option<String>,
}

fn reading_from(
    session: &serde_json::Value,
    meter: &serde_json::Value,
    credits: i64,
) -> UsageOutcome {
    let code = plan_code(session);
    let label = plan_label(plan_name(session).as_deref(), &code);
    let monthly_used =
        ceil_pt(finite_number(meter, &["monthlyUsd", "monthly_usd"]).unwrap_or(0.0)) as f64;
    let manual_used = manual_used_pt(meter);
    let mut rows = Vec::new();
    if unlimited_account(session, &code) {
        rows.push(Row {
            label: "Monthly",
            used: monthly_used,
            limit: None,
            resets_at: None,
        });
    } else if code == "pro" {
        push_rolling(
            &mut rows,
            meter
                .get("roostRolling")
                .or_else(|| meter.get("roost_rolling")),
            "5-hour",
            "Weekly",
        );
        let limit = manual_limit(session, &code).unwrap_or(PRO_MANUAL_MONTHLY_PT);
        rows.push(Row {
            label: "Manual",
            used: manual_used,
            limit: Some(limit),
            resets_at: None,
        });
        push_rolling(
            &mut rows,
            meter
                .get("manualRolling")
                .or_else(|| meter.get("manual_rolling")),
            "Manual 5-hour",
            "Manual weekly",
        );
    } else {
        let limit = monthly_limit(session, &code, &label).filter(|limit| *limit > 0.0);
        rows.push(Row {
            label: "Monthly",
            used: monthly_used,
            limit,
            resets_at: None,
        });
        push_rolling(
            &mut rows,
            meter
                .get("roostRolling")
                .or_else(|| meter.get("roost_rolling")),
            "5-hour",
            "Weekly",
        );
    }
    let mut detail_parts = vec![label];
    if code == "pro" && !unlimited_account(session, &code) {
        detail_parts.push("Roost fair use".into());
    }
    if credits > 0 {
        detail_parts.push(format!("{credits} PT credits"));
    }
    let (windows, meters) = project_rows(&rows);
    UsageOutcome::Reading {
        windows,
        balance: None,
        meters,
        detail: Some(detail_parts.join(" · ")),
    }
}

fn manual_used_pt(meter: &serde_json::Value) -> f64 {
    if let Some(points) = finite_number(meter, &["monthlyManualPt", "monthly_manual_pt"]) {
        return points.max(0.0);
    }
    finite_number(meter, &["monthlyManualUsd", "monthly_manual_usd"])
        .and_then(manual_pt_from_usd)
        .unwrap_or(0) as f64
}

fn push_rolling(
    rows: &mut Vec<Row>,
    rolling: Option<&serde_json::Value>,
    five_hour: &'static str,
    weekly: &'static str,
) {
    let Some(rolling) = rolling.filter(|value| value.is_object()) else {
        return;
    };
    if rolling.get("enabled").and_then(|value| value.as_bool()) != Some(true) {
        return;
    }
    push_rolling_window(
        rows,
        rolling,
        five_hour,
        &["window5hUsd", "window_5h_usd"],
        &["window5hCapUsd", "window_5h_cap_usd"],
        &["window5hNextFreedAt", "window_5h_next_freed_at"],
    );
    push_rolling_window(
        rows,
        rolling,
        weekly,
        &["window7dUsd", "window_7d_usd"],
        &["window7dCapUsd", "window_7d_cap_usd"],
        &["window7dNextFreedAt", "window_7d_next_freed_at"],
    );
}

fn push_rolling_window(
    rows: &mut Vec<Row>,
    rolling: &serde_json::Value,
    label: &'static str,
    used_keys: &[&str],
    cap_keys: &[&str],
    reset_keys: &[&str],
) {
    let cap = round_pt(finite_number(rolling, cap_keys).unwrap_or(0.0));
    if cap <= 0 {
        return;
    }
    rows.push(Row {
        label,
        used: round_pt(finite_number(rolling, used_keys).unwrap_or(0.0)) as f64,
        limit: Some(cap as f64),
        resets_at: reset_instant(json_string(rolling, reset_keys).as_deref()),
    });
}

fn reset_instant(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    Some(raw.to_string())
}

fn used_percent(used: f64, limit: Option<f64>) -> Option<f64> {
    let limit = limit.filter(|value| *value > 0.0)?;
    Some((used / limit * 100.0).clamp(0.0, 100.0))
}

fn project_rows(rows: &[Row]) -> (Vec<UsageWindow>, Vec<UsageMeter>) {
    let mut windows = Vec::new();
    let mut meters = Vec::new();
    for row in rows {
        let percent = used_percent(row.used, row.limit);
        let remaining = row.limit.map(|limit| (limit - row.used).max(0.0));
        let amount = UsageAmount {
            used: row.used,
            limit: row.limit,
            remaining,
            unit: "PT".into(),
            used_percent: percent,
            resets_at: row.resets_at.clone(),
        };
        if let Some(percent) = percent {
            windows.push(UsageWindow {
                label: row.label.into(),
                used_percent: Some(percent),
                resets_at: row.resets_at.clone(),
            });
            meters.push(UsageMeter::Metered { amount });
        } else {
            meters.push(UsageMeter::NoIndividualLimit { amount });
        }
    }
    (windows, meters)
}

#[cfg(test)]
#[path = "perch_tests.rs"]
mod tests;
