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
use tauri::Emitter;
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
    if APP_HANDLE.set(app.clone()).is_ok() {
        use tauri::Listener;
        app.listen("circuit-run-updated", |event| {
            super::events::forward_circuit_update(event.payload());
        });
        app.listen("circuit-runs-attention-dismissed", |_| {
            // Dismissal can remove several runs; any run invalidation causes a
            // full read, so no DB lookup is needed in the event callback.
            super::events::emit(super::events::EventMsg::CircuitRunUpdated { run_id: None });
        });
    }
}

pub(crate) const HTTP_PORT_START: u16 = 1992;
pub(crate) const HTTP_PORT_END: u16 = 1994;

/// Default/expected port for the attention webhook hook.
/// The actual server may bind 1993 or 1994 if 1992 is taken; the hook will
/// silently no-op on the wrong port since `|| true` is appended.
pub const HTTP_PORT_DEFAULT: u16 = HTTP_PORT_START;

/// Environment variable a debug or `dev-tools` build reads to override the
/// identifier-derived port offset, so two dev instances can each bind their own
/// range (issue #2345). A shipped release build does not compile the read, for
/// the same reason the profile-dir override is gated: a user launch is not
/// steerable by an environment variable it inherited.
#[cfg(any(test, debug_assertions, feature = "dev-tools"))]
pub(crate) const PORT_OFFSET_ENV: &str = "BUILDMESH_PORT_OFFSET";

/// Largest offset whose whole range still fits a port: the highest listener is
/// `HTTP_PORT_END + offset` (1994), and the test bridge adds 1991.
#[cfg(any(test, debug_assertions, feature = "dev-tools"))]
const MAX_PORT_OFFSET: u16 = u16::MAX - HTTP_PORT_END;

/// Parse a `BUILDMESH_PORT_OFFSET` value. `None` for anything that is not a
/// plain in-range integer, including an empty value: a value that cannot be
/// read as an offset must not become one.
#[cfg(any(test, debug_assertions, feature = "dev-tools"))]
fn parse_port_offset(raw: &str) -> Option<u16> {
    let offset = raw.trim().parse::<u16>().ok()?;
    (offset <= MAX_PORT_OFFSET).then_some(offset)
}

/// The offset this process was launched with, or `None` to use the
/// identifier-derived one.
///
/// An unusable value warns rather than aborting: the offset is an ergonomic
/// choice, not a data-safety one, and a dev launcher that misspelled it should
/// still get a running app. The warning names the value and the fallback
/// because ignoring it silently would put this instance back on the range the
/// override exists to move it off.
#[cfg(any(test, debug_assertions, feature = "dev-tools"))]
fn port_offset_override() -> Option<u16> {
    let raw = std::env::var(PORT_OFFSET_ENV).ok()?;
    match parse_port_offset(&raw) {
        Some(offset) => Some(offset),
        None => {
            tracing::warn!(
                "{PORT_OFFSET_ENV}={raw:?} is not a port offset in 0..={MAX_PORT_OFFSET}; \
                 falling back to the offset derived from the bundle identifier"
            );
            None
        }
    }
}

/// Port offset applied to every server so a dev build (identifier `*.dev`) can
/// run side-by-side with the stable hub without contending on 1991/1992.
/// Stable → 0, dev → 1000 (test 2991, HTTP 2992-2994).
///
/// A debug or `dev-tools` build launched with `BUILDMESH_PORT_OFFSET` uses that
/// offset instead, which is how two dev instances — two worktrees verifying
/// their own build — each get their own ports without a second config overlay.
pub fn port_offset(identifier: &str) -> u16 {
    #[cfg(any(test, debug_assertions, feature = "dev-tools"))]
    if let Some(offset) = port_offset_override() {
        return offset;
    }
    if identifier.ends_with(".dev") {
        1000
    } else {
        0
    }
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

/// TLS directory under the profile startup installed, or `None` before
/// `preferences::init`. Debug and dev-tools builds can point that profile at
/// `BUILDMESH_PROFILE_DIR`, so certificate files follow the database and the
/// log. Shared by cert routes and the TLS acceptor builder.
pub(crate) fn tls_dir() -> Option<std::path::PathBuf> {
    crate::preferences::app_data_dir().map(|dir| dir.join("tls"))
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
pub(crate) fn set_interface_enumerator_for_testing(ips: Vec<IpAddr>) -> TestEnumeratorGuard {
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
///
/// `pub(crate)` + re-exported via `crate::http::mod` so `interface_watcher::sorted_snapshot`
/// (cfg(not(windows))) and `interface_rank::enumerate_with_classes` (cfg(not(windows)))
/// can call it as `super::enumerate_interfaces()` — same pattern as
/// `enumerate_interfaces_with_classes_fallback` and `read_interface_override_for_test`.
/// Without the re-export the non-Windows callers can't resolve the path and
/// `npm run tauri build` fails on Linux/macOS (issue #591 follow-up regression).
pub(crate) fn enumerate_interfaces() -> Vec<IpAddr> {
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
pub(crate) fn enumerate_interfaces_with_classes_fallback(
) -> (Vec<IpAddr>, HashMap<IpAddr, interface_rank::IfaceClass>) {
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
pub(crate) fn local_classes_if_populated() -> Option<HashMap<IpAddr, interface_rank::IfaceClass>> {
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
        // `port_offset` reads BUILDMESH_PORT_OFFSET, so this test shares the
        // process environment with the tests that set it.
        let _env_guard = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(port_offset("com.alond.buildmesh"), 0);
        assert_eq!(port_offset("com.alond.buildmesh.dev"), 1000);
        assert_eq!(
            HTTP_PORT_START + port_offset("com.alond.buildmesh.dev"),
            2992
        );
        assert_eq!(HTTP_PORT_END + port_offset("com.alond.buildmesh.dev"), 2994);
    }

    #[test]
    fn port_profile_label_matches_known_identifiers() {
        assert_eq!(port_profile_label("com.alond.buildmesh"), "prod");
        assert_eq!(port_profile_label("com.alond.buildmesh.dev"), "dev");
        assert_eq!(port_profile_label("com.alond.buildmesh.prod"), "prod");
        assert_eq!(port_profile_label("com.example.other"), "custom");
    }

    #[test]
    fn port_offset_override_accepts_a_plain_in_range_integer() {
        assert_eq!(parse_port_offset("0"), Some(0));
        assert_eq!(parse_port_offset("1100"), Some(1100));
        assert_eq!(
            parse_port_offset(&MAX_PORT_OFFSET.to_string()),
            Some(MAX_PORT_OFFSET)
        );
        // A launcher may hand the value through a shell, so whitespace and an
        // explicit sign are not a reason to fall back to the identifier's range.
        assert_eq!(parse_port_offset(" 2200\n"), Some(2200));
        assert_eq!(parse_port_offset("+2200"), Some(2200));
    }

    #[test]
    fn port_offset_override_rejects_anything_that_is_not_one() {
        // In range as a number, but past the point where 1994 + offset is still
        // a port.
        let past_max = MAX_PORT_OFFSET.saturating_add(1).to_string();
        for raw in [
            "",
            "   ",
            "abc",
            "1100abc",
            "-1",
            "1_100",
            "70000",
            past_max.as_str(),
        ] {
            assert_eq!(
                parse_port_offset(raw),
                None,
                "{raw:?} must not be an offset"
            );
        }
    }

    /// The override exists so a second dev instance can move off the ports the
    /// identifier already claims. It therefore has to win over the identifier,
    /// and the whole listener range has to move with it.
    #[test]
    fn port_offset_override_wins_over_the_identifier() {
        let _env_guard = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let restore = PortOffsetEnv::set("2200");
        assert_eq!(port_offset("com.alond.buildmesh.dev"), 2200);
        assert_eq!(
            HTTP_PORT_START + port_offset("com.alond.buildmesh.dev"),
            4192
        );
        assert_eq!(HTTP_PORT_END + port_offset("com.alond.buildmesh.dev"), 4194);
        drop(restore);
        assert_eq!(port_offset("com.alond.buildmesh.dev"), 1000);
    }

    #[test]
    fn an_unusable_port_offset_override_falls_back_to_the_identifier() {
        let _env_guard = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let restore = PortOffsetEnv::set("not-a-port");
        assert_eq!(port_offset("com.alond.buildmesh.dev"), 1000);
        assert_eq!(port_offset("com.alond.buildmesh"), 0);
        drop(restore);
    }

    /// Sets `BUILDMESH_PORT_OFFSET` for one test and restores whatever the
    /// runner's own environment held, so a test cannot leak an offset into the
    /// tests that share this binary.
    struct PortOffsetEnv;

    impl PortOffsetEnv {
        fn set(value: &str) -> PortOffsetEnvRestore {
            let previous = std::env::var_os(PORT_OFFSET_ENV);
            // SAFETY: the ENV_LOCK guard above is held for the whole window, so
            // no other test reads or writes the process environment here.
            unsafe { std::env::set_var(PORT_OFFSET_ENV, value) };
            PortOffsetEnvRestore(previous)
        }
    }

    struct PortOffsetEnvRestore(Option<std::ffi::OsString>);

    impl Drop for PortOffsetEnvRestore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(previous) => unsafe { std::env::set_var(PORT_OFFSET_ENV, previous) },
                None => unsafe { std::env::remove_var(PORT_OFFSET_ENV) },
            }
        }
    }

    #[test]
    fn tls_dir_follows_the_installed_profile_not_the_tauri_folder() {
        let scratch = std::env::temp_dir().join(format!(
            "bm-tls-profile-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        crate::preferences::init_for_tests(scratch.clone());
        let dir = tls_dir().expect("installed profile yields a tls directory");
        assert_eq!(dir, scratch.join("tls"));
        crate::preferences::reset_for_tests();
        assert!(
            tls_dir().is_none(),
            "an unset profile must not fall back to another folder"
        );

        let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let state = std::fs::read_to_string(src.join("http/state.rs")).expect("state.rs");
        assert!(
            state.contains("preferences::app_data_dir()"),
            "tls_dir must read the profile startup installed"
        );
        for rel in ["http/server.rs", "commands/network.rs"] {
            let text = std::fs::read_to_string(src.join(rel))
                .unwrap_or_else(|error| panic!("read {rel}: {error}"));
            assert!(
                !text.contains("app_data_dir()"),
                "{rel} still resolves a path through Tauri app_data_dir"
            );
        }
    }
}
