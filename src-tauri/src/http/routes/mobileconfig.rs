//! iOS `.mobileconfig` install profile generation (issue #713).
//!
//! PR #712's `get_root_cert_der` shipped an HTTPS install URL that works
//! for Android Chrome (≥ 90) — Chrome routes `application/x-x509-ca-cert`
//! downloads into the system CA installer. iOS Safari does NOT intercept
//! that MIME reliably, and even when it does, a raw cert is not recognised
//! as a CA without manual Settings → General → About → Certificate Trust
//! Settings enablement.
//!
//! The clean fix is a signed **Apple Configurator 2 `.mobileconfig` profile**
//! wrapping the root CA as a `com.apple.security.root` payload. iOS Safari
//! intercepts the MIME type `application/x-apple-aspen-config` reliably
//! from `data:` URLs, and a profile signed with the root CA's private key
//! (PKCS#7/CMS detached-from-content perspective, i.e. content embedded in
//! the CMS envelope — see [`sign_mobileconfig`]) carries a "Verified"
//! badge instead of "Not Signed". The trust-enablement tap is still
//! required for a self-signed (non-Apple-trusted) root either way —
//! signing does NOT bypass it.
//!
//! Wire shape per Apple:
//! - **Unsigned plist** (XML, `<!DOCTYPE plist ...>`): a normal plist with
//!   outer `PayloadType=Configuration` and an inner
//!   `PayloadType=com.apple.security.root` payload whose `PayloadContent`
//!   is the base64 of the root CA DER.
//! - **Signed `.mobileconfig`**: the unsigned plist is the `encapContentInfo`
//!   of a PKCS#7 `ContentInfo { contentType = signedData }` (DER-encoded
//!   binary), produced by `openssl cms -sign -binary -outform DER -nodetach`.
//!   The file as a whole is a CMS binary blob — NOT a plist with a `Signed`
//!   key.
//!
//! ## Why shell out to `openssl cms -sign` instead of using the `openssl` crate?
//!
//! The `openssl` Rust crate requires either a system OpenSSL installation
//! (with dev headers and static libs) OR the `vendored` feature (which
//! compiles OpenSSL 3.x from source via Perl — Git-for-Windows' bundled
//! MSYS2 perl is missing `Locale::Maketext::Simple`, so `openssl-sys`'s
//! vendored Configure step fails). Shelling out uses the exact same
//! `openssl.exe` binary that the verify-side tests in this file already
//! rely on (and that `scripts\check.ps1:62-67` pins first on PATH), so
//! production and test exercise the same libcrypto. The cost is three
//! temp files per sign (~5–15 ms on Windows); the operation runs once per
//! modal open, so the latency is invisible to the user.

use std::path::Path;

use base64::Engine;

/// Stable reverse-DNS identifiers for the outer profile and inner cert
/// payload. Apple treats the outer as the *profile* identity — a second
/// install with the same outer `PayloadIdentifier` replaces the existing
/// profile, so we want a constant one (rather than per-install) for a
/// frictionless re-install path. The inner identifier is nested under the
/// outer to keep the cert payload logically owned by the profile.
const OUTER_PAYLOAD_IDENTIFIER: &str = "com.buildmesh.dev.profile";
const INNER_PAYLOAD_IDENTIFIER: &str = "com.buildmesh.dev.profile.rootca";
const OUTER_PAYLOAD_DISPLAY_NAME: &str = "Buildmesh Dev Root CA";
const OUTER_PAYLOAD_DESCRIPTION: &str = "Installs the Buildmesh Dev Root CA used by this device's LAN-exposed HTTPS server.";
const INNER_PAYLOAD_DISPLAY_NAME: &str = "Buildmesh Dev Root CA";
const INNER_PAYLOAD_DESCRIPTION: &str = "Trusted root CA for the Buildmesh Dev TLS chain.";
const INNER_PAYLOAD_CERT_FILENAME: &str = "ca.cer";

/// Build the unsigned Apple Configurator 2 XML plist that wraps `root_cert_der`
/// as a `com.apple.security.root` payload. Compact formatting (no pretty-print
/// indentation) so the signed blob's base64 footprint stays small — a
/// single root CA at ~1.5 KB DER produces a ~3 KB base64 `.mobileconfig`,
/// well under the QR-code alphanumeric capacity (≈ 4.3 KB at error-
/// correction L) that the install-QR uses.
///
/// The DOCTYPE is the standard Apple PLIST 1.0 declaration. UUIDs are
/// random v4 per call so re-installing doesn't add a duplicate profile;
/// Apple's behaviour when two profiles share a UUID is "replace", so a
/// stable outer UUID would still be safe, but a fresh UUID per call also
/// keeps the cert payload UUID distinct from the outer (the spec requires
/// them to differ and Apple Configurator 2 emits them that way).
pub fn build_unsigned_xml(root_cert_der: &[u8]) -> String {
    let cert_b64 = base64::engine::general_purpose::STANDARD.encode(root_cert_der);
    let outer_uuid = uuid::Uuid::new_v4().to_string();
    let inner_uuid = uuid::Uuid::new_v4().to_string();

    // The string concatenation matches the shape Apple's tools emit: an
    // outer `<dict>` with profile-level keys (PayloadContent is an array
    // of payload dicts) and one inner `<dict>` describing the cert. No
    // XML escape needed — every value is ASCII (UUIDs, reverse-DNS IDs,
    // base64 alphabet).
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         <key>PayloadContent</key>\n\
         <array>\n\
         <dict>\n\
         <key>PayloadType</key>\n<string>com.apple.security.root</string>\n\
         <key>PayloadVersion</key><integer>1</integer>\n\
         <key>PayloadIdentifier</key><string>{inner_id}</string>\n\
         <key>PayloadUUID</key><string>{inner_uuid}</string>\n\
         <key>PayloadDisplayName</key><string>{inner_disp}</string>\n\
         <key>PayloadDescription</key><string>{inner_desc}</string>\n\
         <key>PayloadCertificateFileName</key><string>{cert_filename}</string>\n\
         <key>PayloadContent</key>\n<data>{cert_b64}</data>\n\
         </dict>\n\
         </array>\n\
         <key>PayloadDisplayName</key><string>{outer_disp}</string>\n\
         <key>PayloadDescription</key><string>{outer_desc}</string>\n\
         <key>PayloadIdentifier</key><string>{outer_id}</string>\n\
         <key>PayloadRemovalDisallowed</key><false/>\n\
         <key>PayloadType</key><string>Configuration</string>\n\
         <key>PayloadUUID</key><string>{outer_uuid}</string>\n\
         <key>PayloadVersion</key><integer>1</integer>\n\
         </dict>\n\
         </plist>\n",
        inner_id = INNER_PAYLOAD_IDENTIFIER,
        inner_uuid = inner_uuid,
        inner_disp = INNER_PAYLOAD_DISPLAY_NAME,
        inner_desc = INNER_PAYLOAD_DESCRIPTION,
        cert_filename = INNER_PAYLOAD_CERT_FILENAME,
        cert_b64 = cert_b64,
        outer_id = OUTER_PAYLOAD_IDENTIFIER,
        outer_uuid = outer_uuid,
        outer_disp = OUTER_PAYLOAD_DISPLAY_NAME,
        outer_desc = OUTER_PAYLOAD_DESCRIPTION,
    )
}

/// Produce a DER-encoded PKCS#7/CMS `ContentInfo { contentType = signedData }`
/// wrapping `payload`, signed with the given (self-signed) root CA cert +
/// PKCS#8 key. The returned bytes are the iOS `.mobileconfig` wire format —
/// iOS extracts the inner plist from the CMS envelope and validates the
/// signature against the embedded signing certificate. Self-signed cert is
/// fine: `openssl cms -sign` doesn't inspect self-signature status, it just
/// uses the cert as a carrier for the public key + issuer/subject DN.
///
/// `-binary` keeps OpenSSL from applying MIME canonicalisation (which would
/// alter the inner XML's CRLF→LF). `-nodetach` is the load-bearing flag:
/// without it the resulting SignedData has no encapsulated content, and iOS
/// rejects the profile as malformed (it expects the inner plist inside the
/// CMS envelope, not a separate payload to fetch alongside).
pub fn sign_mobileconfig(
    cert_der: &[u8],
    key_pkcs8_der: &[u8],
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    // Three temp files: the unsigned plist, the signer cert PEM, the
    // signer key PEM. `tempfile::tempdir` is in the dep tree via the
    // existing dev-dep; using the non-dev `tempfile` is a deliberate
    // addition (Cargo.toml). All three are deleted on drop.
    let dir = tempfile::tempdir()
        .map_err(|e| format!("create tempdir for openssl cms -sign: {e}"))?;
    let unsigned_path = dir.path().join("unsigned.plist");
    let signed_path = dir.path().join("signed.mobileconfig");
    let cert_pem_path = dir.path().join("signer.pem");
    let key_pem_path = dir.path().join("signer.key.pem");

    std::fs::write(&unsigned_path, payload)
        .map_err(|e| format!("write unsigned plist: {e}"))?;
    std::fs::write(
        &cert_pem_path,
        der_to_pem("CERTIFICATE", cert_der),
    )
    .map_err(|e| format!("write signer cert PEM: {e}"))?;
    // PKCS#8 DER → `-----BEGIN PRIVATE KEY-----` (NOT `RSA PRIVATE KEY`,
    // which would require a PKCS#1 PEM and `openssl pkcs8` conversion).
    std::fs::write(
        &key_pem_path,
        der_to_pem("PRIVATE KEY", key_pkcs8_der),
    )
    .map_err(|e| format!("write signer key PEM: {e}"))?;

    let output = std::process::Command::new("openssl")
        .args([
            "cms",
            "-sign",
            "-in",
            unsigned_path.to_str().expect("ascii path"),
            "-outform",
            "DER",
            "-binary",
            "-nodetach",
            "-signer",
            cert_pem_path.to_str().expect("ascii path"),
            "-inkey",
            key_pem_path.to_str().expect("ascii path"),
            "-out",
            signed_path.to_str().expect("ascii path"),
        ])
        .output()
        .map_err(|e| format!("spawn openssl cms -sign: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "openssl cms -sign failed (exit {:?}): stderr={} stdout={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        ));
    }
    std::fs::read(&signed_path).map_err(|e| format!("read signed blob: {e}"))
}

/// Convert DER bytes to a base64 PEM block with the given label. Used by
/// [`sign_mobileconfig`] to bridge `ca.der` (the on-disk root cert) and
/// `ca.key.der` (the on-disk root key) into the PEM form `openssl cms
/// -sign -signer/-inkey` expects.
fn der_to_pem(label: &str, der: &[u8]) -> String {
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
    let mut out = String::with_capacity(b64.len() + 80);
    out.push_str("-----BEGIN ");
    out.push_str(label);
    out.push_str("-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        out.push('\n');
    }
    out.push_str("-----END ");
    out.push_str(label);
    out.push_str("-----\n");
    out
}

/// Read root cert + key from `<dir>/ca.der` + `<dir>/ca.key.der` (the
/// persistence layout `tls::load_or_generate` writes — see `http/tls.rs`);
/// build the unsigned plist, sign it with the root key, and base64-encode
/// the signed blob. The frontend concatenates the returned string after
/// `data:application/x-apple-aspen-config;base64,` to produce the iOS
/// install-QR payload.
///
/// Errors propagate the specific missing file so the Tauri command can
/// surface a useful message (the existing `get_root_cert_der` tests pin
/// the "empty ca.der" error shape for the same reason).
pub fn build_signed_mobileconfig_b64(dir: &Path) -> Result<String, String> {
    let cert_der = std::fs::read(dir.join("ca.der"))
        .map_err(|e| format!("read {}: {e}", dir.join("ca.der").display()))?;
    let key_der = std::fs::read(dir.join("ca.key.der"))
        .map_err(|e| format!("read {}: {e}", dir.join("ca.key.der").display()))?;
    // The empty-`ca.der` rejection is inherited from the .der read
    // contract in `http::tls::load_or_generate` — a zero-byte file is
    // corruption, not a valid cert, and signing it would either
    // silently succeed (the openssl CLI happily parses nothing as an
    // empty key) or fail with a confusing parse error. Reject here.
    if cert_der.is_empty() {
        return Err(format!(
            "root CA at {} is empty (corrupted; refusing to sign a 0-byte .mobileconfig)",
            dir.join("ca.der").display()
        ));
    }
    if key_der.is_empty() {
        return Err(format!(
            "root CA key at {} is empty (corrupted; refusing to sign with a 0-byte key)",
            dir.join("ca.key.der").display()
        ));
    }
    let unsigned_xml = build_unsigned_xml(&cert_der);
    let signed = sign_mobileconfig(&cert_der, &key_der, unsigned_xml.as_bytes())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&signed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a chain + key + unsigned XML on a tempdir, then verify the
    /// base64 round-trips to the same DER bytes the on-disk `ca.der`
    /// contains. Locks the "QR-payload PayloadContent matches the
    /// install-QR's source cert" contract — a regression that double-encodes
    /// or omits the cert would surface as a mismatch here.
    #[test]
    fn build_unsigned_xml_round_trips_root_cert() {
        let dir = tempfile::tempdir().unwrap();
        let chain = crate::http::tls::load_or_generate(dir.path(), &[]).unwrap();

        let xml = build_unsigned_xml(&chain.root_cert_der);

        // Pull the `<data>...</data>` block from the inner PayloadContent.
        // Find the first `<data>` (the inner cert's) and the matching
        // `</data>`.
        let data_start = xml
            .find("<data>")
            .expect("inner PayloadContent <data> element must exist");
        let data_end = xml[data_start..]
            .find("</data>")
            .expect("inner PayloadContent </data> close must exist")
            + data_start;
        let b64 = &xml[data_start + "<data>".len()..data_end];
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .expect("base64");

        assert_eq!(
            decoded, chain.root_cert_der,
            "embedded PayloadContent must decode to the same DER bytes /install-cert.der serves"
        );
    }

    /// Substring sweep: every key Apple's profile installer looks for in a
    /// cert-install profile must appear at least once. iOS is permissive
    /// about extras but strict about missing mandatory keys — `PayloadType`
    /// being absent makes the payload unrecognised, `PayloadContent` being
    /// absent makes the cert unrecoverable, and the outer `PayloadType=
    /// Configuration` being absent makes the whole profile unrecognised.
    /// All other "required" keys (`PayloadIdentifier`, `PayloadUUID`,
    /// `PayloadVersion`) are pinned alongside.
    #[test]
    fn build_unsigned_xml_contains_required_keys() {
        // Use an empty cert payload — base64 of b"" is the empty string, so
        // we assert on `<data></data>` (the canonical "no cert" rendering).
        // A non-empty fixture would base64-encode and obscure the substring
        // sweep — empty input keeps the key/structure assertions readable.
        let xml = build_unsigned_xml(b"");
        let must_contain = [
            "com.apple.security.root",       // inner PayloadType
            "Configuration",                 // outer PayloadType
            "PayloadCertificateFileName",
            INNER_PAYLOAD_CERT_FILENAME,    // cert filename (.cer)
            "PayloadIdentifier",
            INNER_PAYLOAD_IDENTIFIER,        // inner id
            OUTER_PAYLOAD_IDENTIFIER,        // outer id
            "PayloadUUID",
            "PayloadDisplayName",
            "PayloadContent",
            "<data></data>",                 // empty base64 = empty PayloadContent
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"",
        ];
        for needle in must_contain {
            assert!(
                xml.contains(needle),
                "unsigned .mobileconfig XML must contain `{needle}`; full XML:\n{xml}"
            );
        }
    }

    /// CMS signature round-trip: shell out to `openssl cms -verify` against
    /// the root CA. Exit code 0 proves the signature verifies — iOS performs
    /// the same validation when the user taps "Install". We sign with
    /// `-nodetach` (content embedded in the CMS envelope), so the verify
    /// command reads the content from inside the SignedData — no need for
    /// a separate `-content <file>` reference. The `scripts\check.ps1`
    /// openssl pinning (`C:\Program Files\Git\usr\bin\openssl.exe` first on
    /// PATH) makes this work in CI / worktree runs; without it, devkitPro's
    /// MSYS2 openssl dies with "add_item ... failed" before doing any work.
    #[test]
    fn sign_mobileconfig_round_trips_through_openssl_cms_verify() {
        let dir = tempfile::tempdir().unwrap();
        let chain = crate::http::tls::load_or_generate(dir.path(), &[]).unwrap();

        let unsigned_xml = build_unsigned_xml(&chain.root_cert_der);
        let signed = sign_mobileconfig(&chain.root_cert_der, &chain.root_key_der, unsigned_xml.as_bytes())
            .expect("sign_mobileconfig");

        // Self-signed root — `-noverify` skips the chain check (iOS does the
        // same for a not-yet-trusted root) but still validates the SignedData
        // digest + signerInfo.
        let signed_path = dir.path().join("signed.mobileconfig");
        let root_pem_path = dir.path().join("root.pem");
        std::fs::write(&signed_path, &signed).unwrap();
        // The openssl CMS `-CAfile` DER-loading issue (see
        // `http/tls.rs::leaf_cert_chains_to_root_cert`) is specific to
        // *chain verification*; `-verify -CAfile` for *signer verification*
        // accepts DER in OpenSSL 3.x. Use PEM to be safe across versions.
        std::fs::write(&root_pem_path, der_to_pem("CERTIFICATE", &chain.root_cert_der)).unwrap();

        // Verify WITHOUT `-content` — the SignedData was produced with
        // `-nodetach`, so the inner plist is encapsulated inside the CMS
        // envelope and openssl's verifier reads it from there. Passing
        // `-content <file>` would only be needed for a *detached* signature
        // (which we don't produce — iOS needs the content embedded).
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
            .expect("openssl cms -verify must be on PATH (check.ps1 pins Git-for-Windows usr/bin)");
        assert!(
            output.status.success(),
            "PKCS#7/CMS signature MUST verify via openssl cms -verify; \
             stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        );
    }

    /// The full install-QR payload contract: base64-decode the result of
    /// `build_signed_mobileconfig_b64` against the on-disk `ca.der`/`ca.key.der`,
    /// then verify the signature the same way `sign_mobileconfig_round_trips…`
    /// does. Locks the end-to-end "scan the iOS QR, profile installs and
    /// verifies" guarantee the issue's AC3 depends on.
    #[test]
    fn build_signed_mobileconfig_b64_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let chain = crate::http::tls::load_or_generate(dir.path(), &[]).unwrap();

        let b64 = build_signed_mobileconfig_b64(dir.path()).expect("build_signed_mobileconfig_b64");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .expect("base64");

        // The decoded bytes ARE the .mobileconfig — write them straight to
        // disk and feed to `openssl cms -verify`. iOS does the same thing
        // when Safari intercepts the data: URL.
        let signed_path = dir.path().join("signed.mobileconfig");
        let unsigned_path = dir.path().join("unsigned.mobileconfig");
        let root_pem_path = dir.path().join("root.pem");
        std::fs::write(&signed_path, &decoded).unwrap();
        // Write the root PEM BEFORE invoking openssl (the bug we hit when
        // this test first landed was passing `-CAfile` to a path we hadn't
        // written yet — openssl's `fopen` returned ENOENT and surfaced as a
        // confusing `error:02001002:system library:fopen:No such file`).
        std::fs::write(&root_pem_path, der_to_pem("CERTIFICATE", &chain.root_cert_der)).unwrap();

        // The unsigned plist is the inner content — recover it by running
        // `openssl cms -verify -out unsigned.mobileconfig` (writes the
        // recovered content). This also proves the SignedData carries the
        // plist correctly inside the CMS envelope. Same "no `-content`"
        // rationale as `sign_mobileconfig_round_trips…` above — `-nodetach`
        // signing means the content is embedded.
        let verify_output = std::process::Command::new("openssl")
            .args([
                "cms",
                "-verify",
                "-inform", "DER",
                "-in", signed_path.to_str().unwrap(),
                "-CAfile", root_pem_path.to_str().unwrap(),
                "-noverify",
                "-out", unsigned_path.to_str().unwrap(),
            ])
            .output()
            .expect("openssl cms -verify");
        assert!(
            verify_output.status.success(),
            "iOS .mobileconfig must verify via openssl cms -verify; \
             stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&verify_output.stderr),
            String::from_utf8_lossy(&verify_output.stdout),
        );

        // Recovered content is a valid unsigned plist containing the
        // root cert's base64 — we can't byte-compare to a fresh
        // `build_unsigned_xml` because every call regenerates UUIDs, so
        // the cert-b64 substring + required-key sweep is the right
        // assertion. Normalise CRLF→LF first (Git-for-Windows MSYS
        // openssl emits CRLF despite `-binary` signing — the digest is
        // unchanged but raw bytes differ; iOS treats both as equivalent).
        let recovered_bytes = std::fs::read(&unsigned_path).expect("recovered unsigned plist");
        let recovered_normalised: String =
            String::from_utf8_lossy(&recovered_bytes).replace("\r\n", "\n");
        let expected_cert_b64 = base64::engine::general_purpose::STANDARD.encode(&chain.root_cert_der);
        let must_contain = [
            expected_cert_b64.as_str(),
            "com.apple.security.root",
            INNER_PAYLOAD_IDENTIFIER,
            OUTER_PAYLOAD_IDENTIFIER,
            "PayloadCertificateFileName",
            INNER_PAYLOAD_CERT_FILENAME,
        ];
        for needle in must_contain {
            assert!(
                recovered_normalised.contains(needle),
                "recovered plist must contain `{needle}` (signing round-trip preserved the cert); \
                 full recovered plist:\n{recovered_normalised}"
            );
        }
    }

    /// `build_signed_mobileconfig_b64` must surface a useful error when the
    /// root cert is missing — the realistic failure mode for a freshly
    /// installed `com.alond.buildmesh.dev` data dir that hasn't toggled
    /// LAN exposure yet (so `tls::load_or_generate` has never written
    /// `ca.der`). The Tauri command caller (`commands::network::
    /// get_root_cert_mobileconfig_inner`) propagates the same shape.
    #[test]
    fn build_signed_mobileconfig_b64_errors_when_ca_missing() {
        let dir = tempfile::tempdir().unwrap();
        // No `load_or_generate` call → no `ca.der`, no `ca.key.der`.
        let result = build_signed_mobileconfig_b64(dir.path());
        assert!(result.is_err(), "missing ca.der must error, not return an empty QR payload");
    }

    /// Pre-#713 install: `ca.der` is present (the user has toggled LAN
    /// exposure at some point) but `ca.key.der` is missing (it didn't exist
    /// before this issue). The iOS QR path must fail loudly here — silently
    /// returning an empty payload would scan to a broken install. The
    /// remediation is a single LAN-exposure toggle (which forces
    /// `load_or_generate` to regenerate and write `ca.key.der`).
    #[test]
    fn build_signed_mobileconfig_b64_errors_when_root_key_missing() {
        let dir = tempfile::tempdir().unwrap();
        let _chain = crate::http::tls::load_or_generate(dir.path(), &[]).unwrap();
        std::fs::remove_file(dir.path().join("ca.key.der")).unwrap();
        let result = build_signed_mobileconfig_b64(dir.path());
        assert!(
            result.is_err(),
            "missing ca.key.der must error (user must toggle LAN exposure to regenerate)"
        );
    }

    /// Empty `ca.der` is corruption (`tls::load_or_generate` enforces
    /// non-empty at write time). The Android install path already has a
    /// `get_root_cert_der_errors_when_ca_empty` regression test; mirror it
    /// here so the iOS path doesn't regress into emitting a zero-byte
    /// `.mobileconfig` (which the openssl CMS signer would either fail
    /// cryptically or, worse, accept as a valid empty sign input).
    #[test]
    fn build_signed_mobileconfig_b64_errors_when_ca_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ca.der"), b"").unwrap();
        std::fs::write(dir.path().join("ca.key.der"), b"some-non-empty-key").unwrap();
        let result = build_signed_mobileconfig_b64(dir.path());
        assert!(
            result.is_err(),
            "empty ca.der must NOT be signed into a 0-byte .mobileconfig"
        );
    }

    /// Minimal sanity test for the PEM encoder — base64 wraps at 64 cols
    /// (the OpenSSL `cms -sign` parser tolerates other lengths but the
    /// default line-length is what every cert/key on the web emits).
    #[test]
    fn der_to_pem_wraps_at_64_columns() {
        let der = vec![0u8; 200];
        let pem = der_to_pem("CERTIFICATE", &der);
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
        // 200 bytes → 268 chars of base64 → 4 wrapped lines of 64 + 1 of 12.
        let body: Vec<&str> = pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        assert_eq!(body.len(), 5, "expected 4 full + 1 short line; got: {body:?}");
        for line in &body[..4] {
            assert_eq!(line.len(), 64, "full lines must be 64 chars; got {line:?}");
        }
        assert_eq!(body[4].len(), 12);
    }
}
