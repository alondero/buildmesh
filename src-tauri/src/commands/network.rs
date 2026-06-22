//! LAN/VPN exposure control commands (issue #501).
//!
//! The embedded HTTP/WS server binds loopback-only by default (issue #496).
//! These commands drive the opt-in that exposes it on the machine's LAN
//! interfaces over self-signed TLS, and rebind the listeners live so the toggle
//! takes effect without restarting the app.

use std::net::SocketAddr;

use crate::db;
use crate::http::RealizedBind;
use tauri::command;
use ts_rs::TS;

/// Snapshot of the server's network exposure for the Settings surface.
///
/// Generated to src/types/generated/NetworkStatus.ts (issue #359).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, TS)]
#[ts(export, export_to = "NetworkStatus.ts")]
pub struct NetworkStatus {
    /// Whether the server may bind beyond loopback. **DB intent** — reflects
    /// only what the user asked for. The *realized* exposure is on
    /// `exposed_interfaces` / `tls_active`. Issue #586: when this is `true`
    /// but `exposed_interfaces` is empty, the server is loopback-only despite
    /// the toggle being on (TLS init failure, no interface, or per-interface
    /// bind failure), and the UI must surface that.
    pub lan_exposure_enabled: bool,
    /// The port the server is currently bound on (after the 1992→1994 fallback).
    pub port: u16,
    /// `true` iff at least one TLS listener is currently bound on a non-loopback
    /// interface. False when the toggle is off, when TLS init failed, when no
    /// non-loopback interface is available, or when the per-interface bind
    /// failed (issue #586).
    pub tls_active: bool,
    /// Listeners actually bound beyond loopback, in `ip:port` form. Empty when
    /// exposure is off OR when nothing reached beyond loopback regardless of
    /// DB intent (issue #586). The Settings UI renders this so the user sees
    /// the URL their phone should hit, and warns when this is empty under an
    /// enabled toggle.
    pub exposed_interfaces: Vec<RealizedBind>,
}

/// Read the current LAN-exposure setting and the *realized* bind state. Issue
/// #586: previously this returned DB intent only — the Settings toggle could
/// show "on" while the server was actually loopback-only (TLS init failure, no
/// interface, etc.). Now it consults the live `ServerListeners` so the UI can
/// surface the mismatch.
#[command]
pub async fn get_network_status() -> Result<NetworkStatus, String> {
    let lan_exposure_enabled = db::lan_exposure_enabled().map_err(|e| e.to_string())?;
    let port = crate::http::current_http_port();
    let realized = crate::http::realized_binds();
    // Filter to non-loopback via `SocketAddr::is_loopback()` rather than
    // string-prefix matching — robust to the IPv6 bracketing rust uses today
    // (`[::1]:1992`) and any future change to `SocketAddr`'s display format.
    // An unparseable address is treated as non-loopback (show it) so a future
    // producer with a new format fails loud in the UI rather than silent.
    let exposed_interfaces: Vec<RealizedBind> = realized
        .iter()
        .filter(|b| {
            !b.address
                .parse::<SocketAddr>()
                .map(|sa| sa.ip().is_loopback())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    // `tls_active` is scoped to non-loopback listeners so a future loopback
    // listener with `tls: true` (e.g. to encrypt the attention webhook)
    // doesn't flip the public "TLS active" signal without also exposing an
    // interface.
    let tls_active = exposed_interfaces.iter().any(|b| b.tls);
    Ok(NetworkStatus {
        lan_exposure_enabled,
        port,
        tls_active,
        exposed_interfaces,
    })
}

/// Flip the LAN/VPN exposure switch and rebind the listeners immediately. Off by
/// default; enabling it binds the machine's interfaces over self-signed TLS so a
/// phone on the LAN/VPN can reach the hub (existing loopback connections are
/// unaffected; existing LAN connections drop and must reconnect over HTTPS).
#[command]
pub async fn set_lan_exposure_enabled(enabled: bool) -> Result<(), String> {
    db::set_lan_exposure_enabled(enabled).map_err(|e| e.to_string())?;
    crate::http::reapply_binding().await;
    Ok(())
}
