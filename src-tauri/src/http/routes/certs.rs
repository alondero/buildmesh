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
//!
//! `GET /install-cert.der` — one-tap root CA install (issue #636). Same
//! pre-auth, Host-guarded placement as `/__certs/status`; serves the raw
//! `ca.der` bytes with the Android-friendly `application/x-x509-ca-cert`
//! MIME so Chrome's auto-routing into the system cert installer fires.
//! Windows opens its own Certificate Import Wizard from this same MIME;
//! macOS Safari/Firefox download the file for a manual Keychain drag. The
//! `Content-Length` header MUST match the exact byte count of the file
//! served — Chrome aborts mismatched downloads with `ERR_CONTENT_LENGTH_MISMATCH`
//! and the user is left with a half-truncated cert that won't install.

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

/// The DER bytes of the persisted root CA. The dispatcher emits `bytes.len()`
/// as `Content-Length` directly — taking the length from the in-memory
/// `Vec<u8>` (not `Path::metadata`) closes the race where a LAN toggle
/// `load_or_generate`s a fresh chain between the stat and the send,
/// producing a `Content-Length` that no longer matches what we actually
/// write and tripping Chrome's `ERR_CONTENT_LENGTH_MISMATCH`.
///
/// An empty `ca.der` is rejected as an error (not returned as `Ok(vec![])`):
/// `tls::load_or_generate` enforces non-empty at the write side
/// (`tls.rs:226`), so an empty file on disk is corruption (interrupted write,
/// external wipe) and serving it would deliver a 0-byte download to the
/// phone's cert installer — a silent failure the user can't diagnose. A
/// missing file is the realistic "fresh app data dir" failure mode; an
/// empty file is "we wrote garbage, do not install this".
///
/// Errors propagate `String` for the dispatcher's 503 path.
pub fn install_cert_der(dir: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(dir.join("ca.der")).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err(format!(
            "root CA at {} is empty (corrupted; refusing to serve a 0-byte cert)",
            dir.join("ca.der").display()
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A freshly generated chain must round-trip through `install_cert_der`
    /// with the same bytes the cert was generated with — locks the "Content-
    /// Length matches the bytes we send" contract from the issue's ERR_CONTENT_LENGTH_MISMATCH
    /// foot-gun. The dispatcher reads `bytes.len()` for the header, so a
    /// caller-level test exercises the same shape.
    #[test]
    fn install_cert_der_returns_persisted_root_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let chain = crate::http::tls::load_or_generate(dir.path(), &[]).unwrap();

        let bytes = install_cert_der(dir.path()).expect("install_cert_der");
        assert_eq!(bytes, chain.root_cert_der);
    }

    /// Missing `ca.der` is the realistic "fresh app data dir / wiped tls/"
    /// failure. The dispatcher maps the error to 503 — install_cert_der must
    /// NOT panic and NOT return an empty success, because a 200-with-zero-
    /// bytes response installs a useless empty "certificate" on the phone.
    #[test]
    fn install_cert_der_errors_when_ca_missing() {
        let dir = tempfile::tempdir().unwrap();
        // No load_or_generate call → no ca.der on disk.
        assert!(install_cert_der(dir.path()).is_err());
    }

    /// An empty `ca.der` is corruption — `tls::load_or_generate` writes
    /// non-empty bytes at tls.rs:226, so an on-disk 0-byte file means an
    /// interrupted write or external wipe. The docstring's "not return an
    /// empty success" promise is locked here: a regression that drops the
    /// empty-check would pass `install_cert_der_errors_when_ca_missing`
    /// (which covers `std::fs::read` returning `NotFound`) but fail THIS
    /// test, so the two cases are independent guards.
    #[test]
    fn install_cert_der_errors_when_ca_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ca.der"), b"").unwrap();
        let result = install_cert_der(dir.path());
        assert!(
            result.is_err(),
            "empty ca.der must NOT be served (200 + 0-byte body = silent install failure); got Ok of {} bytes",
            result.as_ref().map(|v| v.len()).unwrap_or(0)
        );
    }
}