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
pub mod stream;
pub mod tls;
pub mod ws;
pub mod ws_ticket;

pub use stream::MaybeTls;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use parking_lot::RwLock;
use tauri::{Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use ts_rs::TS;

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

/// The most recently applied port offset, so a live rebind (LAN toggle) reuses
/// the same range the server first started on (stable → 0, dev → 1000).
static PORT_OFFSET: AtomicU16 = AtomicU16::new(0);

/// The persistent loopback skeleton. Bound once at startup and never torn
/// down on a LAN toggle — the local attention webhook posts plain HTTP to
/// 127.0.0.1, and closing that socket on a settings change would briefly drop
/// notifications (issue #587). Stores the realized loopback binds so a
/// post-toggle `get_network_status` can still report "loopback only" when
/// the interface side is empty.
struct LoopbackSkeleton {
    // `shutdown` + `handles` are kept for the future graceful-shutdown path
    // (signal the loopback accept loops, await their exit). Today the OS
    // process exit drops the runtime, which drops the listeners, so the
    // fields are stored but not read. Marking `dead_code` rather than
    // removing them: the design intent — "the skeleton is structurally
    // teardown-able if we ever need to" — is part of the contract.
    #[allow(dead_code)]
    shutdown: watch::Sender<bool>,
    #[allow(dead_code)]
    handles: Vec<tauri::async_runtime::JoinHandle<()>>,
    port: u16,
    realized: Vec<RealizedBind>,
}

/// Interface TLS listeners, replaced wholesale on every LAN toggle. Empty
/// while LAN exposure is off.
struct InterfaceBindings {
    shutdown: Option<watch::Sender<bool>>,
    handles: Vec<tauri::async_runtime::JoinHandle<()>>,
}

/// The live listeners. Held behind an async mutex so concurrent toggles
/// serialize. The split into a persistent skeleton + replaceable interface
/// bindings is what lets a LAN toggle rebind ONLY the interface listeners
/// (issue #587) — a single shared shutdown would force a full teardown
/// every time, briefly closing the loopback socket the attention hook
/// posts to.
struct ServerListeners {
    skeleton: Option<LoopbackSkeleton>,
    interface: InterfaceBindings,
}

static SERVER_LISTENERS: OnceLock<tokio::sync::Mutex<ServerListeners>> = OnceLock::new();

fn server_listeners() -> &'static tokio::sync::Mutex<ServerListeners> {
    SERVER_LISTENERS.get_or_init(|| {
        tokio::sync::Mutex::new(ServerListeners {
            skeleton: None,
            interface: InterfaceBindings {
                shutdown: None,
                handles: Vec::new(),
            },
        })
    })
}

/// Start the HTTP server, trying ports 1992→1993→1994 until one binds.
/// Emits a `remote-access-port` event with the actual port used so the
/// QR code modal can update without recompiling.
pub fn start_http_server(app: tauri::AppHandle, port_offset: u16) {
    let _ = APP_HANDLE.set(app);
    // Publish the offset synchronously, before spawning the initial bind, so a
    // `reapply_binding` fired by an early LAN toggle rebinds on the right range.
    // This matters for the dev profile (+1000): a toggle that raced the startup
    // task would otherwise read the default offset 0 and land on the stable
    // hub's ports (1992–1994 instead of 2992–2994).
    PORT_OFFSET.store(port_offset, Ordering::SeqCst);
    tauri::async_runtime::spawn(async move {
        apply_binding(port_offset).await;
    });
}

/// Re-evaluate the LAN-exposure setting and rebind the listeners live. Called by
/// `set_lan_exposure_enabled` so flipping the toggle takes effect immediately —
/// switching between loopback-only plain HTTP and loopback-plus-interface TLS —
/// without restarting the app.
pub async fn reapply_binding() {
    let port_offset = PORT_OFFSET.load(Ordering::SeqCst);
    apply_binding(port_offset).await;
}

/// Tear down any existing listeners and bind fresh ones for the current
/// `lan_exposure_enabled` setting. Idempotent and safe to call repeatedly.
///
/// Architecture (issue #587):
/// 1. Build / look up the TLS acceptor **off** the listeners mutex — the
///    underlying cert read + keygen is blocking (`spawn_blocking` inside
///    `get_or_build_acceptor`) and would otherwise stall every concurrent
///    `get_network_status` call. The result is cached by interface-IP set
///    so a re-toggle with the same set is free.
/// 2. The **loopback skeleton** is bound once at startup and never torn
///    down on a LAN toggle. Only the interface TLS listeners are replaced
///    on each toggle — that keeps the 127.0.0.1 socket the attention hook
///    posts to alive across a settings change.
/// 3. If the skeleton can't bind ANY port in the range, revert the DB
///    flag so the UI doesn't show "on" while the server has zero listeners.
async fn apply_binding(port_offset: u16) {
    PORT_OFFSET.store(port_offset, Ordering::SeqCst);
    let start = HTTP_PORT_START + port_offset;
    let end = HTTP_PORT_END + port_offset;

    // Secure default (issue #496 / ADR-0012): bind loopback only. Exposing the
    // server to the LAN is an explicit opt-in stored in the DB (issue #501);
    // until then external machines cannot reach the hub.
    let lan_enabled = crate::db::lan_exposure_enabled().unwrap_or(false);
    // Re-enumerate at bind time (issue #585) so a VPN/Wi-Fi adapter that
    // appeared AFTER the first enumeration is bound and covered by the cert
    // SANs. The per-request Host guard still reads the cached snapshot — see
    // `local_interface_ips` for the hot-path contract.
    let interface_ips: Vec<IpAddr> = if lan_enabled {
        refresh_local_interface_ips()
    } else {
        Vec::new()
    };

    // Build/get the TLS acceptor OFF the listeners mutex. The cache is
    // keyed by `tls::interface_san_key` so a re-toggle with the same
    // interface set reuses the previously built acceptor instead of
    // re-reading the DER + re-parsing the ServerConfig (issue #587). A
    // build failure logs and degrades to loopback-only — the UI surfaces
    // that as "enabled but nothing exposed" (issue #586).
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

    // Hold the listeners lock for the whole teardown→bind→store cycle so two
    // rapid toggles (or a toggle racing the startup bind) serialize fully.
    // Releasing it across the bind await let a second call tear down + bind the
    // same port and then overwrite the first call's stored handles, leaking the
    // first call's accept loops (never signalled to stop) and leaving
    // RESOLVED_HTTP_PORT out of sync with the surviving listeners.
    let mut state = server_listeners().lock().await;

    // Tear down the previous interface listeners. The loopback skeleton is
    // explicitly NOT torn down here (issue #587) — the attention hook posts
    // to 127.0.0.1, and closing that socket on every LAN toggle is the
    // half-second "agent says done, hub doesn't notice" gap this PR fixes.
    if let Some(tx) = state.interface.shutdown.take() {
        let _ = tx.send(true);
    }
    for handle in state.interface.handles.drain(..) {
        let _ = handle.await;
    }

    // Ensure the loopback skeleton is bound (startup) or reuse the existing
    // one (subsequent toggles). If we can't bind ANY port in the range —
    // startup failure, all 1992–1994 taken — revert the DB flag so the UI
    // doesn't show "on" while the server has zero listeners (issue #587).
    let (skeleton_port, skeleton_realized) =
        match ensure_loopback_skeleton(&mut state.skeleton, start, end).await {
            Some((port, realized)) => (port, realized),
            None => {
                if lan_enabled {
                    if let Err(e) = crate::db::set_lan_exposure_enabled(false) {
                        tracing::warn!("Failed to revert LAN exposure flag after bind failure: {}", e);
                    }
                }
                realized_binds_store().write().clear();
                tracing::error!("Failed to bind HTTP server on any port {}–{}", start, end);
                return;
            }
        };
    RESOLVED_HTTP_PORT.store(skeleton_port, Ordering::SeqCst);
    if let Some(app) = app_handle() {
        let _ = app.emit("remote-access-port", serde_json::json!({ "port": skeleton_port }));
    }

    // Bind the interface listeners on top of the skeleton (best-effort per
    // interface — a single failed bind warns and continues, matching the
    // pre-refactor behaviour). When the acceptor is None the interface
    // list is empty, so the realized snapshot is just the loopback.
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
        state.interface.shutdown = Some(shutdown_tx);
        state.interface.handles = handles;
        realized
    } else {
        Vec::new()
    };

    // Realized snapshot = skeleton's loopback binds + any interface binds
    // that succeeded. This is the single source of truth the Settings UI
    // uses to surface "no interfaces are actually exposed" (issue #586).
    let mut all_realized = skeleton_realized;
    all_realized.extend(interface_realized);
    // Derive the scope message from the realized snapshot rather than
    // recomputing `lan_enabled && has_non_loopback_interface` — keeps the
    // log + the UI in lockstep (issue #587). Read `needs_tls` BEFORE
    // moving `all_realized` into the store.
    let needs_tls = all_realized.iter().any(|b| b.tls);
    *realized_binds_store().write() = all_realized;

    let scope = if needs_tls {
        "loopback (HTTP) + LAN interfaces (HTTPS)"
    } else if lan_enabled {
        "loopback only (no non-loopback interface to expose)"
    } else {
        "loopback only"
    };
    tracing::info!("HTTP server listening on port {} ({})", skeleton_port, scope);
}

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

/// One listener the server actually opened, exposed to the Settings UI so the
/// toggle can reflect *realized* exposure rather than just DB intent (issue
/// #586). When the DB says LAN exposure is on but `tls_active` is false or
/// `exposed_interfaces` is empty, the UI shows a "no interfaces are actually
/// exposed" warning instead of letting the user hand their phone a dead URL.
///
/// Generated to `src/types/generated/RealizedBind.ts` (issue #359).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, TS)]
#[ts(export, export_to = "RealizedBind.ts")]
pub struct RealizedBind {
    /// `ip:port` form (e.g. `192.168.1.5:1992`) — the frontend uses it to
    /// construct both the HTTPS and HTTP URL the user types into their phone.
    pub address: String,
    /// True iff this listener is serving HTTPS/WSS. Loopback listeners are
    /// always plain (the local attention webhook posts plain `http://localhost`).
    pub tls: bool,
}

/// Live listeners currently bound, tracked so `get_network_status` can report
/// realized exposure instead of DB intent (issue #586). Updated by
/// `apply_binding` on every rebind; cleared when no port bound. Accessed via
/// `realized_binds()` which clones the snapshot for the IPC response.
static REALIZED_BINDS: OnceLock<parking_lot::RwLock<Vec<RealizedBind>>> = OnceLock::new();

fn realized_binds_store() -> &'static parking_lot::RwLock<Vec<RealizedBind>> {
    REALIZED_BINDS.get_or_init(|| parking_lot::RwLock::new(Vec::new()))
}

/// Snapshot of the currently bound non-loopback listeners (issue #586).
pub fn realized_binds() -> Vec<RealizedBind> {
    realized_binds_store().read().clone()
}

/// Pure: convert a planned spec set + the indices that bound into the wire
/// shape the UI consumes. Kept as a free function so the conversion is unit-
/// testable without actually opening sockets — the bind loop is the only thing
/// that should call this, after attempting every spec in order.
fn realized_binds_from_specs(specs: &[BindSpec], bound_indices: &[usize]) -> Vec<RealizedBind> {
    bound_indices
        .iter()
        .map(|&i| RealizedBind {
            address: specs[i].addr.to_string(),
            tls: specs[i].tls.is_some(),
        })
        .collect()
}

/// The listeners to open for `port`. The first two entries are load-bearing
/// loopback plain HTTP: at least the IPv4 one must bind for the port to count
/// as taken, and the local attention webhook posts plain `http://localhost`
/// through them. With `acceptor.is_some()` (LAN exposure on AND a usable
/// non-loopback interface), each non-loopback, non-link-local interface IP
/// becomes a TLS listener carrying that acceptor — the same reachability as
/// binding `0.0.0.0`, but loopback stays plain.
///
/// Folding the acceptor into the spec (issue #587) is what lets the bind
/// loop treat TLS/plain uniformly: every TLS-intended listener carries
/// `Some(acceptor)` and the runtime "no acceptor" check disappears.
pub fn bind_specs(port: u16, interface_ips: &[IpAddr], acceptor: Option<&TlsAcceptor>) -> Vec<BindSpec> {
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

/// Link-local addresses (IPv6 `fe80::/10`, IPv4 `169.254.0.0/16` APIPA) are only
/// reachable on the originating link and need a zone/scope id the remote phone
/// can't share. Binding/advertising them as exposed interfaces hands the QR an
/// unreachable URL (e.g. `https://[fe80::…]:1992`), which mobile browsers reject
/// with `ERR_INVALID_ARGUMENT`. We never expose them for LAN remote access.
fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unicast_link_local(),
    }
}

/// The acceptor cache. `TlsAcceptor` wraps an `Arc<ServerConfig>` so cloning
/// it is cheap; the cache avoids re-reading the DER + re-parsing the
/// `ServerConfig` on every toggle when the interface set hasn't changed
/// (issue #587). Keyed by `tls::interface_san_key` so a network change
/// (new VPN IP, dropped interface) forces a rebuild — the same key the
/// persisted cert sidecar is checked against, so the cached acceptor is
/// guaranteed to still be valid for the current interface set.
type CachedAcceptor = (Vec<String>, TlsAcceptor);

static CACHED_ACCEPTOR: OnceLock<parking_lot::Mutex<Option<CachedAcceptor>>> = OnceLock::new();

fn acceptor_cache() -> &'static parking_lot::Mutex<Option<CachedAcceptor>> {
    CACHED_ACCEPTOR.get_or_init(|| parking_lot::Mutex::new(None))
}

/// Get the TLS acceptor for `interface_ips`, reusing the cached one if its
/// SAN key still matches. Otherwise rebuild — load or generate the persisted
/// cert + parse it into a `ServerConfig` — **off-thread** so the blocking
/// disk I/O + RSA keygen never stall the async runtime. Returns `None` when
/// no non-loopback interface exists (no TLS is needed).
async fn get_or_build_acceptor(interface_ips: &[IpAddr]) -> std::io::Result<Option<TlsAcceptor>> {
    let wanted_key = tls::interface_san_key(interface_ips);
    if wanted_key.is_empty() {
        return Ok(None);
    }
    // Fast path: cache hit.
    {
        let guard = acceptor_cache().lock();
        if let Some((cached_key, cached_acceptor)) = guard.as_ref() {
            if cached_key == &wanted_key {
                return Ok(Some(cached_acceptor.clone()));
            }
        }
    }
    // Slow path: rebuild. `interface_ips` is small (a handful of IPs) so the
    // Vec clone into the blocking task is negligible.
    let ips = interface_ips.to_vec();
    let acceptor = tokio::task::spawn_blocking(move || -> std::io::Result<TlsAcceptor> {
        let app = app_handle().ok_or_else(|| {
            std::io::Error::other("app handle not set; cannot locate cert dir")
        })?;
        let dir: PathBuf = app
            .path()
            .app_data_dir()
            .map_err(std::io::Error::other)?
            .join("tls");
        tls::acceptor(&dir, &ips)
    })
    .await
    .map_err(|e| std::io::Error::other(format!("TLS build task panicked: {}", e)))??;
    *acceptor_cache().lock() = Some((wanted_key, acceptor.clone()));
    Ok(Some(acceptor))
}

/// Spawn the accept loop for one bound listener. `tls` wraps each accepted
/// connection in a TLS handshake before dispatch; `None` serves plain HTTP. The
/// loop exits when `shutdown` flips to `true` (or its sender drops), dropping
/// the listener so the port frees for a rebind.
fn spawn_accept_loop(
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
    mut shutdown: watch::Receiver<bool>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    // Sender signalled (true) or dropped → stop accepting.
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                res = listener.accept() => match res {
                    Ok((tcp, addr)) => {
                        tracing::debug!("HTTP connection from {}", addr);
                        let tls = tls.clone();
                        tauri::async_runtime::spawn(async move {
                            match tls {
                                Some(acceptor) => match acceptor.accept(tcp).await {
                                    Ok(s) => {
                                        handle_connection(MaybeTls::Tls(Box::new(s)), addr).await;
                                    }
                                    Err(e) => {
                                        tracing::debug!("TLS handshake from {} failed: {}", addr, e);
                                    }
                                },
                                None => handle_connection(MaybeTls::Plain(tcp), addr).await,
                            }
                        });
                    }
                    Err(e) => tracing::error!("HTTP accept error: {}", e),
                }
            }
        }
    })
}

/// Bind the loopback skeleton on the first free port in `[start, end]` if it
/// isn't already bound, returning the port and the realized loopback binds.
/// On subsequent calls (after the first successful bind) this is a cheap
/// clone of the stored `port` and `realized` — the loopback listeners stay
/// alive across LAN toggles (issue #587), so the 127.0.0.1 socket the
/// attention hook posts to is never closed by a settings change.
///
/// The primary loopback listener (IPv4 127.0.0.1) is load-bearing: if it
/// can't bind, that port is taken and we move to the next. The IPv6
/// loopback is best-effort — a host with IPv6 disabled still serves over
/// 127.0.0.1, and `realized_binds_from_specs` reflects only what actually
/// bound.
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
        // Primary (IPv4 loopback) must bind. If it can't, the port is in use.
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

/// Bind the interface TLS listeners on top of an already-bound loopback
/// skeleton. Each non-loopback spec is best-effort: a single failed bind
/// warns and continues, matching the pre-refactor behaviour, so a host
/// where one interface is down still exposes the others. Returns the
/// realized interface binds (loopback binds are excluded — the caller
/// combines them with the skeleton's realized).
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
            // The skeleton owns loopback; never re-bind it here.
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
            // Per-interface bind failure is the user-visible "exposure is
            // on but the LAN IP didn't actually bind" signal. Was debug,
            // now warn so the cause surfaces in user logs (issue #586).
            Err(e) => tracing::warn!(
                "Interface bind on {} failed (LAN exposure will skip this address): {}",
                spec.addr,
                e
            ),
        }
    }
    realized_binds_from_specs(&planned, &bound_indices)
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

/// Snapshot override for `enumerate_interfaces`. `None` means "use the system
/// call"; tests install a deterministic value so they can simulate a VPN
/// adapter appearing later in the session (issue #585). The override is read
/// by `enumerate_interfaces`, which is called from `refresh_local_interface_ips`
/// (the bind path). The per-request Host guard does NOT consult the override
/// directly — it reads the cached `LOCAL_IPS`, which is only updated by
/// `refresh_local_interface_ips`. Tests that want the hot path to see the
/// override must therefore call `refresh_local_interface_ips` after installing
/// the override; otherwise the hot path sees whatever was last refreshed.
static INTERFACE_SNAPSHOT_OVERRIDE: std::sync::Mutex<Option<Vec<IpAddr>>> =
    std::sync::Mutex::new(None);

/// RAII guard returned by `set_interface_enumerator_for_testing`. Drops restore
/// the override to the value it had before the guard was created, so test
/// state never leaks to a later test that doesn't install its own override.
#[cfg(test)]
pub(crate) struct TestEnumeratorGuard {
    prev: Option<Vec<IpAddr>>,
}

#[cfg(test)]
impl Drop for TestEnumeratorGuard {
    fn drop(&mut self) {
        // Restore even on panic — the override is process-global, so an
        // unwinding test would otherwise leave stale data for the next one.
        *INTERFACE_SNAPSHOT_OVERRIDE
            .lock()
            .expect("interface snapshot override lock poisoned") = self.prev.take();
    }
}

/// Install a snapshot override for `enumerate_interfaces`. Returns an RAII
/// guard whose `Drop` restores the prior value — bind it to `let _g = ...`
/// so test runs are isolated regardless of panic or test ordering.
#[cfg(test)]
pub(crate) fn set_interface_enumerator_for_testing(
    ips: Vec<IpAddr>,
) -> TestEnumeratorGuard {
    let prev = INTERFACE_SNAPSHOT_OVERRIDE
        .lock()
        .expect("interface snapshot override lock poisoned")
        .replace(ips);
    TestEnumeratorGuard { prev }
}

/// Enumerate the host's interface IPs. Honours a test override if set; otherwise
/// reads the system via `local_ip_address::list_afinet_netifas`. Failures log
/// a warning and return an empty list — the bind path then sees no LAN
/// interfaces, which is a safe degrade (loopback still binds).
fn enumerate_interfaces() -> Vec<IpAddr> {
    let override_value = INTERFACE_SNAPSHOT_OVERRIDE
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    if let Some(ips) = override_value {
        return ips;
    }
    match local_ip_address::list_afinet_netifas() {
        Ok(v) => v.into_iter().map(|(_, ip)| ip).collect(),
        Err(e) => {
            tracing::warn!("interface enumeration failed: {}", e);
            Vec::new()
        }
    }
}

/// The cached interface snapshot. Refreshed by `refresh_local_interface_ips`
/// on each bind; the per-request Host guard reads it via `local_interface_ips`
/// without paying for enumeration (which can stall for seconds behind a
/// VPN/Docker stack on Windows).
static LOCAL_IPS: OnceLock<parking_lot::RwLock<Vec<IpAddr>>> = OnceLock::new();

fn local_ips_lock() -> &'static parking_lot::RwLock<Vec<IpAddr>> {
    LOCAL_IPS.get_or_init(|| parking_lot::RwLock::new(Vec::new()))
}

/// Enumerate the host's interface IPs and replace the cached snapshot. Returns
/// the freshly-enumerated list for immediate use by the caller (the bind path).
/// A VPN or Wi-Fi adapter that appears AFTER the first enumeration is picked up
/// the next time this runs — issue #585 lifts the `OnceLock` cache limitation.
fn refresh_local_interface_ips() -> Vec<IpAddr> {
    let snapshot = enumerate_interfaces();
    *local_ips_lock().write() = snapshot.clone();
    snapshot
}

/// The cached interface snapshot for the per-request Host guard. Cloned per
/// call so the snapshot outlives any concurrent refresh — the hot path reads
/// a stable view and never blocks the bind path's writer.
fn local_interface_ips() -> Vec<IpAddr> {
    local_ips_lock().read().clone()
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
        Ok(_) => request::host_is_allowed(host_header, &local_interface_ips()),
        Err(_) => false,
    }
}

async fn handle_connection(stream: MaybeTls, addr: SocketAddr) {
    // Capture the scheme before the stream is consumed by the buffered reader.
    // This is the authoritative request scheme (the server terminates TLS), used
    // to gate the `Secure` session cookie below (issue #553).
    let secure = stream.is_tls();
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
        // revocation can kick this socket; `None` for the root token. It must
        // also be bound to the `events` surface (issue #551) — a ticket minted
        // for a node's terminal can't be replayed here.
        let requested = ws_ticket::WsTarget {
            surface: ws_ticket::SURFACE_EVENTS.to_string(),
            node_id: None,
        };
        let device_id = match ws_ticket::consume(&ticket, &requested) {
            ws_ticket::ConsumeOutcome::Ok(device_id) => device_id,
            ws_ticket::ConsumeOutcome::TargetMismatch => {
                let _ = request::write_status_only(&mut lines, "403 Forbidden").await;
                return;
            }
            ws_ticket::ConsumeOutcome::Invalid => {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
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
        // the socket mid-stream (issue #502); `None` for the root token. The
        // ticket must be bound to *this* node's terminal (issue #551): a ticket
        // minted for another node — or for the events push — is rejected, and a
        // node mismatch leaves the ticket valid so the real client can retry.
        let requested = ws_ticket::WsTarget {
            surface: ws_ticket::SURFACE_TERMINAL.to_string(),
            node_id: Some(node_id),
        };
        let device_id = match ws_ticket::consume(&ticket, &requested) {
            ws_ticket::ConsumeOutcome::Ok(device_id) => device_id,
            ws_ticket::ConsumeOutcome::TargetMismatch => {
                let _ = request::write_status_only(&mut lines, "403 Forbidden").await;
                return;
            }
            ws_ticket::ConsumeOutcome::Invalid => {
                let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
                return;
            }
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
                        let cookie = request::session_cookie_header(&device_token, secure);
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
        // The ticket is bound to the target the caller will open (issue #551):
        // its `{ surface, node_id }` arrives in the request body. A missing or
        // malformed target is a 400 — we never mint a ticket that could never
        // match an upgrade.
        let content_length: usize = request::extract_header_value(&headers, "Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if content_length > 8 * 1024 {
            let _ = request::write_status_only(&mut lines, "413 Content Too Large").await;
            return;
        }
        let mut body_bytes = vec![0u8; content_length];
        if content_length > 0 && lines.read_exact(&mut body_bytes).await.is_err() {
            let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
            return;
        }
        let target = match ws_ticket::parse_mint_target(&body_bytes) {
            Ok(t) => t,
            Err(()) => {
                let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
                return;
            }
        };
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
            ticket: ws_ticket::mint(device_id, target),
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
    use tokio::net::TcpStream;

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
            handle_connection(MaybeTls::Plain(stream), peer).await;
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
            handle_connection(MaybeTls::Plain(stream), peer).await;
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
            handle_connection(MaybeTls::Plain(stream), peer).await;
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
            handle_connection(MaybeTls::Plain(stream), peer).await;
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
        let ticket = ws_ticket::mint(
            None,
            ws_ticket::WsTarget {
                surface: ws_ticket::SURFACE_TERMINAL.to_string(),
                node_id: Some(123),
            },
        );
        let path = format!("/ws/terminal/123?ticket={}", ticket);
        assert_eq!(ws_status(&path).await, 101);
    }

    #[tokio::test]
    async fn ws_terminal_rejects_ticket_bound_to_other_node_and_keeps_it_valid() {
        // Issue #551: a ticket minted for node 123, presented on node 999, is a
        // 403 — and is NOT consumed, so the legitimate client can still upgrade
        // node 123 within the TTL.
        let ticket = ws_ticket::mint(
            None,
            ws_ticket::WsTarget {
                surface: ws_ticket::SURFACE_TERMINAL.to_string(),
                node_id: Some(123),
            },
        );
        let wrong = format!("/ws/terminal/999?ticket={}", ticket);
        assert_eq!(ws_status(&wrong).await, 403, "wrong node must be Forbidden");
        let right = format!("/ws/terminal/123?ticket={}", ticket);
        assert_eq!(
            ws_status(&right).await,
            101,
            "the rejected upgrade must not have consumed the ticket"
        );
    }

    #[tokio::test]
    async fn ws_events_rejects_terminal_bound_ticket() {
        // Issue #551: surface is part of the binding — a terminal ticket can't
        // open the events push.
        let ticket = ws_ticket::mint(
            None,
            ws_ticket::WsTarget {
                surface: ws_ticket::SURFACE_TERMINAL.to_string(),
                node_id: Some(123),
            },
        );
        let path = format!("/ws/events?ticket={}", ticket);
        assert_eq!(ws_status(&path).await, 403);
    }

    /// Build a `TlsAcceptor` for tests. The acceptor is unused in the
    /// `bind_specs` shape tests (they only assert `tls.is_some()`), but the
    /// production builder requires a real cert + key, so we mint a tiny one
    /// rather than mocking the type. Cheap (a few ms) and stays in-process.
    fn test_acceptor() -> TlsAcceptor {
        let cert = tls::generate(&[]).expect("test cert generation");
        tls::acceptor_from(&cert).expect("test acceptor build")
    }

    /// Regression pin for issue #587: `get_or_build_acceptor`'s fast path.
    /// When the SAN key is empty (no non-loopback interface IPs), the
    /// function MUST short-circuit to `Ok(None)` BEFORE touching the cert
    /// dir or the cache — that way `apply_binding`'s startup path doesn't
    /// need an `app_handle`, and the test binary (which has no app) can
    /// exercise the bind plan even when LAN is on but there are no
    /// non-loopback interfaces.
    #[tokio::test]
    async fn get_or_build_acceptor_short_circuits_on_empty_san_key() {
        // No interfaces at all → no TLS needed.
        let result = get_or_build_acceptor(&[]).await.expect("empty list is a no-op");
        assert!(result.is_none(), "empty interface set must yield no acceptor");

        // A list of only loopback IPs also yields an empty SAN key (the key
        // filters loopback) and the same short-circuit.
        let result = get_or_build_acceptor(&[
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ])
        .await
        .expect("loopback-only is a no-op");
        assert!(result.is_none(), "loopback-only interface set must yield no acceptor");
    }

    #[test]
    fn bind_specs_loopback_only_and_plain_by_default() {
        // The default (LAN off / no acceptor) binds IPv4 + IPv6 loopback, both
        // plain HTTP, and never reaches beyond loopback regardless of what
        // interfaces exist. The structural "acceptor folded into the spec"
        // change (issue #587) makes this the natural call: pass `None`.
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let specs = bind_specs(1992, &[lan_ip], None);
        assert_eq!(specs.len(), 2, "default binds IPv4 + IPv6 loopback");
        assert!(specs[0].addr.is_ipv4() && specs[0].addr.ip().is_loopback(),
            "IPv4 loopback must be primary (the attention hook posts to 127.0.0.1)");
        assert!(specs[1].addr.is_ipv6() && specs[1].addr.ip().is_loopback());
        assert!(specs.iter().all(|s| s.addr.ip().is_loopback()),
            "the default must never expose beyond loopback");
        assert!(specs.iter().all(|s| s.tls.is_none()), "the default is plain HTTP");
    }

    #[test]
    fn bind_specs_lan_keeps_loopback_plain_and_adds_interface_tls() {
        // LAN on: loopback stays plain (so the loopback attention hook still
        // works), and every non-loopback interface IP is added as a TLS
        // listener (issue #501). Loopback IPs in the interface list are not
        // re-bound as TLS. The folded-acceptor invariant (issue #587):
        // every spec with `tls.is_some()` carries the SAME acceptor clone,
        // so the accept loop can never observe "want TLS but no acceptor".
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &[IpAddr::V4(Ipv4Addr::LOCALHOST), lan_ip], Some(&acceptor));
        // 2 loopback (plain) + 1 interface (TLS).
        assert_eq!(specs.len(), 3);
        let loopback_plain = specs
            .iter()
            .filter(|s| s.addr.ip().is_loopback() && s.tls.is_none())
            .count();
        assert_eq!(loopback_plain, 2, "both loopback listeners stay plain HTTP");
        let tls_iface: Vec<_> = specs.iter().filter(|s| s.tls.is_some()).collect();
        assert_eq!(tls_iface.len(), 1, "exactly the one non-loopback interface gets TLS");
        assert_eq!(tls_iface[0].addr.ip(), lan_ip);
        assert!(!tls_iface[0].addr.ip().is_loopback());
    }

    #[test]
    fn bind_specs_lan_without_interfaces_stays_loopback_only() {
        // LAN on but no non-loopback interface (e.g. offline) → nothing is
        // exposed and no TLS listener is created. Safe degrade.
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &[], Some(&acceptor));
        assert_eq!(specs.len(), 2);
        assert!(specs.iter().all(|s| s.addr.ip().is_loopback() && s.tls.is_none()));
    }

    #[test]
    fn bind_specs_skips_link_local_addresses() {
        // The OS interface enumeration surfaces link-local addresses (IPv6
        // `fe80::` from every NIC, IPv4 `169.254.x.x` APIPA) that are only
        // reachable on the originating link with a zone id the phone doesn't
        // share. Binding/advertising them hands the QR an unreachable URL
        // (`https://[fe80::…]:1992`) → the phone's browser rejects it with
        // ERR_INVALID_ARGUMENT. They must never become exposed interfaces.
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
        assert_eq!(
            tls_iface.len(),
            1,
            "only the routable LAN interface should get a TLS listener"
        );
        assert_eq!(tls_iface[0].addr.ip(), routable);
        assert!(
            !specs.iter().any(|s| s.addr.ip() == v6_link_local
                || s.addr.ip() == v4_link_local),
            "link-local addresses must not be bound at all"
        );
    }

    // Realized-bind tracking (issue #586). `bind_specs` describes the *plan*;
    // `realized_binds_from_specs` translates which of those specs actually
    // bound into the wire shape the Settings UI consumes. Pure function so the
    // outcome mapping is testable without a real listener.
    #[test]
    fn realized_binds_loopback_only_when_all_secondaries_skipped() {
        // Default case: both loopback listeners bind, no interface specs.
        let specs = bind_specs(1992, &[], None);
        let realized = realized_binds_from_specs(&specs, &[0, 1]);
        assert_eq!(realized.len(), 2);
        assert!(realized.iter().all(|b| !b.tls));
        assert!(realized.iter().all(|b| b.address.ends_with(":1992")));
    }

    #[test]
    fn realized_binds_drop_unbound_specs() {
        // TLS init failed → `acceptor` is None → `bind_specs` does not emit
        // any interface spec at all (issue #587 structural guarantee). Only
        // loopback binds surface, and they are plain.
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let specs = bind_specs(1992, &[lan_ip], None);
        // Only the two loopback specs were ever produced; the interface spec
        // is structurally absent, not "skipped at bind time".
        assert_eq!(specs.len(), 2, "no interface spec is emitted when acceptor is None");
        let realized = realized_binds_from_specs(&specs, &[0, 1]);
        assert_eq!(realized.len(), 2, "interface listener must NOT appear when acceptor is missing");
        assert!(realized.iter().all(|b| !b.tls),
            "no TLS realized bind can exist without an interface spec");
        assert!(realized.iter().all(|b| b.address.parse::<SocketAddr>().unwrap().ip().is_loopback()),
            "only loopback listeners survived");
    }

    #[test]
    fn realized_binds_marks_tls_when_interface_binds() {
        // Happy path: loopback plain + interface TLS, all bound.
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &[lan_ip], Some(&acceptor));
        let realized = realized_binds_from_specs(&specs, &[0, 1, 2]);
        assert_eq!(realized.len(), 3);
        let tls: Vec<_> = realized.iter().filter(|b| b.tls).collect();
        assert_eq!(tls.len(), 1, "exactly one TLS listener (the interface)");
        assert!(tls[0].address.contains("192.168.1.5"),
            "TLS listener address must carry the interface IP");
        assert!(tls[0].address.ends_with(":1992"));
        let plain: Vec<_> = realized.iter().filter(|b| !b.tls).collect();
        assert_eq!(plain.len(), 2, "both loopback listeners stay plain");
    }

    #[test]
    fn realized_binds_partial_when_some_interfaces_fail() {
        // Per-interface bind can fail (address in use, IPv6 disabled on a
        // specific iface, etc.). Realized binds = those that succeeded.
        let lan1: IpAddr = "192.168.1.5".parse().unwrap();
        let lan2: IpAddr = "10.0.0.2".parse().unwrap();
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &[lan1, lan2], Some(&acceptor));
        // Indices 0,1 = loopback pair, 2 = lan1 (bound), 3 = lan2 (failed)
        let realized = realized_binds_from_specs(&specs, &[0, 1, 2]);
        assert_eq!(realized.len(), 3);
        let ips: Vec<&str> = realized
            .iter()
            .filter(|b| b.tls)
            .map(|b| b.address.split(':').next().unwrap())
            .collect();
        assert_eq!(ips, vec!["192.168.1.5"], "only lan1 bound; lan2 dropped");
    }

    async fn get_request_with_host(path: &str, host: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(MaybeTls::Plain(stream), peer).await;
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
            let mut lines = tokio::io::BufStream::new(MaybeTls::Plain(stream));
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
            handle_connection(MaybeTls::Plain(stream), peer).await;
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

    // --- #585: interface snapshot must refresh on rebind ---
    //
    // Regression pins for issue #585: the bind path and the per-request Host
    // guard both consult a cached interface list. A VPN that connects AFTER
    // first enumeration must be visible on the next bind, and the hot path
    // must read the latest snapshot — without paying for enumeration per
    // request. The override is process-global so each test installs it via an
    // RAII guard (auto-restored on Drop) and a `Mutex<()>` serialises them
    // against the shared `LOCAL_IPS` cache.

    /// Serialises tests that swap the interface enumerator. The enumeration
    /// cache (`LOCAL_IPS`) is process-global, so concurrent runs could
    /// interleave refresh + read and observe a torn snapshot. The override's
    /// RAII guard handles restoration; this serialises the in-flight body.
    static ENUMERATOR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn refresh_local_interface_ips_replaces_cached_snapshot() {
        // Simulate a VPN adapter connecting AFTER first enumeration:
        // first call returns only a stable LAN IP, then a new interface
        // appears and must be reflected in the cache after refresh.
        let _lock = ENUMERATOR_TEST_LOCK.lock().unwrap();
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let vpn_ip: IpAddr = "10.20.30.40".parse().unwrap();

        // Initial enumeration: only the LAN IP is visible.
        let _enum_guard = set_interface_enumerator_for_testing(vec![lan_ip]);
        let initial = refresh_local_interface_ips();
        assert!(
            initial.contains(&lan_ip),
            "initial snapshot must include the LAN IP"
        );
        assert_eq!(
            local_interface_ips(),
            vec![lan_ip],
            "cache reflects initial enumeration"
        );

        // VPN connects mid-session — the next refresh must pick it up.
        let _enum_guard = set_interface_enumerator_for_testing(vec![lan_ip, vpn_ip]);
        let refreshed = refresh_local_interface_ips();
        assert!(
            refreshed.contains(&vpn_ip),
            "refresh must pick up a new interface that appeared after first enumeration"
        );
        assert!(
            local_interface_ips().contains(&vpn_ip),
            "hot-path read after refresh must see the new IP"
        );
    }

    #[test]
    fn host_header_allowed_uses_refreshed_snapshot() {
        // The per-request Host guard must read the latest snapshot, not a
        // frozen one. Pre-refresh: a VPN IP is rejected. Post-refresh:
        // accepted without re-enumeration on the request path.
        let _lock = ENUMERATOR_TEST_LOCK.lock().unwrap();
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let vpn_ip: IpAddr = "10.20.30.40".parse().unwrap();

        let _enum_guard = set_interface_enumerator_for_testing(vec![lan_ip]);
        let _ = refresh_local_interface_ips();
        assert!(
            !host_header_allowed(&format!("{}:1992", vpn_ip)),
            "vpn ip not yet enumerated -> rejected by Host guard"
        );

        let _enum_guard = set_interface_enumerator_for_testing(vec![lan_ip, vpn_ip]);
        let _ = refresh_local_interface_ips();
        assert!(
            host_header_allowed(&format!("{}:1992", vpn_ip)),
            "vpn ip visible after refresh -> accepted by Host guard"
        );
    }

    #[test]
    fn bind_specs_picks_up_new_interface_after_refresh() {
        // The bind path must enumerate at bind time so a TLS listener is
        // opened on every current interface IP, not the frozen first-enumeration
        // set. Pins the #585 regression at the bind-spec level.
        let _lock = ENUMERATOR_TEST_LOCK.lock().unwrap();
        let vpn_ip: IpAddr = "10.20.30.40".parse().unwrap();
        let _enum_guard = set_interface_enumerator_for_testing(vec![vpn_ip]);
        let interface_ips = refresh_local_interface_ips();
        let acceptor = test_acceptor();
        let specs = bind_specs(1992, &interface_ips, Some(&acceptor));
        let tls_vpn = specs
            .iter()
            .any(|s| s.tls.is_some() && s.addr.ip() == vpn_ip);
        assert!(
            tls_vpn,
            "a fresh interface IP must produce a TLS bind spec; got {} specs (loopback={}, tls={})",
            specs.len(),
            specs.iter().filter(|s| s.addr.ip().is_loopback()).count(),
            specs.iter().filter(|s| s.tls.is_some()).count(),
        );
    }

    #[test]
    fn host_header_guard_does_not_consult_enumerator_directly() {
        // Contract pin: the override lives at the `enumerate_interfaces` seam
        // (refresh path), NOT at the hot-path `local_interface_ips` read.
        // Setting the override without calling `refresh_local_interface_ips`
        // leaves the cached `LOCAL_IPS` untouched, so the Host guard sees the
        // prior snapshot — even though `enumerate_interfaces` would now return
        // the override. Documented at the override's declaration site.
        let _lock = ENUMERATOR_TEST_LOCK.lock().unwrap();
        let lan_ip: IpAddr = "192.168.1.5".parse().unwrap();
        let vpn_ip: IpAddr = "10.20.30.40".parse().unwrap();

        // Seed the cache with ONLY the LAN IP via a refresh.
        let _enum_guard = set_interface_enumerator_for_testing(vec![lan_ip]);
        let _ = refresh_local_interface_ips();
        assert!(
            host_header_allowed(&format!("{}:1992", lan_ip)),
            "lan ip accepted after first refresh"
        );

        // Now install an override that includes the VPN IP, but DO NOT refresh.
        // The cache must still reflect the prior [lan_ip] snapshot, so the
        // VPN IP is rejected by the Host guard even though the override
        // contains it. This is the intended contract: the override is a
        // refresh-time switch, not a hot-path switch.
        let _enum_guard = set_interface_enumerator_for_testing(vec![lan_ip, vpn_ip]);
        assert!(
            !host_header_allowed(&format!("{}:1992", vpn_ip)),
            "vpn ip not in cached snapshot -> rejected, even though override includes it"
        );
        assert!(
            host_header_allowed(&format!("{}:1992", lan_ip)),
            "lan ip in cached snapshot -> still accepted"
        );
    }
}
