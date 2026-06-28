//! `GET /__certs/status` — diagnostic JSON snapshot of the on-disk TLS cert
//! chain (issue #635). Reached BEFORE the auth gates, mirroring
//! `__debug/log` placement at `http::mod.rs:1018`, so a phone whose TLS chain
//! is broken still gets a useful response — though, of course, its browser
//! refuses the initial TLS handshake and never reaches this endpoint in that
//! state. The desktop reads the same data (plus `cert_path`) via
//! `commands::network::get_cert_chain_status`.
//!
//! We deliberately do NOT expose `cert_path` here: it embeds the user's
//! Windows username (`%APPDATA%\<user>\...\tls\ca.der`) and the route is
//! reachable from the LAN. The desktop Tauri command is the only caller
//! that needs the path — for the QR modal's "Re-install" affordance that
//! copies it to the clipboard.

use std::path::Path;

/// Serialize a `CertChainStatus` (minus `cert_path`) as the response body
/// for `GET /__certs/status`. Returns a JSON string so the caller in
/// `http::mod::handle_connection` can hand it straight to the writer
/// without re-parsing; mirrors `routes::admin::list_devices_json()`'s
/// stringly-typed return.
///
/// On any error (missing cert files, `APP_HANDLE` unset upstream), the
/// dispatcher short-circuits to 503 — see `handle_connection`.
pub fn status_json(dir: &Path) -> Result<String, String> {
    let status = crate::http::tls::cert_status(dir).map_err(|e| e.to_string())?;
    // Construct the same struct the Tauri command returns, but with
    // `cert_path: None`. `serde(skip_serializing_if = "Option::is_none")`
    // on the struct field drops it from the JSON, so the HTTP response
    // carries only the 4 fingerprint/issuer/validity fields — the Windows
    // username never crosses the LAN. The generated TypeScript type
    // matches both shapes (`cert_path?: string`).
    let out = crate::commands::network::CertChainStatus {
        root_fingerprint_sha256: status.root_fingerprint_sha256,
        leaf_fingerprint_sha256: status.leaf_fingerprint_sha256,
        leaf_issuer: status.leaf_issuer,
        valid_until: status.valid_until,
        cert_path: None,
    };
    serde_json::to_string(&out).map_err(|e| e.to_string())
}