//! HTTP response value returned by route handlers.
//!
//! Handlers never touch the connection: they return a [`Response`] and the
//! server encodes it once through [`Response::encode`] + [`crate::http::request::write_full`].
//! That keeps transport (flush, TLS, BufStream) out of `routes/*`.

use std::fmt;

/// An HTTP response the server will write to the wire.
#[derive(Debug, Clone)]
pub struct Response {
    /// Status text including the code, e.g. `"200 OK"` or `"404 Not Found"`.
    pub status: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    /// Status line only, empty body, `Content-Length: 0`.
    pub fn empty(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// JSON body with `Content-Type: application/json`.
    pub fn json(status: impl Into<String>, body: impl AsRef<str>) -> Self {
        Self {
            status: status.into(),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.as_ref().as_bytes().to_vec(),
        }
    }

    /// `{"error":"..."}` JSON error envelope used by every 4xx/5xx JSON path.
    /// Uses `serde_json` so the body round-trips through a JSON parser even
    /// when `msg` contains characters that aren't safe to hand-escape:
    /// backslashes, control bytes, lone surrogates, or raw Windows paths
    /// like `C:\Users\alondero\src\buildmesh`.
    pub fn json_error(status: impl Into<String>, msg: &str) -> Self {
        let body = serde_json::json!({ "error": msg }).to_string();
        Self::json(status, body)
    }

    /// Arbitrary bytes with an explicit Content-Type.
    pub fn bytes(
        status: impl Into<String>,
        content_type: impl Into<String>,
        body: impl AsRef<[u8]>,
    ) -> Self {
        Self {
            status: status.into(),
            headers: vec![("Content-Type".into(), content_type.into())],
            body: body.as_ref().to_vec(),
        }
    }

    /// `429 Too Many Requests` with `Retry-After` and an empty body — same
    /// wire shape as the auth-failure statuses so a stolen-token flooder
    /// cannot distinguish "rate limited" from "bad token" by reading the body.
    pub fn rate_limited(retry_after_secs: u32) -> Self {
        Self {
            status: "429 Too Many Requests".into(),
            headers: vec![("Retry-After".into(), retry_after_secs.to_string())],
            body: Vec::new(),
        }
    }

    /// Append a response header. `Content-Length` is always derived from the
    /// body at encode time and must not be set here.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    #[cfg(test)]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Numeric status code parsed from the status text (`"200 OK"` → `200`).
    #[cfg(test)]
    pub fn status_code(&self) -> u16 {
        self.status
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// Wire bytes: status line, headers, `Content-Length`, body.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {}\r\n", self.status).into_bytes();
        for (name, value) in &self.headers {
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        let cl = self.body.len().to_string();
        out.extend_from_slice(b"Content-Length: ");
        out.extend_from_slice(cl.as_bytes());
        out.extend_from_slice(b"\r\n\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP/1.1 {}", self.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_encodes_zero_content_length() {
        let bytes = Response::empty("204 No Content").encode();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(text.contains("Content-Length: 0\r\n\r\n"));
        assert_eq!(text.as_bytes()[text.find("\r\n\r\n").unwrap() + 4..].len(), 0);
    }

    #[test]
    fn json_sets_content_type_and_length() {
        let bytes = Response::json("200 OK", "{\"ok\":true}").encode();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains("Content-Length: 11\r\n"));
        assert!(text.ends_with("{\"ok\":true}"));
    }

    #[test]
    fn json_error_escapes_quotes() {
        let bytes = Response::json_error("400 Bad Request", r#"say "hi""#).encode();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(r#"{"error":"say \"hi\""}"#));
    }

    #[test]
    fn json_error_handles_windows_path_with_backslashes() {
        // Hand-rolled `replace('"', "\\\"")` would emit `{"error":"C:\Users\…"}`
        // which the client parser rejects as an invalid escape sequence.
        let path = r"C:\Users\alondero\src\buildmesh";
        let bytes = Response::json_error("500 Internal Server Error", path).encode();
        let text = String::from_utf8(bytes).unwrap();
        let body = text.split("\r\n\r\n").nth(1).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(body).expect("body must round-trip through serde_json");
        assert_eq!(parsed["error"], path);
    }

    #[test]
    fn json_error_handles_newlines_and_tabs() {
        let bytes =
            Response::json_error("400 Bad Request", "line1\nline2\tend").encode();
        let text = String::from_utf8(bytes).unwrap();
        let body = text.split("\r\n\r\n").nth(1).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["error"], "line1\nline2\tend");
    }

    #[test]
    fn rate_limited_is_bodyless_with_retry_after() {
        let bytes = Response::rate_limited(3).encode();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 429 Too Many Requests\r\n"));
        assert!(text.contains("Retry-After: 3\r\n"));
        assert!(text.contains("Content-Length: 0\r\n\r\n"));
        assert!(!text.contains("error"));
    }

    #[test]
    fn extra_headers_precede_content_length() {
        let bytes = Response::json("409 Conflict", r#"{"error":"in_progress"}"#)
            .with_header("Retry-After", "1")
            .encode();
        let text = String::from_utf8(bytes).unwrap();
        let retry = text.find("Retry-After: 1").unwrap();
        let cl = text.find("Content-Length:").unwrap();
        assert!(retry < cl, "Retry-After must precede Content-Length");
    }
}
