//! Static asset serving for the mobile SPA.
//!
//! Single source: `dist/mobile/` built by `vite build --mode mobile` and
//! embedded via `rust-embed` (read from disk in debug, compiled-in in
//! release). The legacy `mobile_app.html` is gone — `GET /` now returns
//! the SPA shell directly so the existing QR code at `http://lan-ip:1992/`
//! keeps working unchanged.

use crate::http::{request, MaybeTls};

#[derive(rust_embed::Embed)]
#[folder = "../dist/mobile"]
struct MobileAssets;

/// Which insertion strategy `inject_debug_shim` ended up using. Surfaced as
/// part of the function return so tests can pin the preferred insertion
/// point (and so a future observability hook can report when only the
/// fallback ran — the signal that a bundler change altered the HTML shell).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum InsertionStrategy {
    /// Inserted just before `</head>` — the original behaviour, kept as the
    /// first choice because it puts the shim before every module script and
    /// captures errors during SPA boot.
    BeforeHead,
    /// `</head>` was absent (HTML5 permits omission before `<body>`); the
    /// shim is inserted just before `<body …>`.
    BeforeBody,
    /// Neither `</head>` nor `<body>` was present; the shim is inserted
    /// right after the `<!doctype …>` declaration so the document still
    /// parses as HTML5.
    AfterDoctype,
    /// No recognisable HTML anchor at all — the shim is prepended to the
    /// body. The caller logs an error so the operator can investigate.
    PrependedFallback,
}

/// Inject `shim` into `html`, returning the modified string and the strategy
/// used. Pure so the four code paths are individually unit-testable; the
/// caller in `serve_spa_shell` is the only thing that decides what to do
/// with the strategy (today: log a warning for the fallback path).
///
/// The returned string also carries the `<!--buildmesh-debug-shim-->` marker
/// comment immediately after the `<script>` block, so a future grep /
/// scraper / regression test can confirm "the shim is present in the served
/// body" by string-match alone — no DOM parser needed.
///
/// Precedence:
/// 1. **`</head>`** (case-insensitive) — preferred; runs the shim before any
///    module script, which is what the original `String::replace` attempted.
/// 2. **`<body …>`** (case-insensitive) — when the optional closing tag was
///    dropped by an aggressive minifier (HTML5 allows omission of `</head>`
///    when followed by `<body>`).
/// 3. **`<!doctype …>`** (case-insensitive) — pathological shells with no
///    `<head>` and no `<body>` still parse as HTML5 with the shim right
///    after the doctype.
/// 4. **Prepend** — last resort; the served body still carries the marker so
///    a developer gets SOME signal rather than a silent blank screen.
fn inject_debug_shim(html: &str, shim: &str) -> (String, InsertionStrategy) {
    const MARKER: &str = "<!--buildmesh-debug-shim-->";
    let marked_shim = format!("{shim}{MARKER}");
    // `<head>` is matched by its CLOSING tag because that's the deterministic
    // position right before the document's body — the open `<head>` tag is
    // the one followed by content, but its close is the only thing that
    // guarantees the shim runs BEFORE every module script in the SPA.
    if let Some(pos) = find_case_insensitive(html, "</head>") {
        return (
            splice(html, pos, &format!("{marked_shim}</head>")),
            InsertionStrategy::BeforeHead,
        );
    }
    if let Some(pos) = find_case_insensitive(html, "<body") {
        return (
            splice(html, pos, &format!("{marked_shim}<body")),
            InsertionStrategy::BeforeBody,
        );
    }
    if let Some(pos) = find_case_insensitive(html, "<!doctype") {
        // Skip past the doctype declaration to insert AFTER it; otherwise the
        // browser sees the shim before `<!doctype>` and quirks-mode kicks in.
        let after = end_of_doctype_declaration(html, pos);
        return (
            splice(html, after, &marked_shim),
            InsertionStrategy::AfterDoctype,
        );
    }
    (
        format!("{marked_shim}{html}"),
        InsertionStrategy::PrependedFallback,
    )
}

/// Return the byte index of the first occurrence of `needle` in `haystack`,
/// case-insensitive. Returns `None` if not found. We avoid `to_lowercase`
/// allocations by walking ASCII byte-equality directly — the HTML we serve
/// is ASCII for tag names (the only place case matters for the anchors
/// `</head>`, `<body`, `<!doctype`).
fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let first = n[0];
    for i in 0..=h.len() - n.len() {
        if h[i].eq_ignore_ascii_case(&first) && h[i + 1..i + n.len()].eq_ignore_ascii_case(&n[1..])
        {
            return Some(i);
        }
    }
    None
}

/// Splice `injected` into `s` at byte position `pos`, returning the new
/// string. `injected` replaces nothing — it's pure insertion — so callers
/// that need to anchor on a specific substring must include that substring
/// in `injected` (e.g. `format!("{shim}</head>")` rather than just `shim`).
fn splice(s: &str, pos: usize, injected: &str) -> String {
    let mut out = String::with_capacity(s.len() + injected.len());
    out.push_str(&s[..pos]);
    out.push_str(injected);
    out.push_str(&s[pos..]);
    out
}

/// Given that `<!doctype` was found at `start`, return the byte index just
/// past the closing `>` of the doctype declaration. We scan for the first
/// `>` after `start`, ignoring any `>` that appears inside a quoted
/// attribute (e.g. `<!doctype html SYSTEM "url">`). Doctype syntax is small
/// enough that a linear scan is the right tool — no parser needed.
fn end_of_doctype_declaration(html: &str, start: usize) -> usize {
    let bytes = html.as_bytes();
    let mut in_quote: Option<u8> = None;
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_quote {
            if b == q {
                in_quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            in_quote = Some(b);
        } else if b == b'>' {
            return i + 1;
        }
        i += 1;
    }
    // Malformed: no `>` found. Insert at the end rather than panicking.
    html.len()
}

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
    // Issue #638: the previous `String::replace("</head>", …)` was
    // case-sensitive and silently no-op'd on a minifier that uppercased the
    // tag or dropped the optional closing `</head>`. `inject_debug_shim`
    // walks a precedence ladder (`</head>` → `<body` → `<!doctype` →
    // prepend) so the marker lands in the served body regardless of the
    // bundler's output, and the fallback case logs an error so the
    // operator gets a signal instead of a silent blank screen.
    let html = String::from_utf8_lossy(&file.data);
    let (body, strategy) = inject_debug_shim(&html, debug_shim);
    if strategy == InsertionStrategy::PrependedFallback {
        // Loud diagnostic — the served shell has no recognisable HTML
        // anchor, which means a bundler has produced something the
        // shim insertion ladder can't place precisely. The marker is
        // still in the body (prepended), but the dev should know the
        // contract drifted. Without this log, the only symptom is a
        // blank screen on the phone and no breadcrumb back to the
        // root cause (issue #638).
        tracing::error!(
            target: "buildmesh_lib::diagnostics",
            "SPA shell has no </head>, <body, or <!doctype anchor — debug shim prepended. Inspect the built dist/mobile/index.html."
        );
    }
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
            Some(BytesRange::Closed {
                start: 0,
                end: 1023
            })
        );
        assert_eq!(
            parse_bytes_range("bytes=100-200"),
            Some(BytesRange::Closed {
                start: 100,
                end: 200
            })
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

    /// Debug shim insertion — the `<!--buildmesh-debug-shim-->` marker MUST
    /// appear in the served SPA shell regardless of how the bundled
    /// `index.html` was shaped. Issue #638: the previous implementation used
    /// `String::replace("</head>", …)` (case-sensitive, exact-substring) so
    /// any minifier that uppercased the tag or dropped the optional
    /// closing `</head>` (HTML5 allows it before `<body>`) silently
    /// no-op'd the injection — a developer staring at a blank phone screen
    /// had no diagnostic in the dev log. These tests pin the contract:
    /// the marker is present and the shim script is positioned before
    /// any module script so it can capture boot errors.
    const DEBUG_SHIM_MARKER: &str = "<!--buildmesh-debug-shim-->";
    const SHIM_SCRIPT_TAG: &str = "<script>\nwindow.addEventListener('error'";

    #[test]
    fn inject_shim_into_well_formed_html_inserts_before_close_head() {
        let html = "<!doctype html><html><head><title>x</title></head><body></body></html>";
        let (out, strategy) = inject_debug_shim(html, SHIM_SCRIPT_TAG);
        assert_eq!(strategy, InsertionStrategy::BeforeHead);
        assert!(out.contains(DEBUG_SHIM_MARKER), "marker missing: {out:?}");
        // The shim must run BEFORE any module script so it captures boot errors.
        assert!(
            out.find(SHIM_SCRIPT_TAG).unwrap() < out.find("</head>").unwrap(),
            "shim must precede </head>, got: {out:?}"
        );
    }

    #[test]
    fn inject_shim_is_case_insensitive_on_close_head() {
        // Minifier upper-cased the closing tag — the previous case-sensitive
        // `String::replace` silently no-op'd here.
        let html = "<!doctype html><html><head><title>x</title></HEAD><body></body></html>";
        let (out, strategy) = inject_debug_shim(html, SHIM_SCRIPT_TAG);
        assert_eq!(strategy, InsertionStrategy::BeforeHead);
        assert!(out.contains(DEBUG_SHIM_MARKER), "marker missing: {out:?}");
    }

    #[test]
    fn inject_shim_falls_back_to_before_body_when_head_close_missing() {
        // HTML5 lets the author omit `</head>` when it's immediately followed
        // by `<body>`. Aggressive minifiers exploit this — the shim must
        // still inject, just before `<body>`.
        let html =
            "<!doctype html><html><head><title>x</title><body><div id=\"r\"></div></body></html>";
        let (out, strategy) = inject_debug_shim(html, SHIM_SCRIPT_TAG);
        assert_eq!(strategy, InsertionStrategy::BeforeBody);
        assert!(out.contains(DEBUG_SHIM_MARKER), "marker missing: {out:?}");
        assert!(
            out.find(SHIM_SCRIPT_TAG).unwrap() < out.find("<body").unwrap(),
            "shim must precede <body>, got: {out:?}"
        );
    }

    #[test]
    fn inject_shim_falls_back_after_doctype_when_neither_head_nor_body() {
        // Pathological: a custom preamble with no `<head>` or `<body>` at
        // all. Inject after the doctype declaration so the document still
        // parses as HTML5.
        let html = "<!doctype html><html><div>only this</div></html>";
        let (out, strategy) = inject_debug_shim(html, SHIM_SCRIPT_TAG);
        assert_eq!(strategy, InsertionStrategy::AfterDoctype);
        assert!(out.contains(DEBUG_SHIM_MARKER), "marker missing: {out:?}");
    }

    #[test]
    fn inject_shim_prepends_when_no_anchor_at_all() {
        // Last-resort: a fragment with no recognisable HTML anchor. The
        // shim is prepended so the served body is never empty. The
        // production caller logs an error in this branch — see the comment
        // in `inject_debug_shim`.
        let html = "fragment without any html tags";
        let (out, strategy) = inject_debug_shim(html, SHIM_SCRIPT_TAG);
        assert_eq!(strategy, InsertionStrategy::PrependedFallback);
        assert!(
            out.starts_with(SHIM_SCRIPT_TAG) || out.starts_with(DEBUG_SHIM_MARKER),
            "shim must be at the start of the output, got: {out:?}"
        );
    }

    #[test]
    fn inject_shim_into_real_built_index_html_contains_marker() {
        // Pin against the actual built artefact — the file Vite emits today.
        // If a future bundler change alters the structure (e.g. uppercases
        // the tag), this test still passes so long as the marker lands;
        // the *strategy* assertion above pins the preferred insertion
        // point so regressions show up as a strategy change, not as a
        // silent no-op.
        let Some(file) = MobileAssets::get("index.html") else {
            // The `mobile_assets_include_built_index_html` test below
            // already covers the missing-file case — skip this assertion
            // in that scenario rather than double-failing.
            return;
        };
        let html = String::from_utf8_lossy(&file.data);
        let (out, _strategy) = inject_debug_shim(&html, SHIM_SCRIPT_TAG);
        assert!(
            out.contains(DEBUG_SHIM_MARKER),
            "shim marker missing from served shell — debug logging is silently broken"
        );
    }

    /// Range resolver — materialises the symbolic forms against the
    /// actual total size. Pins the off-by-one and unsatisfiable paths
    /// so a future change can't regress the 416 response.
    #[test]
    fn resolve_range_closed_truncates_overshoot() {
        assert_eq!(
            resolve_range(
                BytesRange::Closed {
                    start: 0,
                    end: 9999
                },
                100
            ),
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
        assert_eq!(resolve_range(BytesRange::Suffix { last_n: 200 }, 100), None);
        // Empty body
        assert_eq!(
            resolve_range(BytesRange::Closed { start: 0, end: 0 }, 0),
            None
        );
    }
}
