//! Antigravity (`agy`) native adapter — credential discovery for the Usage
//! Meter, detection-gated on the `agy` harness.
//!
//! Current CLI (1.2+) refreshes `<agy_dir>/antigravity-oauth-token` in place.
//! Older Windows installs also keep a blob under Windows Credential Manager
//! `gemini:antigravity`, which the new CLI leaves stale after refresh.
//!
//! Credential *selection* (file then keyring) is separate from credential
//! *validity*: callers try each collected token; on HTTP 401/403 they advance
//! to the next source before returning logged-out.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::types::{ProviderUsage, UsageError};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use crate::services::windows_cred;

/// Credential Manager target older agy CLIs stored the OAuth blob under.
pub(crate) const AGY_CRED_TARGET: &str = "gemini:antigravity";
/// Live OAuth blob filename under [`crate::env::agy_dir`].
pub(crate) const AGY_OAUTH_TOKEN_FILE: &str = "antigravity-oauth-token";

/// Drop-in [`UsageAdapter`] for `agy`.
pub(crate) struct AgyAdapter;

impl UsageAdapter for AgyAdapter {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("agy")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::agy_usage()
    }
}

/// Which store produced an access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgyTokenSource {
    CliFile,
    Keyring,
}

impl AgyTokenSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CliFile => "cli_file",
            Self::Keyring => "keyring",
        }
    }
}

/// One usable Antigravity bearer token plus its origin.
#[derive(Debug, Clone)]
pub(crate) struct AgyAccessToken {
    pub access_token: String,
    pub source: AgyTokenSource,
}

#[derive(serde::Deserialize)]
struct AgyTokenField {
    access_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct AgyCred {
    token: Option<AgyTokenField>,
}

/// Parses the agy credential blob (`{ "token": { "access_token": … }, … }`).
/// Same envelope for the CLI oauth file and the Windows keyring blob.
pub(crate) fn parse_agy_token(blob: &[u8]) -> Result<String, UsageError> {
    let text = std::str::from_utf8(blob).map_err(|e| UsageError::Shape(e.to_string()))?;
    let cred: AgyCred = serde_json::from_str(text).map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.token
        .and_then(|t| t.access_token)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UsageError::NoCredential(AGY_CRED_TARGET.to_string()))
}

pub(crate) fn agy_oauth_token_path() -> PathBuf {
    crate::env::agy_dir().join(AGY_OAUTH_TOKEN_FILE)
}

fn read_agy_cli_file_token(file_path: &Path) -> Result<String, UsageError> {
    let bytes = fs::read(file_path).map_err(|e| {
        UsageError::NoCredential(format!("{} ({e})", file_path.display()))
    })?;
    parse_agy_token(&bytes)
}

#[cfg(windows)]
fn read_agy_keyring_token() -> Result<String, UsageError> {
    parse_agy_token(&windows_cred::read(AGY_CRED_TARGET)?)
}

#[cfg(not(windows))]
fn read_agy_keyring_token() -> Result<String, UsageError> {
    Err(UsageError::NoCredential(
        "Antigravity OAuth token not found (antigravity-oauth-token or gemini:antigravity)"
            .to_string(),
    ))
}

/// Collect usable access tokens in preference order (CLI file, then keyring).
///
/// Matches the Codex multi-source contract: a [`UsageError::Shape`] from the
/// CLI file surfaces immediately (corrupt JSON is not fixed by reading the
/// keyring). [`UsageError::NoCredential`] (missing file, empty token, I/O)
/// advances to the next source. Deduplicates identical bearer strings.
pub(crate) fn collect_agy_access_tokens(
    file_path: &Path,
    keyring: impl FnOnce() -> Result<String, UsageError>,
) -> Result<Vec<AgyAccessToken>, UsageError> {
    let mut tokens = Vec::new();

    match read_agy_cli_file_token(file_path) {
        Ok(access_token) => {
            tracing::debug!(
                target: "services::usage::agy",
                source = AgyTokenSource::CliFile.as_str(),
                path = %file_path.display(),
                "selected Antigravity oauth token"
            );
            tokens.push(AgyAccessToken {
                access_token,
                source: AgyTokenSource::CliFile,
            });
        }
        Err(UsageError::Shape(e)) => {
            tracing::debug!(
                target: "services::usage::agy",
                path = %file_path.display(),
                error = %e,
                "Antigravity CLI oauth file has unexpected shape; not falling back to keyring"
            );
            return Err(UsageError::Shape(e));
        }
        Err(UsageError::NoCredential(path)) => {
            tracing::debug!(
                target: "services::usage::agy",
                path = %path,
                "Antigravity CLI oauth file unavailable"
            );
        }
    }

    match keyring() {
        Ok(access_token) => {
            if tokens.iter().any(|t| t.access_token == access_token) {
                tracing::debug!(
                    target: "services::usage::agy",
                    source = AgyTokenSource::Keyring.as_str(),
                    "keyring token matches cli_file; skipping duplicate"
                );
            } else {
                tracing::debug!(
                    target: "services::usage::agy",
                    source = AgyTokenSource::Keyring.as_str(),
                    "selected Antigravity oauth token"
                );
                tokens.push(AgyAccessToken {
                    access_token,
                    source: AgyTokenSource::Keyring,
                });
            }
        }
        Err(UsageError::Shape(e)) => {
            if tokens.is_empty() {
                tracing::debug!(
                    target: "services::usage::agy",
                    error = %e,
                    "Antigravity keyring blob has unexpected shape"
                );
                return Err(UsageError::Shape(e));
            }
            tracing::debug!(
                target: "services::usage::agy",
                error = %e,
                "Antigravity keyring blob has unexpected shape; keeping cli_file token"
            );
        }
        Err(UsageError::NoCredential(path)) => {
            tracing::debug!(
                target: "services::usage::agy",
                path = %path,
                "Antigravity keyring credential unavailable"
            );
        }
    }

    if tokens.is_empty() {
        Err(UsageError::NoCredential(
            "Antigravity OAuth token not found (antigravity-oauth-token or gemini:antigravity)"
                .to_string(),
        ))
    } else {
        Ok(tokens)
    }
}

/// Production entry: CLI oauth file under [`agy_oauth_token_path`], then keyring.
pub(crate) fn collect_agy_access_tokens_default() -> Result<Vec<AgyAccessToken>, UsageError> {
    collect_agy_access_tokens(&agy_oauth_token_path(), read_agy_keyring_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Redacted capture of a live `antigravity-oauth-token` file (agy 1.2 on
    /// Windows, 2026-09-10). Field set and nesting are load-bearing; secrets
    /// replaced. Proves the CLI file shares the keyring envelope
    /// `{ "token": { "access_token": … }, "auth_method", "id_token"? }`.
    const REAL_CLI_OAUTH_FILE_SHAPE: &[u8] = br#"{"token":{"access_token":"ya29.live-shape-fixture","token_type":"Bearer","refresh_token":"1//redacted","expiry":"2026-09-10T08:52:44.5683799+01:00"},"auth_method":"consumer","id_token":"eyJ.redacted"}"#;

    #[test]
    fn parse_agy_token_accepts_real_cli_oauth_file_shape() {
        assert_eq!(
            parse_agy_token(REAL_CLI_OAUTH_FILE_SHAPE).unwrap(),
            "ya29.live-shape-fixture"
        );
    }

    #[test]
    fn parse_agy_token_extracts_nested_access_token() {
        let blob = br#"{"token":{"access_token":"ya29.agytok","token_type":"Bearer","refresh_token":"1//ref","expiry":"2026-05-31T12:00:00Z"},"auth_method":"consumer"}"#;
        assert_eq!(parse_agy_token(blob).unwrap(), "ya29.agytok");
    }

    #[test]
    fn parse_agy_token_missing_is_no_credential() {
        match parse_agy_token(br#"{"auth_method":"consumer"}"#) {
            Err(UsageError::NoCredential(_)) => {}
            other => panic!("expected NoCredential, got {other:?}"),
        }
        match parse_agy_token(br#"{"token":{"access_token":""}}"#) {
            Err(UsageError::NoCredential(_)) => {}
            other => panic!("expected NoCredential, got {other:?}"),
        }
    }

    #[test]
    fn parse_agy_token_corrupt_json_is_shape() {
        match parse_agy_token(br#"{not-json"#) {
            Err(UsageError::Shape(_)) => {}
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn agy_oauth_token_path_joins_agy_dir() {
        assert_eq!(
            agy_oauth_token_path(),
            crate::env::agy_dir().join(AGY_OAUTH_TOKEN_FILE)
        );
    }

    #[test]
    fn collect_prefers_cli_oauth_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(AGY_OAUTH_TOKEN_FILE);
        fs::write(
            &path,
            br#"{"token":{"access_token":"ya29.fromfile","token_type":"Bearer"},"auth_method":"consumer"}"#,
        )
        .unwrap();
        let tokens =
            collect_agy_access_tokens(&path, || Ok("ya29.from-keyring".to_string())).unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].source, AgyTokenSource::CliFile);
        assert_eq!(tokens[0].access_token, "ya29.fromfile");
        assert_eq!(tokens[1].source, AgyTokenSource::Keyring);
        assert_eq!(tokens[1].access_token, "ya29.from-keyring");
    }

    #[test]
    fn collect_falls_back_when_file_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("absent-oauth-token");
        let tokens =
            collect_agy_access_tokens(&path, || Ok("ya29.from-keyring".to_string())).unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].source, AgyTokenSource::Keyring);
        assert_eq!(tokens[0].access_token, "ya29.from-keyring");
    }

    #[test]
    fn collect_falls_back_when_file_token_empty() {
        // Valid JSON missing a bearer is NoCredential, not Shape — try keyring.
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(AGY_OAUTH_TOKEN_FILE);
        fs::write(&path, br#"{"auth_method":"consumer"}"#).unwrap();
        let tokens =
            collect_agy_access_tokens(&path, || Ok("ya29.from-keyring".to_string())).unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].access_token, "ya29.from-keyring");
    }

    #[test]
    fn collect_corrupt_cli_file_does_not_fall_back() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(AGY_OAUTH_TOKEN_FILE);
        fs::write(&path, br#"{not-json"#).unwrap();
        match collect_agy_access_tokens(&path, || Ok("ya29.from-keyring".to_string())) {
            Err(UsageError::Shape(_)) => {}
            other => panic!("expected Shape without keyring fallback, got {other:?}"),
        }
    }

    #[test]
    fn collect_propagates_fallback_error_when_both_sources_dead() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("absent-oauth-token");
        match collect_agy_access_tokens(&path, || {
            Err(UsageError::NoCredential("keyring-missing".into()))
        }) {
            Err(UsageError::NoCredential(msg)) => {
                assert!(
                    msg.contains("antigravity-oauth-token") || msg.contains("gemini:antigravity"),
                    "expected combined NoCredential message, got {msg}"
                );
            }
            other => panic!("expected NoCredential, got {other:?}"),
        }
    }

    #[test]
    fn collect_dedupes_identical_bearers() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(AGY_OAUTH_TOKEN_FILE);
        fs::write(
            &path,
            br#"{"token":{"access_token":"ya29.same","token_type":"Bearer"},"auth_method":"consumer"}"#,
        )
        .unwrap();
        let tokens = collect_agy_access_tokens(&path, || Ok("ya29.same".to_string())).unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].source, AgyTokenSource::CliFile);
    }

    #[cfg(not(windows))]
    #[test]
    fn keyring_unavailable_message_on_non_windows() {
        match read_agy_keyring_token() {
            Err(UsageError::NoCredential(msg)) => {
                assert!(msg.contains("antigravity-oauth-token"));
                assert!(msg.contains("gemini:antigravity"));
            }
            other => panic!("expected NoCredential, got {other:?}"),
        }
    }
}
