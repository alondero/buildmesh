//! Complete immutable TLS generations, published by a single atomic pointer.
use super::{CertChain, SelfSignedCert};
use std::{
    fs, io,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
};

static STORE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
const FILES: [&str; 6] = [
    "ca.der",
    "ca.key.der",
    "cert.der",
    "key.der",
    "sans.txt",
    "root_gen",
];
const RETAINED_GENERATIONS: usize = 3;

pub(super) fn current_dir(dir: &Path) -> io::Result<PathBuf> {
    match fs::read_to_string(dir.join("current")) {
        Ok(name)
            if name.starts_with("generation-")
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
        {
            Ok(dir.join(name))
        }
        Ok(_) => Err(io::Error::other("Invalid TLS generation pointer")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(dir.to_path_buf()),
        Err(e) => Err(e),
    }
}

fn read_chain(dir: &Path) -> io::Result<CertChain> {
    let chain = CertChain {
        root_cert_der: fs::read(dir.join("ca.der"))?,
        root_key_der: fs::read(dir.join("ca.key.der"))?,
        leaf: SelfSignedCert {
            cert_der: fs::read(dir.join("cert.der"))?,
            key_der: fs::read(dir.join("key.der"))?,
        },
    };
    validate(&chain)?;
    Ok(chain)
}

fn validate(chain: &CertChain) -> io::Result<()> {
    use rustls::{
        client::danger::ServerCertVerifier,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    };
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    rustls::sign::CertifiedKey::from_der(
        vec![CertificateDer::from(chain.root_cert_der.clone())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(chain.root_key_der.clone())),
        &provider,
    )
    .map_err(io::Error::other)?;
    super::acceptor_from(&chain.leaf).map_err(io::Error::other)?;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(chain.root_cert_der.clone()))
        .map_err(io::Error::other)?;
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        std::sync::Arc::new(roots),
        provider,
    )
    .build()
    .map_err(io::Error::other)?;
    verifier
        .verify_server_cert(
            &CertificateDer::from(chain.leaf.cert_der.clone()),
            &[],
            &ServerName::try_from("localhost").unwrap(),
            &[],
            UnixTime::now(),
        )
        .map_err(io::Error::other)?;
    Ok(())
}

pub(super) fn load(dir: &Path, ips: &[IpAddr], reset: bool) -> io::Result<CertChain> {
    load_with_checkpoint(dir, ips, reset, |_| Ok(()))
}

fn load_with_checkpoint(
    dir: &Path,
    ips: &[IpAddr],
    reset: bool,
    mut checkpoint: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<CertChain> {
    let _guard = STORE.lock();
    fs::create_dir_all(dir)?;
    protect(dir, true)?;
    let previous = current_dir(dir)?;
    for name in ["ca.key.der", "key.der", "ca.key.der.new", "key.der.new"] {
        let path = previous.join(name);
        if path.exists() {
            protect(&path, false)?;
        }
    }
    let wanted = super::interface_san_key(ips);
    if !reset && previous != dir && super::persisted_covers(&previous.join("sans.txt"), &wanted) {
        if let Ok(chain) = read_chain(&previous) {
            return Ok(chain);
        }
    }

    let staging = tempfile::Builder::new()
        .prefix("staging-")
        .tempdir_in(dir)?;
    protect(staging.path(), true)?;
    for name in FILES {
        match fs::read(previous.join(name)) {
            Ok(bytes) => fs::write(staging.path().join(name), bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    if !staging.path().join("root_gen").exists() {
        fs::write(staging.path().join("root_gen"), b"0")?;
    }
    // A mismatched legacy root must fail closed rather than silently replacing
    // the user's trust anchor. Missing roots retain the documented migration.
    if !reset
        && staging.path().join("ca.der").exists()
        && staging.path().join("ca.key.der").exists()
    {
        let root = super::RootKeyPair::load(staging.path())?;
        rustls::sign::CertifiedKey::from_der(
            vec![rustls::pki_types::CertificateDer::from(root.cert_der)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(root.key_der.into()),
            &rustls::crypto::ring::default_provider(),
        )
        .map_err(io::Error::other)?;
    }
    // A legacy partial leaf can be renewed with its valid, unchanged root.
    if read_chain(staging.path()).is_err() {
        let leaf = staging.path().join("cert.der");
        if leaf.exists() {
            fs::remove_file(leaf)?;
        }
    }
    let chain = if reset {
        super::reset_staged(staging.path(), ips)?;
        read_chain(staging.path())?
    } else {
        super::load_staged(staging.path(), ips)?
    };
    validate(&chain)?;
    for name in FILES {
        let path = staging.path().join(name);
        protect(&path, false)?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?
            .sync_all()?;
        checkpoint(name)?;
    }
    sync_dir(staging.path())?;
    let name = format!("generation-{}", uuid::Uuid::new_v4());
    // Transfer ownership before renaming. A TempDir destructor must never be
    // left holding a path that has been moved into the live store.
    let staging_path = staging.keep();
    if let Err(error) = fs::rename(&staging_path, dir.join(&name)) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(error);
    }
    sync_dir(dir)?;
    checkpoint("generation")?;
    let mut pointer = tempfile::NamedTempFile::new_in(dir)?;
    pointer.write_all(name.as_bytes())?;
    pointer.as_file().sync_all()?;
    checkpoint("pointer")?;
    pointer.persist(dir.join("current")).map_err(|e| e.error)?;
    // Publication already committed: returning an error here would skip the
    // caller's cache invalidation/rebind and leave it serving the old leaf.
    if let Err(error) = checkpoint("published").and_then(|_| sync_dir(dir)) {
        tracing::warn!("TLS generation published but directory flush failed: {error}");
    }
    prune_generations(dir, &name);
    // Keep prior complete generations for recovery. Public install consumers
    // resolve `current`; they never mix independently read legacy files.
    Ok(chain)
}

/// Keep a small rollback window while preventing every interface change from
/// accumulating another complete copy of the CA and leaf private keys.
fn prune_generations(dir: &Path, current: &str) {
    let mut generations: Vec<(PathBuf, std::time::SystemTime)> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_str()?;
                if !name.starts_with("generation-") || !path.is_dir() {
                    return None;
                }
                let modified = entry.metadata().and_then(|meta| meta.modified()).ok()?;
                Some((path, modified))
            })
            .collect(),
        Err(error) => {
            tracing::warn!("Unable to enumerate old TLS generations: {error}");
            return;
        }
    };
    generations.sort_by_key(|(_, modified)| *modified);
    let mut keep = std::collections::HashSet::new();
    keep.insert(current.to_string());
    for (path, _) in generations.iter().rev() {
        if keep.len() >= RETAINED_GENERATIONS {
            break;
        }
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            keep.insert(name.to_string());
        }
    }
    for (path, _) in generations {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| keep.contains(name))
        {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(&path) {
            tracing::debug!(
                "Unable to prune old TLS generation {}: {error}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_callers_publish_one_identity() {
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = dir.path().to_path_buf();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    load(&path, &[], false).unwrap()
                })
            })
            .collect();
        let chains: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        for chain in &chains {
            assert_eq!(chain.root_cert_der, chains[0].root_cert_der);
            assert_eq!(chain.leaf.cert_der, chains[0].leaf.cert_der);
            validate(chain).unwrap();
        }
        assert_eq!(
            read_chain(&current_dir(dir.path()).unwrap())
                .unwrap()
                .leaf
                .cert_der,
            chains[0].leaf.cert_der
        );
    }

    #[test]
    fn failure_at_each_publish_step_preserves_the_previous_complete_identity() {
        let dir = tempfile::tempdir().unwrap();
        let first = load(dir.path(), &[], false).unwrap();
        let before = current_dir(dir.path()).unwrap();
        for stop in FILES.into_iter().chain(["generation", "pointer"]) {
            let result = load_with_checkpoint(dir.path(), &[], true, |step| {
                if step == stop {
                    Err(io::Error::other("injected crash"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err(), "checkpoint {stop} must execute");
            assert_eq!(current_dir(dir.path()).unwrap(), before);
            let restarted = load(dir.path(), &[], false).unwrap();
            assert_eq!(restarted.root_cert_der, first.root_cert_der);
            assert_eq!(restarted.leaf.cert_der, first.leaf.cert_der);
            validate(&restarted).unwrap();
        }
    }

    #[test]
    fn mismatched_legacy_root_is_rejected_without_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let first = super::super::load_staged(dir.path(), &[]).unwrap();
        let other = super::super::generate(&[]).unwrap();
        fs::write(dir.path().join("ca.key.der"), other.root_key_der).unwrap();
        assert!(load(dir.path(), &[], false).is_err());
        assert!(!dir.path().join("current").exists());
        assert_eq!(
            fs::read(dir.path().join("ca.der")).unwrap(),
            first.root_cert_der
        );
    }

    #[test]
    fn post_publication_flush_failure_still_returns_the_published_identity() {
        let dir = tempfile::tempdir().unwrap();
        let old = load(dir.path(), &[], false).unwrap();
        let old_dir = current_dir(dir.path()).unwrap();
        let published = load_with_checkpoint(dir.path(), &[], true, |step| {
            if step == "published" {
                Err(io::Error::other("injected final flush failure"))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_ne!(published.root_cert_der, old.root_cert_der);
        assert_eq!(
            read_chain(&current_dir(dir.path()).unwrap())
                .unwrap()
                .root_cert_der,
            published.root_cert_der
        );
        assert_eq!(
            read_chain(&old_dir).unwrap().root_cert_der,
            old.root_cert_der
        );
    }

    #[test]
    fn old_generations_are_pruned_to_a_bounded_rollback_window() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..(RETAINED_GENERATIONS + 4) {
            load(&dir.path(), &[], true).unwrap();
        }
        let count = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("generation-")
            })
            .count();
        assert!(count <= RETAINED_GENERATIONS);
    }

    #[test]
    fn valid_legacy_identity_migrates_without_changing_trust() {
        let dir = tempfile::tempdir().unwrap();
        let old = super::super::load_staged(dir.path(), &[]).unwrap();
        fs::remove_file(dir.path().join("root_gen")).unwrap();
        let migrated = load(dir.path(), &[], false).unwrap();
        assert_eq!(old.root_cert_der, migrated.root_cert_der);
        assert_eq!(old.leaf.cert_der, migrated.leaf.cert_der);
        assert_ne!(current_dir(dir.path()).unwrap(), dir.path());
    }

    #[cfg(windows)]
    #[test]
    fn private_keys_have_a_protected_owner_only_dacl() {
        let dir = tempfile::tempdir().unwrap();
        load(dir.path(), &[], false).unwrap();
        for name in ["ca.key.der", "key.der"] {
            let path = current_dir(dir.path()).unwrap().join(name);
            let script = format!(
                "(Get-Acl -LiteralPath '{}').Sddl",
                path.display().to_string().replace('\'', "''")
            );
            let output = crate::process_util::command_no_window("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .output()
                .unwrap();
            assert!(output.status.success(), "ACL inspection failed");
            let descriptor = String::from_utf8_lossy(&output.stdout);
            assert!(
                descriptor.trim_end().ends_with("D:P(A;;FA;;;OW)"),
                "unexpected ACL: {descriptor}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_keys_and_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load(dir.path(), &[], false).unwrap();
        let current = current_dir(dir.path()).unwrap();
        assert_eq!(
            fs::metadata(&current).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["ca.key.der", "key.der"] {
            assert_eq!(
                fs::metadata(current.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}
#[cfg(not(unix))]
fn sync_dir(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(super) fn protect(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(windows)]
pub(super) fn protect(path: &Path, directory: bool) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
        fn SetFileSecurityW(path: *const u16, information: u32, descriptor: *const c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    // Protected DACL containing only OWNER RIGHTS; children inherit it.
    let sddl: Vec<u16> = if directory {
        "D:P(A;OICI;FA;;;OW)"
    } else {
        "D:P(A;;FA;;;OW)"
    }
    .encode_utf16()
    .chain(Some(0))
    .collect();
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let result = SetFileSecurityW(path.as_ptr(), 0x80000004, descriptor);
        let error = io::Error::last_os_error();
        LocalFree(descriptor);
        if result == 0 {
            return Err(error);
        }
    }
    Ok(())
}
