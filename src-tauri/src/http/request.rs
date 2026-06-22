//! HTTP request parsing helpers: tokens, headers, and response writers.

use std::net::IpAddr;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// Pull a bearer token out of an `Authorization: Bearer <token>` header. This is
/// the only header-carried credential shape the server accepts post-#500 (URL
/// `?token=` is gone) — both the Admin/root path and the coordinator path read
/// it. Empty after the scheme is treated as absent.
pub fn bearer_token(headers: &str) -> Option<String> {
    let value = extract_header_value(headers, "Authorization")?;
    value
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
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

/// `Set-Cookie` line attached to a successful `POST /api/session` login (issue
/// #500), which is the sole place the session cookie is now minted. HttpOnly so
/// JS can't read it; SameSite=Lax for the cross-origin POSTs we don't make;
/// Path=/ so it travels with API calls and the WebSocket-ticket request.
pub fn session_cookie_header(token: &str) -> String {
    // Cookie value is the raw root token — validated the same way as the bearer
    // token on the login request. No `Secure` flag: the loopback/LAN server is
    // plain HTTP (TLS for the LAN path is a separate #494 slice).
    format!(
        "Set-Cookie: bm_session={}; HttpOnly; SameSite=Lax; Path=/",
        token
    )
}

/// Strip the optional `:port` (and IPv6 brackets) from a `Host` header value,
/// returning the bare hostname/IP. `127.0.0.1:1992` -> `127.0.0.1`,
/// `[::1]:1992` -> `::1`, `localhost` -> `localhost`.
pub fn strip_host_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6 literal: `[::1]` or `[::1]:port`.
        return rest.split(']').next().unwrap_or(rest);
    }
    // Hostname or IPv4, optionally `:port`.
    host.split(':').next().unwrap_or(host)
}

/// Validate a request's `Host` header to defeat DNS rebinding. A browser cannot
/// forge `Host`, so a rogue page that re-resolved its domain to `127.0.0.1`
/// still sends `Host: evil.com` -- which matches nothing here and is rejected.
///
/// Accepts `localhost`, any loopback IP, and the host's own interface IPs
/// (`local_ips`) so an opt-in LAN client reaching us on the box's LAN address
/// passes. Any other domain name, or an empty header, is rejected.
pub fn host_is_allowed(host_header: &str, local_ips: &[IpAddr]) -> bool {
    let hostname = strip_host_port(host_header.trim());
    if hostname.is_empty() {
        return false;
    }
    if hostname.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match hostname.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback() || local_ips.contains(&ip),
        // A non-`localhost` domain name is never a legitimate target for the
        // loopback/LAN server -- only the DNS-rebinding attacker uses one.
        Err(_) => false,
    }
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
    fn bearer_token_extracts_and_trims() {
        let headers = "Host: localhost\r\nAuthorization: Bearer deadbeef\r\n";
        assert_eq!(bearer_token(headers), Some("deadbeef".to_string()));
    }

    #[test]
    fn bearer_token_none_without_header_or_scheme() {
        assert_eq!(bearer_token("Host: localhost\r\n"), None);
        // A non-Bearer scheme is not a token we honour.
        assert_eq!(bearer_token("Authorization: Basic abc\r\n"), None);
        // Empty after the scheme is rejected.
        assert_eq!(bearer_token("Authorization: Bearer \r\n"), None);
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
    fn strip_host_port_handles_v4_v6_and_names() {
        assert_eq!(strip_host_port("127.0.0.1:1992"), "127.0.0.1");
        assert_eq!(strip_host_port("localhost:1992"), "localhost");
        assert_eq!(strip_host_port("localhost"), "localhost");
        assert_eq!(strip_host_port("[::1]:1992"), "::1");
        assert_eq!(strip_host_port("[::1]"), "::1");
        assert_eq!(strip_host_port("192.168.1.5"), "192.168.1.5");
    }

    #[test]
    fn host_is_allowed_accepts_loopback_and_localhost() {
        let none: &[IpAddr] = &[];
        assert!(host_is_allowed("localhost", none));
        assert!(host_is_allowed("LocalHost:1992", none));
        assert!(host_is_allowed("127.0.0.1", none));
        assert!(host_is_allowed("127.0.0.1:1992", none));
        assert!(host_is_allowed("[::1]:1992", none));
    }

    #[test]
    fn host_is_allowed_accepts_known_local_interface_ip() {
        let local: Vec<IpAddr> = vec!["192.168.1.5".parse().unwrap()];
        assert!(host_is_allowed("192.168.1.5:1992", &local));
        // A different LAN IP that isn't ours is not whitelisted.
        assert!(!host_is_allowed("192.168.1.99:1992", &local));
    }

    #[test]
    fn host_is_allowed_rejects_rebinding_domains_and_empty() {
        let local: Vec<IpAddr> = vec!["192.168.1.5".parse().unwrap()];
        // The DNS-rebinding case: attacker's domain, even resolved to loopback,
        // still arrives as its own name in Host.
        assert!(!host_is_allowed("evil.com", &local));
        assert!(!host_is_allowed("attacker.example:1992", &local));
        assert!(!host_is_allowed("", &local));
        assert!(!host_is_allowed("   ", &local));
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
