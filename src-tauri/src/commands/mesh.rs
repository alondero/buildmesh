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
/// (issue #639 gap 3, hardened by #642.1). Shared sync body used by both the
/// Tauri command below and the HTTP test server's `handle_delete_mesh` shim —
/// `delete_mesh_inner` owns the disk-drain sequencing so the two call sites
/// can't drift.
///
/// Sequence:
///   1. Snapshot the mesh's DROPPABLE pool directory paths (read-only, lock
///      released). `claimed` rows are excluded — their directories may back a
///      live agent process whose `agent_nodes` row was just deleted by step
///      2's cascade, so we have no DB way to ask "is this still a live agent?".
///      The conservative choice is to leave the dir on disk for the user to
///      clean up by hand. (`process_pending_removals` does NOT cover this
///      gap — the mesh's `agent_nodes` rows are cascade-deleted by the same
///      transaction, so no `close` event ever fires to enqueue a tombstone.)
///   2. Cascade-delete the rows via `db::delete_mesh` (which removes the
///      `meshes` row + its `agent_nodes` + its `warm_worktrees` rows).
///   3. `git worktree remove --force` each snapshot'd directory, best-effort.
///
/// The DB cascade is the source of truth for the user-visible state (a future
/// `list_meshes` call won't return the deleted mesh), so the directory teardown
/// runs AFTER it. A dir-remove failure is logged at WARN but never fails the
/// delete — the row cascade has already happened.
///
/// **Known race (accepted for #639, tracked by #642.3)**: between step 1
/// (snapshot) and step 2 (cascade-delete), a concurrent background prewarm on
/// the same mesh can `INSERT` a new `warm_worktrees` row whose path won't
/// appear in the snapshot but WILL be deleted by the cascade. The dir-remove
/// loop never sees that new path, so its directory is orphaned. The orphan
/// is recoverable by the user (manual `rm -rf`) and self-heals on a slug
/// collision: the next prewarm that lands on the same path will hit
/// `create_git_worktree`'s `if host_path.exists() { return Ok(()) }`
/// short-circuit and reuse the stale tree — which is incorrect for that
/// mesh's pool, but only until the next `git reset --hard` lands (issue
/// #613's refresh pass). Fixing this would require restructuring the
/// FILL_LOCK to block user-initiated deletes, which is out of scope.
pub fn delete_mesh_inner(mesh_id: i64) -> Result<(), String> {
    let pool_paths =
        db::list_warm_paths_for_mesh_droppable(mesh_id).map_err(|e| e.to_string())?;
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
    // `delete_mesh_inner` does blocking work — DB lock acquisition and N
    // libgit2 `git worktree remove --force` calls (each one reopens the
    // dir as a Repository and walks the admin gitdir). Calling it inline
    // on the async runtime pins a Tokio worker thread for the full
    // duration (#642.4); `spawn.rs` already wraps its libgit2 work in
    // `tokio::task::spawn_blocking` (see `spawn_blocking(move ||
    // provision_for_spawn(...))`), so the mesh-delete path should match
    // that convention.
    tokio::task::spawn_blocking(move || delete_mesh_inner(mesh_id))
        .await
        .map_err(|e| format!("delete_mesh task panicked: {}", e))?
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
///
/// Returned for the mobile QR code's *display fallback* (`RemoteAccessModal.tsx`)
/// — the actual QR URL comes from `status.exposed_interfaces` (the bind path's
/// ranked snapshot). When the bind path has already refreshed the snapshot we
/// reuse the cached `(IPs, IfaceClass)` so we don't pay for a second
/// `GetAdaptersAddresses` walk (issue #630). On cold start / no-snapshot, we
/// walk fresh via `tauri::async_runtime::spawn_blocking` — a stuck
/// `GetAdaptersAddresses` pins one blocking-pool thread instead of starving the
/// Tokio worker pool (matches the convention in `http::interface_watcher`).
#[command]
pub async fn get_local_ip() -> Result<String, String> {
    let (ips, classes) = if let Some(cached) = crate::http::local_classes_if_populated() {
        (crate::http::local_interface_ips(), cached)
    } else {
        tauri::async_runtime::spawn_blocking(crate::http::interface_rank::enumerate_with_classes)
            .await
            .map_err(|e| format!("interface enumeration task panicked: {}", e))?
    };
    crate::http::interface_rank::pick_best_lan(&ips, &classes)
        .map(|ip| ip.to_string())
        .ok_or_else(|| "no suitable LAN interface found".to_string())
}

/// Get the default provider for a mesh, applying the precedence chain:
///   1. per-mesh DB `default_provider` (set via Mesh Properties)
///   2. buildmesh-wide `preferences::default_provider` (set via Settings)
///   3. hardcoded `claude` fallback (post-#538 unified harness id)
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
