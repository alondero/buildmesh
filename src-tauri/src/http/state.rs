//! Mutable process-wide HTTP server state.
//!
//! Narrow accessors hide the globals (`APP_HANDLE`, snapshot channels,
//! resolved port, listener handles, interface snapshot). Bind logic lives
//! in [`crate::http::server`]; this module only stores what that logic
//! (and route handlers) need to read.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::OnceLock;

use parking_lot::RwLock;
use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::sync::{oneshot, watch};
use ts_rs::TS;

use crate::http::interface_rank;

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

/// Wire type — payload of the `serialize-terminal-request` Tauri event (issue
/// #161). The backend asks the frontend to serialise a live terminal so its
/// scrollback can be embedded into a coordinator WS payload; the reply rides
/// back over a `request_id`-keyed oneshot channel, NOT this event, so the
/// `request_id` is opaque to the listener (it's an idempotency token, not a
/// payload pointer).
///
/// Generated to `src/types/generated/SerializeTerminalRequestPayload.ts`; the
/// TS half is imported by `src/components/Terminal/TerminalRegistry.ts`. The
/// field key is `node_id` (not `session_id`) to match the internal node-id
/// vocabulary — the wire contract here is local to the webview (coordinator
/// / mobile WS use the `attention-needed` channel, not this one).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "SerializeTerminalRequestPayload.ts")]
pub struct SerializeTerminalRequestPayload {
    #[ts(as = "i32")]
    pub node_id: i64,
    pub request_id: String,
}

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

pub(crate) fn app_handle() -> Option<&'static tauri::AppHandle> {
    APP_HANDLE.get()
}

pub(crate) fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

pub(crate) const HTTP_PORT_START: u16 = 1992;
pub(crate) const HTTP_PORT_END: u16 = 1994;

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

/// Short profile label for surface filenames that the user can see
/// (e.g. the `Content-Disposition: filename=` on `/install-cert.der`).
/// Stable hub gets `prod`; the dev profile gets `dev`. A custom identifier
/// (test binary, sideloaded build) falls back to `custom` so the download
/// filename stays distinct per build rather than silently bucketing
/// unknown builds as `prod`.
pub fn port_profile_label(identifier: &str) -> &'static str {
    if identifier.ends_with(".dev") {
        "dev"
    } else if identifier.ends_with(".prod") || identifier == "com.alond.buildmesh" {
        "prod"
    } else {
        "custom"
    }
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

pub(crate) fn store_resolved_http_port(port: u16) {
    RESOLVED_HTTP_PORT.store(port, Ordering::SeqCst);
}

/// The most recently applied port offset, so a live rebind (LAN toggle) reuses
/// the same range the server first started on (stable → 0, dev → 1000).
static PORT_OFFSET: AtomicU16 = AtomicU16::new(0);

pub(crate) fn store_port_offset(offset: u16) {
    PORT_OFFSET.store(offset, Ordering::SeqCst);
}

pub(crate) fn loaded_port_offset() -> u16 {
    PORT_OFFSET.load(Ordering::SeqCst)
}

// --- Snapshot request/response (used by ws.rs to seed initial terminal state) ---

static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

type SnapshotSenders = HashMap<String, oneshot::Sender<String>>;
static SNAPSHOT_REQUESTS: OnceLock<std::sync::Arc<RwLock<SnapshotSenders>>> = OnceLock::new();

fn get_snapshot_requests() -> &'static std::sync::Arc<RwLock<SnapshotSenders>> {
    SNAPSHOT_REQUESTS.get_or_init(|| std::sync::Arc::new(RwLock::new(HashMap::new())))
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
        SerializeTerminalRequestPayload {
            node_id,
            request_id: request_id.clone(),
        },
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

/// TLS directory under the app-data dir, or `None` when the app handle / path
/// is unavailable. Shared by cert routes and the TLS acceptor builder.
pub(crate) fn tls_dir() -> Option<std::path::PathBuf> {
    let app = app_handle()?;
    app.path().app_data_dir().ok().map(|p| p.join("tls"))
}

pub(crate) fn app_identifier() -> Option<String> {
    app_handle().map(|app| app.config().identifier.clone())
}

// --- Listeners (owned here, mutated by server.rs) ---

/// The persistent loopback skeleton. Bound once at startup and never torn
/// down on a LAN toggle — the local attention webhook posts plain HTTP to
/// 127.0.0.1, and closing that socket on a settings change would briefly drop
/// notifications (issue #587). Stores the realized loopback binds so a
/// post-toggle `get_network_status` can still report "loopback only" when
/// the interface side is empty.
pub(crate) struct LoopbackSkeleton {
    // `shutdown` + `handles` are kept for the future graceful-shutdown path
    // (signal the loopback accept loops, await their exit). Today the OS
    // process exit drops the runtime, which drops the listeners, so the
    // fields are stored but not read. Marking `dead_code` rather than
    // removing them: the design intent — "the skeleton is structurally
    // teardown-able if we ever need to" — is part of the contract.
    #[allow(dead_code)]
    pub shutdown: watch::Sender<bool>,
    #[allow(dead_code)]
    pub handles: Vec<tauri::async_runtime::JoinHandle<()>>,
    pub port: u16,
    pub realized: Vec<RealizedBind>,
}

/// Interface TLS listeners, replaced wholesale on every LAN toggle. Empty
/// while LAN exposure is off.
pub(crate) struct InterfaceBindings {
    pub shutdown: Option<watch::Sender<bool>>,
    pub handles: Vec<tauri::async_runtime::JoinHandle<()>>,
}

/// The live listeners. Held behind an async mutex so concurrent toggles
/// serialize. The split into a persistent skeleton + replaceable interface
/// bindings is what lets a LAN toggle rebind ONLY the interface listeners
/// (issue #587) — a single shared shutdown would force a full teardown
/// every time, briefly closing the loopback socket the attention hook
/// posts to.
pub(crate) struct ServerListeners {
    pub skeleton: Option<LoopbackSkeleton>,
    pub interface: InterfaceBindings,
}

static SERVER_LISTENERS: OnceLock<tokio::sync::Mutex<ServerListeners>> = OnceLock::new();

pub(crate) fn server_listeners() -> &'static tokio::sync::Mutex<ServerListeners> {
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

// --- Realized binds ---

static REALIZED_BINDS: OnceLock<parking_lot::RwLock<Vec<RealizedBind>>> = OnceLock::new();

pub(crate) fn realized_binds_store() -> &'static parking_lot::RwLock<Vec<RealizedBind>> {
    REALIZED_BINDS.get_or_init(|| parking_lot::RwLock::new(Vec::new()))
}

/// Snapshot of the currently bound non-loopback listeners (issue #586).
pub fn realized_binds() -> Vec<RealizedBind> {
    realized_binds_store().read().clone()
}

// --- Interface snapshot ---

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

/// Read the test seam at the entry points that need it. Returns the
/// override when a test has installed one via `set_interface_enumerator_for_testing`,
/// or `None` in production builds (the static starts as `None` and is never
/// written outside tests). `pub(crate)` so `interface_rank::enumerate_with_classes`
/// AND `enumerate_interfaces` can both short-circuit on the same flag
/// (#630 review).
pub(crate) fn read_interface_override_for_test() -> Option<Vec<IpAddr>> {
    INTERFACE_SNAPSHOT_OVERRIDE
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
}

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

/// Enumerate the host's interface IPs via `local_ip_address::list_afinet_netifas`,
/// honouring the test override. On Windows this is the FALLBACK path — used
/// only when `interface_rank::windows_impl::walk_adapters` fails (#630 review);
/// the happy path walks `GetAdaptersAddresses` once. On macOS/Linux this is
/// the primary path (no `GetAdaptersAddresses`). Failures log a warning and
/// return an empty list — the bind path then sees no LAN interfaces, which is
/// a safe degrade (loopback still binds).
fn enumerate_interfaces() -> Vec<IpAddr> {
    if let Some(override_ips) = read_interface_override_for_test() {
        return override_ips;
    }
    match local_ip_address::list_afinet_netifas() {
        Ok(v) => v.into_iter().map(|(_, ip)| ip).collect(),
        Err(e) => {
            tracing::warn!("interface enumeration failed: {}", e);
            Vec::new()
        }
    }
}

/// Fallback for `interface_rank::enumerate_with_classes` on Windows — when
/// `walk_adapters` (the primary `GetAdaptersAddresses` walk) errors, return
/// the `list_afinet_netifas` IP list with an empty classes map. The bind path
/// can still proceed with range-heuristic ranking; LAN exposure degrades to
/// "no gateway awareness" but stays functional (#630 review).
pub(crate) fn enumerate_interfaces_with_classes_fallback()
-> (Vec<IpAddr>, HashMap<IpAddr, interface_rank::IfaceClass>) {
    (enumerate_interfaces(), HashMap::new())
}

/// The cached interface snapshot. Refreshed by `refresh_local_interface_ips`
/// on each bind; the per-request Host guard reads it via `local_interface_ips`
/// without paying for enumeration (which can stall for seconds behind a
/// VPN/Docker stack on Windows).
///
/// `LOCAL_SNAPSHOT` holds IPs and classes in ONE lock to make refresh atomic
/// — a reader that observes a new IP always sees its class (which may be
/// `None` on non-Windows or after a `walk_adapters` failure fallback). Splitting
/// them into two locks left a cross-cache window where the new ranked IPs
/// could be paired with stale classes (#630 review).
type LocalSnapshot = (Vec<IpAddr>, HashMap<IpAddr, interface_rank::IfaceClass>);

static LOCAL_SNAPSHOT: OnceLock<parking_lot::RwLock<LocalSnapshot>> = OnceLock::new();

fn local_snapshot_lock() -> &'static parking_lot::RwLock<LocalSnapshot> {
    LOCAL_SNAPSHOT.get_or_init(|| parking_lot::RwLock::new((Vec::new(), HashMap::new())))
}

/// Reader paired with `local_interface_ips`. Returns the cached classes map
/// regardless of whether the most recent refresh was the happy path (Windows
/// `walk_adapters` succeeded — non-empty map) or the fallback path (non-Windows
/// or `walk_adapters` failed — empty map). The QR fallback uses both signals
/// to short-circuit on a prior bind without re-walking (#630 review).
pub(crate) fn local_classes_if_populated()
-> Option<HashMap<IpAddr, interface_rank::IfaceClass>> {
    let snapshot = local_snapshot_lock().read();
    Some(snapshot.1.clone())
}

/// Enumerate the host's interface IPs and replace the cached snapshot. Returns
/// the freshly-enumerated list for immediate use by the caller (the bind path).
/// A VPN or Wi-Fi adapter that appears AFTER the first enumeration is picked up
/// the next time this runs — issue #585 lifts the `OnceLock` cache limitation.
pub(crate) fn refresh_local_interface_ips() -> Vec<IpAddr> {
    // Single walk on Windows (issue #630): `enumerate_with_classes` returns
    // both the IP list and the per-IP routing classification from one
    // `GetAdaptersAddresses` call. On failure it falls back to
    // `local_ip_address::list_afinet_netifas` for the IPs alone (range
    // heuristic decides), preserving the pre-#630 bind path coverage (#630
    // review). Rank best-LAN-first so the realized-bind order (→ cert SANs →
    // `exposed_interfaces`) leads with the interface the phone can actually
    // reach; without this the raw OS enumeration order leaks through and the
    // QR's "first IPv4 TLS bind" pick can land on a VPN tunnel (e.g. NordLynx
    // `10.5.0.2`) the phone has no route to.
    let (ips, classes) = interface_rank::enumerate_with_classes();
    let ranked = interface_rank::rank_with_classes(ips, &classes);
    // Atomic write of `(ranked, classes)` — readers see the new IPs and their
    // matching classes together (or both the previous values). Always write
    // even when the classes map is empty so the snapshot reflects the most
    // recent refresh (#630 review).
    *local_snapshot_lock().write() = (ranked.clone(), classes);
    ranked
}

/// The cached interface snapshot for the per-request Host guard. Cloned per
/// call so the snapshot outlives any concurrent refresh — the hot path reads
/// a stable view and never blocks the bind path's writer. `pub(crate)` so the
/// QR-fallback (`commands::mesh::get_local_ip`) can share the bind snapshot
/// on a hit instead of doing its own walk (issue #630).
pub(crate) fn local_interface_ips() -> Vec<IpAddr> {
    local_snapshot_lock().read().0.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_offset_is_zero_for_stable_and_1000_for_dev() {
        assert_eq!(port_offset("com.alond.buildmesh"), 0);
        assert_eq!(port_offset("com.alond.buildmesh.dev"), 1000);
        assert_eq!(HTTP_PORT_START + port_offset("com.alond.buildmesh.dev"), 2992);
        assert_eq!(HTTP_PORT_END + port_offset("com.alond.buildmesh.dev"), 2994);
    }

    #[test]
    fn port_profile_label_matches_known_identifiers() {
        assert_eq!(port_profile_label("com.alond.buildmesh"), "prod");
        assert_eq!(port_profile_label("com.alond.buildmesh.dev"), "dev");
        assert_eq!(port_profile_label("com.alond.buildmesh.prod"), "prod");
        assert_eq!(port_profile_label("com.example.other"), "custom");
    }
}
