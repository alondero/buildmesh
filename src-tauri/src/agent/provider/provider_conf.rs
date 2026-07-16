//! Read `~/.claude/providers.conf` — the shell `KEY=value` file that used to be
//! sourced by the now-archived `cwrap` launcher for third-party backend API
//! keys (MiniMax, Kimi).
//!
//! Post-#538, node *spawns* no longer read this file: a custom Claude-compatible
//! profile carries its endpoint in a model-provider account, injected at spawn by
//! [`crate::preferences::resolve_provider_env`]. The one remaining reader is
//! [`minimax_backend_env`], used by the session-naming helper
//! ([`crate::session_naming`]) to run a cheap MiniMax model for node slugs — that
//! side-channel keeps its hardcoded `~/.claude/providers.conf` routing.

use std::collections::HashMap;

/// Parse `~/.claude/providers.conf` into a key→value map. A missing/unreadable
/// file yields an empty map (the caller logs when a required key is absent).
pub fn read_providers_conf() -> HashMap<String, String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    let Some(home) = home else {
        return HashMap::new();
    };
    let path = std::path::Path::new(&home)
        .join(".claude")
        .join("providers.conf");
    match std::fs::read_to_string(&path) {
        Ok(contents) => parse_providers_conf(&contents),
        Err(_) => HashMap::new(),
    }
}

/// Parse the shell-style `KEY=value` body. Blank lines and `#` comments are
/// skipped; an optional leading `export ` is stripped; a matched pair of
/// surrounding single/double quotes is removed from the value.
pub fn parse_providers_conf(contents: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if !key.is_empty() {
                map.insert(key.to_string(), strip_matched_quotes(value.trim()));
            }
        }
    }
    map
}

/// Whether a MiniMax API key is configured, either via the legacy
/// `~/.claude/providers.conf` file or the Provider-Accounts UI. Used by
/// [`crate::session_naming`] to gate the legacy-MiniMax fallback without
/// firing the `tracing::error!` that [`minimax_backend_env`] logs when
/// the key is missing. Side-effect-free (no log, no panic), so it's safe
/// to call on every node turn as a precondition check.
///
/// Order matches [`minimax_backend_env`]: legacy conf first, then the
/// #537 Accounts-UI key. Both fallbacks must agree for the legacy path to
/// count as "configured" — a user who has a conf-file URL but no key still
/// won't have a usable auth flow.
pub fn minimax_api_key_present() -> bool {
    let conf = read_providers_conf();
    if conf
        .get("MINIMAX_API_KEY")
        .filter(|v| !v.is_empty())
        .is_some()
    {
        return true;
    }
    crate::preferences::minimax_api_key_resolved().is_some()
}

/// Build the MiniMax backend `ANTHROPIC_*` env from `~/.claude/providers.conf` —
/// the variables the absorbed `cwrap --minimax` arm exported.
///
/// This is the **naming side-channel only**. Node *spawns* now resolve their
/// backend env per-profile from the configured model-provider account (see
/// [`crate::preferences::resolve_provider_env`]); the hardcoded MiniMax routing
/// that used to live in `minimax.rs`'s `provider_env()` survives here for the
/// session-naming helper ([`crate::session_naming`]), which deliberately runs a
/// cheap MiniMax model to summarise a node into a slug regardless of the node's
/// own provider. A missing `MINIMAX_API_KEY` is logged (the naming `claude
/// --print` call would then fail to authenticate) but never panics.
pub fn minimax_backend_env() -> Vec<(String, String)> {
    let conf = read_providers_conf();
    let base_url = conf
        .get("MINIMAX_BASE_URL")
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| "https://api.minimax.io/anthropic".to_string());
    // Issue #571: the per-tier model map is the single source owned by
    // `preferences::minimax_default_tiers` (consumed by both the per-account
    // `model_tiers` and this side-channel). Renaming a MiniMax model now
    // happens in exactly one place.
    let tiers = crate::preferences::minimax_default_tiers();
    let mut env = vec![
        ("ANTHROPIC_BASE_URL".to_string(), base_url),
        ("API_TIMEOUT_MS".to_string(), "3000000".to_string()),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(), "1".to_string()),
        ("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), "512000".to_string()),
        ("ANTHROPIC_MODEL".to_string(), tiers.default.clone().unwrap()),
        ("ANTHROPIC_SMALL_FAST_MODEL".to_string(), tiers.small_fast.clone().unwrap()),
        ("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), tiers.sonnet.clone().unwrap()),
        ("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), tiers.opus.clone().unwrap()),
        ("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), tiers.haiku.clone().unwrap()),
    ];
    // Prefer the legacy `~/.claude/providers.conf` key (Adam's existing setup),
    // then fall back to the key configured via the #537 Accounts UI
    // (`minimax_api_key_resolved` reads the minimax account key then the legacy
    // flat field), so a user who only set their key in the new UI still gets a
    // working naming side-channel.
    let api_key = conf
        .get("MINIMAX_API_KEY")
        .filter(|v| !v.is_empty())
        .cloned()
        .or_else(crate::preferences::minimax_api_key_resolved);
    match api_key {
        Some(key) => env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), key)),
        None => tracing::error!(
            "MiniMax naming: no MINIMAX_API_KEY in ~/.claude/providers.conf nor a configured MiniMax account key — claude --print will fail to authenticate"
        ),
    }
    env
}

fn strip_matched_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{minimax_backend_env, parse_providers_conf};

    #[test]
    fn parses_keys_skips_comments_and_blanks() {
        let conf = "\
# Claude Code provider API keys
MINIMAX_API_KEY=sk-minimax-123

# MINIMAX_BASE_URL=https://api.minimaxi.com/anthropic
MOONSHOT_API_KEY=sk-moonshot-456
";
        let map = parse_providers_conf(conf);
        assert_eq!(map.get("MINIMAX_API_KEY").map(String::as_str), Some("sk-minimax-123"));
        assert_eq!(map.get("MOONSHOT_API_KEY").map(String::as_str), Some("sk-moonshot-456"));
        // A commented key must NOT be picked up.
        assert!(!map.contains_key("MINIMAX_BASE_URL"), "commented line must be ignored");
    }

    #[test]
    fn strips_quotes_and_export_prefix() {
        let map = parse_providers_conf("export MOONSHOT_API_KEY=\"sk-q\"\nMINIMAX_API_KEY='sk-s'\n");
        assert_eq!(map.get("MOONSHOT_API_KEY").map(String::as_str), Some("sk-q"));
        assert_eq!(map.get("MINIMAX_API_KEY").map(String::as_str), Some("sk-s"));
    }

    /// Issue #571 tidy-up: the MiniMax model routing injected into the
    /// session-naming side-channel is the same `ModelTiers` shipped in
    /// `default_provider_accounts`. If a contributor re-hardcodes the strings
    /// in `minimax_backend_env`, this test catches the drift.
    #[test]
    fn minimax_backend_env_model_vars_match_default_tier_map() {
        use crate::preferences::minimax_default_tiers;

        let tiers = minimax_default_tiers();
        let env = minimax_backend_env();
        let get = |key: &str| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("ANTHROPIC_MODEL"), tiers.default);
        assert_eq!(get("ANTHROPIC_SMALL_FAST_MODEL"), tiers.small_fast);
        assert_eq!(get("ANTHROPIC_DEFAULT_SONNET_MODEL"), tiers.sonnet);
        assert_eq!(get("ANTHROPIC_DEFAULT_OPUS_MODEL"), tiers.opus);
        assert_eq!(get("ANTHROPIC_DEFAULT_HAIKU_MODEL"), tiers.haiku);
    }
}
