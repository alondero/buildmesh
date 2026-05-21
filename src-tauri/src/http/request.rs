//! HTTP request parsing helpers: tokens, headers, and response writers.

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::db;

pub fn extract_token_from_path(path: &str) -> Option<String> {
    path.split('?')
        .nth(1)
        .and_then(|query| {
            query
                .split('&')
                .find(|pair| pair.starts_with("token="))
                .map(|pair| pair[6..].to_string())
        })
}

pub fn validate_token(token: Option<String>) -> bool {
    match token {
        Some(t) => db::validate_root_token(&t).unwrap_or(false),
        None => false,
    }
}

/// Extract `bm_session=<token>` from the `Cookie:` header. The cookie is
/// set on the initial `/v2?token=...` load so subsequent fetches and the
/// WebSocket upgrade can authenticate without keeping the token in URLs.
pub fn extract_token_from_cookies(headers: &str) -> Option<String> {
    let header = extract_header_value(headers, "Cookie")?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix("bm_session=") {
            return Some(value.to_string());
        }
    }
    None
}

/// Authenticate a request via either the `bm_session` cookie or the
/// `?token=` URL parameter. The cookie wins when both are present so a
/// stale URL token can't override a fresh session.
pub fn authenticate(headers: &str, url_token: Option<String>) -> bool {
    if validate_token(extract_token_from_cookies(headers)) {
        return true;
    }
    validate_token(url_token)
}

/// Header value attached to the initial `/v2` response after a successful
/// `?token=` validation. HttpOnly so JS can't read it; SameSite=Lax for the
/// cross-origin POSTs we don't make. Path=/ so it travels with API calls and
/// the WebSocket upgrade.
pub fn session_cookie_header(token: &str) -> String {
    // Cookie value is just the raw token — the server validates it the same
    // way as a `?token=` URL param. No `Secure` flag: this is HTTP only.
    format!(
        "Set-Cookie: bm_session={}; HttpOnly; SameSite=Lax; Path=/",
        token
    )
}

pub fn extract_header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    let name_bytes = name.as_bytes();
    headers
        .lines()
        .find(|line| {
            let lb = line.as_bytes();
            lb.len() > name_bytes.len()
                && lb[name_bytes.len()] == b':'
                && lb[..name_bytes.len()].eq_ignore_ascii_case(name_bytes)
        })
        .map(|line| line[name.len() + 1..].trim())
}

pub async fn write_status_only(
    lines: &mut tokio::io::BufStream<TcpStream>,
    status: &str,
) -> std::io::Result<()> {
    let response = format!("HTTP/1.1 {}\r\nContent-Length: 0\r\n\r\n", status);
    lines.get_mut().write_all(response.as_bytes()).await
}

pub async fn write_json(
    lines: &mut tokio::io::BufStream<TcpStream>,
    status: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    lines.get_mut().write_all(response.as_bytes()).await
}

pub async fn send_json_error(
    lines: &mut tokio::io::BufStream<TcpStream>,
    status: &str,
    msg: &str,
) {
    let body = format!(r#"{{"error":"{}"}}"#, msg.replace('"', "\\\""));
    let _ = write_json(lines, status, &body).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_token_from_simple_path() {
        let token = extract_token_from_path("/?token=abc123");
        assert_eq!(token, Some("abc123".to_string()));
    }

    #[test]
    fn extract_token_from_path_with_multiple_params() {
        let token = extract_token_from_path("/api/nodes?foo=bar&token=secret&baz=1");
        assert_eq!(token, Some("secret".to_string()));
    }

    #[test]
    fn extract_token_returns_none_without_token_param() {
        let token = extract_token_from_path("/api/nodes?foo=bar");
        assert_eq!(token, None);
    }

    #[test]
    fn extract_token_returns_none_for_bare_path() {
        let token = extract_token_from_path("/api/nodes");
        assert_eq!(token, None);
    }

    #[test]
    fn extract_token_from_ws_path() {
        let token = extract_token_from_path("/ws/terminal/306?token=deadbeef");
        assert_eq!(token, Some("deadbeef".to_string()));
    }

    #[test]
    fn extract_token_empty_value() {
        let token = extract_token_from_path("/?token=");
        assert_eq!(token, Some("".to_string()));
    }

    #[test]
    fn validate_token_rejects_none() {
        assert!(!validate_token(None));
    }

    #[test]
    fn extract_token_from_single_cookie() {
        let headers = "Host: localhost\r\nCookie: bm_session=abc123\r\n";
        assert_eq!(extract_token_from_cookies(headers), Some("abc123".into()));
    }

    #[test]
    fn extract_token_from_cookie_with_others() {
        let headers =
            "Host: localhost\r\nCookie: foo=bar; bm_session=secret; baz=qux\r\n";
        assert_eq!(extract_token_from_cookies(headers), Some("secret".into()));
    }

    #[test]
    fn extract_token_from_cookies_returns_none_when_missing() {
        let headers = "Host: localhost\r\nCookie: foo=bar\r\n";
        assert_eq!(extract_token_from_cookies(headers), None);
    }

    #[test]
    fn extract_token_from_cookies_returns_none_when_no_cookie_header() {
        let headers = "Host: localhost\r\n";
        assert_eq!(extract_token_from_cookies(headers), None);
    }

    #[test]
    fn session_cookie_header_is_set_cookie() {
        let header = session_cookie_header("deadbeef");
        assert!(header.starts_with("Set-Cookie: bm_session=deadbeef"));
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("SameSite=Lax"));
        assert!(header.contains("Path=/"));
    }

    #[test]
    fn extract_header_value_case_insensitive() {
        let headers = "Host: localhost\r\nSec-WebSocket-Key: abc123\r\nConnection: Upgrade\r\n";
        assert_eq!(extract_header_value(headers, "Sec-WebSocket-Key"), Some("abc123"));
        assert_eq!(extract_header_value(headers, "sec-websocket-key"), Some("abc123"));
        assert_eq!(extract_header_value(headers, "Host"), Some("localhost"));
        assert_eq!(extract_header_value(headers, "Missing"), None);
    }
}
