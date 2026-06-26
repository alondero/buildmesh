//! Mesh management commands

use crate::db;
use crate::models::Mesh;
use crate::services;
use tauri::command;
use tauri_plugin_dialog::DialogExt;

use crate::agent::spawn::inject_attention_hook;

/// Add a mesh by opening a folder picker dialog
#[command]
pub async fn add_mesh(app: tauri::AppHandle) -> Result<Mesh, String> {
    tracing::debug!("add_mesh called");
    let folder_path = app.dialog()
        .file()
        .blocking_pick_folder();
    tracing::debug!("folder picker returned: {:?}", folder_path);
    let folder_path = folder_path.ok_or("No folder selected")?;

    let path = folder_path.to_string();
    tracing::debug!("selected path: {}", path);
    let name = if let tauri_plugin_dialog::FilePath::Path(p) = folder_path {
        std::path::Path::new(&p)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| {
                let path_str = p.to_string_lossy();
                let sep = if path_str.contains('\\') { '\\' } else { '/' };
                #[allow(clippy::manual_pattern_char_comparison)]
                let result = path_str
                    .rsplit(|c| c == sep)
                    .next()
                    .unwrap_or(&p.to_string_lossy())
                    .to_string();
                result
            })
    } else {
        // Url case — rsplit on '/' to get last path segment
        services::mesh::name_from_path(&path)
    };
    tracing::debug!("mesh name: {}", name);

    let mesh = db::create_mesh(&name, &path).map_err(|e| {
        tracing::error!("create_mesh failed: {}", e);
        e.to_string()
    })?;
    inject_attention_hook(std::path::Path::new(&path));
    Ok(mesh)
}

/// Create a new mesh
#[command]
pub async fn create_mesh(name: String, path: String) -> Result<Mesh, String> {
    let mesh = db::create_mesh(&name, &path).map_err(|e| e.to_string())?;
    inject_attention_hook(std::path::Path::new(&path));
    Ok(mesh)
}

/// Create a mesh for testing without dialog (uses temp directory)
#[command]
pub async fn create_test_mesh(name: String) -> Result<Mesh, String> {
    services::mesh::create_test(&name).map_err(|e| e.to_string())
}

/// List all meshes
#[command]
pub async fn list_meshes() -> Result<Vec<Mesh>, String> {
    db::list_meshes().map_err(|e| e.to_string())
}

/// Delete a mesh and its nodes, including the on-disk pool directories
/// (issue #639 gap 3). Shared sync body used by both the Tauri command below
/// and the HTTP test server's `handle_delete_mesh` shim — `delete_mesh_inner`
/// owns the disk-drain sequencing so the two call sites can't drift.
///
/// Sequence:
///   1. Snapshot the mesh's pool directory paths (read-only, lock released).
///   2. Cascade-delete the rows via `db::delete_mesh` (which removes the
///      `meshes` row + its `agent_nodes` + its `warm_worktrees` rows).
///   3. `git worktree remove --force` each snapshot'd directory, best-effort.
///
/// The DB cascade is the source of truth for the user-visible state (a future
/// `list_meshes` call won't return the deleted mesh), so the directory teardown
/// runs AFTER it. A dir-remove failure is logged at WARN but never fails the
/// delete — the row cascade has already happened.
///
/// **Known race (accepted for #639)**: between step 1 (snapshot) and step 2
/// (cascade-delete), a concurrent background prewarm on the same mesh can
/// `INSERT` a new `warm_worktrees` row whose path won't appear in the snapshot
/// but WILL be deleted by the cascade. The dir-remove loop never sees that
/// new path, so its directory is orphaned. The orphan is recoverable by the
/// user (manual `rm -rf`) and self-heals on a slug collision: the next
/// prewarm that lands on the same path will hit `create_git_worktree`'s
/// `if host_path.exists() { return Ok(()) }` short-circuit and reuse the
/// stale tree — which is incorrect for that mesh's pool, but only until the
/// next `git reset --hard` lands (issue #613's refresh pass). Fixing this
/// would require restructuring the FILL_LOCK to block user-initiated deletes,
/// which is out of scope for the gap-3 hygiene fix.
pub fn delete_mesh_inner(mesh_id: i64) -> Result<(), String> {
    let pool_paths = db::list_warm_paths_for_mesh(mesh_id).map_err(|e| e.to_string())?;
    db::delete_mesh(mesh_id).map_err(|e| e.to_string())?;
    for path in pool_paths {
        if let Err(e) = crate::git::worktree::remove_one_worktree(&path) {
            tracing::warn!(
                "delete_mesh: removed DB rows but failed to remove pool dir {}: {}",
                path,
                e
            );
        }
    }
    Ok(())
}

#[command]
pub async fn delete_mesh(mesh_id: i64) -> Result<(), String> {
    delete_mesh_inner(mesh_id)
}

/// Update a mesh's layout preference
#[command]
pub async fn update_mesh_layout(mesh_id: i64, layout: String) -> Result<(), String> {
    services::mesh::update_layout(mesh_id, &layout).map_err(|e| e.to_string())
}

/// Update multiple meshes' sort positions in the sidebar
#[command]
pub async fn update_mesh_positions(updates: Vec<(i64, i64)>) -> Result<(), String> {
    db::update_mesh_positions_batch(&updates).map_err(|e| e.to_string())
}

/// Get or create the root remote access token for the whole buildmesh instance

/// Get or create the root remote access token for the whole buildmesh instance
#[command]
pub async fn get_root_token() -> Result<String, String> {
    db::get_or_create_root_token().map_err(|e| e.to_string())
}

/// Get the local machine's LAN IP address.
#[command]
pub async fn get_local_ip() -> Result<String, String> {
    // Use a timeout because network interface enumeration can hang for 10+ seconds
    // on Windows machines with VPNs, Docker, Hyper-V, or corporate networking software.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            let interfaces = local_ip_address::list_afinet_netifas()
                .map_err(|e| format!("failed to list interfaces: {}", e))?;

            pick_best_lan_ip(&interfaces).ok_or_else(|| "no suitable LAN interface found".to_string())
        }
    )
    .await
    .map_err(|_| "timeout detecting network interfaces (5s exceeded)".to_string())?
}

/// Pick the best LAN IP for the mobile QR code. Prefers 192.168.x.x (common
/// home/office LAN) over 10.x.x.x (often VPN/corporate), because the QR is
/// scanned by a phone on the same Wi-Fi as the hub — a VPN address is useless.
///
/// Two-pass scan: any 192.168.x.x match wins outright; only fall through to
/// 10.x.x.x if no 192.168.x.x interface is present. See commit 66ed767 for
/// the original priority fix and 410cee8 for the regression that collapsed it.
fn pick_best_lan_ip(interfaces: &[(String, std::net::IpAddr)]) -> Option<String> {
    if let Some(ip) = find_first_lan_ip(interfaces, &[0xC0, 0xA8]) {
        return Some(ip);
    }
    find_first_lan_ip(interfaces, &[0x0A])
}

/// Get the default provider for a mesh, applying the precedence chain:
///   1. per-mesh DB `default_provider` (set via Mesh Properties)
///   2. buildmesh-wide `preferences::default_provider` (set via Settings)
///   3. hardcoded `anthropic` fallback
#[command]
pub async fn get_default_provider(mesh_id: i64) -> Result<String, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("{}", e))?;
    Ok(crate::preferences::resolve_default_provider(
        None,
        mesh.default_provider,
        crate::preferences::default_provider(),
    ))
}

/// Find the first IP matching one of the given /8 prefixes (big-endian octets).
fn find_first_lan_ip(
    interfaces: &[(String, std::net::IpAddr)],
    prefixes: &[u8],
) -> Option<String> {
    for (name, ip) in interfaces {
        if let Some(ip_str) = _iface_addr_in_lan_range(name, ip, prefixes) {
            return Some(ip_str);
        }
    }
    None
}

/// Returns the IP address if this interface is a typical LAN address (not Docker/tunnel/VirtualBox).
/// Only considers addresses matching one of the given /8 prefixes.
fn _iface_addr_in_lan_range(_name: &str, ip: &std::net::IpAddr, prefixes: &[u8]) -> Option<String> {
    let ip_str = ip.to_string();

    // Skip Docker bridge (172.16-31.x), VirtualBox Host-Only (192.168.56.x),
    // tunnel addresses (10.0.0.x), Loopback, and wildcard (0.0.0.0)
    if ip.is_loopback() {
        return None;
    }
    if let std::net::IpAddr::V4(ipv4) = ip {
        let octets = ipv4.octets();
        // Docker bridge: 172.16.0.0/12 (172.16–172.31)
        if octets[0] == 172 && (16..=31).contains(&octets[1]) {
            return None;
        }
        if octets[0] == 192 && octets[1] == 168 && octets[2] == 56 {
            return None; // VirtualBox Host-Only
        }
        if octets[0] == 10 && octets[1] == 0 && octets[2] == 0 {
            return None; // Tunnel
        }
        if octets[0] == 0 && octets[1] == 0 && octets[2] == 0 && octets[3] == 0 {
            return None; // Wildcard
        }
    }

    // Only accept addresses whose /8 prefix is in our allow-list
    if let std::net::IpAddr::V4(ipv4) = ip {
        let first_octet = ipv4.octets()[0];
        if prefixes.contains(&first_octet) {
            return Some(ip_str);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    //! Tests for the LAN IP picker used by the mobile QR code.
    //!
    //! Regression context: when the OS returns a 10.x interface (VPN, corporate
    //! client, Docker host network) BEFORE any 192.168.x interface, the QR code
    //! must still encode the 192.168 LAN address — otherwise the phone can't
    //! reach the hub. Originally fixed in #56 (commit 66ed767), regressed in
    //! #104 (commit 410cee8) when the two-pass scan was collapsed into one.
    use super::*;
    use std::net::IpAddr;
    use std::str::FromStr;

    fn iface(name: &str, ip: &str) -> (String, IpAddr) {
        (name.to_string(), IpAddr::from_str(ip).unwrap())
    }

    /// The headline regression test: 10.x appearing first in OS order must NOT
    /// beat a later 192.168.x entry. With the bug, this returns "10.20.30.40".
    /// Uses 10.20.x (not 10.0.0.x) to avoid the separate tunnel-exclusion rule.
    #[test]
    fn pick_best_prefers_192_168_when_10_appears_first() {
        let interfaces = vec![
            iface("vpn0", "10.20.30.40"),
            iface("docker0", "172.17.0.1"),
            iface("eth0", "192.168.1.100"),
        ];
        assert_eq!(
            pick_best_lan_ip(&interfaces).as_deref(),
            Some("192.168.1.100")
        );
    }

    /// No 192.168 available → must fall back to 10.x.
    #[test]
    fn pick_best_falls_back_to_10_when_no_192_168() {
        let interfaces = vec![
            iface("vpn0", "10.20.30.40"),
            iface("docker0", "172.17.0.1"),
        ];
        assert_eq!(pick_best_lan_ip(&interfaces).as_deref(), Some("10.20.30.40"));
    }

    #[test]
    fn pick_best_returns_none_when_nothing_matches() {
        let interfaces = vec![
            iface("docker0", "172.17.0.1"),
            iface("vboxnet0", "192.168.56.1"), // VirtualBox Host-Only — excluded
            iface("tun0", "10.0.0.1"),         // Tunnel — excluded
        ];
        assert_eq!(pick_best_lan_ip(&interfaces), None);
    }

    #[test]
    fn find_first_lan_ip_skips_loopback_and_wildcard() {
        let interfaces = vec![
            iface("lo", "127.0.0.1"),
            iface("eth0", "0.0.0.0"),
            iface("eth1", "192.168.1.50"),
        ];
        assert_eq!(
            find_first_lan_ip(&interfaces, &[0xC0, 0xA8]).as_deref(),
            Some("192.168.1.50")
        );
    }

    #[test]
    fn find_first_lan_ip_skips_docker_bridge_in_10_pass() {
        // 172.16-31 is Docker's default bridge range — must never be returned.
        let interfaces = vec![iface("docker0", "172.17.0.1")];
        assert_eq!(find_first_lan_ip(&interfaces, &[0x0A]), None);
    }

    #[test]
    fn find_first_lan_ip_skips_virtualbox_host_only() {
        // 192.168.56.x is VirtualBox Host-Only — not a real LAN.
        let interfaces = vec![iface("vboxnet0", "192.168.56.1")];
        assert_eq!(find_first_lan_ip(&interfaces, &[0xC0, 0xA8]), None);
    }

    #[test]
    fn find_first_lan_ip_skips_tunnel_range() {
        // 10.0.0.x is sometimes a tunnel pseudo-interface — must be excluded.
        let interfaces = vec![iface("tun0", "10.0.0.1")];
        assert_eq!(find_first_lan_ip(&interfaces, &[0x0A]), None);
    }
}
