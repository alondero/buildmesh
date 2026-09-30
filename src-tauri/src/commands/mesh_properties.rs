//! Mesh property commands — read/write the user-tunable columns on the
//! `meshes` SQLite row.
//!
//! **There is no `mesh.toml` file.** The "properties" / "config" lives on the
//! `meshes` SQLite row (see `db::get_mesh_by_id`), not in any file at the mesh
//! root. The `MeshRow` struct in `models::MeshRow` is a thin DTO over that row.
//!
//! `Worktree.baseRef` is additionally written to `.claude/settings.json`
//! at the mesh root so Claude Code can read it (see
//! [`update_worktree_base_ref`]); that mirror is an output, not a source of
//! truth — the DB column is the source.
//!
//! Pure sync — each command is a single SQLite read/write (with optional
//! `std::fs::*` for the settings.json mirror on `base_ref` updates). Runs on
//! Tauri's IPC worker, NOT the bounded tokio pool. Issue #1380 review point 4.

use crate::db;
use crate::models::MeshRow;
use crate::services::warm_pool;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

/// Tauri event emitted after any worktree-directory setting changes
/// (issue #1519) — both [`update_mesh_worktree_directory`] (payload
/// `Some(mesh_id)`) and the application-default command in
/// `commands::preferences` (payload `None`, every inheriting mesh is
/// affected). Frontend `useWorktreeEffectiveDir` re-resolves on it so
/// mesh-level `GIT_CHANGED` subscriptions never go stale until restart.
/// Symmetric constant in `src/lib/events.ts` as
/// `WORKTREE_DIR_CHANGED_EVENT` — drift is caught by the
/// `worktree_dir_changed_event_name_matches_frontend_constant` test here
/// and the literal test in `tests/unit/use-worktree-effective-dir.test.ts`.
pub const WORKTREE_DIR_CHANGED_EVENT: &str = "worktree-directory-changed";

// ---------------------------------------------------------------------------
// Settings.json helpers (for base_ref only)
// ---------------------------------------------------------------------------

fn write_base_ref(mesh_path: &str, base_ref: &str) -> Result<(), String> {
    let settings_path = PathBuf::from(mesh_path).join(".claude/settings.json");

    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)
            .map_err(|e| format!("failed to read settings.json: {}", e))?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    settings["worktree"]["baseRef"] = serde_json::json!(base_ref);

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create .claude directory: {}", e))?;
    }

    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("failed to serialize settings.json: {}", e))?;
    std::fs::write(&settings_path, content)
        .map_err(|e| format!("failed to write settings.json: {}", e))?;
    Ok(())
}

fn remove_base_ref(mesh_path: &str) -> Result<(), String> {
    let settings_path = PathBuf::from(mesh_path).join(".claude/settings.json");

    let content = std::fs::read_to_string(&settings_path)
        .map_err(|e| format!("failed to read settings.json: {}", e))?;
    let mut settings: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse settings.json: {}", e))?;

    if let Some(obj) = settings.get_mut("worktree") {
        if let Some(obj) = obj.as_object_mut() {
            obj.remove("baseRef");
        }
    }

    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("failed to serialize settings.json: {}", e))?;
    std::fs::write(&settings_path, content)
        .map_err(|e| format!("failed to write settings.json: {}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_mesh_properties(mesh_id: i64) -> Result<MeshRow, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    Ok(MeshRow::from(&mesh))
}

#[tauri::command]
pub fn update_mesh_name(mesh_id: i64, name: String) -> Result<(), String> {
    let db = db::write_conn();
    db.execute(
        "UPDATE meshes SET name = ?1 WHERE id = ?2",
        rusqlite::params![name, mesh_id],
    )
    .map_err(|e| format!("failed to update mesh name: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn update_mesh_column(
    mesh_id: i64,
    column: String,
    value: String,
) -> Result<(), String> {
    // Allowlist of user-tunable `meshes` columns this command can write. The
    // `name` column is a dedicated command (`update_mesh_name`), and
    // `base_ref` / `use_worktree` have settings.json or structural side-effects
    // and route through their own commands. A direct column name is the
    // honest wire shape now that the data lives on the `meshes` row (issue
    // #474); the SQL below still validates against the allowlist rather than
    // interpolating an untrusted column name from the wire.
    const ALLOWED_COLUMNS: &[&str] = &[
        "build_command",
        "run_command",
        // Per-context build/run commands (issue #802). Nullable siblings of
        // build_command/run_command; a Root Node prefers these and falls back
        // to the plain columns when they're unset.
        "root_build_command",
        "root_run_command",
        "model",
        "effort",
        "worktree_mode",
        "default_provider",
    ];
    if !ALLOWED_COLUMNS.contains(&column.as_str()) {
        return Err(format!("unknown mesh column: {}", column));
    }

    let db = db::write_conn();
    db.execute(
        &format!("UPDATE meshes SET {} = ?1 WHERE id = ?2", column),
        rusqlite::params![value, mesh_id],
    )
    .map_err(|e| format!("failed to update mesh column: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn update_mesh_use_worktree(mesh_id: i64, use_worktree: bool) -> Result<(), String> {
    let rows = {
        let db = db::write_conn();
        db.execute(
            "UPDATE meshes SET use_worktree = ?1 WHERE id = ?2",
            rusqlite::params![use_worktree as i32, mesh_id],
        )
        .map_err(|e| format!("failed to update use_worktree: {}", e))?
    };
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    Ok(())
}

/// Set the per-mesh target for the pre-spawn Worktree Pool
/// (`services::warm_pool`, issue #611). `0` disables the pool for the
/// mesh; `1..=5` is the target the worker fills to on startup + after
/// each claim. Clamped at the IPC boundary so a misbehaving frontend
/// (or a future bulk-import path) can't write a garbage value to the
/// DB column and break the worker's count/target comparisons.
///
/// On a successful save, schedules a background drain-and-fill via the
/// same `std::thread::spawn` pattern as `spawn::post_spawn_maintenance`.
/// The drain runs off the IPC thread because `git worktree remove` is a
/// blocking syscall (1-3s per worktree on Windows with Defender) — a
/// 5→1 shrink would otherwise freeze the UI for 4-12 seconds. The
/// inner `drain_excess_warm_entries` / `prewarm_one` emit
/// `pool-count-changed` for every actual state change, so the badge
/// settles as rows drop / fill without any explicit end-of-pass emit
/// here (the previous unconditional settle emit was the source of the
/// double-emit when `drain_excess_warm_entries` already fired).
///
/// Dedicated command (not the generic `update_mesh_column` allowlist)
/// so the typed integer + the `0..=5` invariant are enforced here —
/// the catch-all is intentionally unvalidated.
#[tauri::command]
pub fn update_mesh_pool_size(
    app: AppHandle,
    mesh_id: i64,
    pool_size: i32,
) -> Result<(), String> {
    if !(0..=5).contains(&pool_size) {
        return Err(format!(
            "invalid pool size {}: must be 0 (off) or 1..=5",
            pool_size
        ));
    }
    {
        let db = db::write_conn();
        let rows = db
            .execute(
                "UPDATE meshes SET pre_spawn_pool_size = ?1 WHERE id = ?2",
                rusqlite::params![pool_size, mesh_id],
            )
            .map_err(|e| format!("failed to update pre_spawn_pool_size: {}", e))?;
        // An UPDATE that matches no rows silently succeeds otherwise —
        // returning `Ok(())` would let the frontend believe the save
        // succeeded when the mesh was deleted (or never existed) between
        // the load and the save. Surfaces the same contract as
        // `set_mesh_sandbox_inner`'s zero-rows guard.
        if rows == 0 {
            return Err(format!(
                "mesh {} not found (no rows updated)",
                mesh_id
            ));
        }
    }

    // Drain-then-fill runs on a dedicated OS thread so the IPC handler
    // returns immediately. Inner `drain_excess_warm_entries` /
    // `prewarm_one` emit `pool-count-changed` for each state change
    // they make, so the badge settles naturally as rows drop / fill —
    // no explicit settle emit needed (would double-fire when drain
    // already emitted).
    std::thread::spawn(move || {
        warm_pool::drain_and_fill_for_mesh(&app, mesh_id);
    });

    Ok(())
}

/// Return the number of `available` warm pool entries for `mesh_id`.
/// Powers Project Settings' worktree-strategy pool badge
/// (`usePoolChanged` listener + `WorktreeStrategySection` UI). Thin wrapper
/// over `db::count_available_warm_for_mesh` — the DB layer is the
/// single source of truth for pool state, so the IPC command is just
/// the typed edge.
#[tauri::command]
pub fn get_mesh_pool_count(mesh_id: i64) -> Result<i64, String> {
    db::count_available_warm_for_mesh(mesh_id)
        .map_err(|e| format!("pool count for mesh {} failed: {}", mesh_id, e))
}

/// Toggle whether this mesh's agent nodes run inside an OS process sandbox
/// (Windows AppContainer #498 / macOS Seatbelt #497). Dedicated command (not
/// the generic `update_mesh_column` allowlist) so it takes a typed `bool` and
/// the zero-rows-is-an-error contract is enforced in `db::set_mesh_sandbox`.
#[tauri::command]
pub fn update_mesh_sandbox(mesh_id: i64, sandbox: bool) -> Result<(), String> {
    db::set_mesh_sandbox(mesh_id, sandbox)
        .map_err(|e| format!("failed to update sandbox: {}", e))
}

/// Set the per-Mesh Worktree Node directory override (issue #1519).
/// `None` (or blank, which collapses to `None`) clears the override so the
/// Mesh inherits the application default (or `.claude/worktrees` when that
/// too is unset). Relative values resolve from the Mesh root; absolute
/// values must be in the same host environment (native/Windows versus WSL)
/// as the Mesh — a mismatch is rejected with an actionable message.
/// No shell/`~` expansion. Changing it affects future nodes and warm-pool
/// entries only — live Agent Nodes keep their persisted `worktree_path`
/// and are never moved. On success, schedules a background pool rebuild
/// for this Mesh so idle inventory converges on the new location.
#[tauri::command]
pub fn update_mesh_worktree_directory(
    app: AppHandle,
    mesh_id: i64,
    directory: Option<String>,
) -> Result<(), String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let cleaned = crate::env::validate_worktree_directory(&mesh.path, directory.as_deref())?;
    let rows = db::set_mesh_worktree_directory(mesh_id, cleaned.as_deref())
        .map_err(|e| format!("failed to update worktree_directory: {}", e))?;
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    // Notify first so listeners re-resolve against the committed row, then
    // enqueue the pool rebuild (which reads the same committed row).
    let _ = app.emit(WORKTREE_DIR_CHANGED_EVENT, Some(mesh_id));
    // Enqueues a debounced, fill-lock-serialized rebuild and returns
    // immediately (the function owns its background thread — callers must
    // NOT wrap it in their own `spawn`, which would defeat the
    // single-runner collapsing).
    crate::services::warm_pool::rebuild_pools_for_worktree_dir_change(&app, Some(mesh_id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::WORKTREE_DIR_CHANGED_EVENT;

    #[test]
    fn worktree_dir_changed_event_name_matches_frontend_constant() {
        // Symmetric to the `WORKTREE_DIR_CHANGED_EVENT` export in
        // `src/lib/events.ts` — keeps the two halves of the IPC contract
        // from drifting (same pattern as warm_pool's pool-count-changed pin).
        assert_eq!(WORKTREE_DIR_CHANGED_EVENT, "worktree-directory-changed");
    }
}

/// Set the mesh's admitted Circuit Run limit (1..8), then wake pending admission.
#[tauri::command]
pub fn update_mesh_circuit_run_capacity(mesh_id: i64, capacity: i32) -> Result<(), String> {
    if !(1..=8).contains(&capacity) {
        return Err(format!(
            "invalid circuit run capacity {}: must be 1..=8",
            capacity
        ));
    }
    let rows = db::set_mesh_circuit_run_capacity(mesh_id, capacity)
        .map_err(|e| format!("failed to update circuit_run_capacity: {}", e))?;
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    crate::services::circuit_worker::wake_circuit_worker();
    Ok(())
}



#[tauri::command]
pub fn update_worktree_base_ref(mesh_id: i64, base_ref: String) -> Result<(), String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Map 'fresh' → origin/main and 'head' → HEAD
    let resolved = match base_ref.as_str() {
        "fresh" => "origin/main".to_string(),
        "head" => "HEAD".to_string(),
        other => other.to_string(),
    };

    // Write to both DB and settings.json
    {
        let db = db::write_conn();
        db.execute(
            "UPDATE meshes SET base_ref = ?1 WHERE id = ?2",
            rusqlite::params![resolved, mesh_id],
        )
        .map_err(|e| format!("failed to update base_ref in DB: {}", e))?;
    }

    write_base_ref(&mesh.path, &resolved)
}

#[tauri::command]
pub fn remove_worktree_base_ref(mesh_id: i64) -> Result<(), String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Write default to DB and remove from settings.json
    {
        let db = db::write_conn();
        db.execute(
            "UPDATE meshes SET base_ref = 'origin/main' WHERE id = ?1",
            rusqlite::params![mesh_id],
        )
        .map_err(|e| format!("failed to reset base_ref in DB: {}", e))?;
    }

    remove_base_ref(&mesh.path)
}

