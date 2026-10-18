//! Embedded HTTP/WebSocket server for mobile remote access.
//!
//! Layout (issue #1658):
//! - [`server`] — bind, TLS acceptor, accept loop, request-head parse, Host
//!   guard, body read, WebSocket upgrade.
//! - [`router`] — the route table and [`router::dispatch`].
//! - [`state`] — app handle, snapshots, ports, listeners, interface cache.
//! - [`instances`] — the machine-global registry of the ports every running
//!   instance has bound, so a harness that has to bake its callback URL can
//!   address all of them (issue #2343).
//!
//! Route handlers take [`router::ParsedRequest`] and return [`response::Response`];
//! they never touch `BufStream` / [`MaybeTls`].
//!
//! [`read_status`] is the one transport policy for a failed authoritative
//! persistence read (issue #2330): absent → 404, unavailable → 503, so no
//! route can answer a storage fault with a successful empty body.

pub mod assets;
pub mod auth;
pub mod events;
// Fuzz harness for the request-read path (issue #2156). Test-only: it runs
// under `cargo test --lib http::fuzz`, never in the shipped binary.
#[cfg(test)]
mod fuzz;
pub mod instances;
pub mod interface_rank;
pub mod interface_watcher;
pub mod pairing;
pub mod rate_limit;
pub mod read_status;
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

pub use state::{current_http_port, fulfill_snapshot, port_offset, realized_binds, RealizedBind};

pub(crate) use state::{
    app_handle, enumerate_interfaces_with_classes_fallback, local_classes_if_populated,
    local_interface_ips, read_interface_override_for_test, request_terminal_snapshot,
};

// `enumerate_interfaces` has two distinct consumers: the production
// `#[cfg(not(target_os = "windows"))]` paths (`interface_watcher::sorted_snapshot`
// and `interface_rank::enumerate_with_classes`), and — on Windows, where those
// blocks are compiled out — the regression test in `interface_watcher::tests`
// that resolves `super::super::enumerate_interfaces()`. The attribute that used
// to sit here silenced the unused-import lint on Windows alone; gating to the
// configurations that actually resolve the name says the same thing without a
// suppression (issue #2450).
#[cfg(any(test, not(target_os = "windows")))]
pub(crate) use state::enumerate_interfaces;

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
