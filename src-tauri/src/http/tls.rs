//! Self-signed TLS for the opt-in LAN/VPN exposure path (issue #501).
//!
//! When the server is exposed beyond loopback, the externally-reachable
//! interfaces serve HTTPS/WSS with a self-signed certificate generated here.
//! Loopback stays plain HTTP so the local attention webhook keeps working
//! (see `http::bind_specs`).
//!
//! The certificate is persisted as DER under `<app-data>/tls/` so a phone that
//! has trusted the cert once keeps trusting it across restarts. It carries SANs
//! for `localhost`, both loopback IPs, and every non-loopback interface IP, so a
//! client connecting to `https://<lan-ip>:port` or `https://localhost` gets a
//! name match.
//!
//! Crypto provider: `ring`, selected explicitly via `builder_with_provider` so
//! the server never depends on a process-default `CryptoProvider` (the tree has
//! no aws-lc-rs; see Cargo.toml).

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::Arc;

use rcgen::{
    CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsAcceptor;

/// A self-signed certificate and its private key, both DER-encoded.
pub struct SelfSignedCert {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

/// A root CA + leaf-cert pair. The root is what the user installs on their
/// phone as a trusted root CA (Android refuses a CA install unless the cert
/// declares `CA:TRUE`; iOS wants the same via `.p12`); the leaf is what
/// the dev binary serves over HTTPS. Both have the same private/public key
/// pair as a self-signed cert would, BUT structurally the leaf carries
/// `CA:FALSE` (rustls rejects a `CA:TRUE` cert as a TLS leaf with
/// `CaUsedAsEndEntity`) and chains to the root via a real signature, so
/// Chrome/Safari/Android validate the leaf → root path and the install
/// path is satisfied.
pub struct CertChain {
    pub root_cert_der: Vec<u8>,
    pub leaf: SelfSignedCert,
}

/// Subject Alternative Names for the cert: `localhost`, both loopback IPs, and
/// every supplied reachable (non-loopback, non-link-local) interface IP — the
/// exact set `http::bind_specs` opens TLS listeners for. Reusing the binder's
/// [`super::is_link_local`] is deliberate: the cert must cover precisely what we
/// bind. Link-local addresses (IPv4 APIPA `169.254.0.0/16`, IPv6 `fe80::/10`)
/// are never bound (a phone can't reach a scoped address) AND are the most
/// volatile addresses on a dev box — APIPA appears whenever a NIC loses its
/// DHCP lease, link-local IPv6 can be privacy-randomised. If they were in the
/// SAN set they'd also be in the regeneration key ([`interface_san_key`]), so
/// any network flicker would re-mint the root CA and silently invalidate the
/// cert the user already installed on their phone — the next handshake then
/// fails with `CertificateUnknown` (46). Excluding them keeps the cert stable
/// across network churn.
///
/// Loopback IPs are added once even if `interface_ips` repeats them, and the
/// returned list is deduplicated so a routable IP that exists on multiple
/// physical NICs does NOT appear twice — webpki-based TLS stacks
/// (iOS/Android/Chrome) reject duplicate SAN entries as malformed
/// (`AlertDescription::CertificateUnknown`); RFC 5280 §4.2.1.6 requires
/// "each name … SHALL be specified once".
fn san_entries(interface_ips: &[IpAddr]) -> Vec<SanType> {
    let mut sans = vec![
        SanType::DnsName(
            "localhost"
                .try_into()
                .expect("localhost is a valid DNS name"),
        ),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
    ];
    for ip in interface_ips {
        if !ip.is_loopback() && !super::is_link_local(ip) {
            sans.push(SanType::IpAddress(*ip));
        }
    }
    // Stable sort by a stringified key then dedup — `SanType` itself isn't
    // `Ord`, so we project to a comparable form. `SanType::PartialEq` already
    // compares the inner values, so a content-based dedup collapses a routable
    // interface IP that arrives twice when two physical NICs carry it.
    sans.sort_by_key(|s| match s {
        SanType::DnsName(d) => format!("dns:{}", d.as_ref()),
        SanType::IpAddress(ip) => format!("ip:{}", ip),
        _ => format!("{:?}", s),
    });
    sans.dedup();
    sans
}

/// Build the [`CertificateParams`] used by both `generate` and the regression
/// tests. Pulled out so tests can assert the purpose extensions WITHOUT
/// re-parsing the generated DER — Chrome/Safari reject TLS server certs that
/// don't declare `ExtendedKeyUsage::ServerAuth` (RFC 5280 §4.2.1.12: "If the
/// extension is present, then the certificate MUST only be used for one of
/// the purposes indicated"). The pre-fix cert was missing this and the phone
/// responded with `AlertDescription::CertificateUnknown` (46).
fn build_params(interface_ips: &[IpAddr]) -> Result<CertificateParams, rcgen::Error> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.subject_alt_names = san_entries(interface_ips);
    params
        .distinguished_name
        .push(DnType::CommonName, "Buildmesh (self-signed)");
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2035, 1, 1);
    // Chrome (TLS 1.3, BoringSSL) and Safari (Network.framework) refuse to
    // handshake with a TLS server cert that doesn't carry an EKU of
    // `serverAuth`; the error surfaces as `AlertDescription::CertificateUnknown`
    // (alert 46 — rustls's `CertificateError::Other` catch-all). Pair it with
    // `DigitalSignature` so the KeyUsage extension is also populated (some
    // validators reject a TLS server cert with no KeyUsage at all).
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    // Basic Constraints: CA=FALSE. Chrome rejects end-entity TLS server certs
    // without it (CA/Browser Forum baseline §7.1.2.1). rcgen's default
    // `IsCa::NoCa` would OMIT the extension — `ExplicitNoCa` is what emits it.
    params.is_ca = IsCa::ExplicitNoCa;
    Ok(params)
}

/// Build the [`CertificateParams`] for the root CA. `CA:TRUE` (mandatory
/// for the Android install flow) with `keyCertSign` + `cRLSign` KeyUsage
/// (the bits a CA needs to sign certs and CRLs). No SAN — root CAs don't
/// match a hostname, they validate it.
fn build_root_ca_params() -> Result<CertificateParams, rcgen::Error> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params
        .distinguished_name
        .push(DnType::CommonName, "Buildmesh Dev Root CA");
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2035, 1, 1);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    Ok(params)
}

/// Generate a fresh root-CA + leaf pair for `interface_ips`.
///
/// Validity is pinned to a wide fixed window (2020–2035) rather than "now + N"
/// so the handshake never fails on clock skew and the persisted cert keeps
/// working for years without regeneration. The root has no SAN — it
/// validates the leaf, it isn't itself a TLS endpoint.
pub fn generate(interface_ips: &[IpAddr]) -> Result<CertChain, rcgen::Error> {
    // Root CA — self-signed with CA:TRUE.
    let root_params = build_root_ca_params()?;
    let root_key = KeyPair::generate()?;
    let root_cert = root_params.self_signed(&root_key)?;
    let root_cert_der = root_cert.der().as_ref().to_vec();

    // Leaf — TLS server cert shape (CA:FALSE, serverAuth EKU, DigitalSignature
    // KU) signed by the root above.
    let leaf_params = build_params(interface_ips)?;
    let leaf_key = KeyPair::generate()?;
    let leaf_cert = leaf_params.signed_by(&leaf_key, &root_cert, &root_key)?;
    let leaf_cert_der = leaf_cert.der().as_ref().to_vec();
    let leaf_key_der = leaf_key.serialize_der();

    Ok(CertChain {
        root_cert_der,
        leaf: SelfSignedCert {
            cert_der: leaf_cert_der,
            key_der: leaf_key_der,
        },
    })
}

/// The reachable interface IPs a cert must cover, canonicalised (sorted,
/// deduped, as strings) so it can be persisted and compared. Loopback/localhost
/// SANs are constant and never part of this key; link-local IPs are excluded
/// too (see [`super::is_link_local`]) — they are never bound and their churn
/// would needlessly re-mint the cert, breaking an already-installed phone CA.
///
/// `pub(crate)` so `http::mod` can key the in-process `TlsAcceptor` cache by
/// the same set the persisted cert was minted for (issue #587): a re-toggle
/// with the same interface set must reuse the previously built acceptor
/// instead of re-reading the DER + re-parsing the `ServerConfig`.
pub(crate) fn interface_san_key(interface_ips: &[IpAddr]) -> Vec<String> {
    let mut v: Vec<String> = interface_ips
        .iter()
        .filter(|ip| !ip.is_loopback() && !super::is_link_local(ip))
        .map(|ip| ip.to_string())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Does the persisted SAN sidecar cover every interface IP we now need? A
/// missing/unreadable sidecar (an older cert minted before this check, or a
/// network change that added an IP) returns `false`, forcing regeneration. A
/// shrunk interface set still passes — an extra stale SAN is harmless.
fn persisted_covers(sans_path: &Path, wanted: &[String]) -> bool {
    let Ok(contents) = std::fs::read_to_string(sans_path) else {
        return false;
    };
    let have: std::collections::HashSet<&str> = contents.lines().map(str::trim).collect();
    wanted.iter().all(|ip| have.contains(ip.as_str()))
}

/// Load the persisted cert chain from `dir`, or generate + persist a new one.
/// The persisted chain is reused only while it still covers the current
/// `interface_ips` (a sidecar `sans.txt` records what the leaf was minted for);
/// a LAN IP change (DHCP, VPN, new subnet) forces regeneration so a client
/// never hits a SAN/IP mismatch. A regenerated chain means a phone must
/// re-trust the root; deleting the `tls/` dir is the supported "rotate the
/// cert" gesture.
pub fn load_or_generate(dir: &Path, interface_ips: &[IpAddr]) -> io::Result<CertChain> {
    let ca_path = dir.join("ca.der");
    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.der");
    let sans_path = dir.join("sans.txt");

    let wanted = interface_san_key(interface_ips);
    if let (Ok(ca_der), Ok(cert_der), Ok(key_der)) = (
        std::fs::read(&ca_path),
        std::fs::read(&cert_path),
        std::fs::read(&key_path),
    ) {
        if !ca_der.is_empty() && !cert_der.is_empty() && !key_der.is_empty()
            && persisted_covers(&sans_path, &wanted)
        {
            return Ok(CertChain {
                root_cert_der: ca_der,
                leaf: SelfSignedCert { cert_der, key_der },
            });
        }
    }

    let chain = generate(interface_ips).map_err(io::Error::other)?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&ca_path, &chain.root_cert_der)?;
    std::fs::write(&cert_path, &chain.leaf.cert_der)?;
    std::fs::write(&key_path, &chain.leaf.key_der)?;
    std::fs::write(&sans_path, wanted.join("\n"))?;
    Ok(chain)
}

/// Build a [`TlsAcceptor`] from an in-memory cert + key.
pub fn acceptor_from(cert: &SelfSignedCert) -> Result<TlsAcceptor, rustls::Error> {
    let cert_der = CertificateDer::from(cert.cert_der.clone());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_der.clone()));

    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert_der], key_der)?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Load-or-generate the persisted chain under `dir` and build its acceptor
/// (using the leaf only — the root sits on disk for the user to install
/// on their phone).
pub fn acceptor(dir: &Path, interface_ips: &[IpAddr]) -> io::Result<TlsAcceptor> {
    let chain = load_or_generate(dir, interface_ips)?;
    acceptor_from(&chain.leaf).map_err(io::Error::other)
}

// --- /__certs/status surface (issue #635) ---------------------------------
//
// The QR modal needs to tell the user "the cert you installed on your phone
// has fingerprint X; the server is now serving fingerprint Y" so they know to
// re-install when the dev profile regenerated the root. We don't pull in
// `x509-parser` (50-100 KB compile) just to read two fields — the leaf issuer
// and `not_after` are constants in `build_root_ca_params` / `build_params`,
// pinned by the `cert_status_constants_match_generated_chain` test below.

/// Snapshot of the on-disk cert chain for `GET /__certs/status`.
///
/// Mirrors the `models/mod.rs:262` convention: `valid_until` is the SQLite
/// `YYYY-MM-DD HH:MM:SS` text — the backend never does date math on it, so
/// pulling in `chrono` for an RFC3339 parse is wasted surface. No
/// `chain_valid` field: chain integrity is proven in CI by
/// `leaf_cert_chains_to_root_cert` (openssl verify), not at runtime.
#[derive(Debug, Clone)]
pub struct CertChainStatus {
    pub root_fingerprint_sha256: String,
    pub leaf_fingerprint_sha256: String,
    pub leaf_issuer: String,
    pub valid_until: String,
}

/// SHA-256 fingerprint of a certificate's DER bytes, formatted as colon-
/// separated uppercase hex — the `openssl x509 -fingerprint -sha256 -noout`
/// convention — so a user can paste-compare the modal's text against
/// `openssl` on their own machine. Always 95 chars (32 bytes × 2 hex + 31
/// colons); the length is pinned by `cert_fingerprint_matches_openssl`.
pub fn cert_fingerprint(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    digest
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(":")
}

/// Read the persisted chain from `dir` and produce the diagnostic snapshot
/// served by `GET /__certs/status` (and the desktop Tauri command
/// `get_cert_chain_status`). The HTTP wrapper at `routes::certs::status_json`
/// omits the desktop-only `cert_path` field, and adds a separate accessor for
/// it so the user's Windows username (embedded in `%APPDATA%\<user>\...`)
/// never crosses the LAN.
///
/// Race note: between the two `std::fs::read` calls a concurrent LAN toggle
/// could `load_or_generate` a fresh pair on another thread, returning
/// fingerprints that don't chain. We accept the race — the openssl test
/// `leaf_cert_chains_to_root_cert` proves *generation* integrity, not read
/// integrity, and the window is dominated by user-initiated events.
pub fn cert_status(dir: &Path) -> io::Result<CertChainStatus> {
    let ca_der = std::fs::read(dir.join("ca.der"))?;
    let leaf_der = std::fs::read(dir.join("cert.der"))?;
    Ok(CertChainStatus {
        root_fingerprint_sha256: cert_fingerprint(&ca_der),
        leaf_fingerprint_sha256: cert_fingerprint(&leaf_der),
        // Constant from `build_root_ca_params` — see the pinning test below.
        leaf_issuer: "CN=Buildmesh Dev Root CA".to_string(),
        // Window end from `build_params` (line ~111): pinned to 2035-01-01 so
        // a persisted cert stays valid for years without regen.
        valid_until: "2035-01-01 00:00:00".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn san_entries_cover_localhost_and_interface_ips() {
        let lan: IpAddr = "192.168.1.5".parse().unwrap();
        let sans = san_entries(&[lan, IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        // localhost DNS + 2 loopback IPs + the one non-loopback interface IP.
        // The loopback interface IP passed in is filtered out (already covered).
        assert!(sans
            .iter()
            .any(|s| matches!(s, SanType::DnsName(d) if d.as_ref() == "localhost")));
        assert!(sans
            .iter()
            .any(|s| matches!(s, SanType::IpAddress(ip) if *ip == lan)));
        let loopback_count = sans
            .iter()
            .filter(|s| matches!(s, SanType::IpAddress(ip) if ip.is_loopback()))
            .count();
        assert_eq!(loopback_count, 2, "exactly the two canonical loopback IPs");
    }

    /// Regression pin: a link-local IPv6 that exists on multiple physical NICs
    /// (WiFi + Ethernet both have one) flows through `enumerate_interfaces` more
    /// than once. Without dedup, the SAN list gets that IP twice, and webpki-
    /// based TLS stacks (iOS/Android/Chrome) reject the cert as malformed —
    /// emitting `AlertDescription::CertificateUnknown` (46) on the handshake.
    /// RFC 5280 §4.2.1.6: "Each name … SHALL be specified once".
    #[test]
    fn san_entries_dedup_duplicate_interface_ips() {
        let lan: IpAddr = "192.168.1.5".parse().unwrap();
        let link_local: IpAddr = "fe80::1".parse().unwrap();
        // The same IP twice — the production path that produced the broken
        // cert on Adam's machine (WiFi + Ethernet both carrying fe80::…).
        let sans = san_entries(&[lan, lan, link_local, link_local]);
        let lan_count = sans
            .iter()
            .filter(|s| matches!(s, SanType::IpAddress(ip) if *ip == lan))
            .count();
        let link_local_count = sans
            .iter()
            .filter(|s| matches!(s, SanType::IpAddress(ip) if *ip == link_local))
            .count();
        assert_eq!(lan_count, 1, "duplicate LAN IP must collapse to a single SAN");
        assert_eq!(
            link_local_count, 0,
            "link-local IPs are never bound (see http::bind_specs) and are the most \
             volatile addresses on the box, so they MUST be excluded from the SAN set \
             entirely — including them re-mints the cert on every network flap"
        );
    }

    /// Regression pin for the silent-cert-rotation bug (mobile QR black screen):
    /// link-local addresses — IPv4 APIPA `169.254.0.0/16` and IPv6 `fe80::/10` —
    /// MUST NOT appear in the cert SAN set. They are never bound as exposed
    /// interfaces (`http::bind_specs` skips them), yet they are the most volatile
    /// addresses on a dev box: APIPA appears whenever a NIC loses its DHCP lease
    /// and link-local IPv6 can be privacy-randomised. Including them put them in
    /// the regeneration key, so any network flicker re-minted the root CA and
    /// silently invalidated the cert the user had already installed on their
    /// phone — the handshake then failed with `CertificateUnknown` (46).
    #[test]
    fn san_entries_excludes_link_local() {
        let lan: IpAddr = "192.168.1.10".parse().unwrap();
        let apipa: IpAddr = "169.254.143.41".parse().unwrap();
        let ll6: IpAddr = "fe80::484e:b865:e74e:e8be".parse().unwrap();
        let sans = san_entries(&[lan, apipa, ll6]);
        assert!(
            sans.iter()
                .any(|s| matches!(s, SanType::IpAddress(ip) if *ip == lan)),
            "the reachable LAN IP must be present in the SAN set"
        );
        assert!(
            !sans
                .iter()
                .any(|s| matches!(s, SanType::IpAddress(ip) if *ip == apipa || *ip == ll6)),
            "link-local (APIPA / fe80::) IPs must be excluded from the SAN set"
        );
    }

    /// The regeneration key drives `persisted_covers`: if a link-local IP is in
    /// the key, its appearance/disappearance forces a regenerate. Excluding them
    /// keeps the key — and therefore the persisted cert — stable across the
    /// network churn that was invalidating the user's installed root.
    #[test]
    fn interface_san_key_excludes_link_local() {
        let lan: IpAddr = "192.168.1.10".parse().unwrap();
        let apipa: IpAddr = "169.254.143.41".parse().unwrap();
        let ll6: IpAddr = "fe80::1".parse().unwrap();
        assert_eq!(
            interface_san_key(&[lan, apipa, ll6]),
            vec!["192.168.1.10".to_string()],
            "only reachable, non-link-local interface IPs key the cert"
        );
    }

    /// End-to-end regression for the recurring "I reinstalled the cert and it
    /// broke again" report: a cert minted for the real LAN IP must be REUSED —
    /// not regenerated — when only link-local addresses come and go. A regenerate
    /// here mints a fresh root keypair and invalidates the phone's installed CA.
    #[test]
    fn load_or_generate_stable_across_link_local_churn() {
        let dir = tempfile::tempdir().unwrap();
        let lan: IpAddr = "192.168.1.10".parse().unwrap();
        let apipa: IpAddr = "169.254.5.5".parse().unwrap();
        let ll6: IpAddr = "fe80::abcd".parse().unwrap();

        let first = load_or_generate(dir.path(), &[lan]).unwrap();
        // A NIC drops to APIPA and a link-local IPv6 appears — pure churn.
        let after_churn = load_or_generate(dir.path(), &[lan, apipa, ll6]).unwrap();
        assert_eq!(
            first.root_cert_der, after_churn.root_cert_der,
            "link-local churn must NOT re-mint the root CA (phone keeps trusting it)"
        );
        assert_eq!(
            first.leaf.cert_der, after_churn.leaf.cert_der,
            "link-local churn must NOT re-mint the leaf"
        );
    }

    /// Regression pin: a self-signed TLS server cert MUST declare
    /// `ExtendedKeyUsage::ServerAuth`. Chrome (BoringSSL, TLS 1.3) and Safari
    /// (Network.framework) reject a TLS handshake with a cert that lacks it
    /// — the alert surfaces as `CertificateUnknown` (46) on the rustls server
    /// because BoringSSL's rejection maps through the
    /// `CertificateError::Other` catch-all. The pre-fix cert was missing both
    /// `key_usages` and `extended_key_usages` (rcgen leaves them empty by
    /// default); pinning the values here makes a future regression that drops
    /// them fail this test rather than silently break the mobile QR pairing.
    #[test]
    fn cert_params_declare_server_auth_extended_key_usage() {
        let params = build_params(&[]).expect("build_params");
        assert!(
            params
                .extended_key_usages
                .iter()
                .any(|eku| matches!(eku, ExtendedKeyUsagePurpose::ServerAuth)),
            "self-signed TLS cert MUST declare ExtendedKeyUsagePurpose::ServerAuth \
             (Chrome/Safari otherwise reject with CertificateUnknown)"
        );
        assert!(
            params
                .key_usages
                .iter()
                .any(|ku| matches!(ku, KeyUsagePurpose::DigitalSignature)),
            "self-signed TLS cert SHOULD declare KeyUsagePurpose::DigitalSignature"
        );
    }

    /// Regression pin: the cert MUST emit `BasicConstraints` with `CA:FALSE`
    /// (RFC 5280 §4.2.1.9). Chrome rejects end-entity TLS server certs
    /// without it (CA/Browser Forum baseline §7.1.2.1). rustls also refuses
    /// to terminate a TLS handshake with a CA:TRUE cert as the leaf —
    /// `CaUsedAsEndEntity` alert — so a self-signed cert can NOT double-duty
    /// as both the CA root (for Android install) and the TLS leaf. The
    /// Android-install path needs a separate root + leaf PKI; see the
    /// follow-up note in this test.
    #[test]
    fn cert_params_emit_basic_constraints_ca_false() {
        let params = build_params(&[]).expect("build_params");
        assert!(
            matches!(params.is_ca, IsCa::ExplicitNoCa),
            "self-signed TLS leaf cert MUST emit BasicConstraints CA:FALSE; got {:?}",
            params.is_ca
        );
    }

    #[test]
    fn generate_produces_non_empty_der() {
        let chain = generate(&[]).unwrap();
        assert!(!chain.root_cert_der.is_empty(), "root cert must be non-empty");
        assert!(!chain.leaf.cert_der.is_empty(), "leaf cert must be non-empty");
        assert!(!chain.leaf.key_der.is_empty(), "leaf key must be non-empty");
    }

    #[test]
    fn acceptor_builds_from_generated_cert() {
        let chain = generate(&[]).unwrap();
        // Proves the cert+key parse into a rustls ServerConfig under the ring
        // provider — the failure mode if the provider/feature wiring is wrong.
        assert!(acceptor_from(&chain.leaf).is_ok());
    }

    #[test]
    fn load_or_generate_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate(dir.path(), &[]).unwrap();
        // Second call must reuse the persisted bytes, not regenerate.
        let second = load_or_generate(dir.path(), &[]).unwrap();
        assert_eq!(first.root_cert_der, second.root_cert_der);
        assert_eq!(first.leaf.cert_der, second.leaf.cert_der);
        assert_eq!(first.leaf.key_der, second.leaf.key_der);
    }

    #[test]
    fn load_or_generate_regenerates_when_interface_ip_changes() {
        let dir = tempfile::tempdir().unwrap();
        let ip_a: IpAddr = "192.168.1.5".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.9".parse().unwrap();

        let first = load_or_generate(dir.path(), &[ip_a]).unwrap();
        // Same interface set → reuse the persisted cert.
        let same = load_or_generate(dir.path(), &[ip_a]).unwrap();
        assert_eq!(first.leaf.cert_der, same.leaf.cert_der, "unchanged interface set reuses the cert");
        // A shrunk set (only loopback now) still passes — an extra stale SAN is harmless.
        let shrunk = load_or_generate(dir.path(), &[]).unwrap();
        assert_eq!(first.leaf.cert_der, shrunk.leaf.cert_der, "a covered (subset) request reuses the cert");
        // A new interface IP the persisted cert doesn't cover → regenerate, or a
        // phone connecting to the new IP would hit a SAN/name mismatch.
        let changed = load_or_generate(dir.path(), &[ip_b]).unwrap();
        assert_ne!(first.leaf.cert_der, changed.leaf.cert_der, "a new interface IP forces regeneration");
    }

    /// Regression pin for the root+leaf PKI: the root cert MUST declare
    /// `CA:TRUE` so Android accepts it as a trusted root during install, AND
    /// the leaf MUST declare `CA:FALSE` so rustls accepts it as the TLS leaf
    /// (rustls rejects `CA:TRUE` certs as leaves with `CaUsedAsEndEntity`).
    /// A self-signed cert can't satisfy both at once — that's why we now
    /// generate two distinct certs with the same keypair root.
    #[test]
    fn root_cert_emits_basic_constraints_ca_true() {
        let chain = generate(&[]).expect("generate");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("root.der"), &chain.root_cert_der).unwrap();
        let output = std::process::Command::new("openssl")
            .args(["x509", "-inform", "DER", "-in", dir.path().join("root.der").to_str().unwrap(),
                   "-noout", "-ext", "basicConstraints"])
            .output()
            .expect("openssl must be on PATH");
        assert!(output.status.success(), "openssl failed: {}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("CA:TRUE"),
            "root cert MUST declare BasicConstraints CA:TRUE (Android install requirement); got: {stdout}"
        );
    }

    #[test]
    fn leaf_cert_chains_to_root_cert() {
        // The whole point of the new PKI: openssl can verify the leaf by
        // chaining to the root. Without a real signature this would fail with
        // "unable to get local issuer". We use PEM form because the openssl
        // 1.1.1i that ships with Git for Windows refuses to load DER via
        // `-CAfile` with `Error loading file` regardless of path syntax.
        let chain = generate(&[]).expect("generate");
        let dir = tempfile::tempdir().unwrap();
        let pem = |label: &str, der: &[u8]| -> String {
            use std::fmt::Write;
            let mut s = String::with_capacity(der.len() * 4 / 3 + 64);
            s.push_str("-----BEGIN ");
            s.push_str(label);
            s.push_str("-----\n");
            for chunk in der.chunks(48) {
                // base64 with `STANDARD_NO_PAD` would be nicer but the
                // standard alphabet + `=` padding is what openssl accepts.
                write!(
                    s,
                    "{}\n",
                    base64_encode(chunk)
                ).unwrap();
            }
            s.push_str("-----END ");
            s.push_str(label);
            s.push_str("-----\n");
            s
        };
        std::fs::write(
            dir.path().join("root.pem"),
            pem("CERTIFICATE", &chain.root_cert_der),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("leaf.pem"),
            pem("CERTIFICATE", &chain.leaf.cert_der),
        )
        .unwrap();
        let output = std::process::Command::new("openssl")
            .args([
                "verify",
                "-CAfile",
                dir.path().join("root.pem").to_str().unwrap(),
                dir.path().join("leaf.pem").to_str().unwrap(),
            ])
            .output()
            .expect("openssl must be on PATH");
        assert!(
            output.status.success(),
            "leaf MUST chain to root via real signature; stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Minimal standard-alphabet base64 encoder (with `=` padding) for tests
    /// that need to shell out to openssl. We avoid pulling the `pem` crate
    /// just for two test certs.
    fn base64_encode(data: &[u8]) -> String {
        const ALPHA: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
        let chunks = data.chunks(3);
        let mut last_len = 0;
        for chunk in chunks {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let n = chunk.len();
            last_len = n;
            out.push(ALPHA[(b0 >> 2) as usize] as char);
            out.push(ALPHA[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if n > 1 {
                out.push(ALPHA[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if n > 2 {
                out.push(ALPHA[(b2 & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        let _ = last_len;
        out
    }

    /// End-to-end SSL handshake (issue #501 AC4): a real client completes a TLS
    /// handshake against a listener using the production acceptor, and
    /// application bytes flow both ways. The client trusts the self-signed cert
    /// by adding it to its root store and connects as `localhost` — so this also
    /// proves the cert's `localhost` SAN verifies, not just that a handshake of
    /// any kind occurs.
    #[tokio::test]
    async fn tls_handshake_succeeds_and_round_trips_bytes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};
        use tokio_rustls::TlsConnector;

        let chain = generate(&[]).unwrap();
        let acceptor = acceptor_from(&chain.leaf).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server: accept one connection, TLS-handshake it, echo a reply.
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(tcp).await.expect("server TLS handshake");
            let mut buf = [0u8; 5];
            tls.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello");
            tls.write_all(b"world").await.unwrap();
            tls.flush().await.unwrap();
        });

        // Client: trust the root (the leaf is now signed by the root — see
        // `leaf_cert_chains_to_root_cert` for the validation that proves it).
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(chain.root_cert_der.clone()))
            .unwrap();
        let client_config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_config));

        let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut tls = connector
            .connect(server_name, tcp)
            .await
            .expect("client TLS handshake against self-signed cert");

        tls.write_all(b"hello").await.unwrap();
        tls.flush().await.unwrap();
        let mut reply = [0u8; 5];
        tls.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"world");
    }

    // --- /__certs/status surface (issue #635) ---------------------------------
    // These pin the shape of the diagnostic endpoint that lets the QR modal
    // tell the user "the cert you installed on your phone has fingerprint X;
    // the server is now serving fingerprint Y" without the user having to
    // reach for `openssl` themselves.

    /// SHA-256 of the DER bytes, colon-separated uppercase hex. Matches
    /// `openssl x509 -fingerprint -sha256 -noout` so a user can paste-compare
    /// what the modal shows against `openssl` on their own machine.
    #[test]
    fn cert_fingerprint_matches_openssl() {
        let chain = generate(&[]).expect("generate");
        let dir = tempfile::tempdir().unwrap();
        // `-inform DER` is sufficient for `-fingerprint` (the `-CAfile` DER
        // issue in `leaf_cert_chains_to_root_cert` is specific to *chain
        // verification* with `-CAfile`, not `-fingerprint`).
        std::fs::write(dir.path().join("root.der"), &chain.root_cert_der).unwrap();

        let got = crate::http::tls::cert_fingerprint(&chain.root_cert_der);
        let output = std::process::Command::new("openssl")
            .args([
                "x509",
                "-inform",
                "DER",
                "-in",
                dir.path().join("root.der").to_str().unwrap(),
                "-fingerprint",
                "-sha256",
                "-noout",
            ])
            .output()
            .expect("openssl must be on PATH");
        assert!(
            output.status.success(),
            "openssl failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // openssl prints `SHA256 Fingerprint=AB:CD:...` (OpenSSL 3.x uses
        // uppercase; older versions and LibreSSL emit `sha256 Fingerprint=`).
        // Accept either — the equals + fingerprint is what we care about.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        let openssl_fp = trimmed
            .strip_prefix("SHA256 Fingerprint=")
            .or_else(|| trimmed.strip_prefix("sha256 Fingerprint="))
            .unwrap_or_else(|| panic!("unexpected openssl output: {stdout}"));
        assert_eq!(
            got, openssl_fp,
            "cert_fingerprint output must match openssl's colon-separated uppercase hex"
        );
        // 32 bytes × 2 hex chars + 31 colons = 95 chars.
        assert_eq!(got.len(), 95, "SHA-256 fingerprint must be 95 chars (32 bytes colon-separated)");
    }

    /// `cert_status` on a freshly generated chain populates all four fields.
    #[test]
    fn cert_status_loads_persisted_chain() {
        let dir = tempfile::tempdir().unwrap();
        let _chain = load_or_generate(dir.path(), &[]).expect("load_or_generate");

        let status = crate::http::tls::cert_status(dir.path()).expect("cert_status");
        assert_eq!(status.root_fingerprint_sha256.len(), 95);
        assert_eq!(status.leaf_fingerprint_sha256.len(), 95);
        assert!(
            status.leaf_issuer.contains("Buildmesh Dev Root CA"),
            "leaf issuer must be the root's CN; got: {:?}",
            status.leaf_issuer
        );
        // The validity window is pinned 2020-01-01 .. 2035-01-01 in build_params
        // (see the comment at the top of that function).
        assert!(
            status.valid_until.starts_with("2035-01-01"),
            "valid_until must start with the pinned 2035-01-01 expiry; got: {:?}",
            status.valid_until
        );
        // Root and leaf must have DIFFERENT fingerprints — rcgen generates a
        // fresh keypair per cert in `generate()`.
        assert_ne!(
            status.root_fingerprint_sha256, status.leaf_fingerprint_sha256,
            "root and leaf fingerprints must differ (distinct keypairs)"
        );
    }

    /// The load-bearing property: when load_or_generate regenerates the chain
    /// (because the SAN set changed), cert_status reflects the NEW fingerprints.
    /// This is what lets the modal tell the user their installed root is stale.
    #[test]
    fn cert_status_reflects_regen_after_dir_swap() {
        let dir = tempfile::tempdir().unwrap();
        let ip_a: IpAddr = "192.168.1.5".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.9".parse().unwrap();

        let _first = load_or_generate(dir.path(), &[ip_a]).unwrap();
        let before = crate::http::tls::cert_status(dir.path()).unwrap();

        // Force regeneration via a new interface IP — `persisted_covers` fails
        // because the SAN sidecar doesn't contain 10.0.0.9.
        let _second = load_or_generate(dir.path(), &[ip_b]).unwrap();
        let after = crate::http::tls::cert_status(dir.path()).unwrap();

        assert_ne!(
            before.root_fingerprint_sha256, after.root_fingerprint_sha256,
            "regen after interface IP change MUST produce a new root fingerprint"
        );
        assert_ne!(
            before.leaf_fingerprint_sha256, after.leaf_fingerprint_sha256,
            "regen after interface IP change MUST produce a new leaf fingerprint"
        );
    }

    /// Missing cert files are the realistic failure mode for a fresh install
    /// or a wiped `<app-data>/tls/`. The endpoint must surface a 503, not
    /// panic, so the QR modal can fall back to its other content.
    ///
    /// Note: we deliberately do NOT detect in-content corruption (e.g. a
    /// truncated half-written DER). Detecting that would need an X.509 parser
    /// (`x509-parser`, ~50-100 KB compile) which isn't worth the dep just to
    /// read two constant fields. SHA-256 is computed on whatever bytes are
    /// on disk; a corrupt file still yields a stable fingerprint. The openssl
    /// test `leaf_cert_chains_to_root_cert` proves *generation* integrity, not
    /// read integrity.
    #[test]
    fn cert_status_handles_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        // No ca.der or cert.der present.
        assert!(
            crate::http::tls::cert_status(dir.path()).is_err(),
            "missing cert files must surface as io::Error so the HTTP route can 503"
        );
    }

    /// Drift guard: the `leaf_issuer` and `valid_until` strings hardcoded in
    /// `cert_status` must stay in sync with `build_root_ca_params` /
    /// `build_params`. We shell out to openssl — the same tool a user would
    /// reach for — and assert the parsed values match what `cert_status` reports.
    /// A future bump of the validity window or root CN fails this test and
    /// forces the constant to be updated alongside the gen function.
    #[test]
    fn cert_status_constants_match_generated_chain() {
        let chain = generate(&[]).expect("generate");
        let dir = tempfile::tempdir().unwrap();
        // cert_status reads `ca.der` and `cert.der` — write both, not just the
        // leaf (cert_status computes BOTH fingerprints).
        std::fs::write(dir.path().join("ca.der"), &chain.root_cert_der).unwrap();
        std::fs::write(dir.path().join("cert.der"), &chain.leaf.cert_der).unwrap();

        let status = crate::http::tls::cert_status(dir.path()).unwrap();

        // Parse the leaf's ISSUER via openssl (NOT its subject — `-subject` would
        // show `Buildmesh (self-signed)` from `build_params`, while `-issuer`
        // shows the root's CN, which is what `cert_status` reports as
        // `leaf_issuer`). `issuer=CN = Buildmesh Dev Root CA, ...` is the
        // RFC4514 printable form.
        let issuer_out = std::process::Command::new("openssl")
            .args([
                "x509",
                "-inform", "DER",
                "-in", dir.path().join("cert.der").to_str().unwrap(),
                "-noout", "-issuer",
            ])
            .output()
            .expect("openssl must be on PATH");
        assert!(issuer_out.status.success(), "openssl issuer failed");
        let issuer = String::from_utf8_lossy(&issuer_out.stdout);
        assert!(
            issuer.contains("Buildmesh Dev Root CA"),
            "openssl-parsed leaf issuer must contain the root CN; got: {issuer}"
        );
        assert_eq!(
            status.leaf_issuer, "CN=Buildmesh Dev Root CA",
            "cert_status leaf_issuer must match the constant the user sees"
        );

        // Parse the leaf's notAfter via openssl — `notAfter=Jan  1 00:00:00 2035 GMT`.
        let dates_out = std::process::Command::new("openssl")
            .args([
                "x509",
                "-inform", "DER",
                "-in", dir.path().join("cert.der").to_str().unwrap(),
                "-noout", "-dates",
            ])
            .output()
            .expect("openssl must be on PATH");
        assert!(dates_out.status.success(), "openssl dates failed");
        let dates = String::from_utf8_lossy(&dates_out.stdout);
        assert!(
            dates.contains("2035") && dates.contains("Jan") && dates.contains("GMT"),
            "leaf notAfter must include 2035 Jan ... GMT; got: {dates}"
        );
        assert_eq!(
            status.valid_until, "2035-01-01 00:00:00",
            "cert_status valid_until must match the gen-constant in SQLite text form"
        );
    }
}
