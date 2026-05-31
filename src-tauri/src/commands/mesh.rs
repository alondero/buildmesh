//! Mesh management commands

use crate::db;
use crate::models::Mesh;
use crate::services;
use tauri::command;
use tauri_plugin_dialog::DialogExt;

use crate::agent::spawn::inject_attention_hook;

/// Add a mesh by opening a folder picker dialog
#[command]
pub async fn add_project(app: tauri::AppHandle) -> Result<Mesh, String> {
    tracing::debug!("add_project called");
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
pub async fn create_project(name: String, path: String) -> Result<Mesh, String> {
    let mesh = db::create_mesh(&name, &path).map_err(|e| e.to_string())?;
    inject_attention_hook(std::path::Path::new(&path));
    Ok(mesh)
}

/// Create a mesh for testing without dialog (uses temp directory)
#[command]
pub async fn create_test_project(name: String) -> Result<Mesh, String> {
    services::mesh::create_test(&name).map_err(|e| e.to_string())
}

/// List all meshes
#[command]
pub async fn list_projects() -> Result<Vec<Mesh>, String> {
    db::list_meshes().map_err(|e| e.to_string())
}

/// Delete a mesh and its nodes
#[command]
pub async fn delete_project(project_id: i64) -> Result<(), String> {
    db::delete_mesh(project_id).map_err(|e| e.to_string())
}

/// Update a mesh's layout preference
#[command]
pub async fn update_project_layout(project_id: i64, layout: String) -> Result<(), String> {
    services::mesh::update_layout(project_id, &layout).map_err(|e| e.to_string())
}

/// Update multiple meshes' sort positions in the sidebar
#[command]
pub async fn update_project_positions(updates: Vec<(i64, i64)>) -> Result<(), String> {
    db::update_mesh_positions_batch(&updates).map_err(|e| e.to_string())
}

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

            if let Some(ip) = find_first_lan_ip(&interfaces, &[0xC0, 0xA8, 0x0A]) {
                return Ok(ip);
            }

            Err("no suitable LAN interface found".to_string())
        }
    )
    .await
    .map_err(|_| "timeout detecting network interfaces (5s exceeded)".to_string())?
}

/// Get the default provider for a mesh, applying the precedence chain:
///   1. per-mesh DB `default_provider` (set via mesh.toml or Mesh Properties)
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
