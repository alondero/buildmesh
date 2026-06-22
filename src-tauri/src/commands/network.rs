//! LAN/VPN exposure control commands (issue #501).
//!
//! The embedded HTTP/WS server binds loopback-only by default (issue #496).
//! These commands drive the opt-in that exposes it on the machine's LAN
//! interfaces over self-signed TLS, and rebind the listeners live so the toggle
//! takes effect without restarting the app.

use crate::db;
use tauri::command;
use ts_rs::TS;

/// Snapshot of the server's network exposure for the Settings surface.
///
/// Generated to src/types/generated/NetworkStatus.ts (issue #359).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, TS)]
#[ts(export, export_to = "NetworkStatus.ts")]
pub struct NetworkStatus {
    /// Whether the server may bind beyond loopback. When `true`, the LAN-facing
    /// interfaces serve HTTPS/WSS with a self-signed certificate; loopback stays
    /// plain HTTP.
    pub lan_exposure_enabled: bool,
    /// The port the server is currently bound on (after the 1992→1994 fallback).
    pub port: u16,
}

/// Read the current LAN-exposure setting and bound port.
#[command]
pub async fn get_network_status() -> Result<NetworkStatus, String> {
    let lan_exposure_enabled = db::lan_exposure_enabled().map_err(|e| e.to_string())?;
    Ok(NetworkStatus {
        lan_exposure_enabled,
        port: crate::http::current_http_port(),
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
