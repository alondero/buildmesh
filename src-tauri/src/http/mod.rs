//! Embedded HTTP/WebSocket server for mobile remote access.
//!
//! Layout (issue #1658):
//! - [`server`] — bind, TLS acceptor, accept loop, request-head parse, Host
//!   guard, body read, WebSocket upgrade.
//! - [`router`] — the route table and [`router::dispatch`].
//! - [`state`] — app handle, snapshots, ports, listeners, interface cache.
//!
//! Route handlers take [`router::ParsedRequest`] and return [`response::Response`];
//! they never touch `BufStream` / [`MaybeTls`].

pub mod assets;
pub mod auth;
pub mod events;
pub mod interface_rank;
pub mod interface_watcher;
pub mod pairing;
pub mod rate_limit;
pub mod request;
pub mod response;
pub mod revocation;
pub mod router;
pub mod routes;
pub mod server;
pub mod state;
pub mod stream;
pub mod tls;
pub mod ws;
pub mod ws_ticket;

pub use stream::MaybeTls;

#[allow(unused_imports)] // public API + ts-rs exports used outside this module
pub use state::{
    current_http_port, fulfill_snapshot, port_offset, port_profile_label, realized_binds,
    RealizedBind, SerializeTerminalRequestPayload, HTTP_PORT_DEFAULT,
};

// `enumerate_interfaces` is consumed by `interface_watcher::sorted_snapshot`
// and `interface_rank::enumerate_with_classes`, both `#[cfg(not(windows))]`
// — so on Windows lib builds (which exclude `#[cfg(test)]`) the import is
// technically unused. The re-export still has to exist on Windows for the
// regression test in `interface_watcher::tests` to resolve
// `super::super::enumerate_interfaces()`. Scope the suppression to Windows
// so any genuine future regression on non-Windows surfaces immediately.
#[cfg_attr(target_os = "windows", allow(unused_imports))]
pub(crate) use state::{
    app_handle, enumerate_interfaces, enumerate_interfaces_with_classes_fallback,
    local_classes_if_populated, local_interface_ips, read_interface_override_for_test,
    request_terminal_snapshot,
};

#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use state::set_interface_enumerator_for_testing;

#[allow(unused_imports)]
pub use server::bind_specs;
pub(crate) use server::{clear_cached_acceptor, is_link_local};

/// Start the HTTP server, trying ports 1992→1993→1994 until one binds.
/// Emits a `remote-access-port` event with the actual port used so the
/// QR code modal can update without recompiling.
pub fn start_http_server(app: tauri::AppHandle, port_offset: u16) {
    server::start(app, port_offset);
}

/// Re-evaluate the LAN-exposure setting and rebind the listeners live. Called by
/// `set_lan_exposure_enabled` so flipping the toggle takes effect immediately —
/// switching between loopback-only plain HTTP and loopback-plus-interface TLS —
/// without restarting the app.
pub async fn reapply_binding() {
    server::reapply_binding().await;
}
