//! Embedded HTTP/WebSocket server for mobile remote access.
//!
//! Serves the mobile web app and handles WebSocket terminal connections
//! for remote access from a phone on the same network.

pub mod assets;
pub mod auth;
pub mod events;
pub mod request;
pub mod revocation;
pub mod routes;
pub mod ws;
pub mod ws_ticket;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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

        // Secure default (issue #496 / ADR-0012): bind loopback only. Exposing
        // the server to the LAN is an explicit opt-in stored in the DB; until
        // then external machines cannot reach the hub.
        let lan_enabled = crate::db::lan_exposure_enabled().unwrap_or(false);
        let port = try_bind_ports(start, end, lan_enabled).await;

        if let Some(port) = port {
            RESOLVED_HTTP_PORT.store(port, Ordering::SeqCst);
            let scope = if lan_enabled { "LAN (0.0.0.0)" } else { "loopback only" };
            tracing::info!("HTTP server listening on port {} ({})", port, scope);
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

/// The addresses the server binds for `port`. The secure default is loopback
/// only: IPv4 first (the attention hook posts to `127.0.0.1`) then IPv6
/// loopback. With LAN exposure enabled we bind the IPv4 wildcard so phones on
/// the LAN can reach the hub. The first entry is load-bearing; later ones are
/// best-effort (see `try_bind_ports`).
fn bind_addrs(port: u16, lan_enabled: bool) -> Vec<SocketAddr> {
    if lan_enabled {
        vec![SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))]
    } else {
        vec![
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        ]
    }
}

/// Spawn the accept loop for one bound listener, handing each connection to
/// `handle_connection`.
fn spawn_accept_loop(listener: TcpListener) {
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
}

async fn try_bind_ports(start: u16, end: u16, lan_enabled: bool) -> Option<u16> {
    for port in start..=end {
        let mut addrs = bind_addrs(port, lan_enabled).into_iter();
        // The primary address must bind. If it's taken, the port is in use, so
        // move to the next one. Secondary addresses (IPv6 loopback) are
        // best-effort: a host with IPv6 disabled still serves over 127.0.0.1.
        let primary = addrs.next().expect("bind_addrs is never empty");
        let Ok(listener) = TcpListener::bind(&primary).await else {
            tracing::debug!("Port {} already in use ({}), trying next", port, primary);
            continue;
        };
        spawn_accept_loop(listener);
        for addr in addrs {
            match TcpListener::bind(&addr).await {
                Ok(listener) => spawn_accept_loop(listener),
                Err(e) => tracing::debug!("Secondary bind on {} failed: {}", addr, e),
            }
        }
        return Some(port);
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

/// Parse the `?tail=N` parameter for `GET /nodes/{id}/log`. Defaults to
/// [`DEFAULT_LOG_TAIL`] when the param is absent, empty, or unparseable —
/// a zero-turn response to a default request is never what a caller wants,
/// and a `tail=0` past-call is just a wasted round trip.
const DEFAULT_LOG_TAIL: usize = 10;

fn tail_param(path_with_query: &str) -> usize {
    query_param(path_with_query, "tail")
        .and_then(|t| t.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_LOG_TAIL)
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

/// The host's own interface IPs, enumerated once and cached. Consulted only to
/// validate a non-loopback `Host` (an opt-in LAN client) — loopback and
/// `localhost` short-circuit in `host_header_allowed`, so the common path never
/// pays the enumeration, which can stall for seconds behind a VPN/Docker stack
/// on Windows.
static LOCAL_IPS: OnceLock<Vec<IpAddr>> = OnceLock::new();

fn local_interface_ips() -> &'static [IpAddr] {
    LOCAL_IPS.get_or_init(|| match local_ip_address::list_afinet_netifas() {
        Ok(ifaces) => ifaces.into_iter().map(|(_, ip)| ip).collect(),
        Err(e) => {
            tracing::warn!("Host-header validation: interface enumeration failed: {}", e);
            Vec::new()
        }
    })
}

/// Validate a request's `Host` header against this machine's identities to
/// defeat DNS rebinding (ADR-0012). The fast path — `localhost` or any loopback
/// IP — never enumerates interfaces; only a non-loopback IP `Host` (an opt-in
/// LAN client) consults the cached interface list.
fn host_header_allowed(host_header: &str) -> bool {
    let hostname = request::strip_host_port(host_header.trim());
    if hostname.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match hostname.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => true,
        Ok(_) => request::host_is_allowed(host_header, local_interface_ips()),
        Err(_) => false,
    }
}

async fn handle_connection(stream: TcpStream, addr: SocketAddr) {
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

    let mut headers = String::new();
    while headers.lines().count() == 0 || !headers.ends_with("\r\n\r\n") {
        if lines.read_line(&mut headers).await.is_err() {
            break;
        }
        if headers.trim().is_empty() {
            break;
        }
    }

    // DNS-rebinding guard (issue #496 / ADR-0012): every request — including the
    // WebSocket upgrades below — must carry a `Host` that targets this machine.
    // A rogue browser page can re-resolve its domain to loopback but cannot forge
    // `Host`, so an unmatched value is rejected before any routing.
    let host = request::extract_header_value(&headers, "Host").unwrap_or("");
    if !host_header_allowed(host) {
        let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
        return;
    }

    // WebSocket upgrade: GET /ws/events?ticket=xxx — mobile event push.
    // Authenticated by a single-use ticket (issue #500 AC4) minted via the
    // cookie/header-protected POST /api/ws-ticket: a raw `?token=` is no longer
    // honoured here, and the cookie alone is not trusted on the upgrade.
    if method == "GET"
        && (path_with_query == "/ws/events" || path_with_query.starts_with("/ws/events?"))
    {
        let ticket = query_param(&path_with_query, "ticket").unwrap_or_default();
        // The ticket carries the device that minted it (issue #502), so a later
        // revocation can kick this socket; `None` for the root token.
        let Some(device_id) = ws_ticket::consume(&ticket) else {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        };
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
        tauri::async_runtime::spawn(ws::handle_events_ws_connection(ws_stream, device_id));
        return;
    }

    // WebSocket upgrade: GET /ws/terminal/{nodeId}?ticket=xxx
    // Same single-use ticket handshake as /ws/events (issue #500 AC4) — the
    // long-lived token never rides the URL, and the ticket can only be obtained
    // through the authenticated POST /api/ws-ticket, so a cross-site page cannot
    // forge this upgrade.
    if method == "GET" && path_with_query.starts_with("/ws/terminal/") {
        let node_id: Option<i64> = path_with_query
            .split('/')
            .nth(3)
            .and_then(|s| s.split('?').next().unwrap_or(s).parse().ok());

        let Some(node_id) = node_id else {
            let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
            return;
        };

        let ticket = query_param(&path_with_query, "ticket").unwrap_or_default();
        // Recover the device that minted this ticket so a revocation can kick
        // the socket mid-stream (issue #502); `None` for the root token.
        let Some(device_id) = ws_ticket::consume(&ticket) else {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        };

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
        tauri::async_runtime::spawn(ws::handle_ws_connection(ws_stream, node_id, device_id));
        return;
    }

    let path_without_query = path_with_query
        .split('?')
        .next()
        .unwrap_or(&path_with_query)
        .to_string();

    // GET /admin/devices — list paired devices for the "Authorized Devices"
    // panel (issue #502). Admin-only; the first real operation in the namespace
    // #500 reserved. Matched before the `/admin/*` catch-all below.
    if method == "GET" && path_without_query == "/admin/devices" {
        if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
            .await
            .is_none()
        {
            return;
        }
        let _ = request::write_json(&mut lines, "200 OK", &routes::admin::list_devices_json()).await;
        return;
    }

    // POST /admin/devices/{id}/revoke — revoke a device and kick its live socket
    // (issue #502). Admin-only.
    if method == "POST" {
        if let Some(device_id) =
            path_segment_id(&path_without_query, "/admin/devices/", "/revoke")
        {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            routes::admin::revoke(&mut lines, device_id).await;
            return;
        }
    }

    // Reserved administrative namespace (issue #500 AC1/AC2). The device routes
    // above are the first real operations mounted here; anything else under
    // `/admin/*` still proves the two-tier separation: a coordinator-scoped token
    // is strictly Forbidden (403), a request with no/invalid credentials is
    // Unauthorized (401), and the Admin (root) token reaches a 404 because
    // nothing else is mounted. Placed before all other routing so `/admin/*` can
    // never fall through to another handler.
    if path_without_query == "/admin" || path_without_query.starts_with("/admin/") {
        match auth::authorize(&headers, auth::RequiredScope::Admin) {
            auth::AuthOutcome::Ok(_) => {
                let _ = request::write_status_only(&mut lines, "404 Not Found").await;
            }
            auth::AuthOutcome::Unauthorized => {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            }
            auth::AuthOutcome::Forbidden => {
                let _ = request::write_status_only(&mut lines, "403 Forbidden").await;
            }
        }
        return;
    }

    // POST /api/session — the pairing/login handoff (issue #500, extended by
    // #502). The client POSTs a token as `Authorization: Bearer <token>`:
    //   - the **root token** (the desktop QR's pairing secret) mints a new
    //     persistent *device session* and returns its token;
    //   - an existing **device token** refreshes that device (roaming IP) and
    //     returns the same token.
    // Either way we set the HttpOnly bm_session cookie to the effective device
    // token AND return it in the JSON body, so the client persists its own
    // per-device token (revocable independently) instead of the shared root
    // token. The token never appears in a URL the server validates. No cookie is
    // accepted here — this is how a client *gets* one.
    if method == "POST" && path_without_query == "/api/session" {
        match request::bearer_token(&headers) {
            Some(t) => {
                let label = routes::admin::device_label_from_user_agent(
                    request::extract_header_value(&headers, "User-Agent"),
                );
                let peer_ip = addr.ip().to_string();
                match crate::db::login_device_session(&t, label.as_deref(), Some(&peer_ip)) {
                    Ok(Some((_, device_token))) => {
                        let cookie = request::session_cookie_header(&device_token);
                        let body = serde_json::json!({ "token": device_token }).to_string();
                        // `Cache-Control: no-store` — the body carries a long-lived
                        // device token (issue #502); never let a proxy or bfcache
                        // retain this response.
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Cache-Control: no-store\r\n\
                             Content-Length: {}\r\n{}\r\n\r\n{}",
                            body.len(),
                            cookie,
                            body
                        );
                        let _ = lines.get_mut().write_all(response.as_bytes()).await;
                    }
                    _ => {
                        let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                    }
                }
            }
            None => {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            }
        }
        return;
    }

    // POST /api/ws-ticket — mint a single-use WebSocket handshake ticket (issue
    // #500 AC4). Authenticated as an Admin request (cookie or bearer root
    // token); the returned ticket is then passed as `?ticket=` on the WS upgrade.
    if method == "POST" && path_without_query == "/api/ws-ticket" {
        if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
            .await
            .is_none()
        {
            return;
        }
        // Bind the ticket to the requesting device (issue #502) so the WebSocket
        // it opens can be force-closed on revocation; `None` for the root token.
        // Opening a terminal is also a natural "last active" signal, so refresh
        // the device here too (cheaper than touching on every poll).
        let device_id = auth::resolve_device_session(&headers);
        if let Some(id) = device_id {
            let peer_ip = addr.ip().to_string();
            let _ = crate::db::touch_device_session(id, Some(&peer_ip));
        }
        let body = serde_json::to_string(&ws_ticket::WsTicket {
            ticket: ws_ticket::mint(device_id),
        })
        .unwrap_or_else(|_| r#"{"ticket":""}"#.to_string());
        let _ = request::write_json(&mut lines, "200 OK", &body).await;
        return;
    }

    // Coordinator read API (ADR-0008): GET /nodes — spine-only Node Digests.
    // Distinct from the mobile `/api/nodes`: it authenticates with the
    // off-by-default, read-scoped coordinator token (NOT the root token), so a
    // disabled API or a missing/wrong token is rejected here. Loopback/LAN
    // binding is inherited from the embedded server; no internet port is opened.
    if method == "GET" && path_without_query == "/nodes" {
        if auth::guard(&mut lines, &headers, auth::RequiredScope::CoordinatorRead)
            .await
            .is_none()
        {
            return;
        }
        let body = routes::coordinator::list_nodes_json();
        let _ = request::write_json(&mut lines, "200 OK", &body).await;
        return;
    }

    // Coordinator read API (ADR-0008): GET /nodes/{id}/log?tail=N — the
    // on-demand drill-in returning a node's raw recent transcript turns. Same
    // read-scoped token as GET /nodes. An unknown node id is a 404; every other
    // degrade (no session, missing/unreadable transcript) is a 200 carrying a
    // typed unavailable envelope, so a Coordinator never sees a bare error.
    if method == "GET" {
        if let Some(node_id) = path_segment_id(&path_without_query, "/nodes/", "/log") {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::CoordinatorRead)
                .await
                .is_none()
            {
                return;
            }
            let tail = tail_param(&path_with_query);
            match routes::coordinator::log_json(node_id, tail) {
                Some(body) => {
                    let _ = request::write_json(&mut lines, "200 OK", &body).await;
                }
                None => {
                    let _ = request::write_status_only(&mut lines, "404 Not Found").await;
                }
            }
            return;
        }
    }

    // Coordinator drive API (ADR-0008 §5, issue #319): POST /nodes/{id}/prompt —
    // write a prompt into a live node's PTY and return an honest verdict.
    // Authenticated with the DRIVE-scoped token (a read-only token is rejected
    // here) behind the drive kill-switch; both sit under the coordinator master
    // switch, so disabling the surface disables drive too.
    if method == "POST" {
        if let Some(node_id) = path_segment_id(&path_without_query, "/nodes/", "/prompt") {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::CoordinatorWrite)
                .await
                .is_none()
            {
                return;
            }
            let content_length: usize = request::extract_header_value(&headers, "Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            routes::coordinator::prompt(&mut lines, node_id, content_length).await;
            return;
        }
    }

    // Attention webhook: POST /api/attention/{session_id}
    // Called by Claude Code's Stop hook — no token required, so the handler
    // verifies the peer is loopback (issue #496) and rejects external callers
    // with 403 before publishing the Node Turn.
    if method == "POST" && path_without_query.starts_with("/api/attention/") {
        routes::attention::handle_post(&mut lines, &path_without_query, addr).await;
        return;
    }

    // POST /api/nodes/create
    if method == "POST" && path_without_query == "/api/nodes/create" {
        if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
            .await
            .is_none()
        {
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
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            let content_length: usize = request::extract_header_value(&headers, "Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            routes::pr::create(&mut lines, mesh_id, content_length).await;
            return;
        }
        // POST /api/meshes/{id}/pulls/{n}/merge — merge a PR (issue #422).
        // Pairs with the GET `/pulls/{n}/mergeability` route; the GET
        // branch (which checks the longer suffix first) handles the auth
        // gate for the read path, and this branch owns the write path.
        if let Some((mesh_id, pr_number)) = path_two_segment_ids(
            &path_without_query,
            "/api/meshes/",
            "/pulls/",
            "/merge",
        ) {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            let content_length: usize = request::extract_header_value(&headers, "Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            routes::pr::merge(&mut lines, mesh_id, pr_number, content_length).await;
            return;
        }
        // POST /api/meshes/{id}/agent-nodes/import-and-resume
        if let Some(mesh_id) = path_segment_id(
            &path_without_query,
            "/api/meshes/",
            "/agent-nodes/import-and-resume",
        ) {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            let content_length: usize = request::extract_header_value(&headers, "Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            routes::agent_nodes::import_and_resume(&mut lines, mesh_id, content_length).await;
            return;
        }
        // POST /api/meshes/{mid}/issues/{inum}/spawn
        if let Some((mesh_id, issue_number)) = path_two_segment_ids(
            &path_without_query,
            "/api/meshes/",
            "/issues/",
            "/spawn",
        ) {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
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
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            routes::git::status(&mut lines, agent_id).await;
            return;
        }
        if let Some(agent_id) = path_segment_id(&path_without_query, "/api/agents/", "/git/summary")
        {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            routes::git::summary(&mut lines, agent_id).await;
            return;
        }
        if let Some(agent_id) = path_segment_id(&path_without_query, "/api/agents/", "/git/branch")
        {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            routes::git::branch(&mut lines, agent_id).await;
            return;
        }
        if let Some(agent_id) = path_segment_id(&path_without_query, "/api/agents/", "/diff") {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            let file_path = query_param(&path_with_query, "path").unwrap_or_default();
            routes::git::diff(&mut lines, agent_id, &file_path).await;
            return;
        }
        if path_without_query == "/api/gh/auth" {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            routes::git::gh_auth(&mut lines).await;
            return;
        }
        // GET /api/meshes/{id}/agent-nodes/discover
        if let Some(mesh_id) = path_segment_id(
            &path_without_query,
            "/api/meshes/",
            "/agent-nodes/discover",
        ) {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            routes::agent_nodes::discover(&mut lines, mesh_id).await;
            return;
        }
        // GET /api/meshes/{id}/issues
        if let Some(mesh_id) = path_segment_id(&path_without_query, "/api/meshes/", "/issues") {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            routes::issues::list(&mut lines, mesh_id).await;
            return;
        }
        // GET /api/meshes/{id}/pulls?state=open|closed — list PRs (issue #422).
        // Mirrors the issues route above; the `state` query param defaults
        // to "open" inside the handler.
        if let Some(mesh_id) = path_segment_id(&path_without_query, "/api/meshes/", "/pulls") {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            let state = query_param(&path_with_query, "state").unwrap_or_default();
            routes::pr::list_pulls(&mut lines, mesh_id, &state).await;
            return;
        }
        // GET /api/meshes/{id}/pulls/{n}/mergeability — per-PR enrichment
        // (issue #422). The POST `/merge` route lives in its own `method
        // == "POST"` branch below, so a GET to this URL can never
        // collide with it; the two-segment helper also rejects the
        // cross-suffix case (`/merge` with `/mergeability` suffix) via
        // its numeric-only parse, so dispatch is safe regardless of the
        // order the two GET-side PR routes are listed in.
        if let Some((mesh_id, pr_number)) = path_two_segment_ids(
            &path_without_query,
            "/api/meshes/",
            "/pulls/",
            "/mergeability",
        ) {
            if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
                .await
                .is_none()
            {
                return;
            }
            routes::pr::get_mergeability(&mut lines, mesh_id, pr_number).await;
            return;
        }
    }

    // GET /assets/* — bundled mobile assets (JS/CSS/etc). Public, like the SPA
    // shell (issue #500): the static bundle holds no secrets and is identical
    // for every install. The browser must be able to load these before any JS
    // can run POST /api/session to authenticate; all data APIs stay gated, and
    // the DNS-rebinding Host guard above still runs on every request. Legacy
    // `/v2/assets/*` paths still resolve for any cached mobile bundle.
    if method == "GET"
        && (path_without_query.starts_with("/assets/")
            || path_without_query.starts_with("/v2/assets/"))
    {
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
        if auth::guard(&mut lines, &headers, auth::RequiredScope::Admin)
            .await
            .is_none()
        {
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

    // Default: serve the mobile SPA shell at `/`. Public (issue #500) — the
    // shell carries no secrets and must load so its JS can POST the root token
    // to /api/session, which is what mints the bm_session cookie. Authentication
    // moved entirely to that endpoint; the old `/?token=` URL→cookie handoff is
    // gone.
    let _ = assets::serve_spa_shell(&mut lines, None).await;
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
    async fn root_serves_shell_without_credentials() {
        // Post-#500: the SPA shell is public so its JS can load and POST the
        // token to /api/session. No credentials → still 200 (not 401); the
        // embedded index.html is present once `dist/mobile` is built.
        assert_eq!(get_request("/").await, 200);
    }

    #[tokio::test]
    async fn asset_is_public_not_401() {
        // Assets are public too (issue #500). `/assets/index.js` isn't a real
        // built filename (the bundle is content-hashed), so it 404s — the point
        // is that it is NOT gated behind 401 anymore.
        assert_eq!(get_request("/assets/index.js").await, 404);
    }

    #[tokio::test]
    async fn v2_root_redirects_to_root() {
        // /v2 is a deprecated alias kept around so saved phone bookmarks
        // from stages 3-6 keep working. 301 lets the browser update.
        assert_eq!(get_request("/v2").await, 301);
    }

    #[tokio::test]
    async fn v2_asset_is_public_not_401() {
        // Legacy /v2/assets/* paths still resolve (strip /v2 → /assets/*) and
        // are public like the rest of the static bundle (issue #500).
        assert_eq!(get_request("/v2/assets/index.js").await, 404);
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

    #[tokio::test]
    async fn coordinator_nodes_rejects_without_token() {
        // Off-by-default + no token presented → 401, short-circuited before any
        // DB lookup (the test binary never initialises the global DB). The
        // valid-token → list path is covered by the coordinator integration
        // test against an in-memory connection.
        assert_eq!(get_request("/nodes").await, 401);
    }

    #[tokio::test]
    async fn coordinator_node_log_rejects_without_token() {
        // The drill-in endpoint shares the read-scoped auth gate: no token →
        // 401, short-circuited before the node lookup. The tail query param
        // must not change that.
        assert_eq!(get_request("/nodes/42/log").await, 401);
        assert_eq!(get_request("/nodes/42/log?tail=5").await, 401);
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

    /// The default for `GET /nodes/{id}/log?tail=` must be a useful number of
    /// recent turns — never zero. A caller who copies the user guide's
    /// quickstart without `?tail=N` should get a real tail, not an empty
    /// `{"turns":[]}` that looks like a quiet transcript. (Reviewed in PR #388:
    /// the route originally parsed with `unwrap_or(0)` and the user guide
    /// claimed a default of 10, but the code disagreed.)
    #[test]
    fn tail_param_defaults_to_a_useful_recent_turn_count() {
        // No query string at all → default, NOT zero.
        assert_eq!(tail_param("/nodes/42/log"), 10);
        // Empty `?tail=` → default.
        assert_eq!(tail_param("/nodes/42/log?tail="), 10);
        // Unparseable `?tail=abc` → default.
        assert_eq!(tail_param("/nodes/42/log?tail=abc"), 10);
        // Explicit `?tail=0` → also default: a zero-turn response is never
        // what a caller wants, and a `tail=0` past-call is just a wasted
        // round trip.
        assert_eq!(tail_param("/nodes/42/log?tail=0"), 10);
    }

    #[test]
    fn tail_param_honours_an_explicit_positive_value() {
        assert_eq!(tail_param("/nodes/42/log?tail=5"), 5);
        assert_eq!(tail_param("/nodes/42/log?tail=25"), 25);
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
    async fn agent_nodes_discover_requires_token() {
        assert_eq!(
            get_request("/api/meshes/1/agent-nodes/discover").await,
            401
        );
    }

    #[tokio::test]
    async fn meshes_issues_requires_token() {
        assert_eq!(get_request("/api/meshes/1/issues").await, 401);
    }

    #[tokio::test]
    async fn meshes_pulls_requires_token() {
        // GET /api/meshes/{id}/pulls — list PRs for a mesh (issue #422).
        // Without a valid token the dispatcher must short-circuit to 401
        // before reaching `commands::pr::get_repo_pulls`.
        assert_eq!(get_request("/api/meshes/1/pulls").await, 401);
        // The `?state=closed` variant is the same code path; the token
        // gate is checked first so a query string never reaches GitHub.
        assert_eq!(get_request("/api/meshes/1/pulls?state=closed").await, 401);
    }

    #[tokio::test]
    async fn meshes_pulls_mergeability_requires_token() {
        // GET /api/meshes/{id}/pulls/{n}/mergeability — per-PR enrichment.
        // Must reject with 401 before any GitHub call.
        assert_eq!(
            get_request("/api/meshes/1/pulls/42/mergeability").await,
            401
        );
    }

    #[tokio::test]
    async fn meshes_pulls_merge_requires_token() {
        // POST /api/meshes/{id}/pulls/{n}/merge — write action. The auth
        // gate runs before body parsing, so 401 is returned even for a
        // malformed body. We use a GET helper which still parses status
        // codes from the response line (the server returns 401 regardless
        // of method on an unauthenticated request).
        assert_eq!(
            get_request("/api/meshes/1/pulls/42/merge").await,
            401
        );
    }

    #[test]
    fn path_two_segment_ids_parses_pulls_routes() {
        // The PR routes use the same `path_two_segment_ids` helper as the
        // issues/spawn route, with a different middle/suffix. Lock the
        // shape so a future refactor of the helper can't silently
        // misroute `/api/meshes/7/pulls/42/mergeability`.
        assert_eq!(
            path_two_segment_ids(
                "/api/meshes/7/pulls/42/mergeability",
                "/api/meshes/",
                "/pulls/",
                "/mergeability",
            ),
            Some((7, 42)),
        );
        assert_eq!(
            path_two_segment_ids(
                "/api/meshes/7/pulls/42/merge",
                "/api/meshes/",
                "/pulls/",
                "/merge",
            ),
            Some((7, 42)),
        );
    }

    #[test]
    fn path_two_segment_ids_rejects_non_numeric_pulls_segments() {
        // Either segment being non-numeric must yield None — otherwise
        // the dispatcher would forward `mesh_id="foo"` to
        // `get_repo_pulls`, which would hit the DB and return an error
        // instead of a clean 404.
        assert!(path_two_segment_ids(
            "/api/meshes/foo/pulls/42/mergeability",
            "/api/meshes/",
            "/pulls/",
            "/mergeability",
        )
        .is_none());
        assert!(path_two_segment_ids(
            "/api/meshes/7/pulls/bar/mergeability",
            "/api/meshes/",
            "/pulls/",
            "/mergeability",
        )
        .is_none());
        // The `mergeability` suffix must NOT match `merge` requests (and
        // vice versa) — order-independence: the dispatcher checks both,
        // so each must parse correctly with its OWN suffix.
        assert!(path_two_segment_ids(
            "/api/meshes/7/pulls/42/merge",
            "/api/meshes/",
            "/pulls/",
            "/mergeability",
        )
        .is_none());
    }

    /// Send a WebSocket upgrade for `path` and return its HTTP status code.
    /// Asserts the handler responds within 2s (a hung upgrade is a regression).
    async fn ws_status(path: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET {} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             \r\n",
            path
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        )
        .await
        .expect("handle_connection hung on WebSocket upgrade (regression)")
        .expect("read failed");

        buf.truncate(n);
        String::from_utf8_lossy(&buf)
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// Send a plain HTTP request for `path` and return its status code. DB-free:
    /// an unauthenticated request resolves to no role without a DB lookup, so
    /// this exercises route mounting + the auth guard without an initialized DB.
    async fn http_status(method: &str, path: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!("{} {} HTTP/1.1\r\nHost: localhost\r\n\r\n", method, path);
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("handle_connection hung")
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
    async fn admin_device_routes_are_mounted_and_guarded() {
        // Issue #502: both device routes are Admin-guarded. With no credential
        // they must be 401 — proving they're mounted (not a 404 fall-through)
        // and never public. Wrong-role → 403 is covered in `http::auth::tests`.
        assert_eq!(http_status("GET", "/admin/devices").await, 401);
        assert_eq!(http_status("POST", "/admin/devices/5/revoke").await, 401);
    }

    #[tokio::test]
    async fn ws_terminal_rejects_raw_url_token() {
        // Issue #500 AC4: a raw `?token=` on the WS upgrade is no longer a
        // credential — only a single-use `?ticket=` is. So it's rejected (401)
        // and the handler doesn't hang.
        assert_eq!(ws_status("/ws/terminal/123?token=anything").await, 401);
    }

    #[tokio::test]
    async fn ws_events_rejects_without_ticket() {
        assert_eq!(ws_status("/ws/events").await, 401);
        assert_eq!(ws_status("/ws/events?ticket=bogus").await, 401);
    }

    #[tokio::test]
    async fn ws_terminal_accepts_a_valid_ticket() {
        // A ticket minted via the in-memory store (no DB needed) lets the
        // upgrade through to the 101 switch. Single-use is covered in
        // ws_ticket's own tests.
        let ticket = ws_ticket::mint(None);
        let path = format!("/ws/terminal/123?ticket={}", ticket);
        assert_eq!(ws_status(&path).await, 101);
    }

    #[test]
    fn bind_addrs_loopback_by_default() {
        let addrs = bind_addrs(1992, false);
        assert_eq!(addrs.len(), 2, "default binds IPv4 + IPv6 loopback");
        assert!(addrs[0].is_ipv4() && addrs[0].ip().is_loopback(),
            "IPv4 loopback must be primary (the attention hook posts to 127.0.0.1)");
        assert!(addrs[1].is_ipv6() && addrs[1].ip().is_loopback());
        assert!(addrs.iter().all(|a| a.ip().is_loopback()),
            "the default must never expose beyond loopback");
    }

    #[test]
    fn bind_addrs_lan_uses_ipv4_wildcard() {
        let addrs = bind_addrs(1992, true);
        assert_eq!(addrs.len(), 1);
        assert!(addrs[0].ip().is_unspecified(), "LAN opt-in binds 0.0.0.0");
    }

    async fn get_request_with_host(path: &str, host: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\n\r\n",
            path, host
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
    async fn host_header_rebinding_domain_is_rejected() {
        // DNS-rebinding attempt: the attacker's domain rides in `Host` even
        // when re-resolved to loopback. Rejected with 400 before any routing.
        assert_eq!(get_request_with_host("/api/nodes", "evil.com").await, 400);
        assert_eq!(get_request_with_host("/", "attacker.example:1992").await, 400);
    }

    #[tokio::test]
    async fn host_header_loopback_passes_validation() {
        // A loopback Host clears the rebinding guard; the request then fails the
        // normal auth gate (401), proving the 400 is specific to bad Hosts.
        assert_eq!(get_request_with_host("/api/nodes", "127.0.0.1:1992").await, 401);
        assert_eq!(get_request_with_host("/api/nodes", "localhost").await, 401);
        assert_eq!(get_request_with_host("/api/nodes", "[::1]:1992").await, 401);
    }

    async fn attention_post_with_peer(path: &str, peer: SocketAddr) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let path = path.to_string();

        tokio::spawn(async move {
            let (stream, _real_peer) = listener.accept().await.unwrap();
            let mut lines = tokio::io::BufStream::new(stream);
            routes::attention::handle_post(&mut lines, &path, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        )
        .await
        .expect("attention webhook hung")
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
    async fn attention_webhook_rejects_non_loopback_peer() {
        // An external machine reaching the unauthenticated webhook is refused
        // with 403 before the Node Turn is published.
        let external: SocketAddr = "203.0.113.5:54321".parse().unwrap();
        assert_eq!(attention_post_with_peer("/api/attention/42", external).await, 403);
    }

    #[tokio::test]
    async fn attention_webhook_allows_loopback_peer() {
        // A loopback peer clears the 403 gate and proceeds (503 here because
        // APP_HANDLE is unset in tests), proving the gate is peer-specific.
        let local: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        assert_eq!(attention_post_with_peer("/api/attention/42", local).await, 503);
    }

    // --- RBAC dispatcher gates (issue #500) ---
    //
    // These exercise the credential-free paths, which short-circuit before any
    // DB lookup (the test binary has no initialized global DB). The token→role
    // and coordinator-token→403-on-admin logic lives in `auth`'s unit tests,
    // which seed an in-memory DB.

    async fn post_status(path: &str) -> u16 {
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
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
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
    async fn admin_namespace_rejects_without_credentials() {
        // AC1/AC2: the reserved /admin surface is gated. No credentials → 401
        // (a *coordinator* token would be 403 — see auth::tests).
        assert_eq!(get_request("/admin").await, 401);
        assert_eq!(get_request("/admin/keys").await, 401);
    }

    #[tokio::test]
    async fn session_endpoint_rejects_without_bearer() {
        // POST /api/session is the login handoff; with no Authorization header
        // there is nothing to validate → 401 (and no cookie is set).
        assert_eq!(post_status("/api/session").await, 401);
    }

    #[tokio::test]
    async fn ws_ticket_endpoint_requires_admin_credentials() {
        // Minting a WS ticket requires an authenticated Admin request; with no
        // credentials the guard returns 401 before any ticket is minted.
        assert_eq!(post_status("/api/ws-ticket").await, 401);
    }
}
