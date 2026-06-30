//! Static asset serving for the mobile SPA.
//!
//! Single source: `dist/mobile/` built by `vite build --mode mobile` and
//! embedded via `rust-embed` (read from disk in debug, compiled-in in
//! release). The legacy `mobile_app.html` is gone — `GET /` now returns
//! the SPA shell directly so the existing QR code at `http://lan-ip:1992/`
//! keeps working unchanged.

use crate::http::{MaybeTls, request};

#[derive(rust_embed::Embed)]
#[folder = "../dist/mobile"]
struct MobileAssets;

/// Serve the SPA shell at `/` (or `/v2` for backward compat).
/// `extra_header` lets the dispatcher inject a `Set-Cookie` line on the
/// initial token-bearing request; each extra line MUST end with `\r\n`.
pub async fn serve_spa_shell(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    extra_header: Option<&str>,
) -> std::io::Result<()> {
    let Some(file) = MobileAssets::get("index.html") else {
        let body = "Not found — run `npm run build:mobile` to populate the mobile bundle.";
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        return request::write_full(lines, response.as_bytes()).await;
    };
    let extra = extra_header.unwrap_or("");
    // Inject a tiny error-catcher so JS errors from the SPA land in the dev
    // log instead of disappearing on the phone's screen. `__debug/log` is
    // matched in `handle_connection` (see the diagnostics section) — it
    // 200s immediately and writes the body to the dev log so we can see
    // exactly what threw on a black-screen page. Inserted before the
    // module script so it captures errors during the SPA's boot.
    let debug_shim = r#"<script>
window.addEventListener('error', function (e) {
  try {
    fetch('/__debug/log', {
      method: 'POST',
      headers: {'Content-Type':'application/json'},
      body: JSON.stringify({kind:'error',msg:e.message,src:e.filename,line:e.lineno,col:e.colno,stack:e.error&&e.error.stack||''})
    });
  } catch (_) {}
});
window.addEventListener('unhandledrejection', function (e) {
  try {
    fetch('/__debug/log', {
      method: 'POST',
      headers: {'Content-Type':'application/json'},
      body: JSON.stringify({kind:'promise',reason:String(e.reason),stack:(e.reason&&e.reason.stack)||''})
    });
  } catch (_) {}
});
</script>"#;
    let body = format!(
        "{}<!--buildmesh-debug-shim-->{}",
        String::from_utf8_lossy(&file.data).replace("</head>", &format!("{}</head>", debug_shim)),
        ""
    );
    // Compute length off the rendered string rather than `file.data.len()` —
    // we just appended the shim to the body, so the Content-Length header
    // must match what we actually write.
    let body_bytes = body.as_bytes();
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-cache\r\n{}\r\n",
        body_bytes.len(),
        extra
    );
    // Single write+flush for headers + body. Without the flush, the last
    // partial chunk of the body can sit in the BufStream buffer when the
    // function returns — the connection drops before it reaches the wire,
    // and Chrome surfaces that as ERR_CONTENT_LENGTH_MISMATCH on the
    // shell HTML too.
    let mut combined = Vec::with_capacity(headers.len() + body_bytes.len());
    combined.extend_from_slice(headers.as_bytes());
    combined.extend_from_slice(body_bytes);
    request::write_full(lines, &combined).await
}

/// Serve a single bundled asset by request path. `path_without_query` is
/// the full path like `/assets/index-abc.js`; we strip the leading slash
/// and look it up in the embedded asset list. `range_header` is the
/// optional value of the `Range:` request header — when present and
/// well-formed, we honour it with `206 Partial Content` so module
/// scripts can stream-parse without tripping Chrome's
/// `ERR_CONTENT_LENGTH_MISMATCH` (the issue this was added for).
pub async fn serve_asset(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    path_without_query: &str,
    range_header: Option<&str>,
) -> std::io::Result<()> {
    let relative = path_without_query.trim_start_matches('/');
    let Some(file) = MobileAssets::get(relative) else {
        let body = "Asset not found.";
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        return request::write_full(lines, response.as_bytes()).await;
    };
    let mime = mime_for(relative);
    let total = file.data.len();

    // Single satisfiable range → 206 Partial Content. We refuse
    // multi-range rather than emit multipart: none of our callers
    // (Vite-emitted JS/CSS/PNG/font chunks) need it, and multipart
    // would re-introduce the same Content-Length trap we're fixing.
    // Suffix (`bytes=-N`) and open-ended (`bytes=N-`) forms both
    // expand against `total`; unsatisfiable ranges get `416` so the
    // client can fall back to a full GET.
    if let Some(spec) = range_header.and_then(parse_bytes_range) {
        let Some((start, end)) = resolve_range(spec, total) else {
            let resp = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\n\
                 Content-Range: bytes */{total}\r\n\
                 Content-Length: 0\r\n\r\n"
            );
            return request::write_full(lines, resp.as_bytes()).await;
        };
        let slice = &file.data[start..=end];
        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\n\
             Content-Type: {mime}\r\n\
             Content-Length: {len}\r\n\
             Content-Range: bytes {start}-{end}/{total}\r\n\
             Accept-Ranges: bytes\r\n\
             Cache-Control: public, max-age=31536000, immutable\r\n\r\n",
            len = slice.len(),
        );
        // Coalesce headers + body into one write so a single flush
        // covers both — eliminates the last-partial-chunk race that
        // produced ERR_CONTENT_LENGTH_MISMATCH on Android Chrome.
        let mut combined = Vec::with_capacity(headers.len() + slice.len());
        combined.extend_from_slice(headers.as_bytes());
        combined.extend_from_slice(slice);
        return request::write_full(lines, &combined).await;
    }

    let headers = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {mime}\r\n\
         Content-Length: {total}\r\n\
         Accept-Ranges: bytes\r\n\
         Cache-Control: public, max-age=31536000, immutable\r\n\r\n"
    );
    // Same coalesce+flush pattern as the 206 branch above.
    let mut combined = Vec::with_capacity(headers.len() + total);
    combined.extend_from_slice(headers.as_bytes());
    combined.extend_from_slice(&file.data);
    request::write_full(lines, &combined).await
}

/// One of the three forms of a `Range: bytes=...` value, before
/// resolution against the actual total size. Both open-ended
/// `bytes=N-` and suffix `bytes=-N` need the total to materialise
/// into concrete `(start, end)` indices.
#[derive(Debug, PartialEq, Eq)]
enum BytesRange {
    Closed { start: usize, end: usize },
    OpenEnded { start: usize },
    Suffix { last_n: usize },
}

fn parse_bytes_range(header: &str) -> Option<BytesRange> {
    let rest = header.trim().strip_prefix("bytes=")?;
    // Multi-range — refuse.
    if rest.contains(',') {
        return None;
    }
    let (s, e) = rest.split_once('-')?;
    if s.is_empty() && e.is_empty() {
        return None; // `-` alone is malformed
    }
    if s.is_empty() {
        let n: usize = e.parse().ok()?;
        Some(BytesRange::Suffix { last_n: n })
    } else if e.is_empty() {
        let start: usize = s.parse().ok()?;
        Some(BytesRange::OpenEnded { start })
    } else {
        let start: usize = s.parse().ok()?;
        let end: usize = e.parse().ok()?;
        if start > end {
            return None;
        }
        Some(BytesRange::Closed { start, end })
    }
}

/// Materialise a `BytesRange` against an actual total size. Returns
/// `None` when the range is unsatisfiable (start >= total) or empty
/// after suffix resolution.
fn resolve_range(spec: BytesRange, total: usize) -> Option<(usize, usize)> {
    if total == 0 {
        return None;
    }
    let (start, end) = match spec {
        BytesRange::Closed { start, end } => (start, end.min(total - 1)),
        BytesRange::OpenEnded { start } => (start, total - 1),
        BytesRange::Suffix { last_n } => {
            if last_n == 0 || last_n > total {
                return None;
            }
            (total - last_n, total - 1)
        }
    };
    if start >= total || start > end {
        return None;
    }
    Some((start, end))
}

fn mime_for(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if lower.ends_with(".js") || lower.ends_with(".mjs") {
        "application/javascript; charset=utf-8"
    } else if lower.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if lower.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".woff2") {
        "font/woff2"
    } else if lower.ends_with(".woff") {
        "font/woff"
    } else if lower.ends_with(".map") {
        "application/json"
    } else if lower.ends_with(".txt") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_assets_include_built_index_html() {
        assert!(
            MobileAssets::get("index.html").is_some(),
            "dist/mobile/index.html missing — run `npm run build:mobile`"
        );
    }

    #[test]
    fn mime_for_known_extensions() {
        assert!(mime_for("index.html").starts_with("text/html"));
        assert!(mime_for("app.js").starts_with("application/javascript"));
        assert!(mime_for("style.css").starts_with("text/css"));
        assert!(mime_for("logo.svg") == "image/svg+xml");
        assert!(mime_for("unknown.xyz") == "application/octet-stream");
    }

    /// Range parser — the unit that decides whether a Range header is
    /// well-formed. Each test pins one form of the documented RFC 9110
    /// grammar; combined they lock the rejection paths so a future
    /// tweak doesn't quietly widen what's accepted.
    #[test]
    fn parse_bytes_range_closed() {
        assert_eq!(
            parse_bytes_range("bytes=0-1023"),
            Some(BytesRange::Closed { start: 0, end: 1023 })
        );
        assert_eq!(
            parse_bytes_range("bytes=100-200"),
            Some(BytesRange::Closed { start: 100, end: 200 })
        );
    }

    #[test]
    fn parse_bytes_range_open_ended() {
        assert_eq!(
            parse_bytes_range("bytes=1024-"),
            Some(BytesRange::OpenEnded { start: 1024 })
        );
    }

    #[test]
    fn parse_bytes_range_suffix() {
        assert_eq!(
            parse_bytes_range("bytes=-512"),
            Some(BytesRange::Suffix { last_n: 512 })
        );
    }

    #[test]
    fn parse_bytes_range_rejects_garbage() {
        assert_eq!(parse_bytes_range(""), None);
        assert_eq!(parse_bytes_range("bits=0-99"), None);
        assert_eq!(parse_bytes_range("bytes="), None);
        assert_eq!(parse_bytes_range("bytes=-"), None);
        assert_eq!(parse_bytes_range("bytes=0-99,200-299"), None); // multi-range
        assert_eq!(parse_bytes_range("bytes=abc-def"), None);
        assert_eq!(parse_bytes_range("bytes=200-100"), None); // start > end
    }

    /// Range resolver — materialises the symbolic forms against the
    /// actual total size. Pins the off-by-one and unsatisfiable paths
    /// so a future change can't regress the 416 response.
    #[test]
    fn resolve_range_closed_truncates_overshoot() {
        assert_eq!(
            resolve_range(BytesRange::Closed { start: 0, end: 9999 }, 100),
            Some((0, 99))
        );
    }

    #[test]
    fn resolve_range_open_ended_lands_on_last_byte() {
        assert_eq!(
            resolve_range(BytesRange::OpenEnded { start: 50 }, 100),
            Some((50, 99))
        );
    }

    #[test]
    fn resolve_range_suffix_counts_back_from_total() {
        assert_eq!(
            resolve_range(BytesRange::Suffix { last_n: 10 }, 100),
            Some((90, 99))
        );
        assert_eq!(
            resolve_range(BytesRange::Suffix { last_n: 100 }, 100),
            Some((0, 99))
        );
    }

    #[test]
    fn resolve_range_unsatisfiable_yields_none() {
        // Past the end
        assert_eq!(
            resolve_range(BytesRange::OpenEnded { start: 100 }, 100),
            None
        );
        // Suffix longer than the body
        assert_eq!(
            resolve_range(BytesRange::Suffix { last_n: 200 }, 100),
            None
        );
        // Empty body
        assert_eq!(
            resolve_range(BytesRange::Closed { start: 0, end: 0 }, 0),
            None
        );
    }
}
