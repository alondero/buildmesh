//! Static asset serving.
//!
//! Two surfaces coexist during the mobile-UI refactor:
//!   * `GET /` — legacy single-file mobile HTML (`mobile_app.html`), still
//!     production until stage 7. Embedded via `include_str!`.
//!   * `GET /v2[/...]` — the new buildable mobile SPA, output by Vite into
//!     `dist/mobile/` and embedded via `rust-embed`. Empty until stage 3
//!     starts adding screens; in dev (debug builds) files are read from
//!     disk so frontend iteration doesn't require recompiling Rust.

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

const MOBILE_APP_HTML: &str = include_str!("../mobile_app.html");

#[derive(rust_embed::Embed)]
#[folder = "../dist/mobile"]
struct MobileV2Assets;

pub async fn serve_mobile_root(
    lines: &mut tokio::io::BufStream<TcpStream>,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        MOBILE_APP_HTML.len(),
        MOBILE_APP_HTML
    );
    lines.get_mut().write_all(response.as_bytes()).await
}

/// Serve the new mobile SPA at `/v2`. `path_without_query` is the full request
/// path (e.g. `/v2`, `/v2/`, `/v2/assets/foo.js`); we strip the prefix and
/// look it up against the embedded asset list. Empty path → `index.html`.
///
/// `extra_header` lets the dispatcher inject a `Set-Cookie` line when the
/// caller authenticated via `?token=` so subsequent fetches use the cookie.
/// Each extra line MUST end with `\r\n`.
pub async fn serve_v2(
    lines: &mut tokio::io::BufStream<TcpStream>,
    path_without_query: &str,
    extra_header: Option<&str>,
) -> std::io::Result<()> {
    let relative = path_without_query
        .strip_prefix("/v2")
        .unwrap_or("")
        .trim_start_matches('/');
    let asset_path = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };

    let Some(file) = MobileV2Assets::get(asset_path) else {
        let body = "Not found — run `npm run build:mobile` to populate the v2 bundle.";
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        return lines.get_mut().write_all(response.as_bytes()).await;
    };

    let mime = mime_for(asset_path);
    let extra = extra_header.unwrap_or("");
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\n{}\r\n",
        mime,
        file.data.len(),
        extra
    );
    let stream = lines.get_mut();
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&file.data).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_v2_assets_includes_built_index_html() {
        // Requires `npm run build:mobile` to have run. The build.rs guard
        // creates the directory, so cargo build won't fail on a fresh
        // checkout — but the asset list will be empty and this test will
        // tell you to run the mobile build.
        assert!(
            MobileV2Assets::get("index.html").is_some(),
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
