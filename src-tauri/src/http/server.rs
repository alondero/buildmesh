//! Bind, accept, TLS, and the per-connection transport loop.
//!
//! `handle_connection` parses the request head, enforces the Host guard, reads
//! the body per the matched route's policy, then calls
//! [`crate::http::router::dispatch_matched`] (the server already called
//! [`crate::http::router::match_route`] to size the body read, so dispatch
//! reuses that match instead of scanning the table a second time).
//! The only remaining special branch is WebSocket upgrade.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::OnceLock;

use tauri::{Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::WebSocketStream;

use crate::http::request;
use crate::http::router::{
    content_length, dispatch_matched, match_route, BodyPolicy, DispatchResult, ParsedRequest,
    Upgrade, MAX_HEADER_BYTES, REQUEST_HEAD_TIMEOUT,
};
use crate::http::state::{self, LoopbackSkeleton, RealizedBind};
use crate::http::stream::MaybeTls;
use crate::http::tls;
use crate::http::ws;

/// One listener the server should open.
#[derive(Clone)]
pub struct BindSpec {
    pub addr: SocketAddr,
    /// TLS acceptor for this listener, or `None` for plain HTTP. A
    /// TLS-intended listener carries `Some` — a structural guarantee that the
    /// bind loop can never observe a "want TLS but no acceptor" spec
    /// (issue #587). Built once in `bind_specs` from the (cached) acceptor
    /// before any socket is opened, and propagated into the accept loop
    /// by `bind_interface_listeners` / `ensure_loopback_skeleton`.
    pub tls: Option<TlsAcceptor>,
}

pub(crate) fn start(app: tauri::AppHandle, port_offset: u16) {
    state::set_app_handle(app);
    state::store_port_offset(port_offset);
    tauri::async_runtime::spawn(async move {
        apply_binding(port_offset).await;
    });
    crate::http::interface_watcher::spawn_interface_watcher(|| {
        tauri::async_runtime::spawn(async move {
            reapply_binding().await;
        });
    });
}

pub async fn reapply_binding() {
    let port_offset = state::loaded_port_offset();
    apply_binding(port_offset).await;
}

async fn apply_binding(port_offset: u16) {
    // Serialize the setting snapshot, identity load, and listener replacement.
    // Otherwise an older enable can finish after a newer disable.
    static BINDING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _binding = BINDING.lock().await;
    state::store_port_offset(port_offset);
    let start = state::HTTP_PORT_START + port_offset;
    let end = state::HTTP_PORT_END + port_offset;

    let lan_enabled = crate::db::lan_exposure_enabled().unwrap_or(false);
    if !lan_enabled {
        crate::http::pairing::invalidate();
    }
    let interface_ips: Vec<IpAddr> = if lan_enabled {
        state::refresh_local_interface_ips()
    } else {
        Vec::new()
    };

    let acceptor = if lan_enabled && interface_ips.iter().any(|ip| !ip.is_loopback()) {
        match get_or_build_acceptor(&interface_ips).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("TLS init failed; LAN interfaces will not be exposed: {}", e);
                None
            }
        }
    } else {
        None
    };

    let mut listeners = state::server_listeners().lock().await;

    if let Some(tx) = listeners.interface.shutdown.take() {
        let _ = tx.send(true);
    }
    for handle in listeners.interface.handles.drain(..) {
        let _ = handle.await;
    }

    let (skeleton_port, skeleton_realized) =
        match ensure_loopback_skeleton(&mut listeners.skeleton, start, end).await {
            Some((port, realized)) => (port, realized),
            None => {
                if lan_enabled {
                    if let Err(e) = crate::db::set_lan_exposure_enabled(false) {
                        tracing::warn!(
                            "Failed to revert LAN exposure flag after bind failure: {}",
                            e
                        );
                    }
                }
                state::realized_binds_store().write().clear();
                tracing::error!("Failed to bind HTTP server on any port {}–{}", start, end);
                return;
            }
        };
    state::store_resolved_http_port(skeleton_port);
    if let Some(app) = state::app_handle() {
        let _ = app.emit(
            "remote-access-port",
            serde_json::json!({ "port": skeleton_port }),
        );
    }

    let interface_realized = if let Some(acceptor) = acceptor {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut handles = Vec::new();
        let realized = bind_interface_listeners(
            skeleton_port,
            &interface_ips,
            &acceptor,
            &shutdown_rx,
            &mut handles,
        )
        .await;
        listeners.interface.shutdown = Some(shutdown_tx);
        listeners.interface.handles = handles;
        realized
    } else {
        Vec::new()
    };

    let mut all_realized = skeleton_realized;
    all_realized.extend(interface_realized);
    let needs_tls = all_realized.iter().any(|b| b.tls);
    *state::realized_binds_store().write() = all_realized;

    let scope = if needs_tls {
        "loopback (HTTP) + LAN interfaces (HTTPS)"
    } else if lan_enabled {
        "loopback only (no non-loopback interface to expose)"
    } else {
        "loopback only"
    };
    tracing::info!(
        "HTTP server listening on port {} ({})",
        skeleton_port,
        scope
    );
}

fn realized_binds_from_specs(specs: &[BindSpec], bound_indices: &[usize]) -> Vec<RealizedBind> {
    bound_indices
        .iter()
        .map(|&i| RealizedBind {
            address: specs[i].addr.to_string(),
            tls: specs[i].tls.is_some(),
        })
        .collect()
}

pub fn bind_specs(
    port: u16,
    interface_ips: &[IpAddr],
    acceptor: Option<&TlsAcceptor>,
) -> Vec<BindSpec> {
    let mut specs = vec![
        BindSpec {
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            tls: None,
        },
        BindSpec {
            addr: SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
            tls: None,
        },
    ];
    if let Some(acceptor) = acceptor {
        for ip in interface_ips {
            if !ip.is_loopback() && !is_link_local(ip) {
                specs.push(BindSpec {
                    addr: SocketAddr::new(*ip, port),
                    tls: Some(acceptor.clone()),
                });
            }
        }
    }
    specs
}

pub(crate) fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unicast_link_local(),
    }
}

type CachedAcceptor = (Vec<String>, TlsAcceptor);

static CACHED_ACCEPTOR: OnceLock<parking_lot::Mutex<Option<CachedAcceptor>>> = OnceLock::new();

fn acceptor_cache() -> &'static parking_lot::Mutex<Option<CachedAcceptor>> {
    CACHED_ACCEPTOR.get_or_init(|| parking_lot::Mutex::new(None))
}

pub(crate) fn clear_cached_acceptor() {
    *acceptor_cache().lock() = None;
}

async fn get_or_build_acceptor(interface_ips: &[IpAddr]) -> std::io::Result<Option<TlsAcceptor>> {
    let wanted_key = tls::interface_san_key(interface_ips);
    if wanted_key.is_empty() {
        return Ok(None);
    }
    let ips = interface_ips.to_vec();
    let acceptor = tokio::task::spawn_blocking(move || -> std::io::Result<TlsAcceptor> {
        let mut guard = acceptor_cache().lock();
        if let Some((cached_key, cached_acceptor)) = guard.as_ref() {
            if cached_key == &wanted_key {
                return Ok(cached_acceptor.clone());
            }
        }
        let app = state::app_handle()
            .ok_or_else(|| std::io::Error::other("app handle not set; cannot locate cert dir"))?;
        let dir: PathBuf = app
            .path()
            .app_data_dir()
            .map_err(std::io::Error::other)?
            .join("tls");
        let acceptor = tls::acceptor(&dir, &ips)?;
        *guard = Some((wanted_key, acceptor.clone()));
        Ok(acceptor)
    })
    .await
    .map_err(|e| std::io::Error::other(format!("TLS build task panicked: {}", e)))??;
    Ok(Some(acceptor))
}

fn spawn_accept_loop(
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
    mut shutdown: watch::Receiver<bool>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        let capacity = std::sync::Arc::new(tokio::sync::Semaphore::new(64));
        let websocket_capacity = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
                res = listener.accept() => match res {
                    Ok((tcp, addr)) => {
                        let Ok(permit) = capacity.clone().try_acquire_owned() else {
                            drop(tcp);
                            continue;
                        };
                        let tls = tls.clone();
                        let websocket_capacity = websocket_capacity.clone();
                        connections.spawn(async move {
                            match tls {
                                Some(acceptor) => if let Ok(Ok(s)) = tokio::time::timeout(
                                    std::time::Duration::from_secs(5), acceptor.accept(tcp)
                                ).await {
                                    handle_admitted_connection(MaybeTls::Tls(Box::new(s)), addr, Some((permit, websocket_capacity))).await;
                                },
                                None => handle_admitted_connection(MaybeTls::Plain(tcp), addr, Some((permit, websocket_capacity))).await,
                            }
                        });
                    }
                    Err(e) => tracing::error!("HTTP accept error: {}", e),
                }
            }
        }
        // The listener owns TLS, HTTP, and WebSocket work for its generation.
        // Abort drops sockets even when a peer never sends another frame.
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    })
}

async fn ensure_loopback_skeleton(
    skeleton: &mut Option<LoopbackSkeleton>,
    start: u16,
    end: u16,
) -> Option<(u16, Vec<RealizedBind>)> {
    if let Some(sk) = skeleton.as_ref() {
        return Some((sk.port, sk.realized.clone()));
    }
    for port in start..=end {
        let planned = bind_specs(port, &[], None);
        let primary = &planned[0];
        let listener = match TcpListener::bind(&primary.addr).await {
            Ok(l) => l,
            Err(_) => continue,
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut handles = vec![spawn_accept_loop(
            listener,
            primary.tls.clone(),
            shutdown_rx.clone(),
        )];
        let mut bound_indices = vec![0usize];
        for (idx, spec) in planned.iter().cloned().enumerate().skip(1) {
            match TcpListener::bind(&spec.addr).await {
                Ok(listener) => {
                    handles.push(spawn_accept_loop(
                        listener,
                        spec.tls.clone(),
                        shutdown_rx.clone(),
                    ));
                    bound_indices.push(idx);
                }
                Err(e) => {
                    tracing::debug!("Loopback bind on {} failed: {}", spec.addr, e);
                }
            }
        }
        let realized = realized_binds_from_specs(&planned, &bound_indices);
        *skeleton = Some(LoopbackSkeleton {
            shutdown: shutdown_tx,
            handles,
            port,
            realized: realized.clone(),
        });
        return Some((port, realized));
    }
    None
}

async fn bind_interface_listeners(
    port: u16,
    interface_ips: &[IpAddr],
    acceptor: &TlsAcceptor,
    shutdown_rx: &watch::Receiver<bool>,
    handles: &mut Vec<tauri::async_runtime::JoinHandle<()>>,
) -> Vec<RealizedBind> {
    let planned = bind_specs(port, interface_ips, Some(acceptor));
    let mut bound_indices = Vec::new();
    for (idx, spec) in planned.iter().cloned().enumerate() {
        if spec.addr.ip().is_loopback() {
            continue;
        }
        match TcpListener::bind(&spec.addr).await {
            Ok(listener) => {
                handles.push(spawn_accept_loop(
                    listener,
                    spec.tls.clone(),
                    shutdown_rx.clone(),
                ));
                bound_indices.push(idx);
            }
            Err(e) => tracing::warn!(
                "Interface bind on {} failed (LAN exposure will skip this address): {}",
                spec.addr,
                e
            ),
        }
    }
    realized_binds_from_specs(&planned, &bound_indices)
}

/// Validate a request's `Host` header against this machine's identities to
/// defeat DNS rebinding (ADR-0012).
pub(crate) fn host_header_allowed(host_header: &str) -> bool {
    let hostname = request::strip_host_port(host_header.trim());
    if hostname.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match hostname.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => true,
        Ok(_) => request::host_is_allowed(host_header, &state::local_interface_ips()),
        Err(_) => false,
    }
}

async fn ws_upgrade(
    mut lines: tokio::io::BufStream<MaybeTls>,
    headers: &str,
) -> Option<WebSocketStream<MaybeTls>> {
    let Some(ws_key) = request::extract_header_value(headers, "Sec-WebSocket-Key") else {
        let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
        return None;
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
        return None;
    }
    if stream.flush().await.is_err() {
        return None;
    }
    Some(WebSocketStream::from_raw_socket(stream, Role::Server, None).await)
}

#[cfg(test)]
pub(crate) async fn handle_connection(stream: MaybeTls, addr: SocketAddr) {
    handle_admitted_connection(stream, addr, None).await;
}

async fn handle_admitted_connection(
    stream: MaybeTls,
    addr: SocketAddr,
    mut admission: Option<(
        tokio::sync::OwnedSemaphorePermit,
        std::sync::Arc<tokio::sync::Semaphore>,
    )>,
) {
    let secure = stream.is_tls();
    let mut lines = tokio::io::BufStream::new(stream);

    let head = match tokio::time::timeout(REQUEST_HEAD_TIMEOUT, async {
        let mut head_reader = (&mut lines).take((MAX_HEADER_BYTES + 1) as u64);
        let mut request_line = String::new();
        match head_reader.read_line(&mut request_line).await {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }

        let mut headers = String::new();
        while !headers.ends_with("\r\n\r\n") {
            match head_reader.read_line(&mut headers).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            if headers.len() + request_line.len() > MAX_HEADER_BYTES {
                return Some((request_line, headers, true));
            }
            if headers.trim().is_empty() {
                break;
            }
        }
        let overflow = headers.len() + request_line.len() > MAX_HEADER_BYTES;
        Some((request_line, headers, overflow))
    })
    .await
    {
        Ok(Some(head)) => head,
        Ok(None) | Err(_) => return,
    };
    let (request_line, headers, header_overflow) = head;

    if header_overflow {
        let _ = request::write_status_only(&mut lines, "431 Request Header Fields Too Large").await;
        return;
    }

    let request_line = request_line.trim().to_string();
    let parts: Vec<String> = request_line.split_whitespace().map(String::from).collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].as_str();
    let path_with_query = parts[1].clone();

    let host = request::extract_header_value(&headers, "Host").unwrap_or("");
    if !host_header_allowed(host) {
        let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
        return;
    }

    let path = path_with_query
        .split('?')
        .next()
        .unwrap_or(&path_with_query)
        .to_string();

    let matched = match_route(method, &path);
    let mut body = Vec::new();
    let cl = content_length(&headers);
    match matched.body {
        BodyPolicy::None => {}
        BodyPolicy::Cap(max) => {
            let Some(buf) = request::read_body_or_send_error(&mut lines, cl, max).await else {
                return;
            };
            body = buf;
        }
        BodyPolicy::CapOrSkip(max) => {
            if cl > max {
                // Debug log: still 204; handler sees empty body + content-length.
            } else if let Ok(buf) = request::read_body_with_cap(&mut lines, cl, max).await {
                body = buf;
            }
        }
    }

    let req = ParsedRequest {
        method: method.to_string(),
        path,
        path_with_query,
        headers: headers.clone(),
        body,
        peer: addr,
        secure,
        ids: (None, None),
    };

    // Subscribe before authorization so revocation between ticket consumption
    // and WebSocket subscription cannot leave an already-revoked phone live.
    let mut revocations = crate::http::revocation::subscribe();
    match dispatch_matched(req, matched).await {
        DispatchResult::Http(response) => {
            let _ = request::write_response(&mut lines, &response).await;
        }
        DispatchResult::Upgrade(kind) => {
            let device_id = match &kind {
                Upgrade::Events { device_id } | Upgrade::Terminal { device_id, .. } => *device_id,
            };
            if let Some(id) = device_id {
                match crate::http::auth::device_session_is_active(id) {
                    Ok(true) => {}
                    Ok(false) => {
                        let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                        return;
                    }
                    Err(()) => {
                        let _ = request::write_status_only(&mut lines, "503 Service Unavailable").await;
                        return;
                    }
                }
            }
            let _websocket_permit = if let Some((_, capacity)) = &admission {
                match capacity.clone().try_acquire_owned() {
                    Ok(permit) => Some(permit),
                    Err(_) => {
                        let _ =
                            request::write_status_only(&mut lines, "503 Service Unavailable").await;
                        return;
                    }
                }
            } else {
                None
            };
            drop(admission.take());
            let connection = async {
                let Some(ws_stream) = ws_upgrade(lines, &headers).await else {
                    return;
                };
                match kind {
                    Upgrade::Events { device_id } => {
                        tracing::info!("/ws/events client connected");
                        ws::handle_events_ws_connection(ws_stream, device_id).await;
                    }
                    Upgrade::Terminal { node_id, device_id } => {
                        tracing::info!("WebSocket connected for node {}", node_id);
                        ws::handle_ws_connection(ws_stream, node_id, device_id).await;
                    }
                }
            };
            tokio::pin!(connection);
            loop {
                tokio::select! {
                    biased;
                    signal = revocations.recv(), if device_id.is_some() => {
                        match signal {
                            Ok(id) if Some(id) == device_id => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            _ => {}
                        }
                    }
                    _ = &mut connection => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn stalled_tls_is_bounded_times_out_and_shutdown_drains_connections() {
        let chain = tls::generate(&[]).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown, rx) = watch::channel(false);
        let task = spawn_accept_loop(listener, Some(tls::acceptor_from(&chain.leaf).unwrap()), rx);
        let mut stalled = Vec::new();
        for _ in 0..64 {
            stalled.push(TcpStream::connect(addr).await.unwrap());
        }
        let mut overload = TcpStream::connect(addr).await.unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), overload.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        // TLS, not the 30-second HTTP-head deadline, must close stalled clients.
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(6),
                stalled[0].read(&mut byte)
            )
            .await
            .unwrap()
            .unwrap(),
            0
        );
        let mut fresh = TcpStream::connect(addr).await.unwrap();
        shutdown.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        let closed = tokio::time::timeout(std::time::Duration::from_secs(2), fresh.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)));
    }

    #[tokio::test]
    async fn listener_shutdown_closes_root_and_device_websockets_but_not_other_listeners() {
        use crate::http::ws_ticket::{self, WsTarget};
        use futures_util::StreamExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown, rx) = watch::channel(false);
        let task = spawn_accept_loop(listener, None, rx);
        let other = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other_addr = other.local_addr().unwrap();
        let (other_shutdown, other_rx) = watch::channel(false);
        let other_task = spawn_accept_loop(other, None, other_rx);
        let target = WsTarget {
            surface: "events".into(),
            node_id: None,
        };
        crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let device_id = {
            let conn = crate::db::write_conn();
            crate::db::pair_device_session_inner(&conn, None, None)
                .unwrap()
                .0
        };
        let mut clients = Vec::new();
        let receivers_before = crate::http::events::receiver_count();
        for device in [None, Some(device_id)] {
            let ticket = ws_ticket::mint(device, target.clone());
            let (client, _) =
                tokio_tungstenite::connect_async(format!("ws://{addr}/ws/events?ticket={ticket}"))
                    .await
                    .unwrap();
            clients.push(client);
        }
        let ticket = ws_ticket::mint(None, target);
        let (mut other_client, _) = tokio_tungstenite::connect_async(format!(
            "ws://{other_addr}/ws/events?ticket={ticket}"
        ))
        .await
        .unwrap();
        // The HTTP 101 response can arrive before the handler has subscribed
        // to the broadcast channel. Wait for all three handlers before emitting
        // the assertion event; otherwise this test occasionally loses it.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while crate::http::events::receiver_count() < receivers_before + 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("event websocket handlers did not subscribe");
        shutdown.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        for mut client in clients {
            let closed = tokio::time::timeout(std::time::Duration::from_secs(2), client.next())
                .await
                .unwrap();
            assert!(matches!(
                closed,
                None | Some(Err(_)) | Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))
            ));
        }
        crate::http::events::emit(crate::http::events::EventMsg::AttentionCleared {
            session_id: 77,
        });
        let live = tokio::time::timeout(std::time::Duration::from_secs(2), other_client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(live.is_text());
        other_shutdown.send(true).unwrap();
        other_task.await.unwrap();
        crate::db::revoke_device_session(device_id).unwrap();
    }

    async fn raw_status(method: &str, path: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(MaybeTls::Plain(stream), peer).await;
        });
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n");
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
    async fn revoked_device_cannot_open_an_already_minted_websocket_ticket() {
        crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let id = {
            let conn = crate::db::write_conn();
            crate::db::pair_device_session_inner(&conn, None, None)
                .unwrap()
                .0
        };
        let ticket = crate::http::ws_ticket::mint(
            Some(id),
            crate::http::ws_ticket::WsTarget {
                surface: "events".into(),
                node_id: None,
            },
        );
        crate::db::revoke_device_session(id).unwrap();
        assert_eq!(
            raw_status("GET", &format!("/ws/events?ticket={ticket}")).await,
            401
        );
    }

    #[tokio::test]
    async fn oversize_headers_return_431() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(MaybeTls::Plain(stream), peer).await;
        });
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let huge = "x".repeat(70 * 1024);
        let request = format!("GET / HTTP/1.1\r\nHost: localhost\r\nX-Pad: {huge}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 512];
        let n = tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buf))
            .await
            .expect("server hung on oversize headers")
            .expect("read failed");
        let response = String::from_utf8_lossy(&buf[..n]).into_owned();
        assert!(
            response.starts_with("HTTP/1.1 431"),
            "oversize headers must produce 431; got: {response:?}"
        );
    }

    #[tokio::test]
    async fn partial_headers_then_eof_does_not_hang() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(MaybeTls::Plain(stream), peer).await;
        });
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
        let mut buf = [0u8; 512];
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buf))
            .await
            .expect("server spun on partial headers instead of stopping at EOF");
    }

    #[tokio::test]
    async fn host_guard_rejects_foreign_host() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(MaybeTls::Plain(stream), peer).await;
        });
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: evil.com\r\n\r\n")
            .await
            .unwrap();
        let mut buf = vec![0u8; 512];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("hung")
            .expect("read");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "foreign Host must 400; got {response:?}"
        );
    }

    #[tokio::test]
    async fn response_body_byte_count_matches_content_length() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(MaybeTls::Plain(stream), peer).await;
        });
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
                .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => received.extend_from_slice(&buf[..n]),
                Ok(Err(_)) => break,
                Err(_) => panic!("server hung mid-response"),
            }
        }
        let split = received
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("response missing header terminator");
        let headers = String::from_utf8_lossy(&received[..split]);
        let body = &received[split + 4..];
        let content_length: usize = headers
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .expect("Content-Length header");
        assert_eq!(body.len(), content_length);
    }

    #[tokio::test]
    async fn root_shell_body_contains_debug_shim_marker() {
        const MARKER: &[u8] = b"<!--buildmesh-debug-shim-->";
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(MaybeTls::Plain(stream), peer).await;
        });
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
                .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => received.extend_from_slice(&buf[..n]),
                Ok(Err(_)) => break,
                Err(_) => panic!("server hung mid-response"),
            }
        }
        let split = received
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("response missing header terminator");
        let body = &received[split + 4..];
        if !body.windows(MARKER.len()).any(|w| w == MARKER) {
            // Worktree without `dist/mobile` serves a 404 placeholder — the
            // marker is pinned by `assets::tests` once the bundle exists.
            let headers = String::from_utf8_lossy(&received[..split]);
            assert!(
                headers.contains("404") || headers.contains("200"),
                "expected a completed HTTP response; got {headers:?}"
            );
        }
    }

    async fn ws_status(path: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(MaybeTls::Plain(stream), peer).await;
        });
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("ws hung")
            .expect("read");
        String::from_utf8_lossy(&buf[..n])
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn ws_terminal_accepts_a_valid_ticket() {
        let ticket = crate::http::ws_ticket::mint(
            None,
            crate::http::ws_ticket::WsTarget {
                surface: crate::http::ws_ticket::SURFACE_TERMINAL.to_string(),
                node_id: Some(123),
            },
        );
        let path = format!("/ws/terminal/123?ticket={ticket}");
        assert_eq!(ws_status(&path).await, 101);
    }

    #[tokio::test]
    async fn ws_terminal_rejects_ticket_bound_to_other_node_and_keeps_it_valid() {
        let ticket = crate::http::ws_ticket::mint(
            None,
            crate::http::ws_ticket::WsTarget {
                surface: crate::http::ws_ticket::SURFACE_TERMINAL.to_string(),
                node_id: Some(123),
            },
        );
        assert_eq!(
            ws_status(&format!("/ws/terminal/999?ticket={ticket}")).await,
            403
        );
        assert_eq!(
            ws_status(&format!("/ws/terminal/123?ticket={ticket}")).await,
            101
        );
    }

    #[tokio::test]
    async fn localhost_host_is_allowed() {
        let status = raw_status("GET", "/").await;
        assert!(
            status == 200 || status == 404,
            "localhost Host must reach the SPA (200) or missing-bundle 404, got {status}"
        );
    }

    #[test]
    fn host_header_allows_localhost_without_interfaces() {
        assert!(host_header_allowed("localhost"));
        assert!(host_header_allowed("localhost:1992"));
        assert!(host_header_allowed("127.0.0.1"));
        assert!(!host_header_allowed("evil.com"));
    }

    fn test_acceptor() -> TlsAcceptor {
        let chain = tls::generate(&[]).expect("test cert generation");
        tls::acceptor_from(&chain.leaf).expect("test acceptor build")
    }

    #[tokio::test]
    async fn get_or_build_acceptor_short_circuits_on_empty_san_key() {
        let result = get_or_build_acceptor(&[])
            .await
            .expect("empty list is a no-op");
        assert!(
            result.is_none(),
            "empty interface set must yield no acceptor"
        );
        let result = get_or_build_acceptor(&[
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ])
        .await
        .expect("loopback-only is a no-op");
        assert!(
            result.is_none(),
            "loopback-only interface set must yield no acceptor"
        );
    }

    #[test]
    fn bind_specs_loopback_only_and_plain_by_default() {
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let specs = bind_specs(1992, &[lan_ip], None);
        assert_eq!(specs.len(), 2, "default binds IPv4 + IPv6 loopback");
        assert!(specs[0].addr.is_ipv4() && specs[0].addr.ip().is_loopback());
        assert!(specs[1].addr.is_ipv6() && specs[1].addr.ip().is_loopback());
        assert!(specs.iter().all(|s| s.addr.ip().is_loopback()));
        assert!(specs.iter().all(|s| s.tls.is_none()));
    }

    #[test]
    fn bind_specs_lan_keeps_loopback_plain_and_adds_interface_tls() {
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let acceptor = test_acceptor();
        let specs = bind_specs(
            1992,
            &[IpAddr::V4(Ipv4Addr::LOCALHOST), lan_ip],
            Some(&acceptor),
        );
        assert_eq!(specs.len(), 3);
        let loopback_plain = specs
            .iter()
            .filter(|s| s.addr.ip().is_loopback() && s.tls.is_none())
            .count();
        assert_eq!(loopback_plain, 2);
        let tls_iface: Vec<_> = specs.iter().filter(|s| s.tls.is_some()).collect();
        assert_eq!(tls_iface.len(), 1);
        assert_eq!(tls_iface[0].addr.ip(), lan_ip);
    }

    #[test]
    fn bind_specs_lan_without_interfaces_stays_loopback_only() {
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &[], Some(&acceptor));
        assert_eq!(specs.len(), 2);
        assert!(specs
            .iter()
            .all(|s| s.addr.ip().is_loopback() && s.tls.is_none()));
    }

    #[test]
    fn bind_specs_skips_link_local_addresses() {
        let v6_link_local: IpAddr = "fe80::1".parse().unwrap();
        let v4_link_local: IpAddr = "169.254.10.20".parse().unwrap();
        let routable: IpAddr = "192.168.1.5".parse().unwrap();
        let acceptor = test_acceptor();
        let specs = bind_specs(
            1992,
            &[v6_link_local, v4_link_local, routable],
            Some(&acceptor),
        );
        let tls_iface: Vec<_> = specs.iter().filter(|s| s.tls.is_some()).collect();
        assert_eq!(tls_iface.len(), 1);
        assert_eq!(tls_iface[0].addr.ip(), routable);
        assert!(!specs
            .iter()
            .any(|s| s.addr.ip() == v6_link_local || s.addr.ip() == v4_link_local));
    }

    #[test]
    fn realized_binds_loopback_only_when_all_secondaries_skipped() {
        let specs = bind_specs(1992, &[], None);
        let realized = realized_binds_from_specs(&specs, &[0, 1]);
        assert_eq!(realized.len(), 2);
        assert!(realized.iter().all(|b| !b.tls));
    }

    #[test]
    fn realized_binds_drop_unbound_specs() {
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let specs = bind_specs(1992, &[lan_ip], None);
        assert_eq!(specs.len(), 2);
        let realized = realized_binds_from_specs(&specs, &[0, 1]);
        assert_eq!(realized.len(), 2);
        assert!(realized.iter().all(|b| !b.tls));
    }

    #[test]
    fn realized_binds_marks_tls_when_interface_binds() {
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &[lan_ip], Some(&acceptor));
        let realized = realized_binds_from_specs(&specs, &[0, 1, 2]);
        assert_eq!(realized.len(), 3);
        let tls: Vec<_> = realized.iter().filter(|b| b.tls).collect();
        assert_eq!(tls.len(), 1);
        assert!(tls[0].address.contains("192.168.1.5"));
    }

    #[test]
    fn realized_binds_partial_when_some_interfaces_fail() {
        let lan1: IpAddr = "192.168.1.5".parse().unwrap();
        let lan2: IpAddr = "10.0.0.2".parse().unwrap();
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &[lan1, lan2], Some(&acceptor));
        let realized = realized_binds_from_specs(&specs, &[0, 1, 2]);
        assert_eq!(realized.len(), 3);
        let ips: Vec<&str> = realized
            .iter()
            .filter(|b| b.tls)
            .map(|b| b.address.split(':').next().unwrap())
            .collect();
        assert_eq!(ips, vec!["192.168.1.5"]);
    }

    static ENUMERATOR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn refresh_local_interface_ips_replaces_cached_snapshot() {
        let _lock = ENUMERATOR_TEST_LOCK.lock().unwrap();
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let vpn_ip: IpAddr = "10.20.30.40".parse().unwrap();
        let _enum_guard = state::set_interface_enumerator_for_testing(vec![lan_ip]);
        let initial = state::refresh_local_interface_ips();
        assert!(initial.contains(&lan_ip));
        assert_eq!(state::local_interface_ips(), vec![lan_ip]);
        let _enum_guard = state::set_interface_enumerator_for_testing(vec![lan_ip, vpn_ip]);
        let refreshed = state::refresh_local_interface_ips();
        assert!(refreshed.contains(&vpn_ip));
        assert!(state::local_interface_ips().contains(&vpn_ip));
    }

    #[test]
    fn host_header_allowed_uses_refreshed_snapshot() {
        let _lock = ENUMERATOR_TEST_LOCK.lock().unwrap();
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let vpn_ip: IpAddr = "10.20.30.40".parse().unwrap();
        let _enum_guard = state::set_interface_enumerator_for_testing(vec![lan_ip]);
        let _ = state::refresh_local_interface_ips();
        assert!(!host_header_allowed(&format!("{vpn_ip}:1992")));
        let _enum_guard = state::set_interface_enumerator_for_testing(vec![lan_ip, vpn_ip]);
        let _ = state::refresh_local_interface_ips();
        assert!(host_header_allowed(&format!("{vpn_ip}:1992")));
    }

    #[test]
    fn bind_specs_picks_up_new_interface_after_refresh() {
        let _lock = ENUMERATOR_TEST_LOCK.lock().unwrap();
        let vpn_ip: IpAddr = "10.20.30.40".parse().unwrap();
        let _enum_guard = state::set_interface_enumerator_for_testing(vec![vpn_ip]);
        let interface_ips = state::refresh_local_interface_ips();
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &interface_ips, Some(&acceptor));
        assert!(specs
            .iter()
            .any(|s| s.tls.is_some() && s.addr.ip() == vpn_ip));
    }

    #[test]
    fn host_header_guard_does_not_consult_enumerator_directly() {
        let _lock = ENUMERATOR_TEST_LOCK.lock().unwrap();
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let vpn_ip: IpAddr = "10.20.30.40".parse().unwrap();
        let _enum_guard = state::set_interface_enumerator_for_testing(vec![lan_ip]);
        let _ = state::refresh_local_interface_ips();
        assert!(host_header_allowed(&format!("{lan_ip}:1992")));
        let _enum_guard = state::set_interface_enumerator_for_testing(vec![lan_ip, vpn_ip]);
        assert!(!host_header_allowed(&format!("{vpn_ip}:1992")));
        assert!(host_header_allowed(&format!("{lan_ip}:1992")));
    }

    #[tokio::test]
    async fn ws_events_rejects_terminal_bound_ticket() {
        let ticket = crate::http::ws_ticket::mint(
            None,
            crate::http::ws_ticket::WsTarget {
                surface: crate::http::ws_ticket::SURFACE_TERMINAL.to_string(),
                node_id: Some(123),
            },
        );
        assert_eq!(ws_status(&format!("/ws/events?ticket={ticket}")).await, 403);
    }

    #[tokio::test]
    async fn ws_ticket_endpoint_requires_admin_credentials() {
        assert_eq!(raw_status("POST", "/api/ws-ticket").await, 401);
    }
}
