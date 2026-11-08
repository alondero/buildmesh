//! OpenCode Go's usage API, authenticated with the native CLI's active
//! Console credential (V2) or Go API key (V1).

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{fetch_usage, UsageAdapter, UsageIdentityFingerprint};
use crate::services::usage::outcome::{ResponsePolicy, UsageOutcome, UsageReading};
use crate::services::usage::types::{home_dir, UsageError, UsagePage, UsageWindow};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const USAGE_URL: &str = "https://opencode.ai/inference/go/v1/usage";

pub(crate) struct OpencodeAdapter;

impl UsageAdapter for OpencodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new(
            "https://opencode.ai/console",
            "Open console",
        ))
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("opencode")
    }

    fn additional_native_harnesses(&self) -> &'static [&'static str] {
        &["opencode2"]
    }

    fn cache_identity(&self, _accounts: &[ProviderAccount]) -> UsageIdentityFingerprint {
        let dir = data_dir();
        match read_credential(&dir, &database_path(&dir)) {
            Ok(credential) => credential.cache_identity(),
            Err(_) => UsageIdentityFingerprint::new("opencode_missing", b"opencode"),
        }
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        opencode_usage()
    }
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/share"))
        .join("opencode")
}

fn database_path(dir: &Path) -> PathBuf {
    match std::env::var_os("OPENCODE_DB").filter(|path| !path.is_empty()) {
        Some(path) => dir.join(path),
        None => dir.join("opencode.db"),
    }
}

// No Debug: native credentials contain secrets.
struct Credential {
    token: String,
    org_id: Option<String>,
    expires: Option<i64>,
}

impl Credential {
    fn cache_identity(&self) -> UsageIdentityFingerprint {
        let identity = format!(
            "{}\0{}",
            self.token,
            self.org_id.as_deref().unwrap_or_default()
        );
        UsageIdentityFingerprint::new("opencode_native", identity.as_bytes())
    }
}

#[derive(Deserialize)]
struct NativeCredential {
    #[serde(rename = "type")]
    kind: String,
    key: Option<String>,
    access: Option<String>,
    expires: Option<i64>,
    #[serde(default)]
    metadata: Metadata,
    #[serde(default)]
    configuration: Metadata,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    #[serde(rename = "orgID")]
    org_id: Option<String>,
    server: Option<String>,
}

fn shape(message: &str) -> UsageError {
    UsageError::Shape(message.to_owned())
}

fn read_native_credential(db: &Path) -> Result<Option<Credential>, UsageError> {
    if !db
        .try_exists()
        .map_err(|_| shape("Cannot access OpenCode credential database"))?
    {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| shape("Cannot read OpenCode credential database"))?;
    conn.busy_timeout(std::time::Duration::from_millis(250))
        .map_err(|_| shape("Cannot configure OpenCode credential database"))?;
    let has_credentials: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='credential')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| shape("Cannot inspect OpenCode credential database"))?;
    if !has_credentials {
        return Ok(None);
    }
    // Match the CLI's active-first, newest-first connection selection.
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM credential WHERE integration_id='opencode' ORDER BY active DESC,time_created DESC,id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| shape("Cannot select active OpenCode Console credential"))?;
    let Some(value) = value else {
        return Ok(None);
    };
    let credential: NativeCredential = serde_json::from_str(&value)
        .map_err(|_| shape("Invalid active OpenCode Console credential"))?;
    let org_id = credential
        .configuration
        .org_id
        .or(credential.metadata.org_id);
    let server = credential
        .configuration
        .server
        .or(credential.metadata.server);
    if server.as_deref().is_some_and(|server| {
        !matches!(
            server.trim_end_matches('/'),
            "https://opencode.ai/console" | "https://console.opencode.ai"
        )
    }) {
        return Err(shape("Go usage requires the OpenCode hosted Console"));
    }
    let token = match credential.kind.as_str() {
        "oauth" => credential.access,
        "key" | "api" => credential.key,
        _ => return Err(shape("Unsupported active OpenCode credential type")),
    }
    .filter(|token| !token.trim().is_empty())
    .ok_or_else(|| shape("Active OpenCode Console credential has no token"))?;
    let org_id = org_id
        .filter(|org| !org.trim().is_empty())
        .ok_or_else(|| shape("Select a workspace for OpenCode Console in OpenCode"))?;
    Ok(Some(Credential {
        token,
        org_id: Some(org_id),
        expires: credential.expires,
    }))
}

fn read_credential(dir: &Path, db: &Path) -> Result<Credential, UsageError> {
    if db != dir.join("opencode.db")
        && !db
            .try_exists()
            .map_err(|_| shape("Cannot access configured OpenCode database"))?
    {
        return Err(shape("Configured OpenCode database does not exist"));
    }
    if let Some(credential) = read_native_credential(db)? {
        return Ok(credential);
    }
    let path = dir.join("auth.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(UsageError::NoCredential(path.display().to_string()));
        }
        Err(_) => return Err(shape("Cannot read OpenCode auth.json")),
    };
    let entries: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| shape("Invalid OpenCode auth.json"))?;
    let token = entries
        .get("opencode-go")
        .and_then(|entry| entry.get("key"))
        .and_then(|key| key.as_str())
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| UsageError::NoCredential(path.display().to_string()))?;
    Ok(Credential {
        token: token.to_owned(),
        org_id: None,
        expires: None,
    })
}

#[derive(Deserialize)]
struct GoResponse {
    usage: GoUsage,
}

#[derive(Deserialize)]
struct GoUsage {
    rolling: GoWindow,
    weekly: GoWindow,
    monthly: GoWindow,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoWindow {
    #[serde(rename = "status")]
    _status: GoStatus,
    percent: f64,
    resets_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum GoStatus {
    Ok,
    RateLimited,
}

impl GoWindow {
    fn into_window(self, label: &str) -> Result<UsageWindow, UsageError> {
        if !self.percent.is_finite() || !(0.0..=100.0).contains(&self.percent) {
            return Err(shape("Invalid OpenCode Go usage percentage"));
        }
        let reset = chrono::DateTime::parse_from_rfc3339(&self.resets_at)
            .map_err(|_| shape("Invalid OpenCode Go reset datetime"))?;
        // Both validated statuses carry quota data, including a depleted quota.
        Ok(UsageWindow {
            label: label.to_owned(),
            used_percent: Some(self.percent),
            resets_at: Some(reset.with_timezone(&chrono::Utc).to_rfc3339()),
        })
    }
}

fn parse_usage(body: &str) -> Result<UsageReading, UsageError> {
    let response: GoResponse =
        serde_json::from_str(body).map_err(|_| shape("Invalid OpenCode Go usage response"))?;
    Ok(UsageReading {
        windows: vec![
            response.usage.rolling.into_window("5-hour")?,
            response.usage.weekly.into_window("Weekly")?,
            response.usage.monthly.into_window("Monthly")?,
        ],
        balance: None,
        meters: vec![],
        detail: None,
    })
}

fn usage_at(dir: &Path, db: &Path, url: &str) -> UsageOutcome {
    let credential = match read_credential(dir, db) {
        Ok(credential) => credential,
        Err(UsageError::NoCredential(hint)) => return UsageOutcome::NoCredential { hint },
        Err(error) => {
            return UsageOutcome::Unavailable {
                reason: error.to_string(),
            }
        }
    };
    if credential
        .expires
        .is_some_and(|expires| expires <= chrono::Utc::now().timestamp_millis())
    {
        // Native OAuth rotates refresh tokens under a cross-process lock. Let
        // OpenCode own renewal rather than racing its credential writer.
        return UsageOutcome::Rejected {
            hint:
                "OpenCode Console session expired. Open OpenCode to renew it, then refresh usage."
                    .to_owned(),
        };
    }
    fetch_usage(
        ResponsePolicy {
            rejected_hint: Some("OpenCode Go access was rejected. Check your Console workspace and subscription; replace expired API keys in OpenCode."),
            ..ResponsePolicy::default()
        },
        |client| {
            let request = client.get(url)
                .bearer_auth(&credential.token)
                .header("Accept", "application/json")
                .header("User-Agent", "Buildmesh-Usage-Probe/1.0");
            match &credential.org_id {
                Some(org) => request.header("x-opencode-org-id", org),
                None => request,
            }
        },
        parse_usage,
    )
}

pub fn opencode_usage() -> UsageOutcome {
    let dir = data_dir();
    usage_at(&dir, &database_path(&dir), USAGE_URL)
}

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;
