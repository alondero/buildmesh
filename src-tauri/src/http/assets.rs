//! Static asset serving for the mobile SPA.
//!
//! Single source: `dist/mobile/` built by `vite build --mode mobile` and
//! embedded via `rust-embed` (read from disk in debug, compiled-in in
//! release). The legacy `mobile_app.html` is gone — `GET /` now returns
//! the SPA shell directly so the existing QR code at `http://lan-ip:1992/`
//! keeps working unchanged.

use tokio::io::AsyncWriteExt;
use crate::http::MaybeTls;

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
        return lines.get_mut().write_all(response.as_bytes()).await;
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
    let stream = lines.get_mut();
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body_bytes).await
}

/// Serve a single bundled asset by request path. `path_without_query` is
/// the full path like `/assets/index-abc.js`; we strip the leading slash
/// and look it up in the embedded asset list.
pub async fn serve_asset(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    path_without_query: &str,
) -> std::io::Result<()> {
    let relative = path_without_query.trim_start_matches('/');
    let Some(file) = MobileAssets::get(relative) else {
        let body = "Asset not found.";
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        return lines.get_mut().write_all(response.as_bytes()).await;
    };
    let mime = mime_for(relative);
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: public, max-age=31536000, immutable\r\n\r\n",
        mime,
        file.data.len()
    );
    let stream = lines.get_mut();
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&file.data).await
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
}
