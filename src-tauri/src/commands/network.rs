//! LAN/VPN exposure control commands (issue #501).
//!
//! The embedded HTTP/WS server binds loopback-only by default (issue #496).
//! These commands drive the opt-in that exposes it on the machine's LAN
//! interfaces over self-signed TLS, and rebind the listeners live so the toggle
//! takes effect without restarting the app.

use std::net::SocketAddr;
use std::path::Path;

use base64::Engine;
use tauri::Manager;

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
    let lan_exposure_enabled = crate::commands::run_blocking("get_network_status", || {
        db::lan_exposure_enabled().map_err(|e| e.to_string())
    })
    .await?;
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
    crate::commands::run_blocking("set_lan_exposure_enabled", move || {
        db::set_lan_exposure_enabled(enabled).map_err(|e| e.to_string())
    })
    .await?;
    crate::http::reapply_binding().await;
    Ok(())
}

/// Snapshot of the on-disk TLS cert chain for the QR modal (issue #635). The
/// desktop surfaces this so a user whose installed root CA doesn't match the
/// server's can see the discrepancy and re-install. **Only the desktop reads
/// `cert_path`; the HTTP route at `routes::certs::status_json` sets it to
/// `None` so the user's Windows username (in `%APPDATA%\<user>\...`) never
/// crosses the LAN boundary.** The same struct serialises over both wires —
/// `serde` skips `cert_path` when it's `None` so the HTTP JSON has only the
/// 4 fingerprint/issuer/validity fields.
///
/// Generated to `src/types/generated/CertChainStatus.ts` (ADR-0009, issue #359).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, TS)]
#[ts(export, export_to = "CertChainStatus.ts")]
pub struct CertChainStatus {
    pub root_fingerprint_sha256: String,
    pub leaf_fingerprint_sha256: String,
    pub leaf_issuer: String,
    pub valid_until: String,
    /// Absolute path to `ca.der`. `Some` for the desktop Tauri command
    /// response; `None` (and serialised-as-absent) for the HTTP route — see
    /// the type-level docstring.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cert_path: Option<String>,
}

/// Read the on-disk TLS cert chain and return the snapshot the QR modal
/// needs (issue #635). Resolves `<app-data>/tls/` from the Tauri app handle
/// — the same dir `http::tls::acceptor` reads at startup, so the chain the
/// user sees is guaranteed to match what's currently being served.
#[command]
pub fn get_cert_chain_status(app: tauri::AppHandle) -> Result<CertChainStatus, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("tls");
    let status = crate::http::tls::cert_status(&dir).map_err(|e| e.to_string())?;
    Ok(CertChainStatus {
        root_fingerprint_sha256: status.root_fingerprint_sha256,
        leaf_fingerprint_sha256: status.leaf_fingerprint_sha256,
        leaf_issuer: status.leaf_issuer,
        valid_until: status.valid_until,
        cert_path: Some(dir.join("ca.der").to_string_lossy().into_owned()),
    })
}

/// QR-payload source for the desktop→phone one-tap root-CA install
/// (issue #702). Returns the persisted root cert DER as base64 so the
/// frontend can paste it directly into a `data:application/x-x509-ca-
/// cert;base64,...` URL inside a second QR. Routing the install through
/// a data: URL (rather than `window.location.href = /install-cert.der`)
/// is the only mechanism that hands the bytes to the phone's OS cert
/// installer instead of the desktop's Edge WebView2 — the same endpoint
/// on the same host routes differently per origin's policy.
///
/// Reuses `routes::certs::install_cert_der` for the read + empty check
/// so a corrupt / 0-byte ca.der cannot leak into a QR payload and
/// produce a silent install failure on the phone.
#[command]
pub fn get_root_cert_der(app: tauri::AppHandle) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("tls");
    get_root_cert_der_inner(&dir)
}

/// Inner implementation of [`get_root_cert_der`] extracted so the
/// `#[command]` wrapper (which takes a `tauri::AppHandle` we can't
/// construct in a unit test) and the tests below share the same code
/// path. The same shape as `routes::certs::install_cert_der` but
/// returns the base64 string the QR data: URL needs.
fn get_root_cert_der_inner(dir: &Path) -> Result<String, String> {
    let bytes = crate::http::routes::certs::install_cert_der(dir)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// QR-payload source for the iOS one-tap root-CA install (issue #713).
/// Returns the signed `.mobileconfig` profile (DER-encoded PKCS#7/CMS
/// SignedData wrapping the unsigned plist, base64-encoded) so the
/// frontend can paste it directly into a
/// `data:application/x-apple-aspen-config;base64,...` URL inside a
/// third QR tab.
///
/// The sibling `get_root_cert_der` covers Android Chrome, which routes
/// the bare `.der` MIME through the OS cert installer. iOS Safari does
/// NOT — it needs an Apple Configurator 2 `.mobileconfig` profile, and
/// a signed one (PKCS#7/CMS detached signature over the unsigned plist
/// using the root CA's private key) gets the "Verified" badge and a
/// frictionless install. The trust-enablement tap in Settings → General
/// → About → Certificate Trust Settings is still required for a
/// self-signed (non-Apple-trusted) root, but signing does not bypass
/// that — it just removes the unsigned-profile warning.
///
/// Both the unsigned plist and the CMS signing live in
/// `http::routes::mobileconfig`; this command is the IPC boundary that
/// turns the persisted root cert + key into the QR payload.
#[command]
pub fn get_root_cert_mobileconfig(app: tauri::AppHandle) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("tls");
    get_root_cert_mobileconfig_inner(&dir)
}

/// Inner implementation of [`get_root_cert_mobileconfig`] — mirrors the
/// `get_root_cert_der_inner` pattern (extracted so the `tauri::AppHandle`
/// the `#[command]` macro requires can be bypassed in tests).
fn get_root_cert_mobileconfig_inner(dir: &Path) -> Result<String, String> {
    crate::http::routes::mobileconfig::build_signed_mobileconfig_b64(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip contract: the base64 the desktop embeds in the
    /// install-QR must decode to the same bytes `install_cert_der`
    /// serves to `/install-cert.der`, so the phone sees the same cert
    /// via either path. Locks the "both endpoints expose the same
    /// bytes" contract the issue is built on.
    #[test]
    fn get_root_cert_der_decodes_to_install_cert_der_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let chain = crate::http::tls::load_or_generate(dir.path(), &[]).unwrap();
        let encoded = get_root_cert_der_inner(dir.path()).expect("get_root_cert_der");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .expect("base64");
        assert_eq!(
            decoded, chain.root_cert_der,
            "install-QR payload must decode to the same DER bytes /install-cert.der serves"
        );
    }

    /// Empty `ca.der` is corruption (the issue explicitly warns about
    /// silent install failure on the phone). `install_cert_der` rejects
    /// it; this command must propagate the error rather than serving
    /// an empty base64 string that would scan to a 0-byte "cert".
    #[test]
    fn get_root_cert_der_errors_when_ca_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ca.der"), b"").unwrap();
        let result = get_root_cert_der_inner(dir.path());
        assert!(
            result.is_err(),
            "empty ca.der must NOT be served as a 0-byte base64 QR payload; got Ok of {} chars",
            result.as_ref().map(|s| s.len()).unwrap_or(0)
        );
    }

    /// End-to-end install-QR payload contract for iOS: the base64 the
    /// desktop embeds must decode to a DER blob whose CMS signature
    /// verifies against the same root CA `install_cert_der` would serve
    /// on the Android path. Shells out to `openssl cms -verify` (the
    /// same tool `http::routes::mobileconfig::tests` uses) so a
    /// regression in either the plist generation or the signing step
    /// fails here, not at the install-verify step on the user's phone.
    #[test]
    fn get_root_cert_mobileconfig_decodes_to_signed_payload() {
        let dir = tempfile::tempdir().unwrap();
        let chain = crate::http::tls::load_or_generate(dir.path(), &[]).unwrap();

        let b64 = get_root_cert_mobileconfig_inner(dir.path())
            .expect("get_root_cert_mobileconfig");
        let signed = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .expect("base64");

        let signed_path = dir.path().join("signed.mobileconfig");
        let root_pem_path = dir.path().join("root.pem");
        std::fs::write(&signed_path, &signed).unwrap();
        let pem = pem_encode("CERTIFICATE", &chain.root_cert_der);
        std::fs::write(&root_pem_path, pem).unwrap();

        let output = std::process::Command::new("openssl")
            .args([
                "cms",
                "-verify",
                "-inform", "DER",
                "-in", signed_path.to_str().unwrap(),
                "-CAfile", root_pem_path.to_str().unwrap(),
                "-noverify",
            ])
            .output()
            .expect("openssl cms -verify must be on PATH");
        assert!(
            output.status.success(),
            "iOS .mobileconfig payload MUST verify via openssl cms -verify; \
             stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        );
    }

    /// The `get_root_cert_der`-equivalent error case: a fresh install
    /// hasn't toggled LAN exposure, so `ca.der` (and `ca.key.der`) are
    /// absent. The Tauri command must propagate the error rather than
    /// return an empty base64 string that would scan to a broken
    /// install — the modal's `Promise.allSettled` hides the iOS tab on
    /// rejection, but only if the rejection actually surfaces.
    #[test]
    fn get_root_cert_mobileconfig_errors_when_ca_missing() {
        let dir = tempfile::tempdir().unwrap();
        let result = get_root_cert_mobileconfig_inner(dir.path());
        assert!(
            result.is_err(),
            "missing ca.der must NOT be served as an empty .mobileconfig QR payload; got Ok of {} chars",
            result.as_ref().map(|s| s.len()).unwrap_or(0)
        );
    }

    /// Mirrors `http::routes::mobileconfig::tests::pem_encode` — local
    /// copy so the test stays self-contained (the inner module's
    /// `pem_encode` is `pub(super)`-private by the test's `mod tests`
    /// scope and not reachable from here).
    fn pem_encode(label: &str, der: &[u8]) -> String {
        use std::fmt::Write;
        const ALPHA: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::with_capacity(der.len().div_ceil(3) * 4);
        for chunk in der.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let n = chunk.len();
            b64.push(ALPHA[(b0 >> 2) as usize] as char);
            b64.push(ALPHA[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if n > 1 {
                b64.push(ALPHA[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                b64.push('=');
            }
            if n > 2 {
                b64.push(ALPHA[(b2 & 0x3f) as usize] as char);
            } else {
                b64.push('=');
            }
        }
        let mut s = String::with_capacity(b64.len() + 64);
        s.push_str("-----BEGIN ");
        s.push_str(label);
        s.push_str("-----\n");
        for chunk in b64.as_bytes().chunks(64) {
            writeln!(s, "{}", std::str::from_utf8(chunk).unwrap()).unwrap();
        }
        s.push_str("-----END ");
        s.push_str(label);
        s.push_str("-----\n");
        s
    }
}
