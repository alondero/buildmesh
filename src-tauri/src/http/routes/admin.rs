//! Admin device-management routes (issue #502) — the first real operations
//! mounted in the `/admin/*` namespace #500 reserved. Both are Admin-guarded by
//! the dispatcher (`auth::guard(.., RequiredScope::Admin)`) before they're
//! reached, so a coordinator token is 403 and an anonymous request is 401.
//!
//! - `GET /admin/devices` → list paired devices (id, label, IP, timestamps).
//! - `POST /admin/devices/{id}/revoke` → delete the device row and kick any live
//!   socket it holds, so revocation takes effect immediately rather than at the
//!   device's next request.
//!
//! The same operations are also exposed as desktop Tauri commands
//! (`commands::devices`); both back onto the shared `db` + `revocation` layer.

use crate::db;
use crate::http::{request, revocation};
use tokio::io::BufStream;
use tokio::net::TcpStream;

/// Best-effort human label for a paired device, derived from its `User-Agent`
/// at pairing (e.g. "Safari on iPhone"). Pure so it's unit-testable without a
/// request. Returns `None` for an absent/empty header rather than inventing a
/// name — the panel then just shows the IP. Deliberately coarse: it's a
/// recognition aid in a revocation list, not analytics.
pub fn device_label_from_user_agent(ua: Option<&str>) -> Option<String> {
    let ua = ua.map(str::trim).filter(|s| !s.is_empty())?;

    // Platform first — it's what a user recognises ("my iPhone").
    let platform = if ua.contains("iPhone") {
        Some("iPhone")
    } else if ua.contains("iPad") {
        Some("iPad")
    } else if ua.contains("Android") {
        Some("Android")
    } else if ua.contains("Macintosh") || ua.contains("Mac OS") {
        Some("Mac")
    } else if ua.contains("Windows") {
        Some("Windows")
    } else if ua.contains("Linux") {
        Some("Linux")
    } else {
        None
    };

    // Browser, checked in an order that avoids the classic UA-substring traps:
    // Edge's UA contains "Chrome", Chrome's contains "Safari", so test the more
    // specific tokens first.
    let browser = if ua.contains("Edg") {
        Some("Edge")
    } else if ua.contains("Firefox") {
        Some("Firefox")
    } else if ua.contains("Chrome") || ua.contains("CriOS") {
        Some("Chrome")
    } else if ua.contains("Safari") {
        Some("Safari")
    } else {
        None
    };

    match (browser, platform) {
        (Some(b), Some(p)) => Some(format!("{b} on {p}")),
        (Some(b), None) => Some(b.to_string()),
        (None, Some(p)) => Some(p.to_string()),
        // No recognisable tokens — fall back to a trimmed slice of the raw UA so
        // the row is still identifiable rather than blank.
        (None, None) => Some(ua.chars().take(40).collect()),
    }
}

/// `GET /admin/devices` → JSON array of `DeviceSession` (never the token hash).
/// Degrades to `[]` on a read error rather than 500ing, mirroring the
/// coordinator read routes.
pub fn list_devices_json() -> String {
    match db::list_device_sessions() {
        Ok(devices) => serde_json::to_string(&devices).unwrap_or_else(|_| "[]".to_string()),
        Err(_) => "[]".to_string(),
    }
}

/// `POST /admin/devices/{id}/revoke` — delete the device row and immediately
/// kick any live WebSocket it holds. `204 No Content` when a device was revoked,
/// `404 Not Found` when the id was already gone (idempotent-ish: a double-revoke
/// is a clear 404, not a silent 204).
pub async fn revoke(lines: &mut BufStream<TcpStream>, device_id: i64) {
    match db::revoke_device_session(device_id) {
        Ok(true) => {
            // Row gone (blocks future requests); now drop any open socket.
            revocation::revoke(device_id);
            let _ = request::write_status_only(lines, "204 No Content").await;
        }
        Ok(false) => {
            request::send_json_error(lines, "404 Not Found", "Unknown device").await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e.to_string()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_common_mobile_user_agents() {
        let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
                      AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
        assert_eq!(
            device_label_from_user_agent(Some(iphone)).as_deref(),
            Some("Safari on iPhone")
        );

        let android = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 \
                       (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";
        assert_eq!(
            device_label_from_user_agent(Some(android)).as_deref(),
            Some("Chrome on Android")
        );
    }

    #[test]
    fn edge_is_not_misread_as_chrome() {
        // Edge's UA contains "Chrome" — the specific token must win.
        let edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                    (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0";
        assert_eq!(
            device_label_from_user_agent(Some(edge)).as_deref(),
            Some("Edge on Windows")
        );
    }

    #[test]
    fn absent_or_empty_user_agent_has_no_label() {
        assert_eq!(device_label_from_user_agent(None), None);
        assert_eq!(device_label_from_user_agent(Some("   ")), None);
    }

    #[test]
    fn unrecognised_user_agent_falls_back_to_a_trimmed_slice() {
        let label = device_label_from_user_agent(Some("curl/8.4.0")).unwrap();
        assert_eq!(label, "curl/8.4.0");
    }
}
