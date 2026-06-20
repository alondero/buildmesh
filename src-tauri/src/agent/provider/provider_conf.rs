//! Read `~/.claude/providers.conf` — the shell `KEY=value` file that `cwrap`
//! sources for third-party backend API keys (MiniMax, Kimi).
//!
//! The Windows OS-sandbox path can't run `cwrap`: it routes through MSYS2
//! `bash`, which fails to initialize inside an AppContainer
//! (`STATUS_DLL_INIT_FAILED`). So the MiniMax/Kimi adapters reconstruct the
//! backend environment in-process from this same file and inject it directly
//! into the claude.exe spawn. First slice of absorbing cwrap into buildmesh
//! (see the cwrap-absorption tracking issue).

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
    use super::parse_providers_conf;

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
}
