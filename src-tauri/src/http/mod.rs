//! Embedded HTTP/WebSocket server for mobile remote access.
//!
//! Serves the mobile web app and handles WebSocket terminal connections
//! for remote access from a phone on the same network.

pub mod assets;
pub mod events;
pub mod request;
pub mod routes;
pub mod ws;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use parking_lot::RwLock;
use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

// --- App handle for emitting Tauri events from HTTP handlers ---

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

pub(crate) fn app_handle() -> Option<&'static tauri::AppHandle> {
    APP_HANDLE.get()
}

const HTTP_PORT_START: u16 = 1992;
const HTTP_PORT_END: u16 = 1994;

/// Default/expected port for the attention webhook hook.
/// The actual server may bind 1993 or 1994 if 1992 is taken; the hook will
/// silently no-op on the wrong port since `|| true` is appended.
pub const HTTP_PORT_DEFAULT: u16 = HTTP_PORT_START;

/// Port offset applied to every server so a dev build (identifier `*.dev`) can
/// run side-by-side with the stable hub without contending on 1991/1992.
/// Stable → 0, dev → 1000 (test 2991, HTTP 2992-2994).
pub fn port_offset(identifier: &str) -> u16 {
    if identifier.ends_with(".dev") { 1000 } else { 0 }
}

/// The HTTP port the server actually bound, published once `start_http_server`
/// succeeds. Agent spawning reads this (via `current_http_port`) so the
/// attention webhook points at *this* instance — both for the dev-profile
/// offset and the 1992→1993→1994 fallback. Defaults to `HTTP_PORT_DEFAULT`
/// before bind (and in unit tests where no server runs).
static RESOLVED_HTTP_PORT: AtomicU16 = AtomicU16::new(HTTP_PORT_DEFAULT);

/// The HTTP port this instance bound, for callers that must reach it (agent
/// attention hooks). Returns `HTTP_PORT_DEFAULT` until the server binds.
pub fn current_http_port() -> u16 {
    RESOLVED_HTTP_PORT.load(Ordering::SeqCst)
}

// --- Snapshot request/response (used by ws.rs to seed initial terminal state) ---

static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

type SnapshotSenders = HashMap<String, oneshot::Sender<String>>;
static SNAPSHOT_REQUESTS: OnceLock<Arc<RwLock<SnapshotSenders>>> = OnceLock::new();

fn get_snapshot_requests() -> &'static Arc<RwLock<SnapshotSenders>> {
    SNAPSHOT_REQUESTS.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

/// Called by the Tauri command when the frontend responds with a serialized terminal snapshot.
pub fn fulfill_snapshot(request_id: &str, data: String) {
    let mut requests = get_snapshot_requests().write();
    if let Some(tx) = requests.remove(request_id) {
        if tx.send(data).is_err() {
            tracing::warn!(
                "fulfill_snapshot: receiver already dropped for {}",
                request_id
            );
        }
    }
}

pub(crate) async fn request_terminal_snapshot(
    app: &tauri::AppHandle,
    node_id: i64,
) -> Option<String> {
    let seq = SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let request_id = format!("snap-{}-{}", node_id, seq);

    let (tx, rx) = oneshot::channel();
    {
        let mut requests = get_snapshot_requests().write();
        requests.insert(request_id.clone(), tx);
    }

    let _ = app.emit(
        "serialize-terminal-request",
        serde_json::json!({
            "node_id": node_id,
            "request_id": request_id,
        }),
    );

    match tokio::time::timeout(std::time::Duration::from_millis(500), rx).await {
        Ok(Ok(data)) => Some(data),
        _ => {
            let mut requests = get_snapshot_requests().write();
            requests.remove(&request_id);
            None
        }
    }
}

/// Start the HTTP server, trying ports 1992→1993→1994 until one binds.
/// Emits a `remote-access-port` event with the actual port used so the
/// QR code modal can update without recompiling.
pub fn start_http_server(app: tauri::AppHandle, port_offset: u16) {
    let _ = APP_HANDLE.set(app.clone());

    tauri::async_runtime::spawn(async move {
        let start = HTTP_PORT_START + port_offset;
        let end = HTTP_PORT_END + port_offset;
        let port = try_bind_ports(start, end).await;

        if let Some(port) = port {
            RESOLVED_HTTP_PORT.store(port, Ordering::SeqCst);
            tracing::info!("HTTP server listening on http://0.0.0.0:{}", port);
            let _ = app.emit("remote-access-port", serde_json::json!({ "port": port }));
        } else {
            tracing::error!(
                "Failed to bind HTTP server on any port {}–{}",
                start,
                end
            );
        }
    });
}

async fn try_bind_ports(start: u16, end: u16) -> Option<u16> {
    for port in start..=end {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        if let Ok(listener) = TcpListener::bind(&addr).await {
            let port = addr.port();
            tauri::async_runtime::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, addr)) => {
                            tracing::debug!("HTTP connection from {}", addr);
                            tauri::async_runtime::spawn(handle_connection(stream, addr));
                        }
                        Err(e) => {
                            tracing::error!("HTTP accept error: {}", e);
                        }
                    }
                }
            });
            return Some(port);
        } else {
            tracing::debug!("Port {} already in use, trying next", port);
        }
    }
    None
}

/// Extract a numeric id from a URL path that looks like `prefix{id}suffix`.
/// Returns None if the prefix/suffix don't match or the id segment isn't a
/// valid i64. Used by the dispatcher to route `/api/agents/{id}/git/status`
/// and friends without a regex dep.
fn path_segment_id(path: &str, prefix: &str, suffix: &str) -> Option<i64> {
    let rest = path.strip_prefix(prefix)?;
    let id_str = rest.strip_suffix(suffix)?;
    id_str.parse().ok()
}

/// Extract two numeric ids from a URL like `prefix{id1}middle{id2}suffix`.
/// e.g. `/api/meshes/7/issues/42/spawn` → `(7, 42)`.
fn path_two_segment_ids(
    path: &str,
    prefix: &str,
    middle: &str,
    suffix: &str,
) -> Option<(i64, i64)> {
    let rest = path.strip_prefix(prefix)?;
    let (id1_str, rest) = rest.split_once(middle)?;
    let id2_str = rest.strip_suffix(suffix)?;
    Some((id1_str.parse().ok()?, id2_str.parse().ok()?))
}

/// Pull `?name=value` out of a request URL — first match wins. URL-decoding
/// is intentionally minimal (just `+` → space and `%xx`) since we only use
/// this for file paths and small string fields.
fn query_param(path_with_query: &str, name: &str) -> Option<String> {
    let query = path_with_query.split('?').nth(1)?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == name {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.replace('+', " ");
    let mut out = Vec::with_capacity(bytes.len());
    let raw = bytes.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            let hi = (raw[i + 1] as char).to_digit(16);
            let lo = (raw[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn handle_connection(stream: TcpStream, _addr: SocketAddr) {
    let mut lines = tokio::io::BufStream::new(stream);
    let mut request_line = String::new();
    if lines.read_line(&mut request_line).await.is_err() {
        return;
    }

    let request_line = request_line.trim().to_string();
    let parts: Vec<String> = request_line
        .split_whitespace()
        .map(String::from)
        .collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].as_str();
    let path_with_query = parts[1].clone();

    let token = request::extract_token_from_path(&path_with_query);

    let mut headers = String::new();
    while headers.lines().count() == 0 || !headers.ends_with("\r\n\r\n") {
        if lines.read_line(&mut headers).await.is_err() {
            break;
        }
        if headers.trim().is_empty() {
            break;
        }
    }

    // WebSocket upgrade: GET /ws/events?token=xxx — mobile event push.
    if method == "GET"
        && (path_with_query == "/ws/events" || path_with_query.starts_with("/ws/events?"))
    {
        if !request::authenticate(&headers, token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }
        let Some(ws_key) = request::extract_header_value(&headers, "Sec-WebSocket-Key") else {
            let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
            return;
        };
        let accept_key = derive_accept_key(ws_key.as_bytes());
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {}\r\n\
             \r\n",
            accept_key
        );
        let mut stream = lines.into_inner();
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        if stream.flush().await.is_err() {
            return;
        }
        let ws_stream = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        tracing::info!("/ws/events client connected");
        tauri::async_runtime::spawn(ws::handle_events_ws_connection(ws_stream));
        return;
    }

    // WebSocket upgrade: GET /ws/terminal/{nodeId}?token=xxx
    // Accepts either the bm_session cookie or the ?token= URL param —
    // some proxies strip cookies on the WS handshake, so URL tokens stay
    // a supported fallback even after stage 3's cookie migration.
    if method == "GET" && path_with_query.starts_with("/ws/terminal/") {
        let node_id: Option<i64> = path_with_query
            .split('/')
            .nth(3)
            .and_then(|s| s.split('?').next().unwrap_or(s).parse().ok());

        let Some(node_id) = node_id else {
            let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
            return;
        };

        if !request::authenticate(&headers, token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }

        let Some(ws_key) = request::extract_header_value(&headers, "Sec-WebSocket-Key") else {
            let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
            return;
        };

        let accept_key = derive_accept_key(ws_key.as_bytes());
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {}\r\n\
             \r\n",
            accept_key
        );

        let mut stream = lines.into_inner();
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        if stream.flush().await.is_err() {
            return;
        }

        let ws_stream = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        tracing::info!("WebSocket connected for node {}", node_id);
        tauri::async_runtime::spawn(ws::handle_ws_connection(ws_stream, node_id));
        return;
    }

    let path_without_query = path_with_query
        .split('?')
        .next()
        .unwrap_or(&path_with_query)
        .to_string();

    // Attention webhook: POST /api/attention/{session_id}
    // Called by Claude Code's Stop hook — no token required (localhost-only).
    if method == "POST" && path_without_query.starts_with("/api/attention/") {
        routes::attention::handle_post(&mut lines, &path_without_query).await;
        return;
    }

    // POST /api/nodes/create
    if method == "POST" && path_without_query == "/api/nodes/create" {
        if !request::authenticate(&headers, token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }
        let content_length: usize = request::extract_header_value(&headers, "Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        routes::nodes::create(&mut lines, content_length).await;
        return;
    }

    // POST /api/meshes/{id}/pr — create a GitHub PR for the mesh's branch.
    if method == "POST" {
        if let Some(mesh_id) = path_segment_id(&path_without_query, "/api/meshes/", "/pr") {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            let content_length: usize = request::extract_header_value(&headers, "Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            routes::pr::create(&mut lines, mesh_id, content_length).await;
            return;
        }
        // POST /api/meshes/{id}/sessions/import-and-resume
        if let Some(mesh_id) = path_segment_id(
            &path_without_query,
            "/api/meshes/",
            "/sessions/import-and-resume",
        ) {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            let content_length: usize = request::extract_header_value(&headers, "Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            routes::sessions::import_and_resume(&mut lines, mesh_id, content_length).await;
            return;
        }
        // POST /api/meshes/{mid}/issues/{inum}/spawn
        if let Some((mesh_id, issue_number)) = path_two_segment_ids(
            &path_without_query,
            "/api/meshes/",
            "/issues/",
            "/spawn",
        ) {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            let content_length: usize = request::extract_header_value(&headers, "Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            routes::issues::spawn(&mut lines, mesh_id, issue_number, content_length).await;
            return;
        }
    }

    // GET /api/agents/{id}/git/{status|summary|branch} and /api/agents/{id}/diff
    if method == "GET" {
        if let Some(agent_id) = path_segment_id(&path_without_query, "/api/agents/", "/git/status")
        {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            routes::git::status(&mut lines, agent_id).await;
            return;
        }
        if let Some(agent_id) = path_segment_id(&path_without_query, "/api/agents/", "/git/summary")
        {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            routes::git::summary(&mut lines, agent_id).await;
            return;
        }
        if let Some(agent_id) = path_segment_id(&path_without_query, "/api/agents/", "/git/branch")
        {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            routes::git::branch(&mut lines, agent_id).await;
            return;
        }
        if let Some(agent_id) = path_segment_id(&path_without_query, "/api/agents/", "/diff") {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            let file_path = query_param(&path_with_query, "path").unwrap_or_default();
            routes::git::diff(&mut lines, agent_id, &file_path).await;
            return;
        }
        if path_without_query == "/api/gh/auth" {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            routes::git::gh_auth(&mut lines).await;
            return;
        }
        // GET /api/meshes/{id}/sessions/discover
        if let Some(mesh_id) = path_segment_id(
            &path_without_query,
            "/api/meshes/",
            "/sessions/discover",
        ) {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            routes::sessions::discover(&mut lines, mesh_id).await;
            return;
        }
        // GET /api/meshes/{id}/issues
        if let Some(mesh_id) = path_segment_id(&path_without_query, "/api/meshes/", "/issues") {
            if !request::authenticate(&headers, token) {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
            routes::issues::list(&mut lines, mesh_id).await;
            return;
        }
    }

    // GET /assets/* — bundled mobile assets (JS/CSS/etc). Same auth as
    // the shell; legacy `/v2/assets/*` paths still work for any cached
    // mobile bundle that referenced them before the stage 7 flip.
    if method == "GET"
        && (path_without_query.starts_with("/assets/")
            || path_without_query.starts_with("/v2/assets/"))
    {
        if !request::authenticate(&headers, token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }
        let normalized = path_without_query
            .strip_prefix("/v2")
            .unwrap_or(&path_without_query);
        let _ = assets::serve_asset(&mut lines, normalized).await;
        return;
    }

    // GET /v2 — backward-compat redirect to / so any saved phone bookmarks
    // from stages 3-6 keep working. Will be removed once Adam has tapped
    // the new QR at least once.
    if method == "GET" && (path_without_query == "/v2" || path_without_query == "/v2/") {
        let preserve_query = path_with_query
            .split_once('?')
            .map(|(_, q)| format!("?{}", q))
            .unwrap_or_default();
        let response = format!(
            "HTTP/1.1 301 Moved Permanently\r\nLocation: /{}\r\nContent-Length: 0\r\n\r\n",
            preserve_query
        );
        let _ = lines.get_mut().write_all(response.as_bytes()).await;
        return;
    }

    // GET /api/*
    if method == "GET" && path_without_query.starts_with("/api/") {
        if !request::authenticate(&headers, token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }
        let body = match path_without_query.as_str() {
            "/api/nodes" => routes::nodes::list_json(),
            "/api/providers" => routes::providers::list_json(),
            "/api/meshes" => routes::meshes::list_json(),
            _ => r#"{"error":"not found"}"#.to_string(),
        };
        let _ = request::write_json(&mut lines, "200 OK", &body).await;
        return;
    }

    // Default: serve the mobile SPA shell at `/`. On the initial load with
    // a valid `?token=`, issue an HttpOnly bm_session cookie so subsequent
    // fetches and the WebSocket upgrade authenticate without keeping the
    // token in URLs.
    let cookie_valid = request::validate_token(request::extract_token_from_cookies(&headers));
    let url_token_valid = request::validate_token(token.clone());
    if !cookie_valid && !url_token_valid {
        let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
        return;
    }
    let set_cookie = if url_token_valid && !cookie_valid {
        token.as_deref().map(request::session_cookie_header)
    } else {
        None
    };
    let extra = set_cookie.as_ref().map(|h| format!("{}\r\n", h));
    let _ = assets::serve_spa_shell(&mut lines, extra.as_deref()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn port_offset_is_zero_for_stable_and_1000_for_dev() {
        assert_eq!(port_offset("com.alond.buildmesh"), 0);
        assert_eq!(port_offset("com.alond.buildmesh.dev"), 1000);
        // Dev offset shifts the HTTP range clear of the stable hub: 1992 → 2992.
        assert_eq!(HTTP_PORT_START + port_offset("com.alond.buildmesh.dev"), 2992);
        assert_eq!(HTTP_PORT_END + port_offset("com.alond.buildmesh.dev"), 2994);
    }

    async fn attention_post(path: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            path
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        )
        .await
        .expect("attention webhook hung")
        .expect("read failed");

        buf.truncate(n);
        let status_line = String::from_utf8_lossy(&buf);
        status_line
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn attention_webhook_returns_503_when_app_handle_unset() {
        // Locks the hot-path contract: the Claude Code Stop hook POSTs to
        // /api/attention/{session_id} without a token. In tests APP_HANDLE
        // is never initialised, so the handler short-circuits to 503 — but
        // a refactor that drops the path-parsing or short-circuit would
        // surface as a different status code or a hang.
        let status = attention_post("/api/attention/42").await;
        assert_eq!(status, 503, "expected 503 when APP_HANDLE is not set in tests");
    }

    #[tokio::test]
    async fn attention_webhook_returns_400_for_unparseable_session_id() {
        let status = attention_post("/api/attention/not-an-int").await;
        assert_eq!(status, 400, "expected 400 for non-integer session id");
    }

    async fn get_request(path: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            path
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        )
        .await
        .expect("request hung")
        .expect("read failed");

        buf.truncate(n);
        String::from_utf8_lossy(&buf)
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn root_returns_401_without_token() {
        // Post-flip: / serves the SPA shell and requires auth like
        // everything else.
        assert_eq!(get_request("/").await, 401);
    }

    #[tokio::test]
    async fn asset_returns_401_without_token() {
        assert_eq!(get_request("/assets/index.js").await, 401);
    }

    #[tokio::test]
    async fn v2_root_redirects_to_root() {
        // /v2 is a deprecated alias kept around so saved phone bookmarks
        // from stages 3-6 keep working. 301 lets the browser update.
        assert_eq!(get_request("/v2").await, 301);
    }

    #[tokio::test]
    async fn v2_asset_returns_401_without_token() {
        // Legacy /v2/assets/* paths still resolve to the same files so
        // any mid-flip cached HTML doesn't 404 on its scripts.
        assert_eq!(get_request("/v2/assets/index.js").await, 401);
    }

    #[tokio::test]
    async fn agents_git_status_requires_token() {
        assert_eq!(get_request("/api/agents/42/git/status").await, 401);
    }

    #[tokio::test]
    async fn agents_git_summary_requires_token() {
        assert_eq!(get_request("/api/agents/42/git/summary").await, 401);
    }

    #[tokio::test]
    async fn agents_git_branch_requires_token() {
        assert_eq!(get_request("/api/agents/42/git/branch").await, 401);
    }

    #[tokio::test]
    async fn agents_diff_requires_token() {
        assert_eq!(get_request("/api/agents/42/diff?path=foo").await, 401);
    }

    #[tokio::test]
    async fn gh_auth_requires_token() {
        assert_eq!(get_request("/api/gh/auth").await, 401);
    }

    #[test]
    fn path_segment_id_matches_id_between_prefix_and_suffix() {
        assert_eq!(
            path_segment_id("/api/agents/42/git/status", "/api/agents/", "/git/status"),
            Some(42)
        );
        assert_eq!(
            path_segment_id("/api/meshes/7/pr", "/api/meshes/", "/pr"),
            Some(7)
        );
    }

    #[test]
    fn path_segment_id_rejects_wrong_prefix_or_suffix() {
        assert_eq!(path_segment_id("/api/x/42/foo", "/api/agents/", "/foo"), None);
        assert_eq!(
            path_segment_id("/api/agents/42/other", "/api/agents/", "/git/status"),
            None
        );
        assert_eq!(
            path_segment_id("/api/agents/not-an-int/git/status", "/api/agents/", "/git/status"),
            None
        );
    }

    #[test]
    fn query_param_extracts_named_value() {
        assert_eq!(
            query_param("/api/agents/1/diff?path=src/foo.rs", "path"),
            Some("src/foo.rs".to_string())
        );
        assert_eq!(
            query_param("/x?token=t&path=a%20b", "path"),
            Some("a b".to_string())
        );
        assert_eq!(query_param("/x", "path"), None);
        assert_eq!(query_param("/x?other=1", "path"), None);
    }

    #[test]
    fn percent_decode_handles_spaces_and_hex() {
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("clean"), "clean");
    }

    #[test]
    fn path_two_segment_ids_extracts_pair() {
        assert_eq!(
            path_two_segment_ids(
                "/api/meshes/7/issues/42/spawn",
                "/api/meshes/",
                "/issues/",
                "/spawn",
            ),
            Some((7, 42)),
        );
    }

    #[test]
    fn path_two_segment_ids_rejects_non_numeric() {
        assert!(path_two_segment_ids(
            "/api/meshes/foo/issues/42/spawn",
            "/api/meshes/",
            "/issues/",
            "/spawn",
        )
        .is_none());
        assert!(path_two_segment_ids(
            "/api/meshes/7/issues/bar/spawn",
            "/api/meshes/",
            "/issues/",
            "/spawn",
        )
        .is_none());
    }

    #[tokio::test]
    async fn sessions_discover_requires_token() {
        assert_eq!(
            get_request("/api/meshes/1/sessions/discover").await,
            401
        );
    }

    #[tokio::test]
    async fn meshes_issues_requires_token() {
        assert_eq!(get_request("/api/meshes/1/issues").await, 401);
    }

    #[tokio::test]
    async fn ws_upgrade_does_not_hang() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = "GET /ws/terminal/123?token=invalid HTTP/1.1\r\n\
             Host: localhost\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             \r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        )
        .await;

        assert!(result.is_ok(), "handle_connection hung on WebSocket upgrade (regression)");
    }
}
