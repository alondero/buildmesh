//! Resolve Claude Code's active authentication source.
//!
//! Precedence matches Claude's documented order: cloud provider flags,
//! `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_API_KEY`, `apiKeyHelper`,
//! `CLAUDE_CODE_OAUTH_TOKEN`, Anthropic profile/federation variables, then
//! subscription OAuth from the platform credential store. Process environment
//! wins over the user `settings.json` `env` block. Cloud configuration must
//! not fall through to a dormant OAuth credential.

use super::parse::plan_label;
use crate::services::usage::adapter::UsageIdentityFingerprint;
use crate::services::usage::types::{home_dir, UsageError};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(crate) const PLATFORM_BEDROCK: &str = "AWS Bedrock";
pub(crate) const PLATFORM_VERTEX: &str = "Google Vertex AI";
pub(crate) const PLATFORM_FOUNDRY: &str = "Microsoft Foundry";
pub(crate) const PLATFORM_CONSOLE: &str = "Anthropic Console";
pub(crate) const PLATFORM_GATEWAY: &str = "an LLM gateway";
pub(crate) const PLATFORM_HELPER: &str = "Claude apiKeyHelper";
pub(crate) const PLATFORM_PROFILE: &str = "Anthropic profile";

const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Non-secret description of the credential Claude will actually use.
pub(crate) enum ClaudeAuthSource {
    Managed {
        platform: &'static str,
        cache_tag: &'static str,
        identity: Vec<u8>,
    },
    Oauth {
        token: String,
        plan: Option<String>,
    },
    Missing {
        error: UsageError,
    },
}

impl std::fmt::Debug for ClaudeAuthSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Managed {
                platform,
                cache_tag,
                ..
            } => f
                .debug_struct("Managed")
                .field("platform", platform)
                .field("cache_tag", cache_tag)
                .field("identity", &"<redacted>")
                .finish(),
            Self::Oauth { plan, .. } => f
                .debug_struct("Oauth")
                .field("plan", plan)
                .field("token", &"<redacted>")
                .finish(),
            Self::Missing { error } => f.debug_struct("Missing").field("error", &error.to_string()).finish(),
        }
    }
}

pub(crate) trait AuthLookup {
    fn env(&self, key: &str) -> Option<String>;
    fn read_file(&self, path: &Path) -> Result<String, UsageError>;
    fn read_keychain(&self, service: &str) -> Result<String, UsageError>;
    fn home(&self) -> PathBuf;
}

pub(crate) struct ProductionLookup;

impl AuthLookup for ProductionLookup {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|value| !value.is_empty())
    }

    fn read_file(&self, path: &Path) -> Result<String, UsageError> {
        std::fs::read_to_string(path)
            .map_err(|_| UsageError::NoCredential(path.to_string_lossy().to_string()))
    }

    fn read_keychain(&self, service: &str) -> Result<String, UsageError> {
        read_platform_keychain(service)
    }

    fn home(&self) -> PathBuf {
        home_dir()
    }
}

pub(crate) fn resolve_claude_auth(lookup: &impl AuthLookup) -> ClaudeAuthSource {
    let config_dir = config_dir(lookup);
    let settings = read_settings(lookup, &config_dir);
    let var = |key: &str| lookup.env(key).or_else(|| settings.env.get(key).cloned());

    if flag_set(var("CLAUDE_CODE_USE_BEDROCK").as_deref()) {
        return managed(PLATFORM_BEDROCK, "bedrock", b"aws");
    }
    if flag_set(var("CLAUDE_CODE_USE_VERTEX").as_deref()) {
        return managed(PLATFORM_VERTEX, "vertex", b"gcp");
    }
    if flag_set(var("CLAUDE_CODE_USE_FOUNDRY").as_deref()) {
        return managed(PLATFORM_FOUNDRY, "foundry", b"azure");
    }
    if let Some(token) = var("ANTHROPIC_AUTH_TOKEN") {
        return managed(PLATFORM_GATEWAY, "auth_token", token.as_bytes());
    }
    if let Some(key) = var("ANTHROPIC_API_KEY") {
        return managed(PLATFORM_CONSOLE, "api_key", key.as_bytes());
    }
    if settings
        .api_key_helper
        .as_deref()
        .is_some_and(|helper| !helper.trim().is_empty())
    {
        return managed(PLATFORM_HELPER, "api_key_helper", b"configured");
    }
    if let Some(token) = var("CLAUDE_CODE_OAUTH_TOKEN") {
        return ClaudeAuthSource::Oauth {
            token,
            plan: None,
        };
    }
    if var("ANTHROPIC_PROFILE").is_some()
        || (var("ANTHROPIC_FEDERATION_RULE_ID").is_some()
            && var("ANTHROPIC_ORGANIZATION_ID").is_some())
    {
        return managed(PLATFORM_PROFILE, "profile", b"configured");
    }

    read_oauth_store(lookup, &config_dir)
}

pub(crate) fn cache_identity_from(source: &ClaudeAuthSource) -> UsageIdentityFingerprint {
    match source {
        ClaudeAuthSource::Managed {
            cache_tag,
            identity,
            ..
        } => UsageIdentityFingerprint::new(cache_tag, identity),
        ClaudeAuthSource::Oauth { token, .. } => {
            UsageIdentityFingerprint::new("oauth", token.as_bytes())
        }
        ClaudeAuthSource::Missing { .. } => {
            UsageIdentityFingerprint::new("unconfigured", b"anthropic")
        }
    }
}

pub(crate) fn keychain_service_for(config_dir: &Path, default_dir: &Path) -> String {
    if paths_equal(config_dir, default_dir) {
        return KEYCHAIN_SERVICE.to_string();
    }
    let rendered = config_dir.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(rendered.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("{KEYCHAIN_SERVICE}-{}", &digest[..8])
}

fn managed(platform: &'static str, cache_tag: &'static str, identity: &[u8]) -> ClaudeAuthSource {
    ClaudeAuthSource::Managed {
        platform,
        cache_tag,
        identity: identity.to_vec(),
    }
}

fn config_dir(lookup: &impl AuthLookup) -> PathBuf {
    lookup
        .env("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| lookup.home().join(".claude"))
}

fn default_config_dir(lookup: &impl AuthLookup) -> PathBuf {
    lookup.home().join(".claude")
}

fn flag_set(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        Some(value) if !value.is_empty() => !matches!(value, "0" | "false" | "FALSE" | "False"),
        _ => false,
    }
}

struct ClaudeSettings {
    env: HashMap<String, String>,
    api_key_helper: Option<String>,
}

impl Default for ClaudeSettings {
    fn default() -> Self {
        Self {
            env: HashMap::new(),
            api_key_helper: None,
        }
    }
}

fn read_settings(lookup: &impl AuthLookup, config_dir: &Path) -> ClaudeSettings {
    let path = config_dir.join("settings.json");
    let Ok(body) = lookup.read_file(&path) else {
        return ClaudeSettings::default();
    };
    parse_settings(&body)
}

fn parse_settings(body: &str) -> ClaudeSettings {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(rename = "apiKeyHelper")]
        api_key_helper: Option<String>,
    }
    match serde_json::from_str::<File>(body) {
        Ok(file) => ClaudeSettings {
            env: file.env,
            api_key_helper: file.api_key_helper,
        },
        Err(_) => ClaudeSettings::default(),
    }
}

#[derive(Deserialize)]
struct ClaudeAiOauth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicOAuthCred {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

fn read_oauth_store(lookup: &impl AuthLookup, config_dir: &Path) -> ClaudeAuthSource {
    let service = keychain_service_for(config_dir, &default_config_dir(lookup));
    if let Ok(body) = lookup.read_keychain(&service) {
        match parse_oauth_json(&body) {
            Ok(source) => return source,
            Err(UsageError::Shape(message)) => {
                return ClaudeAuthSource::Missing {
                    error: UsageError::Shape(message),
                }
            }
            Err(_) => {}
        }
    }

    let path = config_dir.join(".credentials.json");
    match lookup.read_file(&path) {
        Ok(body) => match parse_oauth_json(&body) {
            Ok(source) => source,
            Err(error) => ClaudeAuthSource::Missing { error },
        },
        Err(error) => ClaudeAuthSource::Missing { error },
    }
}

fn parse_oauth_json(body: &str) -> Result<ClaudeAuthSource, UsageError> {
    let cred: AnthropicOAuthCred =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    let oauth = cred
        .claude_ai_oauth
        .ok_or_else(|| UsageError::NoCredential("claudeAiOauth".to_string()))?;
    let token = oauth
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| UsageError::NoCredential("accessToken".to_string()))?;
    Ok(ClaudeAuthSource::Oauth {
        plan: plan_label(
            oauth.subscription_type.as_deref(),
            oauth.rate_limit_tier.as_deref(),
        ),
        token,
    })
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(target_os = "macos")]
fn read_platform_keychain(service: &str) -> Result<String, UsageError> {
    let output = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let body = String::from_utf8(output.stdout)
                .map_err(|_| UsageError::NoCredential(service.to_string()))?;
            let trimmed = body.trim();
            if trimmed.is_empty() {
                Err(UsageError::NoCredential(service.to_string()))
            } else {
                Ok(trimmed.to_string())
            }
        }
        _ => Err(UsageError::NoCredential(service.to_string())),
    }
}

#[cfg(not(target_os = "macos"))]
fn read_platform_keychain(service: &str) -> Result<String, UsageError> {
    Err(UsageError::NoCredential(service.to_string()))
}

#[cfg(test)]
pub(crate) struct FakeLookup {
    pub env: HashMap<String, String>,
    pub files: HashMap<PathBuf, String>,
    pub keychain: HashMap<String, String>,
    pub home: PathBuf,
}

#[cfg(test)]
impl Default for FakeLookup {
    fn default() -> Self {
        Self {
            env: HashMap::new(),
            files: HashMap::new(),
            keychain: HashMap::new(),
            home: PathBuf::from("/home/user"),
        }
    }
}

#[cfg(test)]
impl AuthLookup for FakeLookup {
    fn env(&self, key: &str) -> Option<String> {
        self.env.get(key).cloned()
    }

    fn read_file(&self, path: &Path) -> Result<String, UsageError> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| UsageError::NoCredential(path.to_string_lossy().to_string()))
    }

    fn read_keychain(&self, service: &str) -> Result<String, UsageError> {
        self.keychain
            .get(service)
            .cloned()
            .ok_or_else(|| UsageError::NoCredential(service.to_string()))
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OAUTH_JSON: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-secret","subscriptionType":"pro"}}"#;
    const ENTERPRISE_JSON: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-ent","subscriptionType":"enterprise"}}"#;

    fn cred_path(lookup: &FakeLookup) -> PathBuf {
        lookup.home.join(".claude").join(".credentials.json")
    }

    fn with_oauth_file(json: &str) -> FakeLookup {
        let mut lookup = FakeLookup::default();
        lookup.files.insert(cred_path(&lookup), json.to_string());
        lookup
    }

    fn platform(source: &ClaudeAuthSource) -> Option<&'static str> {
        match source {
            ClaudeAuthSource::Managed { platform, .. } => Some(*platform),
            _ => None,
        }
    }

    #[test]
    fn bedrock_flag_beats_dormant_oauth() {
        let mut lookup = with_oauth_file(OAUTH_JSON);
        lookup
            .env
            .insert("CLAUDE_CODE_USE_BEDROCK".into(), "1".into());
        let source = resolve_claude_auth(&lookup);
        assert_eq!(platform(&source), Some(PLATFORM_BEDROCK));
        assert!(!format!("{source:?}").contains("sk-ant-"));
    }

    #[test]
    fn vertex_and_foundry_have_explicit_platforms() {
        let mut vertex = with_oauth_file(OAUTH_JSON);
        vertex
            .env
            .insert("CLAUDE_CODE_USE_VERTEX".into(), "true".into());
        assert_eq!(
            platform(&resolve_claude_auth(&vertex)),
            Some(PLATFORM_VERTEX)
        );

        let mut foundry = with_oauth_file(OAUTH_JSON);
        foundry
            .env
            .insert("CLAUDE_CODE_USE_FOUNDRY".into(), "1".into());
        assert_eq!(
            platform(&resolve_claude_auth(&foundry)),
            Some(PLATFORM_FOUNDRY)
        );
    }

    #[test]
    fn bedrock_wins_when_multiple_cloud_flags_are_set() {
        let mut lookup = with_oauth_file(OAUTH_JSON);
        lookup
            .env
            .insert("CLAUDE_CODE_USE_BEDROCK".into(), "1".into());
        lookup
            .env
            .insert("CLAUDE_CODE_USE_VERTEX".into(), "1".into());
        lookup
            .env
            .insert("CLAUDE_CODE_USE_FOUNDRY".into(), "1".into());
        assert_eq!(
            platform(&resolve_claude_auth(&lookup)),
            Some(PLATFORM_BEDROCK)
        );
    }

    #[test]
    fn settings_json_cloud_flag_beats_oauth_file() {
        let mut lookup = with_oauth_file(OAUTH_JSON);
        lookup.files.insert(
            lookup.home.join(".claude").join("settings.json"),
            r#"{"env":{"CLAUDE_CODE_USE_VERTEX":"1"}}"#.into(),
        );
        assert_eq!(
            platform(&resolve_claude_auth(&lookup)),
            Some(PLATFORM_VERTEX)
        );
    }

    #[test]
    fn process_env_beats_settings_json_env() {
        let mut lookup = with_oauth_file(OAUTH_JSON);
        lookup
            .env
            .insert("CLAUDE_CODE_USE_BEDROCK".into(), "0".into());
        lookup.files.insert(
            lookup.home.join(".claude").join("settings.json"),
            r#"{"env":{"CLAUDE_CODE_USE_BEDROCK":"1"}}"#.into(),
        );
        match resolve_claude_auth(&lookup) {
            ClaudeAuthSource::Oauth { .. } => {}
            other => panic!("process env must disable the settings.json cloud flag, got {other:?}"),
        }
    }

    #[test]
    fn api_key_and_auth_token_beat_oauth() {
        let mut token = with_oauth_file(OAUTH_JSON);
        token
            .env
            .insert("ANTHROPIC_AUTH_TOKEN".into(), "sk-gateway".into());
        assert_eq!(platform(&resolve_claude_auth(&token)), Some(PLATFORM_GATEWAY));

        let mut key = with_oauth_file(OAUTH_JSON);
        key.env
            .insert("ANTHROPIC_API_KEY".into(), "sk-ant-api03-secret".into());
        let source = resolve_claude_auth(&key);
        assert_eq!(platform(&source), Some(PLATFORM_CONSOLE));
        assert!(!format!("{source:?}").contains("sk-ant-api03"));
    }

    #[test]
    fn api_key_helper_beats_oauth_without_running_the_script() {
        let mut lookup = with_oauth_file(OAUTH_JSON);
        lookup.files.insert(
            lookup.home.join(".claude").join("settings.json"),
            r#"{"apiKeyHelper":"/usr/bin/vault-token"}"#.into(),
        );
        assert_eq!(
            platform(&resolve_claude_auth(&lookup)),
            Some(PLATFORM_HELPER)
        );
    }

    #[test]
    fn oauth_env_token_beats_stored_login() {
        let mut lookup = with_oauth_file(ENTERPRISE_JSON);
        lookup
            .env
            .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "sk-ant-oat01-env".into());
        match resolve_claude_auth(&lookup) {
            ClaudeAuthSource::Oauth { token, .. } => assert_eq!(token, "sk-ant-oat01-env"),
            other => panic!("expected env oauth, got {other:?}"),
        }
    }

    #[test]
    fn profile_env_beats_stored_oauth() {
        let mut lookup = with_oauth_file(OAUTH_JSON);
        lookup
            .env
            .insert("ANTHROPIC_PROFILE".into(), "work".into());
        assert_eq!(
            platform(&resolve_claude_auth(&lookup)),
            Some(PLATFORM_PROFILE)
        );
    }

    #[test]
    fn file_oauth_includes_enterprise_plan() {
        match resolve_claude_auth(&with_oauth_file(ENTERPRISE_JSON)) {
            ClaudeAuthSource::Oauth { token, plan } => {
                assert_eq!(token, "sk-ant-oat01-ent");
                assert_eq!(plan.as_deref(), Some("Enterprise"));
            }
            other => panic!("expected oauth, got {other:?}"),
        }
    }

    #[test]
    fn keychain_is_preferred_over_file_via_injected_seam() {
        let mut lookup = with_oauth_file(OAUTH_JSON);
        lookup.keychain.insert(
            KEYCHAIN_SERVICE.into(),
            ENTERPRISE_JSON.into(),
        );
        match resolve_claude_auth(&lookup) {
            ClaudeAuthSource::Oauth { plan, .. } => {
                assert_eq!(plan.as_deref(), Some("Enterprise"))
            }
            other => panic!("expected keychain oauth, got {other:?}"),
        }
    }

    #[test]
    fn claude_config_dir_changes_file_and_keychain_identity() {
        let mut lookup = FakeLookup::default();
        lookup
            .env
            .insert("CLAUDE_CONFIG_DIR".into(), "/tmp/other-claude".into());
        lookup.files.insert(
            PathBuf::from("/tmp/other-claude/.credentials.json"),
            ENTERPRISE_JSON.into(),
        );
        lookup.files.insert(cred_path(&lookup), OAUTH_JSON.into());
        match resolve_claude_auth(&lookup) {
            ClaudeAuthSource::Oauth { plan, .. } => {
                assert_eq!(plan.as_deref(), Some("Enterprise"))
            }
            other => panic!("expected config-dir oauth, got {other:?}"),
        }

        let default = PathBuf::from("/home/user/.claude");
        let suffixed = keychain_service_for(Path::new("/tmp/other-claude"), &default);
        assert!(suffixed.starts_with("Claude Code-credentials-"));
        assert_ne!(suffixed, KEYCHAIN_SERVICE);
        assert_eq!(
            keychain_service_for(&default, &default),
            KEYCHAIN_SERVICE
        );
    }

    #[test]
    fn missing_credentials_are_logged_out() {
        match resolve_claude_auth(&FakeLookup::default()) {
            ClaudeAuthSource::Missing { error } => {
                let message = error.to_string();
                assert!(message.contains("No credential"), "{message}");
                assert!(!message.contains("sk-ant-"));
            }
            other => panic!("expected missing, got {other:?}"),
        }
    }

    #[test]
    fn malformed_credentials_are_shape_errors() {
        let mut lookup = FakeLookup::default();
        lookup
            .files
            .insert(cred_path(&lookup), "{not json".into());
        match resolve_claude_auth(&lookup) {
            ClaudeAuthSource::Missing {
                error: UsageError::Shape(_),
            } => {}
            other => panic!("expected shape error, got {other:?}"),
        }
    }

    #[test]
    fn cache_identity_changes_with_auth_source_and_hides_secrets() {
        let oauth = resolve_claude_auth(&with_oauth_file(OAUTH_JSON));
        let mut bedrock = with_oauth_file(OAUTH_JSON);
        bedrock
            .env
            .insert("CLAUDE_CODE_USE_BEDROCK".into(), "1".into());
        let cloud = resolve_claude_auth(&bedrock);
        let oauth_id = cache_identity_from(&oauth);
        let cloud_id = cache_identity_from(&cloud);
        let cloud_again = cache_identity_from(&resolve_claude_auth(&bedrock));
        assert!(oauth_id != cloud_id);
        assert!(cloud_id == cloud_again);
        let debug = format!("{:?} {:?}", oauth, cloud);
        assert!(!debug.contains("sk-ant-"));
        assert!(!debug.contains("secret"));
    }
}
