//! Mesh configuration — all stored in SQLite `meshes` table.
//!
//! **There is no `mesh.toml` file.** The module name is historical; the
//! "config" lives on the `meshes` SQLite row (see `db::get_mesh_by_id`),
//! not in any file at the mesh root. The `MeshConfig` struct in
//! `models::MeshConfig` is a thin DTO over that row.
//!
//! `Worktree.baseRef` is additionally written to `.claude/settings.json`
//! at the mesh root so Claude Code can read it (see
//! [`update_worktree_base_ref`]); that mirror is an output, not a
//! source of truth — the DB column is the source.

use crate::db;
use crate::models::MeshConfig;
use std::path::PathBuf;

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
pub async fn get_mesh_properties(mesh_id: i64) -> Result<MeshConfig, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    Ok(MeshConfig::from(&mesh))
}

#[tauri::command]
pub async fn update_mesh_name(mesh_id: i64, name: String) -> Result<(), String> {
    let db = db::get().lock().unwrap();
    db.execute(
        "UPDATE meshes SET name = ?1 WHERE id = ?2",
        rusqlite::params![name, mesh_id],
    )
    .map_err(|e| format!("failed to update mesh name: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn update_mesh_field(
    mesh_id: i64,
    section: String,
    key: String,
    value: String,
) -> Result<(), String> {
    let col_name = match (section.as_str(), key.as_str()) {
        ("build", "command") => "build_command",
        ("run", "command") => "run_command",
        ("agent", "model") => "model",
        ("agent", "effort") => "effort",
        ("agent", "worktree_mode") => "worktree_mode",
        ("agent", "default_provider") => "default_provider",
        _ => return Err(format!("unknown field {}.{}", section, key)),
    };

    let db = db::get().lock().unwrap();
    db.execute(
        &format!("UPDATE meshes SET {} = ?1 WHERE id = ?2", col_name),
        rusqlite::params![value, mesh_id],
    )
    .map_err(|e| format!("failed to update mesh field: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn update_mesh_use_worktree(mesh_id: i64, use_worktree: bool) -> Result<(), String> {
    let db = db::get().lock().unwrap();
    db.execute(
        "UPDATE meshes SET use_worktree = ?1 WHERE id = ?2",
        rusqlite::params![use_worktree as i32, mesh_id],
    )
    .map_err(|e| format!("failed to update use_worktree: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn update_worktree_base_ref(mesh_id: i64, base_ref: String) -> Result<(), String> {
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
        let db = db::get().lock().unwrap();
        db.execute(
            "UPDATE meshes SET base_ref = ?1 WHERE id = ?2",
            rusqlite::params![resolved, mesh_id],
        )
        .map_err(|e| format!("failed to update base_ref in DB: {}", e))?;
    }

    write_base_ref(&mesh.path, &resolved)
}

#[tauri::command]
pub async fn remove_worktree_base_ref(mesh_id: i64) -> Result<(), String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Write default to DB and remove from settings.json
    {
        let db = db::get().lock().unwrap();
        db.execute(
            "UPDATE meshes SET base_ref = 'origin/main' WHERE id = ?1",
            rusqlite::params![mesh_id],
        )
        .map_err(|e| format!("failed to reset base_ref in DB: {}", e))?;
    }

    remove_base_ref(&mesh.path)
}