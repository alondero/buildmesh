//! HTTP request parsing helpers: tokens, headers, and response writers.

use std::net::IpAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufStream};

use crate::http::MaybeTls;

/// Wall-clock budget for reading a request body once the head is in. Bounds the
/// same slowloris class that `http::mod::REQUEST_HEAD_TIMEOUT` bounds for the
/// head: a client that advertises `Content-Length: 262144` and dribbles bytes
/// once a second would otherwise pin a tokio worker for the entire upload
/// window. 60 s is long enough for a legitimate 256 KB upload over a constrained
/// link (≈ 4 KB/s sustained) and short enough that a stalled connection drops
/// inside a single connection-handling task's lifetime.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Outcome of a bounded body read.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadBodyError {
    /// `Content-Length` exceeded the route's per-call cap. The connection is
    /// left in a usable state — caller writes `413` and returns.
    TooLarge,
    /// EOF or a read error before `content_length` bytes arrived. Caller writes
    /// `400`.
    ReadFailed,
    /// The read did not finish within [`BODY_READ_TIMEOUT`]. Caller writes
    /// `408` and the connection is dropped (the body may be partially read).
    TimedOut,
}

/// Read a request body of exactly `content_length` bytes under a wall-clock
/// deadline ([`BODY_READ_TIMEOUT`]), refusing anything larger than `max_bytes`.
///
/// The cap and the timeout are independent guards: the cap bounds *how much*
/// the client can force the server to buffer, the timeout bounds *how long* a
/// stalled client can pin the connection. Without the timeout a slowloris that
/// sends `Content-Length: 262144` and dribbles 1 byte/sec would pin a worker
/// for the full upload window — the same DoS class `handle_connection`'s head
/// read closes for the request line + headers. Both halves live behind this
/// helper so every body-reading route carries the same guard.
pub async fn read_body_with_cap(
    lines: &mut BufStream<MaybeTls>,
    content_length: usize,
    max_bytes: usize,
) -> Result<Vec<u8>, ReadBodyError> {
    if content_length > max_bytes {
        return Err(ReadBodyError::TooLarge);
    }
    let mut buf = vec![0u8; content_length];
    let read = async {
        if content_length > 0 {
            lines
                .read_exact(&mut buf)
                .await
                .map_err(|_| ReadBodyError::ReadFailed)?;
        }
        Ok::<_, ReadBodyError>(buf)
    };
    match tokio::time::timeout(BODY_READ_TIMEOUT, read).await {
        Ok(result) => result,
        Err(_) => Err(ReadBodyError::TimedOut),
    }
}

/// Convenience wrapper for the common dispatch shape: read a body, mapping
/// each failure to the status line the route would have written by hand
/// (`413` / `400` / `408`). Returns `None` on any failure so the caller can
/// `return` immediately without writing its own status. The empty-body case
/// (`Content-Length: 0`) returns `Some(vec![])` — same shape every existing
/// route already produces.
pub async fn read_body_or_send_error(
    lines: &mut BufStream<MaybeTls>,
    content_length: usize,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    match read_body_with_cap(lines, content_length, max_bytes).await {
        Ok(buf) => Some(buf),
        Err(ReadBodyError::TooLarge) => {
            send_json_error(lines, "413 Content Too Large", "Body too large").await;
            None
        }
        Err(ReadBodyError::ReadFailed) => {
            let _ = write_status_only(lines, "400 Bad Request").await;
            None
        }
        Err(ReadBodyError::TimedOut) => {
            let _ = write_status_only(lines, "408 Request Timeout").await;
            None
        }
    }
}

/// Write `bytes` to the connection **and flush them to the wire**.
///
/// Every HTTP response MUST go through this. Skipping the flush lets the
/// last partial chunk sit in `BufStream`'s 8 KB buffer; when the function
/// returns the connection drops before the chunk reaches Rustls, and
/// the client sees fewer bytes than the `Content-Length` header
/// advertised. Chrome surfaces this as `ERR_CONTENT_LENGTH_MISMATCH`
/// and aborts the module fetch — which is exactly the symptom that
/// black-screens the mobile SPA on Android.
///
/// The fix is systemic: one helper, one invariant ("flush after every
/// write"), every route passes through it.
pub async fn write_full(lines: &mut BufStream<MaybeTls>, bytes: &[u8]) -> std::io::Result<()> {
    let writer = lines.get_mut();
    writer.write_all(bytes).await?;
    writer.flush().await
}

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
///
/// `secure` adds the `Secure` attribute (issue #553) so the device-token cookie
/// is never replayed over plaintext. It is gated on the *connection*, not a
/// blanket flag: the loopback listener is always plain HTTP (the local
/// attention webhook posts plain `http://localhost`, issue #501), where a
/// `Secure` cookie would be silently dropped and break login. The caller passes
/// `MaybeTls::is_tls()` — ground truth, since the server terminates TLS itself
/// (loopback → `Plain`, LAN interfaces → `Tls`).
pub fn session_cookie_header(token: &str, secure: bool) -> String {
    format!(
        "Set-Cookie: bm_session={}; HttpOnly; SameSite=Lax; Path=/{}",
        token,
        if secure { "; Secure" } else { "" }
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
    lines: &mut tokio::io::BufStream<MaybeTls>,
    status: &str,
) -> std::io::Result<()> {
    let response = format!("HTTP/1.1 {}\r\nContent-Length: 0\r\n\r\n", status);
    write_full(lines, response.as_bytes()).await
}

/// Write a `429 Too Many Requests` whose **body** is uniform with the
/// auth-failure shapes (empty) and whose **`Retry-After` header** carries
/// the pacing hint. The body uniformity is the load-bearing security
/// property: a caller presenting a stolen token MUST NOT be able to
/// distinguish "rate-limited" from "bad token" by reading the response —
/// the only signal they get is the status line + header. `retry_after_secs`
/// is taken straight from the rate-limit [`crate::http::rate_limit::Outcome`]
/// computation (always `>= 1`) so we never emit `Retry-After: 0`,
/// which would invite an instant retry loop.
pub async fn write_rate_limited(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    retry_after_secs: u32,
) -> std::io::Result<()> {
    // Header-only line, no body — same wire shape as every other
    // 4xx/5xx the auth paths emit (issue #552 AC: "uniform with the other
    // auth-failure shapes").
    let response = format!(
        "HTTP/1.1 429 Too Many Requests\r\n\
         Retry-After: {retry_after_secs}\r\n\
         Content-Length: 0\r\n\r\n"
    );
    write_full(lines, response.as_bytes()).await
}

pub async fn write_json(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    status: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    write_full(lines, response.as_bytes()).await
}

/// Write a JSON response that also carries a `Retry-After` header (issue
/// #750, item 1). Used for the drive route's `409 in_progress` arm — the
/// orchestrator briefly waits for a peer to finalize a `pending` claim; if
/// the wait window expires the route returns 409 with `Retry-After: 1` so the
/// Coordinator retries after a short backoff rather than hammering
/// immediately. Body shape matches the other 4xx JSON errors the route
/// emits (`{"error":"..."}`).
pub async fn write_json_with_retry_after(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    status: &str,
    body: &str,
    retry_after_secs: u32,
) -> std::io::Result<()> {
    // Same wire shape as `write_json` plus the `Retry-After` header. The
    // floor on `retry_after_secs` is the caller's responsibility — the
    // rate-limit-style `>= 1` rule keeps a Coordinator from spinning in an
    // instant-retry loop.
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nRetry-After: {}\r\nContent-Length: {}\r\n\r\n{}",
        status,
        retry_after_secs,
        body.len(),
        body
    );
    write_full(lines, response.as_bytes()).await
}

pub async fn send_json_error(lines: &mut tokio::io::BufStream<MaybeTls>, status: &str, msg: &str) {
    let body = format!(r#"{{"error":"{}"}}"#, msg.replace('"', "\\\""));
    let _ = write_json(lines, status, &body).await;
}

/// Parse and canonicalise a string as the `cli_session_id` column value
/// (issue #1237).
///
/// Returns `Some(canonical_lowercase_uuid)` on success or `None` if the
/// input is not a well-formed UUID. The two routes that accept
/// `cli_session_id` from external input — `POST /api/attention/{id}` (hook
/// payload, loopback peer) and
/// `POST /api/meshes/{id}/agent-nodes/import-and-resume` (mobile client) —
/// share this validator so an arbitrary string can never reach a harness
/// argv. The resume path appends `id` raw to the CLI as `--resume <id>`
/// (`crate::agent::provider::AgentProvider::resume_args`); a value beginning
/// with `-` would land in flag position and the harness would interpret it
/// as an additional flag (`--dangerously-skip-permissions`,
/// `--output-format=...`, etc.) — the issue's injection vector.
///
/// Canonicalisation to lowercase happens here rather than only at the hook
/// boundary so the value stored in `agent_nodes.cli_session_id` matches the
/// form the orchestrator's spawn pipeline writes (`Uuid::to_string()` is
/// already lowercase), and downstream resume lookups don't need a separate
/// case-fold step.
pub fn parse_cli_session_id(s: &str) -> Option<String> {
    uuid::Uuid::parse_str(s).ok().map(|u| u.to_string())
}

/// Parse an OpenCode session id as posted by the project plugin's
/// `session.created` event (issue #1294).
///
/// OpenCode's `SessionID` schema is `ses_<hex+base62>` (12 hex timestamp
/// chars + 14 base62 chars), e.g. `ses_fc52ccfb9ffek1jl23ZwpRuSP7`
/// (`docs/learning/opencode-harness-capabilities.md`). The hex segment is
/// lowercase, but the base62 tail is **case-sensitive** — OpenCode looks
/// up ids case-sensitively (`opencode export ses_…Zwp…` succeeds,
/// `ses_…zwp…` returns `Session not found`), so a "helpful" lower-case
/// fold here destroys the id and breaks `AgentProvider::resume_args`.
/// The hook payload cannot use [`parse_cli_session_id`] because OpenCode
/// ids are NOT UUIDs — they would fail `Uuid::parse_str` and be silently
/// dropped, leaving `agent_nodes.cli_session_id` NULL forever.
///
/// Like the UUID validator, this gate closes the argv flag-position
/// injection vector for the `--session <id>` resume path
/// (`AgentProvider::resume_args`). An id like `-dangerously-skip-permissions`
/// would land in flag position; an id like `ses_$(whoami)` would let
/// shell metacharacters through. Bounds:
///   * Must start with the `ses_` prefix (case-insensitive — live 1.18.3
///     rejects unknown `ses_…` ids rather than creating them, so any
///     upstream plugin revision that ships a different casing is still
///     legitimate input; see `opencode-harness-capabilities.md` for the
///     three settled failure cases). The prefix is normalised to
///     lowercase on output.
///   * Total length is bounded to `[5, 129]` — `ses_` alone (4 chars)
///     is rejected as too short, a 1-char remainder (5 total) is the
///     shortest well-formed id. The upper bound leaves headroom for
///     any future OpenCode schema bump without admitting flag-shaped
///     strings (live ids are 30 chars; 124 chars of remainder is still
///     well-formed).
///   * The remainder after `ses_` must be `[0-9a-zA-Z_]` — base62
///     (case-sensitive) plus the underscore separator. Hyphens, dots,
///     spaces, and non-ASCII letters are rejected. The remainder's
///     original casing is preserved on output.
///
/// Returns `Some(canonical_form)` where the prefix is `ses_` and the
/// remainder keeps its original casing, or `None` if the input doesn't
/// match the gate.
pub fn parse_opencode_session_id(s: &str) -> Option<String> {
    let s = s.trim();
    // Case-insensitive prefix check on the original (un-lowered) input
    // so we can preserve the remainder's casing. `eq_ignore_ascii_case`
    // is allocation-free; the prefix itself is only four bytes.
    if s.len() < 4 || !s.as_bytes()[..4].eq_ignore_ascii_case(b"ses_") {
        return None;
    }
    let rest = &s[4..];
    if rest.is_empty() || rest.len() > 124 {
        return None;
    }
    // Base62 + underscore. `is_ascii_alphanumeric` covers 0-9, A-Z, a-z;
    // we add the underscore explicitly. Anything outside ASCII (e.g.
    // Unicode look-alikes) is rejected by `is_ascii_*`.
    if !rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    // Canonical form: lowercase prefix + original-cased remainder.
    Some(format!("ses_{rest}"))
}

/// Pick the right per-provider validator for a hook payload's session id
/// (issue #1294).
///
/// Returns `None` for unknown providers so the route defaults to its
/// pre-#1294 behaviour (UUID validator) — a future harness that ships a
/// non-UUID id shape must update both this dispatcher and its adapter's
/// `claude_session_id`-style column before the value reaches
/// `agent_nodes.cli_session_id`. The provider string is the value stored
/// in `agent_nodes.provider`; OpenCode's id gate lives here, not in
/// `agent::opencode`, so the attention route has one extraction point.
pub fn parse_session_id_for_provider(provider: &str, raw: &str) -> Option<String> {
    match provider {
        "opencode" => parse_opencode_session_id(raw),
        // Codex/Claude/AGY/Grok/Cursor all use UUIDs (the alias stack on
        // `HookPayload::session_id` accepts every casing each harness
        // ships; canonicalisation to lowercase happens in
        // `parse_cli_session_id`). Defaulting here means a future
        // harness adopting UUIDs without code changes "just works".
        _ => parse_cli_session_id(raw),
    }
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

    // ---- parse_cli_session_id (issue #1237) ----------------------------
    //
    // The validator closes the argv flag-position injection vector for the
    // `--resume <id>` resume path. A well-formed UUID is the only accepted
    // shape; everything else (flag-like strings, garbage, shell
    // metacharacters) returns None so the route returns 400.

    #[test]
    fn parse_cli_session_id_canonicalises_mixed_case_uuid() {
        // The exact payload from the issue spec's "valid UUID" example.
        assert_eq!(
            parse_cli_session_id("C1234567-89AB-CDEF-0123-456789ABCDEF"),
            Some("c1234567-89ab-cdef-0123-456789abcdef".to_string())
        );
    }

    #[test]
    fn parse_cli_session_id_passes_through_lowercase_uuid() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(parse_cli_session_id(id), Some(id.to_string()));
    }

    #[test]
    fn parse_cli_session_id_rejects_flag_like_string() {
        // The exact attack from issue #1237: a string beginning with `-`
        // would land in argv flag position. The validator must reject it.
        assert_eq!(parse_cli_session_id("--dangerously-skip-permissions"), None);
        assert_eq!(parse_cli_session_id("--resume"), None);
        assert_eq!(parse_cli_session_id("-x"), None);
    }

    #[test]
    fn parse_cli_session_id_rejects_empty_and_garbage() {
        assert_eq!(parse_cli_session_id(""), None);
        assert_eq!(parse_cli_session_id("not-a-uuid"), None);
        // Same value the attention route's pre-validator was already
        // rejecting — regression-pin so the move to a shared helper doesn't
        // drop a coverage branch.
        assert_eq!(parse_cli_session_id("most-recent"), None);
        assert_eq!(parse_cli_session_id("1234"), None);
        // Truncated UUID (missing the final segment) — `Uuid::parse_str` is
        // strict about the hyphenated 8-4-4-4-12 shape.
        assert_eq!(parse_cli_session_id("550e8400-e29b-41d4-a716"), None);
    }

    #[test]
    fn parse_cli_session_id_rejects_shell_metacharacters() {
        // Belt-and-braces: even if a future bypass tried to slip a UUID with
        // extra payload past the parser, the format check rejects it.
        assert_eq!(
            parse_cli_session_id("550e8400-e29b-41d4-a716-446655440000; rm -rf /"),
            None
        );
        assert_eq!(parse_cli_session_id("$(whoami)"), None);
        assert_eq!(parse_cli_session_id("`id`"), None);
    }

    // ---- parse_opencode_session_id (issue #1294) ------------------------
    //
    // OpenCode mints `ses_<hex+base62>` ids. Live 1.18.3 rejects unknown
    // shapes rather than creating them; the validator mirrors that
    // contract so an unknown id never reaches `--session` argv.

    #[test]
    fn parse_opencode_session_id_accepts_live_format() {
        // Canonical example from
        // `docs/learning/opencode-harness-capabilities.md`. The Base62
        // tail (`ZwpRuSP7`) is case-sensitive — OpenCode's CLI rejects
        // unknown ids and looks them up case-sensitively, so the
        // remainder MUST round-trip in its original casing.
        assert_eq!(
            parse_opencode_session_id("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
            Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()),
            "the live id round-trips with Base62 casing preserved"
        );
    }

    #[test]
    fn parse_opencode_session_id_preserves_mixed_case_remainder() {
        // The base62 tail is case-sensitive: lowercasing it on output
        // breaks `opencode --session <id>` resume. Only the `ses_`
        // prefix is normalised to lowercase; the remainder keeps its
        // original casing (lower, upper, or mixed).
        assert_eq!(
            parse_opencode_session_id("ses_Fc52ccFb9ffEk1jL23zWprUsP7"),
            Some("ses_Fc52ccFb9ffEk1jL23zWprUsP7".to_string()),
            "all-upper / all-lower / mixed remainders round-trip as-is"
        );
        // Uppercase-only remainder (legal Base62) round-trips unchanged.
        assert_eq!(
            parse_opencode_session_id("ses_FC52CCFB9FFEK1JL23ZWPRUSP7"),
            Some("ses_FC52CCFB9FFEK1JL23ZWPRUSP7".to_string()),
        );
        // Lowercase-only remainder round-trips unchanged (no folding).
        assert_eq!(
            parse_opencode_session_id("ses_fc52ccfb9ffek1jl23zwprusp7"),
            Some("ses_fc52ccfb9ffek1jl23zwprusp7".to_string()),
        );
    }

    #[test]
    fn parse_opencode_session_id_normalises_prefix_case() {
        // The `ses_` prefix check is case-insensitive — an upstream
        // plugin revision could ship `SES_`/`Ses_`/etc. — but the
        // canonical form stored in `agent_nodes.cli_session_id` always
        // uses lowercase `ses_` so downstream comparison is uniform.
        // The remainder keeps its original casing.
        assert_eq!(
            parse_opencode_session_id("SES_FC52CCFB9FFE"),
            Some("ses_FC52CCFB9FFE".to_string()),
            "uppercase prefix is normalised, remainder casing preserved"
        );
        assert_eq!(
            parse_opencode_session_id("Ses_Fc52CcFb9"),
            Some("ses_Fc52CcFb9".to_string()),
        );
        // Leading whitespace (plugin payload could carry it) is trimmed.
        assert_eq!(
            parse_opencode_session_id("  ses_abc123  "),
            Some("ses_abc123".to_string()),
        );
    }

    #[test]
    fn parse_opencode_session_id_accepts_well_formed_unknown_id() {
        // The validator only gates SHAPE — whether an id actually
        // resolves in OpenCode is the harness's concern (`opencode
        // --session ses_000…` returns `Session not found`, so the plugin
        // never POSTs a phantom id from inside a real OpenCode process).
        // A well-formed but unknown id round-trips through the gate;
        // the "don't write unknown ids" AC is enforced by the plugin
        // running inside OpenCode, not by this Rust validator.
        assert_eq!(
            parse_opencode_session_id("ses_0000000000000000000"),
            Some("ses_0000000000000000000".to_string())
        );
    }

    #[test]
    fn parse_opencode_session_id_rejects_uuid_shaped_input() {
        // The UUID validator (`parse_cli_session_id`) accepts UUIDs; the
        // OpenCode validator must NOT — they're disjoint shapes and a
        // regression that lets a UUID through here would corrupt
        // `cli_session_id` for an OpenCode resume (`opencode --session <uuid>`
        // is `Invalid session ID`).
        assert_eq!(
            parse_opencode_session_id("550e8400-e29b-41d4-a716-446655440000"),
            None
        );
    }

    #[test]
    fn parse_opencode_session_id_rejects_short_or_oversize_input() {
        // `ses_` alone (zero remainder) is rejected — too short to be a
        // real id. The upper bound (124 chars of remainder) bounds
        // hostile inputs while leaving room for any future OpenCode
        // schema bump (live ids are 26 chars; a hypothetical 124-char
        // id is still well-formed).
        assert_eq!(parse_opencode_session_id("ses_"), None);
        // A 1-char remainder is well-formed; OpenCode's documented
        // generator produces 26-char remainders today, but the
        // validator stays format-only so a future schema bump doesn't
        // silently reject valid ids.
        assert_eq!(
            parse_opencode_session_id("ses_a"),
            Some("ses_a".to_string())
        );
        // Length cap: 132 chars total (5 prefix + 127 remainder > 124 max).
        let oversize = format!("ses_{}", "a".repeat(127));
        assert_eq!(parse_opencode_session_id(&oversize), None);
    }

    #[test]
    fn parse_opencode_session_id_rejects_flag_like_and_shell_metachars() {
        // Same argv-injection guard as the UUID validator (issue #1237):
        // a value beginning with `-` lands in flag position once spliced
        // into `--session <id>`; a value with shell metacharacters opens
        // a much wider injection vector.
        assert_eq!(
            parse_opencode_session_id("--dangerously-skip-permissions"),
            None
        );
        assert_eq!(parse_opencode_session_id("ses_$(whoami)"), None);
        assert_eq!(parse_opencode_session_id("ses_a; rm -rf /"), None);
        assert_eq!(parse_opencode_session_id("ses_a`id`"), None);
    }

    #[test]
    fn parse_opencode_session_id_rejects_punctuation_and_non_ascii() {
        // The character class for the remainder is `[0-9a-zA-Z_]`
        // (Base62 + underscore, case-preserving). Hyphens, dots,
        // spaces, and non-ASCII letters are all foreign. Uppercase in
        // the input is fine — `parse_opencode_session_id_preserves_mixed_case_remainder`
        // pins that — so we don't re-test it here.
        assert_eq!(parse_opencode_session_id("ses_abc-123"), None);
        assert_eq!(parse_opencode_session_id("ses_abc.123"), None);
        assert_eq!(parse_opencode_session_id("ses_abc 123"), None);
        assert_eq!(parse_opencode_session_id("ses_café"), None);
        // Unicode look-alike for `_` (U+1806, MONGOLIAN FOUR DOT
        // PUNCTUATION sometimes used as a visual underscore) must be
        // rejected — only ASCII underscore is legal.
        assert_eq!(parse_opencode_session_id("ses_a\u{1806}b"), None);
    }

    #[test]
    fn parse_session_id_for_provider_dispatches_opencode_to_ses_validator() {
        // The dispatcher used by `attention::hook_session_id` routes
        // OpenCode through the new gate and every other provider
        // through the legacy UUID gate. A regression that hard-codes the
        // UUID validator for all providers would silently drop every
        // OpenCode plugin POST (the exact symptom from issue #1294).
        //
        // Base62 casing is preserved: OpenCode's CLI looks ids up
        // case-sensitively, so a "helpful" lower-case fold on the way
        // through the dispatcher would break `AgentProvider::resume_args`.
        let opencode_id = "ses_fc52ccfb9ffek1jl23ZwpRuSP7";
        assert_eq!(
            parse_session_id_for_provider("opencode", opencode_id).as_deref(),
            Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
        );
        // Uppercase-prefixed input still routes through OpenCode's
        // gate and the prefix is normalised to lowercase on output.
        assert_eq!(
            parse_session_id_for_provider("opencode", "SES_abcDEF").as_deref(),
            Some("ses_abcDEF"),
        );
        // UUID-shaped input to an OpenCode node — must be rejected, NOT
        // silently accepted (would corrupt `--session` argv on resume).
        assert_eq!(
            parse_session_id_for_provider("opencode", "550e8400-e29b-41d4-a716-446655440000"),
            None,
        );
        // Other providers fall through to the UUID validator (their
        // contract; a regression that routed them through the OpenCode
        // gate would break Claude/Codex/AGY/Grok/Cursor captures).
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            parse_session_id_for_provider("claude", uuid).as_deref(),
            Some(uuid)
        );
        assert_eq!(
            parse_session_id_for_provider("codex", uuid).as_deref(),
            Some(uuid)
        );
        // An unknown provider defaults to the UUID validator — same as
        // pre-#1294 behaviour, so a future harness adopting UUIDs works
        // without changes here.
        assert_eq!(
            parse_session_id_for_provider("", uuid).as_deref(),
            Some(uuid)
        );
    }

    #[test]
    fn extract_token_from_single_cookie() {
        let headers = "Host: localhost\r\nCookie: bm_session=abc123\r\n";
        assert_eq!(extract_token_from_cookies(headers), Some("abc123".into()));
    }

    #[test]
    fn extract_token_from_cookie_with_others() {
        let headers = "Host: localhost\r\nCookie: foo=bar; bm_session=secret; baz=qux\r\n";
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
        let header = session_cookie_header("deadbeef", false);
        assert!(header.starts_with("Set-Cookie: bm_session=deadbeef"));
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("SameSite=Lax"));
        assert!(header.contains("Path=/"));
    }

    #[test]
    fn session_cookie_omits_secure_on_plain_loopback() {
        // The loopback listener is always plain HTTP (issue #501); a `Secure`
        // cookie there would be silently dropped by the browser, breaking the
        // local login. So a non-TLS request must NOT get `Secure`.
        let header = session_cookie_header("deadbeef", false);
        assert!(!header.contains("Secure"), "got: {header}");
    }

    #[test]
    fn session_cookie_sets_secure_over_tls() {
        // Over the LAN HTTPS path the device-token cookie must be `Secure` so it
        // can never be replayed over plaintext (issue #553).
        let header = session_cookie_header("deadbeef", true);
        assert!(header.contains("; Secure"), "got: {header}");
        // The other hardening attributes survive alongside it.
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
        assert_eq!(
            extract_header_value(headers, "Sec-WebSocket-Key"),
            Some("abc123")
        );
        assert_eq!(
            extract_header_value(headers, "sec-websocket-key"),
            Some("abc123")
        );
        assert_eq!(extract_header_value(headers, "Host"), Some("localhost"));
        assert_eq!(extract_header_value(headers, "Missing"), None);
    }

    // --- read_body_with_cap / read_body_or_send_error ------------------------
    //
    // These pin the two paths a test can reliably exercise: TooLarge
    // (advertised content_length > max_bytes) and ReadFailed (premature EOF
    // before content_length bytes arrive). The happy-path read and the
    // empty-body case round it out. The TimedOut path needs a stalled socket
    // and a 60 s+ test wall-clock; the `BODY_READ_TIMEOUT` constant itself is
    // the single audit surface for that branch.

    #[tokio::test]
    async fn read_body_with_cap_rejects_oversize_with_too_large() {
        // 11-byte body with a 10-byte cap is the smallest failing case. The
        // server side only needs to accept — the cap check fires before any
        // read, so the server doesn't have to send anything; we drop it once
        // accepted and let the helper return TooLarge.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Accept the connection and drop both halves — the client never
            // tries to read, so this closes the socket.
            let _ = listener.accept().await;
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut lines = BufStream::new(MaybeTls::Plain(stream));
        let result = read_body_with_cap(&mut lines, 11, 10).await;
        assert_eq!(result, Err(ReadBodyError::TooLarge));
    }

    #[tokio::test]
    async fn read_body_with_cap_returns_read_failed_on_early_eof() {
        // Server sends 5 bytes then drops the connection — read_exact waits for
        // 10 and gets EOF instead, which the helper maps to ReadFailed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncWriteExt;
            let mut s = stream;
            s.write_all(b"hello").await.unwrap();
            s.shutdown().await.unwrap();
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut lines = BufStream::new(MaybeTls::Plain(stream));
        let result = read_body_with_cap(&mut lines, 10, 1024).await;
        assert_eq!(result, Err(ReadBodyError::ReadFailed));
    }

    #[tokio::test]
    async fn read_body_with_cap_reads_exact_bytes_on_happy_path() {
        // Server sends exactly content_length bytes; helper returns them.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncWriteExt;
            let mut s = stream;
            s.write_all(b"abc123").await.unwrap();
            s.shutdown().await.unwrap();
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut lines = BufStream::new(MaybeTls::Plain(stream));
        let buf = read_body_with_cap(&mut lines, 6, 1024).await.unwrap();
        assert_eq!(buf, b"abc123");
    }

    #[tokio::test]
    async fn read_body_with_cap_handles_zero_length_body() {
        // Content-Length: 0 — the read fast-paths to an empty Vec without
        // touching the wire. Tests the `if content_length > 0` branch.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Drop the listener without accepting — proves we never read.
            drop(listener);
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut lines = BufStream::new(MaybeTls::Plain(stream));
        let buf = read_body_with_cap(&mut lines, 0, 1024).await.unwrap();
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn read_body_or_send_error_writes_413_on_too_large() {
        // The wrapper must translate TooLarge into a 413 with a JSON body so
        // the SPA can surface it; the call site then returns None.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 256];
            use tokio::io::AsyncReadExt;
            let mut s = stream;
            // Read the full response the server writes back.
            let _ = s.read(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut lines = BufStream::new(MaybeTls::Plain(stream));
        let result = read_body_or_send_error(&mut lines, 1024, 100).await;
        assert_eq!(result, None, "wrapper must return None on TooLarge");
        drop(lines);

        let response = server.await.unwrap();
        assert!(
            response.starts_with("HTTP/1.1 413"),
            "wrapper must write a 413; got: {response:?}"
        );
        assert!(
            response.contains("Body too large"),
            "413 body must explain the cap; got: {response:?}"
        );
    }

    #[tokio::test]
    async fn read_body_or_send_error_writes_400_on_read_failed() {
        // Server sends fewer bytes than advertised → ReadFailed → 400.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut s = stream;
            s.write_all(b"hi").await.unwrap();
            s.shutdown().await.unwrap();
            let mut buf = vec![0u8; 256];
            let _ = s.read(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut lines = BufStream::new(MaybeTls::Plain(stream));
        let result = read_body_or_send_error(&mut lines, 10, 1024).await;
        assert_eq!(result, None);
        drop(lines);

        let response = server.await.unwrap();
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "wrapper must write a 400 on ReadFailed; got: {response:?}"
        );
    }
}
