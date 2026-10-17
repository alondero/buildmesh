//! Machine-global registry of the HTTP ports Buildmesh instances have bound
//! on this machine.
//!
//! Most harnesses expand `$BUILDMESH_PORT` when their hook runs, so a callback
//! always reaches the instance that spawned the node. MiniMax Code cannot:
//! it clears `BUILDMESH_*` before invoking a handler and keeps exactly one
//! user-global plugin manifest, so the URL is baked into a file every
//! instance shares. Baking the *spawning* instance's port made the most
//! recent MiniMax spawn steal the other instances' callbacks — those nodes
//! then stayed `Running` forever because the callback landed on an instance
//! that does not own the node (issue #2343).
//!
//! So a baked handler names every live instance: each instance publishes
//! its bound port as `<profile-parent>/buildmesh-instances/<port>` on bind and
//! removes it on a clean exit, and the handler addresses all of them. The
//! attention route is node-independent — it resolves the node from the
//! payload's session id and cwd — so the instance that owns the node accepts
//! the callback and the others discard it as unknown. That is safe in either
//! direction, which is why the port list can be baked at provision time.
//!
//! The registry is a *hint*, never a source of truth: a crashed instance
//! leaves its file behind, so [`live_ports`] probes each entry with a TCP
//! connect and drops the ones that no longer answer. `own` is always kept
//! without probing, because it is the one address that cannot be wrong.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

/// Directory that holds one file per running instance, named after its port.
///
/// The parent of the profile directory (`%APPDATA%`,
/// `~/Library/Application Support`, `$XDG_DATA_HOME`) is the one location
/// every profile of this app on this machine already shares — the hub and a
/// dev profile differ only by the leaf directory name. Putting the registry
/// there is what makes a second instance discoverable at all.
const REGISTRY_DIR: &str = "buildmesh-instances";

/// Bound on one liveness probe. A closed loopback port is refused in well
/// under a millisecond; the timeout only covers a filtered port, so a stale
/// entry cannot stall a spawn.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Where the registry lives, or `None` before the profile is resolved (a
/// headless tool, a test that never called `preferences::init`).
fn registry_dir() -> Option<PathBuf> {
    let profile = crate::preferences::app_data_dir()?;
    Some(profile.parent()?.join(REGISTRY_DIR))
}

/// Publish `port` so sibling instances can address this one.
///
/// Best-effort: remote access works without the registry, so a failure to
/// write must never stop the server from starting. It only degrades to the
/// old last-writer-wins behaviour for a *sibling* instance's MiniMax nodes.
pub(crate) fn publish(port: u16) {
    let Some(dir) = registry_dir() else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::warn!("http::instances: cannot create {}: {error}", dir.display());
        return;
    }
    // The file name carries the port and is all that is ever read back; the
    // body only helps a human map a port back to the instance holding it.
    let body = serde_json::json!({
        "port": port,
        "pid": std::process::id(),
        "profile": profile_label(),
    })
    .to_string();
    if let Err(error) = std::fs::write(dir.join(port.to_string()), body) {
        tracing::warn!("http::instances: cannot publish port {port}: {error}");
    }
}

/// Drop this instance's entry after a clean exit. A crash leaves the file
/// behind for [`live_ports`]' probe to prune.
pub(crate) fn unpublish(port: u16) {
    let Some(dir) = registry_dir() else {
        return;
    };
    let path = dir.join(port.to_string());
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        // Another instance may have replaced a stale file of the same port
        // with its own; either way there is nothing to clean up.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!("http::instances: cannot retire port {port}: {error}");
        }
    }
}

/// Every HTTP port a Buildmesh instance is currently listening on, sorted and
/// deduplicated, `own` included unconditionally.
///
/// This is what a harness without `$BUILDMESH_PORT` bakes into its handler:
/// one callback, delivered to each live instance, accepted by the one that
/// owns the node.
pub fn live_ports(own: u16) -> Vec<u16> {
    let entries = registry_dir()
        .map(|dir| published_names(&dir))
        .unwrap_or_default();
    ports_from(&entries, accepts_connection, own)
}

fn published_names(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        // A directory or an unreadable entry cannot name a port.
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// The probe is injected so the decision table is testable without binding
/// ports: `entries` are the published file names, `own` is always kept.
fn ports_from(entries: &[String], probe: impl Fn(u16) -> bool, own: u16) -> Vec<u16> {
    let mut ports: Vec<u16> = entries
        .iter()
        .filter_map(|name| name.parse::<u16>().ok())
        // `own` is re-added below without a probe, so probing our own listener
        // (the common case: we published it at startup) can only cost a
        // loopback connect per spawn and never change the answer.
        .filter(|port| *port != own && probe(*port))
        .collect();
    // Never gated behind the probe: this instance's own listener is the one
    // entry that is certain, and dropping it would silence the callbacks that
    // only it can answer.
    ports.push(own);
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn accepts_connection(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        PROBE_TIMEOUT,
    )
    .is_ok()
}

fn profile_label() -> String {
    super::state::app_identifier()
        .map(|identifier| super::state::port_profile_label(&identifier))
        .unwrap_or("custom")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn names(ports: &[u16]) -> Vec<String> {
        ports.iter().map(u16::to_string).collect()
    }

    #[test]
    fn keeps_every_live_entry_plus_own_sorted() {
        let ports = ports_from(&names(&[4992, 1992]), |_| true, 2992);
        assert_eq!(ports, vec![1992, 2992, 4992]);
    }

    /// A crashed instance leaves its file behind (there is no clean exit to
    /// retire it from), so a port that no longer answers must be dropped rather
    /// than accumulate in every hook the machine writes.
    #[test]
    fn drops_entries_that_no_longer_answer() {
        let ports = ports_from(&names(&[1992, 4992]), |port| port == 1992, 1992);
        assert_eq!(ports, vec![1992]);
    }

    /// A refused probe on the *own* port (a saturated accept backlog, a
    /// filtered hop) must not silence this instance's own callbacks.
    #[test]
    fn own_port_survives_a_failed_probe() {
        let ports = ports_from(&[], |_| false, 2992);
        assert_eq!(ports, vec![2992]);
    }

    #[test]
    fn own_port_is_not_duplicated_by_its_own_entry() {
        let ports = ports_from(&names(&[1992]), |_| true, 1992);
        assert_eq!(ports, vec![1992]);
    }

    /// Anything the registry directory holds that is not a port (a lock file,
    /// an editor backup, a half-written temp file) is ignored rather than
    /// turned into a connect attempt.
    #[test]
    fn ignores_names_that_are_not_ports() {
        let entries = vec![
            "1992".to_string(),
            "README.txt".to_string(),
            "2992.tmp".to_string(),
            String::new(),
            "-1".to_string(),
            "70000".to_string(),
        ];
        let ports = ports_from(&entries, |_| true, 1992);
        assert_eq!(ports, vec![1992]);
    }

    /// `own` is `u16`, so a value above 65535 cannot reach the probe through
    /// a file name; assert the parse is the only filter that matters.
    #[test]
    fn probe_never_sees_an_out_of_range_port() {
        let probed = std::cell::RefCell::new(BTreeSet::new());
        let ports = ports_from(
            &["99999".to_string(), "4992".to_string()],
            |port| {
                probed.borrow_mut().insert(port);
                true
            },
            1992,
        );
        assert_eq!(*probed.borrow(), BTreeSet::from([4992]));
        assert_eq!(ports, vec![1992, 4992]);
    }

    /// The caller's own port is re-added without probing, so a published entry
    /// for it costs no loopback connect on every spawn and cannot change the
    /// answer either way.
    #[test]
    fn own_port_is_not_probed() {
        let probed = std::cell::RefCell::new(Vec::new());
        let ports = ports_from(
            &names(&[1992, 4992]),
            |port| {
                probed.borrow_mut().push(port);
                true
            },
            1992,
        );
        assert_eq!(*probed.borrow(), vec![4992]);
        assert_eq!(ports, vec![1992, 4992]);
    }

    /// End to end through the filesystem, with the real probe: publish from
    /// two "instances" sharing a profile parent, confirm both are listed, then
    /// retire one and confirm the closed port is pruned.
    #[test]
    fn publish_then_retire_is_visible_to_a_sibling_instance() {
        let root = tempfile::tempdir().expect("tempdir");
        let hub = root.path().join("com.alond.buildmesh");
        let dev = root.path().join("com.alond.buildmesh.dev");
        std::fs::create_dir_all(&hub).expect("hub profile dir");
        std::fs::create_dir_all(&dev).expect("dev profile dir");

        // A real listener standing in for the sibling instance.
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let live_port = listener.local_addr().expect("addr").port();
        let dead_port = {
            let taken = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
            taken.local_addr().expect("addr").port()
            // `taken` closes here, so nothing answers on `dead_port`.
        };

        // The dev instance publishes itself into the shared parent.
        crate::preferences::init_for_tests(dev.clone());
        publish(dead_port);

        // The hub instance reads the same shared directory.
        crate::preferences::init_for_tests(hub);
        let published: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                registry_dir()
                    .expect("registry dir")
                    .join(dead_port.to_string()),
            )
            .expect("published file"),
        )
        .expect("published file is JSON");
        assert_eq!(published["port"], dead_port, "the file name and body agree");
        assert_eq!(
            published["pid"],
            std::process::id() as u64,
            "the body identifies the instance that published it"
        );
        assert_eq!(
            registry_dir(),
            Some(root.path().to_path_buf().join(REGISTRY_DIR)),
            "both profiles must resolve the one shared registry directory"
        );

        // A crashed instance's entry is dropped by the probe; a live sibling
        // is kept alongside our own port.
        let mut entries = published_names(&registry_dir().expect("registry dir"));
        assert_eq!(entries.len(), 1, "the dev instance published one entry");
        entries.push(live_port.to_string());
        let ports = ports_from(&entries, accepts_connection, 2992);
        assert_eq!(
            ports.len(),
            2,
            "the closed port must not reach the handler: {ports:?}"
        );
        assert!(ports.contains(&live_port), "a live sibling must: {ports:?}");
        assert!(ports.contains(&2992), "our own port must: {ports:?}");

        // A clean exit retires the entry instead of leaving it for the probe.
        crate::preferences::init_for_tests(dev.clone());
        unpublish(dead_port);
        assert_eq!(
            published_names(&registry_dir().expect("registry dir")),
            Vec::<String>::new(),
            "unpublish must remove exactly this instance's entry"
        );
        // Retiring an entry that is already gone is a no-op, not a warning:
        // a crashed instance's file outlives it, and a later instance can bind
        // the same port and replace it.
        unpublish(dead_port);

        crate::preferences::reset_for_tests();
    }

    /// Without a resolved profile there is no registry to write, and no
    /// registry to read — the caller still gets its own port back.
    #[test]
    fn without_a_profile_the_registry_is_inert() {
        crate::preferences::reset_for_tests();
        assert_eq!(registry_dir(), None);
        publish(1992);
        unpublish(1992);
        assert_eq!(live_ports(1992), vec![1992]);
    }
}
