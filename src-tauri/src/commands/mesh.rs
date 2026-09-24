//! Mesh management commands

use crate::db;
use crate::models::{Mesh, PickedFolder};
use crate::services;
use tauri::command;
use tauri_plugin_dialog::DialogExt;

use crate::agent::spawn::inject_attention_hook;

/// Derive a display name (last path segment) from a picked folder, handling
/// both the native `Path` case and the URL fallback the dialog can return.
fn folder_display_name(folder_path: &tauri_plugin_dialog::FilePath, path: &str) -> String {
    if let tauri_plugin_dialog::FilePath::Path(p) = folder_path {
        std::path::Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| {
                let path_str = p.to_string_lossy();
                let sep = if path_str.contains('\\') { '\\' } else { '/' };
                #[allow(clippy::manual_pattern_char_comparison)]
                path_str
                    .rsplit(|c| c == sep)
                    .next()
                    .unwrap_or(&p.to_string_lossy())
                    .to_string()
            })
    } else {
        // Url case — rsplit on '/' to get last path segment
        services::mesh::name_from_path(path)
    }
}

/// Shared tail for every mesh-creation entrypoint (`add_mesh`, `create_mesh`,
/// `clone_mesh_repo`): apply the optional accent colour, inject the attention
/// hook, and re-read the row so the returned `Mesh` carries the colour we just
/// wrote. One owner so the three callers can't drift.
fn finalize_new_mesh(
    mesh: Mesh,
    color: Option<&str>,
    path: &std::path::Path,
) -> Result<Mesh, String> {
    if let Some(color) = color.filter(|c| !c.is_empty()) {
        db::set_mesh_color(mesh.id, Some(color)).map_err(|e| e.to_string())?;
    }
    if let Err(e) = inject_attention_hook(path) {
        tracing::warn!(
            "attention hook injection failed for {}: {e}",
            path.display()
        );
    }
    db::get_mesh_by_id(mesh.id).map_err(|e| e.to_string())
}

/// Open the native folder picker and return the chosen path + derived name,
/// WITHOUT creating a mesh. The "New mesh" modal (location + colour) calls
/// this so it can show the selection and let the user pick a colour before
/// committing via `create_mesh`. Returns `None` when the user cancels.
#[command]
pub async fn pick_mesh_folder(app: tauri::AppHandle) -> Result<Option<PickedFolder>, String> {
    crate::commands::run_blocking("pick_mesh_folder", move || {
        tracing::debug!("pick_mesh_folder called");
        let folder_path = app.dialog().file().blocking_pick_folder();
        tracing::debug!("folder picker returned: {:?}", folder_path);
        let Some(folder_path) = folder_path else {
            return Ok(None);
        };
        let path = folder_path.to_string();
        let name = folder_display_name(&folder_path, &path);
        tracing::debug!("picked folder: {} ({})", path, name);
        Ok(Some(PickedFolder { path, name }))
    })
    .await
}

/// Add a mesh by opening a folder picker dialog. Retained for callers that
/// want the one-shot "pick + create" behaviour; the desktop "New mesh" flow
/// now splits this into `pick_mesh_folder` + `create_mesh` so a colour can be
/// chosen in between.
#[command]
pub async fn add_mesh(app: tauri::AppHandle) -> Result<Mesh, String> {
    crate::commands::run_blocking("add_mesh", move || {
        tracing::debug!("add_mesh called");
        let folder_path = app.dialog()
            .file()
            .blocking_pick_folder();
        tracing::debug!("folder picker returned: {:?}", folder_path);
        let folder_path = folder_path.ok_or("No folder selected")?;

        let path = folder_path.to_string();
        tracing::debug!("selected path: {}", path);
        let name = folder_display_name(&folder_path, &path);
        tracing::debug!("mesh name: {}", name);

        let mesh = db::create_mesh(&name, &path).map_err(|e| {
            tracing::error!("create_mesh failed: {}", e);
            e.to_string()
        })?;
        finalize_new_mesh(mesh, None, std::path::Path::new(&path))
    })
    .await
}

/// Create a new mesh, optionally with a user-picked accent `color` (a
/// `#rrggbb` hex string). `None` leaves the colour unset so the frontend
/// falls back to the deterministic palette.
#[command]
pub async fn create_mesh(
    name: String,
    path: String,
    color: Option<String>,
) -> Result<Mesh, String> {
    crate::commands::run_blocking("create_mesh", move || {
        let mesh = db::create_mesh(&name, &path).map_err(|e| e.to_string())?;
        finalize_new_mesh(mesh, color.as_deref(), std::path::Path::new(&path))
    })
    .await
}

/// Wall-clock budget for the clone shell-out. Generous: a large repo over a
/// slow link can legitimately take minutes while the user watches an
/// indeterminate "Cloning…" state.
const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Resolve and validate the clone destination `<parent_dir>/<repo>`.
///
/// Returns the destination plus whether it already existed — the caller needs
/// that to decide what to clean up after a failure. Rejects a missing parent
/// and an already-occupied destination so `git clone` is never pointed at a
/// non-empty tree; a pre-existing *empty* directory is allowed, because git
/// clones into one happily.
pub(crate) fn resolve_clone_destination(
    parent_dir: &str,
    repo: &str,
) -> Result<(std::path::PathBuf, bool), String> {
    let parent = std::path::Path::new(parent_dir);
    if !parent.is_dir() {
        return Err(format!("Destination folder does not exist: {parent_dir}"));
    }
    let dest = parent.join(repo);
    let existed = dest.exists();
    let occupied = dest
        .read_dir()
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if occupied {
        return Err(format!(
            "A folder already exists at {} — choose a different parent folder.",
            dest.display()
        ));
    }
    Ok((dest, existed))
}

/// Drop the partial tree a failed or timed-out clone may have left, so the
/// obvious retry (same parent, same repo) isn't blocked by the collision guard.
/// Only a directory we created is removed — a destination the user already had
/// is left as they had it.
fn remove_partial_clone(dest: &std::path::Path, dest_existed: bool) {
    if !dest_existed {
        let _ = std::fs::remove_dir_all(dest);
    }
}

/// The stderr line worth showing the user.
///
/// `git clone` writes progress chatter (`Cloning into 'x'…`) to stderr *before*
/// it fails, so the first line is the least informative one. Prefer git's own
/// `fatal:` / `error:` line, then fall back to the last non-empty line; `None`
/// when git said nothing at all.
pub(crate) fn first_error_line(stderr: &str) -> Option<&str> {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines
        .iter()
        .find(|line| line.starts_with("fatal:") || line.starts_with("error:"))
        .or_else(|| lines.last())
        .copied()
}

/// Clone a GitHub repository into `<parent_dir>/<repo>` and register the result
/// as a Mesh, in one step.
///
/// The clone runs with the machine's own git auth (SSH agent, credential
/// manager, or `gh auth setup-git`) through
/// [`crate::process_util::git_command`] — the URL is used as given and no token
/// is injected, so nothing secret is written into the new repo's `.git/config`.
/// `GIT_TERMINAL_PROMPT=0` / `GCM_INTERACTIVE=never` make a credentials-needing
/// clone fail fast instead of hanging on a prompt no GUI can answer.
pub(crate) fn clone_mesh_repo_blocking(
    url: &str,
    parent_dir: &str,
    color: Option<String>,
) -> Result<Mesh, String> {
    let target = services::github::parse_clone_input(url).ok_or_else(|| {
        "Enter a GitHub repository as `owner/repo` or a full github.com URL.".to_string()
    })?;

    // Clone into a new `<parent>/<repo>` subfolder so we never clone into a
    // directory the user didn't intend to become the repo root.
    let (dest, dest_existed) = resolve_clone_destination(parent_dir, &target.repo)?;

    let mut cmd = crate::process_util::git_command();
    cmd.arg("clone").arg(&target.url).arg(&dest);
    let output = match crate::process_util::run_command_with_timeout(cmd, "git clone", CLONE_TIMEOUT)
    {
        Ok(output) => output,
        Err(e) => {
            // A timed-out clone is killed and reaped by the runner, but may
            // have left a partial tree.
            remove_partial_clone(&dest, dest_existed);
            return Err(e);
        }
    };

    if !output.status.success() {
        remove_partial_clone(&dest, dest_existed);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(match first_error_line(&stderr) {
            Some(line) => format!("git clone failed: {line}"),
            None => "git clone failed (git produced no diagnostic output)".to_string(),
        });
    }

    let dest_str = dest.to_string_lossy().to_string();

    // Cut worktrees from the cloned repo's real default branch rather than the
    // `origin/main` literal `create_mesh` writes — otherwise a repo whose
    // default is `master` lands as a **drifted root**.
    let base_ref = crate::git::primitives::open_from_host_path(&dest_str)
        .map(|repo| {
            format!(
                "origin/{}",
                crate::commands::git::default_branch_from_repo(&repo)
            )
        })
        .unwrap_or_else(|_| "origin/main".to_string());

    let mesh = db::create_mesh_with_base_ref(&target.repo, &dest_str, &base_ref)
        .map_err(|e| e.to_string())?;
    finalize_new_mesh(mesh, color.as_deref(), dest.as_path())
}

/// Clone a GitHub repo into `<parent_dir>/<repo>` and create a mesh from it.
/// See [`clone_mesh_repo_blocking`] for the clone/auth rules.
#[command]
pub async fn clone_mesh_repo(
    url: String,
    parent_dir: String,
    color: Option<String>,
) -> Result<Mesh, String> {
    crate::commands::run_blocking("clone_mesh_repo", move || {
        clone_mesh_repo_blocking(&url, &parent_dir, color)
    })
    .await
}

/// Set (or clear) a mesh's accent colour — used by the sidebar swatch's
/// "recolour" flow. Passing `None`/empty clears it back to the palette
/// fallback. Errors if the mesh no longer exists (zero rows updated).
#[command]
pub async fn update_mesh_color(mesh_id: i64, color: Option<String>) -> Result<(), String> {
    crate::commands::run_blocking("update_mesh_color", move || {
        let normalized = color.as_deref().filter(|c| !c.is_empty());
        let rows = db::set_mesh_color(mesh_id, normalized).map_err(|e| e.to_string())?;
        if rows == 0 {
            return Err(format!("mesh {} not found (no rows updated)", mesh_id));
        }
        Ok(())
    })
    .await
}

/// Create a mesh for testing without dialog (uses temp directory)
#[command]
pub async fn create_test_mesh(name: String) -> Result<Mesh, String> {
    crate::commands::run_blocking("create_test_mesh", move || {
        services::mesh::create_test(&name).map_err(|e| e.to_string())
    })
    .await
}

/// List all meshes
#[command]
pub async fn list_meshes() -> Result<Vec<Mesh>, String> {
    crate::commands::run_blocking("list_meshes", || {
        db::list_meshes().map_err(|e| e.to_string())
    })
    .await
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
/// `create_git_worktree`'s `is_valid_linked_worktree` check (issue #1230),
/// see that the orphan is not a linked worktree of this mesh, refuse to
/// silently reuse it, and either rename aside an empty husk or refuse the
/// non-empty one with a descriptive error. Fixing this would require
/// restructuring the FILL_LOCK to block user-initiated deletes, which is
/// out of scope.
pub fn delete_mesh_inner(mesh_id: i64) -> Result<(), String> {
    let pool_paths =
        db::list_warm_paths_for_mesh_droppable(mesh_id).map_err(|e| e.to_string())?;
    // Mesh deletion cascades its agent rows directly in SQLite, so the
    // per-node service delete hook is not called. Snapshot the ids before the
    // cascade and release their process-lifetime attention state after the
    // database commit; otherwise a deleted mesh leaves one HookState entry per
    // node in the global map forever.
    let node_ids = db::list_agent_nodes_by_mesh(mesh_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|node| node.id)
        .collect::<Vec<_>>();
    db::delete_mesh(mesh_id).map_err(|e| e.to_string())?;
    for node_id in node_ids {
        crate::agent::node_teardown::release(node_id);
    }
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
    // duration (#642.4 / #1380).
    crate::commands::run_blocking("delete_mesh", move || delete_mesh_inner(mesh_id)).await
}

/// Update a mesh's layout preference
#[command]
pub async fn update_mesh_layout(mesh_id: i64, layout: String) -> Result<(), String> {
    crate::commands::run_blocking("update_mesh_layout", move || {
        services::mesh::update_layout(mesh_id, &layout).map_err(|e| e.to_string())
    })
    .await
}

/// Update multiple meshes' sort positions in the sidebar
#[command]
pub async fn update_mesh_positions(updates: Vec<(i64, i64)>) -> Result<(), String> {
    crate::commands::run_blocking("update_mesh_positions", move || {
        db::update_mesh_positions_batch(&updates).map_err(|e| e.to_string())
    })
    .await
}

/// Get or create the root remote access token for the whole buildmesh instance
#[command]
pub async fn get_root_token() -> Result<String, String> {
    crate::commands::run_blocking("get_root_token", || {
        db::get_or_create_root_token().map_err(|e| e.to_string())
    })
    .await
}

/// Get the local machine's LAN IP address.
///
/// Returned for the mobile QR code's *display fallback* (`RemoteAccessModal.tsx`)
/// — the actual QR URL comes from `status.exposed_interfaces` (the bind path's
/// ranked snapshot). When the bind path has already refreshed the snapshot we
/// reuse the cached `(IPs, IfaceClass)` so we don't pay for a second
/// `GetAdaptersAddresses` walk (issue #630). On cold start / no-snapshot, we
/// walk fresh via `tauri::async_runtime::spawn_blocking` — a stuck
/// `GetAdaptersAddresses` pins one blocking-pool thread (cheap, large pool)
/// instead of starving the Tokio worker pool (matches the convention in
/// `http::interface_watcher`). The `tokio::time::timeout` is reintroduced as a
/// load-bearing safety net (#630 review): without it, a stuck adapter driver
/// can keep `spawn_blocking` alive indefinitely, the QR modal's `Promise.all`
/// never resolves, and the user sees an indefinite blank modal rather than the
/// graceful `.catch(() => '192.168.1.x')` fallback. 5 s matches the historical
/// band-aid (PR #104 / commit 410cee8) — long enough for a real adapter walk,
/// short enough that a stuck filter driver produces a usable UX degradation.
#[command]
pub async fn get_local_ip() -> Result<String, String> {
    let (ips, classes) = if let Some(cached) = crate::http::local_classes_if_populated() {
        (crate::http::local_interface_ips(), cached)
    } else {
        // `tokio::time::timeout` + `spawn_blocking` together: the blocking
        // pool's stuck-thread cost is bounded by 5 s, after which the future
        // returns `Err` and the wrapping `.catch(() => '192.168.1.x')` in
        // RemoteAccessModal renders a placeholder rather than hanging. The
        // blocking thread itself may stay stuck until its driver recovers
        // (out of our hands) but the user's Tauri promise resolves.
        let walk = tauri::async_runtime::spawn_blocking(
            crate::http::interface_rank::enumerate_with_classes,
        );
        match tokio::time::timeout(std::time::Duration::from_secs(5), walk).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                return Err(format!("interface enumeration task panicked: {}", e));
            }
            Err(_elapsed) => {
                return Err(
                    "timeout enumerating interfaces (5s exceeded); \
                     a stuck adapter driver may be blocking GetAdaptersAddresses"
                        .to_string(),
                );
            }
        }
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
    crate::commands::run_blocking("get_default_provider", move || {
        let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| format!("{}", e))?;
        Ok(crate::preferences::resolve_default_provider(
            None,
            mesh.default_provider,
            crate::preferences::default_provider(),
        ))
    })
    .await
}
