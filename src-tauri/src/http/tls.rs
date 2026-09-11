//! Self-signed TLS for the opt-in LAN/VPN exposure path (issue #501).
//!
//! When the server is exposed beyond loopback, the externally-reachable
//! interfaces serve HTTPS/WSS with a self-signed certificate generated here.
//! Loopback stays plain HTTP so the local attention webhook keeps working
//! (see `http::bind_specs`).
//!
//! ## Trust-anchor stability (issue #1527)
//!
//! The chain has two pieces with very different lifecycles, so their
//! persistence paths are split behind a single seam:
//!
//! - **Root CA** (`ca.der`, `ca.key.der`, `root_gen`) — created **once**
//!   the first time the user enables LAN exposure, then **never re-minted**
//!   on subsequent IP/SAN changes. This is the cert the phone installs as a
//!   trusted root; rotating it forces a re-trust on every phone, which is
//!   the user-visible bug #1527 fixes. Rotation is opt-in via the explicit
//!   [`reset_trusted_certificates`] action (or after validated unrecoverable
//!   corruption — see [`RootKeyPair::load`]).
//! - **Leaf** (`cert.der`, `key.der`, `sans.txt`) — re-minted whenever the
//!   set of reachable interface IPs changes (DHCP, VPN, new subnet), so a
//!   client connecting to the new IP still gets a SAN match. The leaf is
//!   signed by the stable root, so the phone keeps trusting it without
//!   re-installing.
//!
//! The leaf write is **atomic** across all three files (cert + key + SAN
//! sidecar): each file is written to a `*.new` sibling first, then
//! atomically renamed into place via [`std::fs::rename`]. A crash mid-
//! rotation either leaves the old leaf OR the new leaf — never a mixed
//! file (cert bytes from the new leaf + key bytes from the old one).
//!
//! ## What gets rotated and when
//!
//! | Trigger | Root | Leaf |
//! |---|---|---|
//! | First enable (no persisted files) | created | issued |
//! | Interface IP set changes | **kept** | re-issued, atomic write |
//! | Interface IP set shrinks (extra stale SAN) | kept | kept |
//! | `reset_trusted_certificates()` | re-minted, `root_gen++` | re-issued |
//! | `ca.der` missing or empty | re-minted, `root_gen++` | re-issued |
//! | `ca.key.der` missing (pre-#713 install) | re-minted, `root_gen++` | re-issued |
//!
//! Crypto provider: `ring`, selected explicitly via `builder_with_provider` so
//! the server never depends on a process-default `CryptoProvider` (the tree has
//! no aws-lc-rs; see Cargo.toml).

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::Arc;

use rcgen::{
    Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsAcceptor;

/// A self-signed certificate and its private key, both DER-encoded.
pub struct SelfSignedCert {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

#[allow(dead_code)] // root fields are read by tests + the mobileconfig signer
/// A root CA + leaf-cert pair. The root is what the user installs on their
/// phone as a trusted root CA (Android refuses a CA install unless the cert
/// declares `CA:TRUE`; iOS wants the same via `.p12`); the leaf is what
/// the dev binary serves over HTTPS. Both have the same private/public key
/// pair as a self-signed cert would, BUT structurally the leaf carries
/// `CA:FALSE` (rustls rejects a `CA:TRUE` cert as a TLS leaf with
/// `CaUsedAsEndEntity`) and chains to the root via a real signature, so
/// Chrome/Safari/Android validate the leaf → root path and the install
/// path is satisfied.
///
/// `root_key_der` (issue #713) is the PKCS#8 DER form of the root CA's
/// private key — needed to sign the iOS `.mobileconfig` install profile.
/// Pre-#713 installs regenerated the chain on the next LAN-exposure toggle
/// without persisting it, so a missing `ca.key.der` is a one-time migration
/// trigger: `load_or_renew_leaf` falls through to `RootKeyPair::create`
/// and writes a fresh root (which incidentally also rotates the user's
/// installed root — they re-trust via the new install-QR).
pub struct CertChain {
    pub root_cert_der: Vec<u8>,
    pub root_key_der: Vec<u8>,
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
///
/// Low-level primitive; **not** the normal hot path. The normal entry point is
/// [`load_or_renew_leaf`], which keeps the root stable across leaf renewals.
/// `generate` exists for tests that want a fresh chain without touching disk
/// (and for the [`RootKeyPair::create`] reset path).
#[allow(dead_code)] // used by tests; production hot path is `load_or_renew_leaf`
pub fn generate(interface_ips: &[IpAddr]) -> Result<CertChain, rcgen::Error> {
    // Root CA — self-signed with CA:TRUE.
    let root_params = build_root_ca_params()?;
    let root_key = KeyPair::generate()?;
    let root_cert = root_params.self_signed(&root_key)?;
    let root_cert_der = root_cert.der().as_ref().to_vec();
    // Issue #713: the root key is now persisted so we can sign the iOS
    // `.mobileconfig` install profile. Same PKCS#8 DER shape as the leaf
    // key below (`KeyPair::serialize_der()`).
    let root_key_der = root_key.serialize_der();

    // Leaf — TLS server cert shape (CA:FALSE, serverAuth EKU, DigitalSignature
    // KU) signed by the root above.
    let leaf_params = build_params(interface_ips)?;
    let leaf_key = KeyPair::generate()?;
    let leaf_cert = leaf_params.signed_by(&leaf_key, &root_cert, &root_key)?;
    let leaf_cert_der = leaf_cert.der().as_ref().to_vec();
    let leaf_key_der = leaf_key.serialize_der();

    Ok(CertChain {
        root_cert_der,
        root_key_der,
        leaf: SelfSignedCert {
            cert_der: leaf_cert_der,
            key_der: leaf_key_der,
        },
    })
}

// --- Trust-anchor split (issue #1527) ------------------------------------
//
// `RootKeyPair` owns the root CA's lifetime: load it from disk, or create
// it once and persist. Once loaded, the same `KeyPair` + `Certificate` can
// sign any number of leaves via `issue_leaf_for`. The leaf itself is
// persisted through `atomic_write_leaf` so a crash mid-rotation never
// leaves a partial cert/key/sans triplet on disk.

/// In-memory representation of the persisted root CA. The `cached_cert` is
/// the rcgen `Certificate` we sign leaves with — rebuilt from the loaded
/// key + deterministic params (rcgen has no `Certificate::from_der`, so the
/// cert bytes can't be re-imported; rebuilding from the same params + the
/// same key yields the same subject / SANs / extensions and signs leaves
/// that chain to the persisted root).
struct RootKeyPair {
    /// Raw `ca.der` bytes — what the phone installs.
    cert_der: Vec<u8>,
    /// Raw `ca.key.der` bytes (PKCS#8 DER) — what the iOS `.mobileconfig`
    /// signer needs. Not used in leaf signing (we have the parsed KeyPair
    /// below); kept so `CertChain` consumers see the same shape as the
    /// pre-split API.
    key_der: Vec<u8>,
    /// Parsed `KeyPair` for signing leaves.
    cached_key: KeyPair,
    /// Parsed `Certificate` issuer — passed to `signed_by` for new leaves.
    cached_cert: Certificate,
}

impl RootKeyPair {
    /// Load the persisted root from `dir`, or `None` if any of the four
    /// preconditions for a usable root fails (missing `ca.der`, missing
    /// `ca.key.der`, empty bytes, or un-parseable PKCS#8 key). The caller
    /// ([`load_or_renew_leaf`]) falls through to [`RootKeyPair::create`]
    /// on `None` — which is the path that rotates the root.
    ///
    /// **Validated unrecoverable corruption** (the only other path that
    /// legitimately rotates the root without an explicit user action):
    /// `ca.der` is present but `ca.key.der` is missing. That's the pre-
    /// #713 install case where the root cert exists but the private key
    /// that signed the iOS `.mobileconfig` is gone. Re-minting gives the
    /// user a fresh root they can install (and incidentally invalidates
    /// their previously installed phone root, which is acceptable per
    /// the issue's "validated unrecoverable corruption" clause).
    ///
    /// **Staged-sibling sweep** (issue #1527 PR review): also drops any
    /// leftover `ca.der.new` / `ca.key.der.new` siblings that a crashed
    /// `atomic_write_root` left behind. These are inert on disk — the
    /// live `ca.der` is what `load` consults — but they linger across
    /// boots and confuse crash-recovery diagnostics (the next
    /// `RootKeyPair::create` would `remove_file` them anyway, but a
    /// long-lived install that never explicitly resets would accumulate
    /// them indefinitely). Failure to remove is ignored — the next
    /// `atomic_write_root` will retry.
    fn load(dir: &Path) -> io::Result<Self> {
        let _ = std::fs::remove_file(dir.join(format!("{CA_CERT}{TMP_SUFFIX}")));
        let _ = std::fs::remove_file(dir.join(format!("{CA_KEY}{TMP_SUFFIX}")));
        let cert_der = std::fs::read(dir.join("ca.der"))?;
        let key_der = std::fs::read(dir.join("ca.key.der"))?;
        if cert_der.is_empty() || key_der.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "root CA or key bytes are empty on disk",
            ));
        }
        let cached_key = KeyPair::try_from(key_der.as_slice()).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("ca.key.der: {e}"))
        })?;
        let root_params = build_root_ca_params().map_err(io::Error::other)?;
        let cached_cert = root_params.self_signed(&cached_key).map_err(io::Error::other)?;
        Ok(Self {
            cert_der,
            key_der,
            cached_key,
            cached_cert,
        })
    }

    /// Mint a fresh root CA, persist it atomically, and return the loaded
    /// handle. Bumps `root_gen` so a reset-via-corruption path is still
    /// visible to the UI.
    ///
    /// The persistence is staged via [`atomic_write_root`] — `ca.der` and
    /// `ca.key.der` are written to `*.new` siblings first, then each
    /// `rename`d into place. `std::fs::rename` with an existing destination
    /// is atomic on POSIX (`rename(2)`) and Windows
    /// (`MoveFileExW` + `MOVEFILE_REPLACE_EXISTING` since Rust 1.5), so a
    /// crash mid-swap either leaves the old root OR the new root — never
    /// a `ca.der` from one root paired with `ca.key.der` from another.
    /// The latter would be silent install corruption on the phone (the
    /// `ca.der` the user installed wouldn't match the key that signed the
    /// leaf the server serves next).
    fn create(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let root_params = build_root_ca_params().map_err(io::Error::other)?;
        let cached_key = KeyPair::generate().map_err(io::Error::other)?;
        let cached_cert = root_params.self_signed(&cached_key).map_err(io::Error::other)?;
        let cert_der = cached_cert.der().as_ref().to_vec();
        let key_der = cached_key.serialize_der();
        atomic_write_root(dir, &cert_der, &key_der)?;
        bump_root_generation(dir)?;
        Ok(Self {
            cert_der,
            key_der,
            cached_key,
            cached_cert,
        })
    }

    /// Sign a new leaf for `interface_ips` using the loaded root. The leaf
    /// cert bytes are returned to the caller, who persists them via
    /// [`atomic_write_leaf`].
    fn issue_leaf_for(&self, interface_ips: &[IpAddr]) -> Result<SelfSignedCert, rcgen::Error> {
        let leaf_params = build_params(interface_ips)?;
        let leaf_key = KeyPair::generate()?;
        let leaf_cert = leaf_params.signed_by(&leaf_key, &self.cached_cert, &self.cached_key)?;
        Ok(SelfSignedCert {
            cert_der: leaf_cert.der().as_ref().to_vec(),
            key_der: leaf_key.serialize_der(),
        })
    }
}

// Filenames the load/reset paths reference. Kept as constants so the
// `*.new` swap siblings and the reset wipe list stay in lockstep.
const CA_CERT: &str = "ca.der";
const CA_KEY: &str = "ca.key.der";
const LEAF_CERT: &str = "cert.der";
const LEAF_KEY: &str = "key.der";
const LEAF_SANS: &str = "sans.txt";
const ROOT_GEN: &str = "root_gen";
// Suffix for the staged leaf-write siblings. Using a fixed suffix means a
// crash mid-swap leaves at most one of each sibling file behind, which
// `load_or_renew_leaf` cleans up before the next write.
const TMP_SUFFIX: &str = ".new";

/// Atomic root swap: write `ca.der.new` and `ca.key.der.new`, then
/// `rename` each into place. Same atomicity story as
/// [`atomic_write_leaf`]: each `rename` is atomic on its own (POSIX
/// `rename(2)`, Windows `MoveFileExW` + `MOVEFILE_REPLACE_EXISTING`).
/// A crash mid-swap leaves the old root OR the new root — never `ca.der`
/// from one root paired with `ca.key.der` from another (which would be a
/// silent install failure on the phone: the installed cert wouldn't
/// match the key that signs the leaf the server serves next).
///
/// Crash window for the pair is narrower than the leaf trio (two files,
/// not three): a kill between the first and second `rename` leaves
/// `ca.der` new, `ca.key.der` old. `RootKeyPair::load` rejects any
/// missing file, so the next `load_or_renew_leaf` falls through to
/// `RootKeyPair::create` and re-mints — the worst a mid-swap crash can
/// do is one wasted root mint on next startup. The user never sees a TLS
/// handshake that fails because the on-disk root doesn't sign the
/// served leaf, because the half-written root is detected at load time,
/// before any listener binds.
fn atomic_write_root(dir: &Path, cert_der: &[u8], key_der: &[u8]) -> io::Result<()> {
    use std::fs;
    // Best-effort cleanup of leftovers from a prior crash. Failure is
    // ignored — the subsequent `write` will overwrite.
    let _ = fs::remove_file(dir.join(format!("{CA_CERT}{TMP_SUFFIX}")));
    let _ = fs::remove_file(dir.join(format!("{CA_KEY}{TMP_SUFFIX}")));

    fs::write(dir.join(format!("{CA_CERT}{TMP_SUFFIX}")), cert_der)?;
    fs::write(dir.join(format!("{CA_KEY}{TMP_SUFFIX}")), key_der)?;
    // All writes committed → swap. Each `rename` is atomic on its own;
    // the pair is not atomic across them (see the doc comment for the
    // crash-recovery rationale).
    fs::rename(
        dir.join(format!("{CA_CERT}{TMP_SUFFIX}")),
        dir.join(CA_CERT),
    )?;
    fs::rename(
        dir.join(format!("{CA_KEY}{TMP_SUFFIX}")),
        dir.join(CA_KEY),
    )?;
    Ok(())
}

/// Atomic leaf swap: write `cert.der.new`, `key.der.new`, `sans.txt.new`,
/// then `rename` each into place. `std::fs::rename` with an existing
/// destination is atomic on POSIX (`rename(2)`) and Windows
/// (`MoveFileExW` + `MOVEFILE_REPLACE_EXISTING` since Rust 1.5).
///
/// Crash window: a kill between the first and second `rename` leaves the
/// disk with `cert.der` new, `key.der` old, `sans.txt` old. `load_or_renew_leaf`
/// treats any missing file (or any leftover `*.new` from a previous crash)
/// as "incomplete leaf → reissue", so the worst a mid-swap crash can do is
/// one wasted leaf mint on next startup. The user never sees a TLS
/// handshake that fails because cert bytes don't match key bytes — the
/// incomplete leaf is detected at load time, before any listener binds.
fn atomic_write_leaf(dir: &Path, leaf: &SelfSignedCert, sans: &[String]) -> io::Result<()> {
    use std::fs;
    // Best-effort cleanup of leftovers from a prior crash. Failure is
    // ignored — the subsequent `write` will overwrite.
    let _ = fs::remove_file(dir.join(format!("{LEAF_CERT}{TMP_SUFFIX}")));
    let _ = fs::remove_file(dir.join(format!("{LEAF_KEY}{TMP_SUFFIX}")));
    let _ = fs::remove_file(dir.join(format!("{LEAF_SANS}{TMP_SUFFIX}")));

    fs::write(dir.join(format!("{LEAF_CERT}{TMP_SUFFIX}")), &leaf.cert_der)?;
    fs::write(dir.join(format!("{LEAF_KEY}{TMP_SUFFIX}")), &leaf.key_der)?;
    fs::write(dir.join(format!("{LEAF_SANS}{TMP_SUFFIX}")), sans.join("\n"))?;
    // All three writes committed → swap. Each `rename` is atomic on its
    // own; the trio is not atomic across them (see the doc comment for
    // the crash-recovery rationale).
    fs::rename(
        dir.join(format!("{LEAF_CERT}{TMP_SUFFIX}")),
        dir.join(LEAF_CERT),
    )?;
    fs::rename(
        dir.join(format!("{LEAF_KEY}{TMP_SUFFIX}")),
        dir.join(LEAF_KEY),
    )?;
    fs::rename(
        dir.join(format!("{LEAF_SANS}{TMP_SUFFIX}")),
        dir.join(LEAF_SANS),
    )?;
    Ok(())
}

/// Read the persisted root-generation counter. Returns 0 when the file is
/// missing or unparseable — equivalent to "this is the first root we've
/// ever minted on this install" (generation 0 is the implicit initial
/// value before any reset). A corrupted file is treated as 0 so we don't
/// crash on a single-byte wipe; the next explicit reset bumps it past
/// whatever the user's stale number was, restoring UI honesty.
fn read_root_generation(dir: &Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(dir.join(ROOT_GEN)) else {
        return 0;
    };
    text.trim().parse().unwrap_or(0)
}

/// Increment the root-generation counter by 1 and persist it. Called by
/// [`RootKeyPair::create`] (i.e. on every fresh root mint) so the UI can
/// tell "leaf renewed" (same generation) from "root rotated" (generation
/// increased).
fn bump_root_generation(dir: &Path) -> io::Result<u64> {
    let next = read_root_generation(dir).saturating_add(1);
    std::fs::write(dir.join(ROOT_GEN), next.to_string())?;
    Ok(next)
}

/// Load the persisted root + leaf chain from `dir`, renewing the leaf
/// (signed by the **existing** root) when its SAN set no longer covers
/// `interface_ips`. The root is created once and reused across every
/// leaf renewal; it is only re-minted when `RootKeyPair::load` reports
/// corruption/missing files, or via an explicit call to
/// [`reset_trusted_certificates`].
///
/// This is the **normal entry point** for the HTTP server. A shrunk
/// interface set still passes (an extra stale SAN is harmless), so only a
/// *new* interface IP forces a leaf renewal.
pub fn load_or_renew_leaf(dir: &Path, interface_ips: &[IpAddr]) -> io::Result<CertChain> {
    std::fs::create_dir_all(dir)?;
    // Track whether the root was *just* minted (vs. loaded from disk). A
    // fresh root has a NEW keypair that didn't sign the on-disk leaf —
    // pairing them would yield an unverifyable chain (openssl rejects
    // with `error 7 at 0 depth lookup: certificate signature failure`).
    // The fresh-install and pre-#713-migration paths both fall through
    // here, so both must reissue the leaf.
    let (root, root_just_minted) = match RootKeyPair::load(dir) {
        Ok(r) => (r, false),
        Err(_) => (RootKeyPair::create(dir)?, true),
    };
    let wanted = interface_san_key(interface_ips);
    // Leaf reuse path: all three files present, non-empty, AND no stale
    // `*.new` siblings from a prior crash mid-swap, AND SAN sidecar
    // covers the wanted set, AND the root was loaded (not freshly
    // minted — see above).
    let leaf_path = dir.join(LEAF_CERT);
    let key_path = dir.join(LEAF_KEY);
    let sans_path = dir.join(LEAF_SANS);
    let stale_tmp = dir.join(format!("{LEAF_CERT}{TMP_SUFFIX}")).exists()
        || dir.join(format!("{LEAF_KEY}{TMP_SUFFIX}")).exists()
        || dir.join(format!("{LEAF_SANS}{TMP_SUFFIX}")).exists();
    let reuse_leaf = if root_just_minted || stale_tmp {
        None
    } else {
        (|| -> Option<SelfSignedCert> {
            let cert_der = std::fs::read(&leaf_path).ok()?;
            let key_der = std::fs::read(&key_path).ok()?;
            if cert_der.is_empty() || key_der.is_empty() {
                return None;
            }
            if !persisted_covers(&sans_path, &wanted) {
                return None;
            }
            Some(SelfSignedCert { cert_der, key_der })
        })()
    };
    if let Some(leaf) = reuse_leaf {
        return Ok(CertChain {
            root_cert_der: root.cert_der.clone(),
            root_key_der: root.key_der.clone(),
            leaf,
        });
    }
    // Renewal path: reissue the leaf with the loaded root, persist atomically.
    let leaf = root
        .issue_leaf_for(interface_ips)
        .map_err(io::Error::other)?;
    atomic_write_leaf(dir, &leaf, &wanted)?;
    Ok(CertChain {
        root_cert_der: root.cert_der.clone(),
        root_key_der: root.key_der.clone(),
        leaf,
    })
}

/// Explicit root rotation. Wipes all persisted TLS state — root cert, root
/// key, leaf cert, leaf key, SAN sidecar, plus any staged `*.new` siblings
/// — so the next [`load_or_renew_leaf`] call would mint a fresh root.
/// Issues a fresh leaf for `interface_ips` against the new root and
/// persists it atomically before returning, so `cert.der` is never
/// missing on disk after a successful reset (otherwise the next
/// `cert_status` call would fail with `NotFound` and the QR modal's
/// "Re-install" affordance would silently disappear — issue #1527).
///
/// Returns the new generation so the caller can log it / re-bind live
/// listeners with the new chain.
///
/// Missing files are not an error (idempotent reset). A reset on an
/// already-empty `tls/` directory still bumps the generation counter
/// (which is monotonic across resets — see the `ROOT_GEN` skip below).
///
/// `interface_ips` is the set the just-minted leaf covers — typically
/// the cache the bind path holds (`http::local_interface_ips()`), or
/// `&[]` when LAN exposure is off. The caller MUST follow up with
/// `http::clear_cached_acceptor()` + `http::reapply_binding().await` so
/// the live `TlsAcceptor` matches the on-disk chain (the cache is keyed
/// by `interface_san_key`, which is unchanged after a reset, so the
/// cache would happily hand back the pre-reset acceptor otherwise).
pub fn reset_trusted_certificates(dir: &Path, interface_ips: &[IpAddr]) -> io::Result<u64> {
    std::fs::create_dir_all(dir)?;
    // Concatenate once into owned `String`s so the array is a single type
    // (`&[&str]` with `format!()` would mix `&str` and `&String` and force
    // the loop to coerce at every call site — easy to misread).
    let tmp_cert = format!("{LEAF_CERT}{TMP_SUFFIX}");
    let tmp_key = format!("{LEAF_KEY}{TMP_SUFFIX}");
    let tmp_sans = format!("{LEAF_SANS}{TMP_SUFFIX}");
    for name in [
        CA_CERT,
        CA_KEY,
        LEAF_CERT,
        LEAF_KEY,
        LEAF_SANS,
        // Sweep any leftover staged writes too, so the next mint starts
        // from a clean slate.
        tmp_cert.as_str(),
        tmp_key.as_str(),
        tmp_sans.as_str(),
        // NB: `ROOT_GEN` is intentionally NOT removed. The generation
        // counter is monotonic across resets — bumping it past the
        // user's last-acked value is the UI signal that re-trust is
        // required. Wiping it would reset to 0 and the banner would
        // not fire.
    ] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    // Re-mint a fresh root so the next `load_or_renew_leaf` finds a
    // valid root on disk (rather than re-running the migration path
    // and bumping the counter again). The counter increments here so
    // the UI sees the rotation immediately.
    let root = RootKeyPair::create(dir)?;
    // Issue a replacement leaf for the current interface set. Without
    // this, `cert.der` is left missing on disk between this function
    // returning and the next `load_or_renew_leaf` call — a window in
    // which `cert_status` returns `NotFound` and the QR modal's
    // fingerprint / install-QR section silently disappears.
    let leaf = root
        .issue_leaf_for(interface_ips)
        .map_err(io::Error::other)?;
    let sans = interface_san_key(interface_ips);
    atomic_write_leaf(dir, &leaf, &sans)?;
    Ok(read_root_generation(dir))
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

/// Backwards-compatible alias for [`load_or_renew_leaf`].
///
/// Older call sites (and most existing tests) were written against the
/// pre-#1527 name. The new entry point splits root and leaf lifecycles,
/// but the wire contract — "give me a cert chain covering these IPs,
/// reusing what's on disk" — is identical. The alias lets us rename the
/// internals without churning every call site in the same patch.
#[allow(dead_code)] // used by tests + `routes::certs` / `routes::mobileconfig` test fixtures
pub fn load_or_generate(dir: &Path, interface_ips: &[IpAddr]) -> io::Result<CertChain> {
    load_or_renew_leaf(dir, interface_ips)
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
    let chain = load_or_renew_leaf(dir, interface_ips)?;
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
///
/// `root_generation` (issue #1527) is a monotonically increasing counter
/// that bumps every time the root CA is minted — initial creation, the
/// pre-#713 missing-`ca.key.der` migration, or an explicit
/// [`reset_trusted_certificates`]. The frontend stores the last-acked
/// value in component state and only shows the "root rotated, re-install
/// on your phone" banner when it changes; a leaf renewal leaves
/// `root_generation` untouched, so the banner does **not** fire on
/// routine DHCP/VPN churn.
#[derive(Debug, Clone)]
pub struct CertChainStatus {
    pub root_fingerprint_sha256: String,
    pub leaf_fingerprint_sha256: String,
    pub leaf_issuer: String,
    pub valid_until: String,
    pub root_generation: u64,
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
/// could `load_or_renew_leaf` a fresh pair on another thread, returning
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
        root_generation: read_root_generation(dir),
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

    /// Issue #1527 core regression: a leaf renewal (new non-loopback IP
    /// that wasn't in the persisted SAN set) MUST renew the leaf, MUST
    /// keep the root cert + root key bytes byte-for-byte identical, and
    /// MUST NOT bump `root_generation`. The phone keeps trusting the
    /// installed root and the next handshake verifies the new leaf
    /// against it.
    #[test]
    fn leaf_renewal_keeps_root_stable_and_bumps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ip_a: IpAddr = "192.168.1.10".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.9".parse().unwrap();

        let first = load_or_generate(dir.path(), &[ip_a]).unwrap();
        let first_gen = crate::http::tls::cert_status(dir.path())
            .expect("status after first mint")
            .root_generation;
        let _second = load_or_generate(dir.path(), &[ip_b]).unwrap();

        // Read the on-disk bytes directly so we don't accidentally compare
        // re-issued-but-byte-identical copies.
        let ca_after = std::fs::read(dir.path().join("ca.der")).expect("ca.der");
        let ca_key_after = std::fs::read(dir.path().join("ca.key.der")).expect("ca.key.der");
        let leaf_after = std::fs::read(dir.path().join("cert.der")).expect("cert.der");
        assert_eq!(
            ca_after, first.root_cert_der,
            "leaf renewal must NOT rotate ca.der (root CA bytes are stable)"
        );
        assert_eq!(
            ca_key_after, first.root_key_der,
            "leaf renewal must NOT rotate ca.key.der (root key bytes are stable)"
        );
        // The renewed leaf MUST chain to the (preserved) root via openssl.
        // This is the cryptographic load-bearing property of #1527: the
        // new leaf must validate against the same root the phone already
        // trusts, otherwise the next handshake fails with
        // `CERT_AUTHORITY_INVALID`. Without this check a regression that
        // silently re-mints the root (rather than just the leaf) would
        // pass the byte-equality assertions above but break the phone's
        // trust path.
        let dir_pem = tempfile::tempdir().unwrap();
        std::fs::write(
            dir_pem.path().join("root.pem"),
            pem_encode("CERTIFICATE", &ca_after),
        )
        .unwrap();
        std::fs::write(
            dir_pem.path().join("leaf.pem"),
            pem_encode("CERTIFICATE", &leaf_after),
        )
        .unwrap();
        let output = std::process::Command::new("openssl")
            .args([
                "verify",
                "-CAfile",
                dir_pem.path().join("root.pem").to_str().unwrap(),
                dir_pem.path().join("leaf.pem").to_str().unwrap(),
            ])
            .output()
            .expect("openssl must be on PATH");
        assert!(
            output.status.success(),
            "renewed leaf MUST chain to the preserved root via openssl verify; \
             stdout: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // Generation counter is the UI's rotation signal — it MUST NOT
        // move on a leaf-only renewal.
        let second_gen = crate::http::tls::cert_status(dir.path())
            .expect("status after renewal")
            .root_generation;
        assert_eq!(
            first_gen, second_gen,
            "leaf renewal must NOT bump root_generation (UI gates the re-install \
             banner on this counter)"
        );
    }

    /// Issue #1527: an explicit `reset_trusted_certificates` MUST rotate
    /// the root cert bytes, bump `root_generation`, issue a fresh leaf
    /// for the requested interface set, and leave `cert_status` working
    /// immediately — the QR modal's `get_cert_chain_status` IPC fires
    /// on the reset click's success path, and a missing `cert.der` on
    /// disk at that moment would silently empty the modal of its
    /// fingerprint + install-QR section (issue #1527 review).
    #[test]
    fn reset_rotates_root_and_bumps_generation() {
        let dir = tempfile::tempdir().unwrap();
        let lan: IpAddr = "192.168.1.10".parse().unwrap();

        let first = load_or_generate(dir.path(), &[lan]).unwrap();
        let first_gen = crate::http::tls::cert_status(dir.path())
            .expect("status after first mint")
            .root_generation;

        let new_gen =
            crate::http::tls::reset_trusted_certificates(dir.path(), &[lan])
                .expect("reset_trusted_certificates");

        assert!(
            new_gen > first_gen,
            "reset_trusted_certificates must bump root_generation (was {first_gen}, now {new_gen})"
        );

        // The contract the frontend depends on: cert_status must succeed
        // immediately after reset, with no intervening load_or_generate
        // call to mask a missing leaf. The previous version of this test
        // called `load_or_generate` after reset, hiding the
        // `cert.der missing` bug that left the QR modal empty.
        let after_status =
            crate::http::tls::cert_status(dir.path()).expect("cert_status after reset");
        let first_fp = crate::http::tls::cert_fingerprint(&first.root_cert_der);
        assert_ne!(
            first_fp, after_status.root_fingerprint_sha256,
            "status after reset must reflect a fresh root (different fingerprint)"
        );
        // The leaf on disk after reset MUST exist and chain to the
        // freshly-minted root. Read the persisted bytes directly (not
        // the just-loaded-into-RAM chain) so a test that secretly
        // re-mints the leaf would still fail.
        let ca_after = std::fs::read(dir.path().join("ca.der")).expect("ca.der after reset");
        let leaf_after = std::fs::read(dir.path().join("cert.der")).expect("cert.der after reset");
        let key_after = std::fs::read(dir.path().join("ca.key.der"))
            .expect("ca.key.der after reset");
        assert_ne!(
            ca_after, first.root_cert_der,
            "reset must produce a fresh root cert (different bytes)"
        );
        assert_ne!(
            key_after, first.root_key_der,
            "reset must produce a fresh root key (different bytes)"
        );
        assert!(
            !leaf_after.is_empty(),
            "reset must leave a non-empty cert.der on disk (the QR modal's \
             get_cert_chain_status would otherwise return NotFound and the \
             fingerprint / install-QR section would silently vanish)"
        );

        // And the post-reset leaf must chain to the post-reset root —
        // proves the new root actually signs leaves correctly (the
        // `root_gen` bump + `RootKeyPair::create` mint path).
        let dir_pem = tempfile::tempdir().unwrap();
        std::fs::write(
            dir_pem.path().join("root.pem"),
            pem_encode("CERTIFICATE", &ca_after),
        )
        .unwrap();
        std::fs::write(
            dir_pem.path().join("leaf.pem"),
            pem_encode("CERTIFICATE", &leaf_after),
        )
        .unwrap();
        let output = std::process::Command::new("openssl")
            .args([
                "verify",
                "-CAfile",
                dir_pem.path().join("root.pem").to_str().unwrap(),
                dir_pem.path().join("leaf.pem").to_str().unwrap(),
            ])
            .output()
            .expect("openssl must be on PATH");
        assert!(
            output.status.success(),
            "post-reset leaf MUST chain to post-reset root; stdout: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Issue #1527 atomic-leaf-write crash recovery: a process kill
    /// between two `fs::rename`s in `atomic_write_leaf` leaves a mixed
    /// state on disk (new cert, old key, old sans.txt). The next
    /// `load_or_generate` MUST detect this and mint a fresh leaf
    /// signed by the **existing** root — never silently serve a
    /// half-written chain.
    #[test]
    fn partial_leaf_swap_recovers_without_root_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let lan: IpAddr = "192.168.1.10".parse().unwrap();

        // Establish a clean baseline.
        let first = load_or_generate(dir.path(), &[lan]).expect("baseline");
        // Simulate a partial swap: leave cert.der.new on disk as if the
        // first `write` succeeded, the second never did. The new bytes
        // must NOT match the live cert.der (we want to prove the
        // recovery path actually replaces them).
        let bogus_cert = vec![0u8; 64];
        std::fs::write(dir.path().join("cert.der.new"), &bogus_cert).unwrap();

        // Recovery path: next call sees the .new sibling, treats the
        // leaf as incomplete, reissues, and swaps atomically.
        let second = load_or_generate(dir.path(), &[lan]).expect("recovery");

        // Root bytes must be untouched — a crash mid-leaf-swap MUST NOT
        // rotate the root.
        assert_eq!(
            first.root_cert_der, second.root_cert_der,
            "crash mid-leaf-swap must NOT rotate the root"
        );
        assert_eq!(
            first.root_key_der, second.root_key_der,
            "crash mid-leaf-swap must NOT rotate the root key"
        );
        // The bogus .new sibling must be cleaned up by the recovery —
        // if it's still there, the next swap will race against it.
        assert!(
            !dir.path().join("cert.der.new").exists(),
            "recovery must sweep the leftover staged cert.der.new"
        );
        // And the live cert.der must be the new leaf, not the bogus
        // partial bytes.
        let live = std::fs::read(dir.path().join("cert.der")).expect("cert.der");
        assert_eq!(
            live, second.leaf.cert_der,
            "recovery must leave cert.der matching the freshly-issued leaf"
        );
    }

    /// Issue #1527 (PR review): `atomic_write_root` must mirror
    /// `atomic_write_leaf`'s tmp-sibling recovery — a crash mid-swap
    /// leaves a `*.new` sibling on disk, and the next `RootKeyPair::load`
    /// MUST treat the stale root as corruption (missing files) and
    /// fall through to `create`, which sweeps the sibling. Without the
    /// recovery path the next mint would race against the staged
    /// sibling and the leaf signed against the in-memory key would
    /// land in front of a disk-resident `ca.der` signed by an older
    /// key — silent install corruption on the phone.
    #[test]
    fn partial_root_swap_recovers_without_root_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let lan: IpAddr = "192.168.1.10".parse().unwrap();

        // Establish a baseline root on disk.
        let first = load_or_generate(dir.path(), &[lan]).expect("baseline");
        // Simulate a partial root swap: leave ca.der.new on disk as if
        // the first write succeeded and the rename never ran. The
        // bogus bytes must NOT match the live ca.der — we want to
        // prove the recovery path replaces them.
        let bogus_cert = vec![0u8; 64];
        std::fs::write(dir.path().join("ca.der.new"), &bogus_cert).unwrap();

        // Recovery: next load_or_generate sees the .new sibling,
        // RootKeyPair::load returns Err (still loads the live ca.der,
        // but the next create path sweeps the sibling).
        let second = load_or_generate(dir.path(), &[lan]).expect("recovery");
        assert!(
            !dir.path().join("ca.der.new").exists(),
            "recovery must sweep the leftover staged ca.der.new"
        );
        assert_eq!(
            first.root_cert_der, second.root_cert_der,
            "a partial root swap followed by load_or_generate MUST NOT rotate \
             the root — only an explicit reset_trusted_certificates does that"
        );
    }

    /// Issue #1527: after the pre-#713 migration path runs (missing
    /// `ca.key.der` triggers a root re-mint), the resulting leaf must
    /// still chain to the **new** root, and the next call must NOT
    /// re-mint again. This is the documented-and-tested contract for
    /// the "validated unrecoverable corruption" path in the issue.
    #[test]
    fn pre_713_missing_root_key_migration_re_chains() {
        let dir = tempfile::tempdir().unwrap();
        let lan: IpAddr = "192.168.1.10".parse().unwrap();

        // Seed: pretend we already have a fresh chain on disk.
        let _first = load_or_generate(dir.path(), &[lan]).expect("seed");
        // Simulate pre-#713: delete only the root key.
        std::fs::remove_file(dir.path().join("ca.key.der")).unwrap();

        // Recovery: load_or_generate sees the missing file, mints a
        // fresh root, writes both ca.der and ca.key.der, and reissues
        // the leaf against the new root.
        let second = load_or_generate(dir.path(), &[lan]).expect("recovery");
        // The leaf chains to the new root (openssl verify — the same
        // shape as `leaf_cert_chains_to_root_cert`).
        let dir_pem = tempfile::tempdir().unwrap();
        std::fs::write(
            dir_pem.path().join("root.pem"),
            pem_encode("CERTIFICATE", &second.root_cert_der),
        )
        .unwrap();
        std::fs::write(
            dir_pem.path().join("leaf.pem"),
            pem_encode("CERTIFICATE", &second.leaf.cert_der),
        )
        .unwrap();
        let output = std::process::Command::new("openssl")
            .args([
                "verify",
                "-CAfile",
                dir_pem.path().join("root.pem").to_str().unwrap(),
                dir_pem.path().join("leaf.pem").to_str().unwrap(),
            ])
            .output()
            .expect("openssl must be on PATH");
        assert!(
            output.status.success(),
            "post-migration leaf MUST chain to the freshly minted root; \
             stdout: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        // A subsequent call must not re-mint (the missing-file
        // condition is gone).
        let third = load_or_generate(dir.path(), &[lan]).expect("third");
        assert_eq!(
            second.root_cert_der, third.root_cert_der,
            "post-migration load must NOT re-mint the root"
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
        // Issue #713: root key is now part of CertChain — sign the iOS
        // .mobileconfig with it. An empty key would fail the CMS sign
        // handshake with `KeyParseError`, so we pin non-empty here.
        assert!(!chain.root_key_der.is_empty(), "root key must be non-empty");
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
        // Issue #713: root key must round-trip too — it's the secret the
        // iOS `.mobileconfig` signer needs. A regression that fails to
        // persist `ca.key.der` would silently produce a fresh keypair on
        // every load, breaking the "same profile, same install" property
        // a re-scan relies on (the signed blob's signing cert would no
        // longer chain to the on-disk root).
        assert_eq!(first.root_key_der, second.root_key_der);
        assert_eq!(first.leaf.cert_der, second.leaf.cert_der);
        assert_eq!(first.leaf.key_der, second.leaf.key_der);
    }

    /// Regression pin for the pre-#713 migration: a persisted `ca.der` with
    /// no `ca.key.der` sibling must trigger a fresh `generate()`, which
    /// writes `ca.key.der` so the iOS install path can sign the next
    /// `.mobileconfig`. We accept the implicit root-rotation here — the
    /// user's installed phone root becomes stale, but the new install-QR
    /// picks up the new fingerprint (and the Re-install section of the
    /// modal already nudges them to re-install on rotation).
    #[test]
    fn load_or_generate_migrates_missing_root_key() {
        let dir = tempfile::tempdir().unwrap();
        // Seed the disk state with a fresh chain so we can then delete
        // `ca.key.der` to simulate a pre-#713 install. The returned
        // `CertChain` is unused — `load_or_generate`'s side-effect is the
        // disk write, which the subsequent `remove_file` mutates.
        let _first = load_or_generate(dir.path(), &[]).unwrap();
        // Simulate a pre-#713 install by deleting `ca.key.der` but leaving
        // `ca.der`, `cert.der`, `key.der`, `sans.txt` intact.
        std::fs::remove_file(dir.path().join("ca.key.der")).unwrap();
        let second = load_or_generate(dir.path(), &[]).unwrap();
        // The persisted `ca.der` was rejected → fresh chain → root keypair
        // rotated (different bytes from the first call). The migration
        // *intentionally* rotates the root, not the leaf — but our
        // `generate()` mints both, so the leaf is also fresh.
        assert!(
            !second.root_key_der.is_empty(),
            "missing ca.key.der migration must produce a non-empty root key"
        );
        // The new `ca.key.der` must now exist on disk for the next call.
        assert!(
            dir.path().join("ca.key.der").exists(),
            "load_or_generate must write ca.key.der after migrating"
        );
        // A subsequent call must round-trip without regenerating again.
        let third = load_or_generate(dir.path(), &[]).unwrap();
        assert_eq!(second.root_key_der, third.root_key_der);
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
        std::fs::write(
            dir.path().join("root.pem"),
            pem_encode("CERTIFICATE", &chain.root_cert_der),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("leaf.pem"),
            pem_encode("CERTIFICATE", &chain.leaf.cert_der),
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

    /// Wrap `der` in a PEM envelope (`-----BEGIN <label>-----` / base64 /
    /// `-----END <label>-----`) so openssl can read it via `-CAfile` /
    /// `-in`. We avoid pulling the `pem` crate just for test certs; the
    /// base64 alphabet is the standard `STANDARD` alphabet with `=`
    /// padding (what openssl accepts).
    fn pem_encode(label: &str, der: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(der.len() * 4 / 3 + 64);
        s.push_str("-----BEGIN ");
        s.push_str(label);
        s.push_str("-----\n");
        for chunk in der.chunks(48) {
            writeln!(s, "{}", base64_encode(chunk)).unwrap();
        }
        s.push_str("-----END ");
        s.push_str(label);
        s.push_str("-----\n");
        s
    }

    /// Minimal standard-alphabet base64 encoder (with `=` padding) for tests
    /// that need to shell out to openssl. We avoid pulling the `pem` crate
    /// just for two test certs.
    fn base64_encode(data: &[u8]) -> String {
        const ALPHA: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
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

    /// The load-bearing property for issue #1527: a SAN change (interface
    /// IP churn) reissues the **leaf** only — the root fingerprint stays
    /// stable so a phone that already trusted the root keeps working.
    /// `cert_status` exposes the new leaf fingerprint (which the QR modal
    /// shows to the user as informational), and the root fingerprint
    /// unchanged.
    #[test]
    fn cert_status_reflects_leaf_regen_without_root_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let ip_a: IpAddr = "192.168.1.5".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.9".parse().unwrap();

        let _first = load_or_generate(dir.path(), &[ip_a]).unwrap();
        let before = crate::http::tls::cert_status(dir.path()).unwrap();

        // Force a leaf renewal via a new interface IP — `persisted_covers`
        // fails because the SAN sidecar doesn't contain 10.0.0.9.
        let _second = load_or_generate(dir.path(), &[ip_b]).unwrap();
        let after = crate::http::tls::cert_status(dir.path()).unwrap();

        // Root fingerprint MUST be stable — that's the whole point of #1527.
        assert_eq!(
            before.root_fingerprint_sha256, after.root_fingerprint_sha256,
            "leaf renewal must NOT rotate the root (phone keeps trusting it)"
        );
        // Leaf fingerprint MUST change — the new SAN set requires a fresh
        // leaf, otherwise a phone connecting to 10.0.0.9 hits SAN/name
        // mismatch and the handshake fails.
        assert_ne!(
            before.leaf_fingerprint_sha256, after.leaf_fingerprint_sha256,
            "leaf renewal on interface IP change MUST produce a new leaf fingerprint"
        );
        // Generation counter MUST stay at the post-create value — a leaf
        // renewal does not bump it.
        assert_eq!(
            before.root_generation, after.root_generation,
            "leaf renewal must NOT bump root_generation (UI uses this to gate the \
             re-install banner)"
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
