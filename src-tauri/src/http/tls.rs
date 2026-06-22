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

use rcgen::{CertificateParams, DnType, KeyPair, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

/// A self-signed certificate and its private key, both DER-encoded.
pub struct SelfSignedCert {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

/// Subject Alternative Names for the cert: `localhost`, both loopback IPs, and
/// every supplied non-loopback interface IP. Loopback IPs are added once even
/// if `interface_ips` repeats them.
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
        if !ip.is_loopback() {
            sans.push(SanType::IpAddress(*ip));
        }
    }
    sans
}

/// Generate a fresh self-signed certificate covering `interface_ips`.
///
/// Validity is pinned to a wide fixed window (2020–2035) rather than "now + N"
/// so the handshake never fails on clock skew and the persisted cert keeps
/// working for years without regeneration.
pub fn generate(interface_ips: &[IpAddr]) -> Result<SelfSignedCert, rcgen::Error> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.subject_alt_names = san_entries(interface_ips);
    params
        .distinguished_name
        .push(DnType::CommonName, "Buildmesh (self-signed)");
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2035, 1, 1);

    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    Ok(SelfSignedCert {
        cert_der: cert.der().as_ref().to_vec(),
        key_der: key_pair.serialize_der(),
    })
}

/// The non-loopback interface IPs a cert must cover, canonicalised (sorted,
/// deduped, as strings) so it can be persisted and compared. Loopback/localhost
/// SANs are constant and never part of this key.
fn interface_san_key(interface_ips: &[IpAddr]) -> Vec<String> {
    let mut v: Vec<String> = interface_ips
        .iter()
        .filter(|ip| !ip.is_loopback())
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

/// Load the persisted cert from `dir`, or generate + persist a new one. The
/// persisted cert is reused only while it still covers the current
/// `interface_ips` (a sidecar `sans.txt` records what it was minted for); a LAN
/// IP change (DHCP, VPN, new subnet) forces regeneration so a client never
/// hits a SAN/IP mismatch. A regenerated cert means a phone must re-trust it;
/// deleting the `tls/` dir is the supported "rotate the cert" gesture.
pub fn load_or_generate(dir: &Path, interface_ips: &[IpAddr]) -> io::Result<SelfSignedCert> {
    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.der");
    let sans_path = dir.join("sans.txt");

    let wanted = interface_san_key(interface_ips);
    if let (Ok(cert_der), Ok(key_der)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
        if !cert_der.is_empty() && !key_der.is_empty() && persisted_covers(&sans_path, &wanted) {
            return Ok(SelfSignedCert { cert_der, key_der });
        }
    }

    let cert = generate(interface_ips).map_err(io::Error::other)?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&cert_path, &cert.cert_der)?;
    std::fs::write(&key_path, &cert.key_der)?;
    std::fs::write(&sans_path, wanted.join("\n"))?;
    Ok(cert)
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

/// Load-or-generate the persisted cert under `dir` and build its acceptor.
pub fn acceptor(dir: &Path, interface_ips: &[IpAddr]) -> io::Result<TlsAcceptor> {
    let cert = load_or_generate(dir, interface_ips)?;
    acceptor_from(&cert).map_err(io::Error::other)
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

    #[test]
    fn generate_produces_non_empty_der() {
        let cert = generate(&[]).unwrap();
        assert!(!cert.cert_der.is_empty());
        assert!(!cert.key_der.is_empty());
    }

    #[test]
    fn acceptor_builds_from_generated_cert() {
        let cert = generate(&[]).unwrap();
        // Proves the cert+key parse into a rustls ServerConfig under the ring
        // provider — the failure mode if the provider/feature wiring is wrong.
        assert!(acceptor_from(&cert).is_ok());
    }

    #[test]
    fn load_or_generate_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate(dir.path(), &[]).unwrap();
        // Second call must reuse the persisted bytes, not regenerate.
        let second = load_or_generate(dir.path(), &[]).unwrap();
        assert_eq!(first.cert_der, second.cert_der);
        assert_eq!(first.key_der, second.key_der);
    }

    #[test]
    fn load_or_generate_regenerates_when_interface_ip_changes() {
        let dir = tempfile::tempdir().unwrap();
        let ip_a: IpAddr = "192.168.1.5".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.9".parse().unwrap();

        let first = load_or_generate(dir.path(), &[ip_a]).unwrap();
        // Same interface set → reuse the persisted cert.
        let same = load_or_generate(dir.path(), &[ip_a]).unwrap();
        assert_eq!(first.cert_der, same.cert_der, "unchanged interface set reuses the cert");
        // A shrunk set (only loopback now) still passes — an extra stale SAN is harmless.
        let shrunk = load_or_generate(dir.path(), &[]).unwrap();
        assert_eq!(first.cert_der, shrunk.cert_der, "a covered (subset) request reuses the cert");
        // A new interface IP the persisted cert doesn't cover → regenerate, or a
        // phone connecting to the new IP would hit a SAN/name mismatch.
        let changed = load_or_generate(dir.path(), &[ip_b]).unwrap();
        assert_ne!(first.cert_der, changed.cert_der, "a new interface IP forces regeneration");
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

        let cert = generate(&[]).unwrap();
        let acceptor = acceptor_from(&cert).unwrap();

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

        // Client: trust the self-signed cert, connect as `localhost`.
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(cert.cert_der.clone()))
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
}
