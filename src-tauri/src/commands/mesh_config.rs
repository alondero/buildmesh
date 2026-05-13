//! Mesh configuration — reads/writes mesh.toml and worktree settings.json
//!
//! Uses build_run::extract_toml_value for TOML parsing (shared implementation).

use crate::commands::build_run::extract_toml_value;
use crate::commands::build_run::MESH_CONFIG_FILENAME;
use crate::db;
use std::path::PathBuf;

const MESH_CONFIG_EMPTY_TEMPLATE: &str = r"# Buildmesh configuration
[build]
[run]
[agent]
";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct MeshConfig {
    pub name: Option<String>,
    pub build_command: Option<String>,
    pub run_command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub base_ref: Option<String>,
    pub in_place: bool,
}

// ---------------------------------------------------------------------------
// mesh.toml helpers
// ---------------------------------------------------------------------------

/// Read all fields from mesh.toml
fn parse_mesh_toml(mesh_path: &PathBuf) -> Result<MeshConfig, String> {
    let config_path = mesh_path.join(MESH_CONFIG_FILENAME);
    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("mesh.toml not found at {:?}: {}", config_path, e))?;

    Ok(MeshConfig {
        name: None, // name lives in DB, not mesh.toml
        build_command: extract_toml_value(&content, "build", "command"),
        run_command: extract_toml_value(&content, "run", "command"),
        model: extract_toml_value(&content, "agent", "model"),
        effort: extract_toml_value(&content, "agent", "effort"),
        base_ref: None, // baseRef lives in settings.json, not mesh.toml
        in_place: extract_toml_value(&content, "agent", "in_place")
            .map(|v| v == "true")
            .unwrap_or(false),
    })
}

/// Read worktree.baseRef from {mesh_path}/.claude/settings.json
fn read_base_ref(mesh_path: &str) -> Option<String> {
    let settings_path = PathBuf::from(mesh_path).join(".claude/settings.json");
    let content = std::fs::read_to_string(&settings_path).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&content).ok()?;
    settings
        .get("worktree")?
        .get("baseRef")?
        .as_str()
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// mesh.toml write helpers
// ---------------------------------------------------------------------------

/// Update a single key inside an existing [section] in mesh.toml.
/// If the key exists, replaces it. If the section exists but key doesn't, appends it.
/// Creates the file with default sections (and the section if needed) if it doesn't exist yet.
fn write_mesh_toml_field(
    mesh_path: &PathBuf,
    section: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let config_path = mesh_path.join(MESH_CONFIG_FILENAME);
    let content = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|_| MESH_CONFIG_EMPTY_TEMPLATE.to_string());

    let section_pattern = format!("[{}]", section);
    let new_line = format!("{} = \"{}\"", key, value);

    if let Some(section_start) = content.find(&section_pattern) {
        // Section exists — update/add key within it
        let section_end = content[section_start + section_pattern.len()..]
            .find('[')
            .map(|i| section_start + section_pattern.len() + i)
            .unwrap_or(content.len());

        let before = &content[..section_start];
        let section_content = &content[section_start..section_end];
        let after = &content[section_end..];

        let key_pattern = format!("{} = ", key);

        let updated_section = if let Some(key_start) = section_content.find(&key_pattern) {
            let key_line_end = section_content[key_start..]
                .find('\n')
                .map(|e| key_start + e)
                .unwrap_or(section_content.len());
            let before_key = &section_content[..key_start];
            let after_key = &section_content[key_line_end..];
            format!("{}{}{}", before_key, new_line, after_key)
        } else {
            let insert_pos = section_content.trim_end().trim_end_matches('\n').len();
            let mut s = section_content[..insert_pos].to_string();
            s.push_str(&format!("\n{} = \"{}\"", key, value));
            if insert_pos < section_content.len() {
                s.push_str(&section_content[insert_pos..]);
            }
            s
        };

        let updated = format!("{}{}{}", before, updated_section, after);
        std::fs::write(&config_path, updated)
            .map_err(|e| format!("failed to write mesh.toml: {}", e))?;
    } else {
        // Section doesn't exist — append it with the key
        let updated = format!("{}\n[{}]\n{}\n", content.trim_end(), section, new_line);
        std::fs::write(&config_path, updated)
            .map_err(|e| format!("failed to write mesh.toml: {}", e))?;
    }

    Ok(())
}

/// Write worktree.baseRef to {mesh_path}/.claude/settings.json
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

/// Remove worktree.baseRef key from {mesh_path}/.claude/settings.json
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
    let mesh =
        db::get_mesh_by_id(mesh_id).map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let path = PathBuf::from(&mesh.path);

    let mut config = match parse_mesh_toml(&path) {
        Ok(c) => c,
        Err(_) => MeshConfig {
            name: None,
            build_command: None,
            run_command: None,
            model: None,
            effort: None,
            base_ref: None,
            in_place: false,
        },
    };
    config.base_ref = read_base_ref(&mesh.path);
    config.name = if mesh.name.is_empty() {
        None
    } else {
        Some(mesh.name)
    };

    Ok(config)
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
    let mesh =
        db::get_mesh_by_id(mesh_id).map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let path = PathBuf::from(&mesh.path);
    write_mesh_toml_field(&path, &section, &key, &value)?;
    Ok(())
}

#[tauri::command]
pub async fn update_worktree_base_ref(mesh_id: i64, base_ref: String) -> Result<(), String> {
    let mesh =
        db::get_mesh_by_id(mesh_id).map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Map 'fresh' → origin/<default> and 'head' → HEAD
    let resolved = match base_ref.as_str() {
        "fresh" => "origin/main".to_string(),
        "head" => "HEAD".to_string(),
        other => other.to_string(),
    };

    write_base_ref(&mesh.path, &resolved)
}

#[tauri::command]
pub async fn remove_worktree_base_ref(mesh_id: i64) -> Result<(), String> {
    let mesh =
        db::get_mesh_by_id(mesh_id).map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    remove_base_ref(&mesh.path)
}

/// Write in_place setting to mesh.toml [agent] section.
/// Unlike write_mesh_toml_field, this writes bare booleans (true/false) not quoted strings.
#[tauri::command]
pub async fn update_mesh_in_place(mesh_id: i64, in_place: bool) -> Result<(), String> {
    let mesh =
        db::get_mesh_by_id(mesh_id).map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let config_path = PathBuf::from(&mesh.path).join(MESH_CONFIG_FILENAME);
    let content =
        std::fs::read_to_string(&config_path).map_err(|e| format!("mesh.toml not found: {}", e))?;

    let bool_str = if in_place { "true" } else { "false" };
    let key = "in_place";

    let section_pattern = "[agent]";
    let section_start = content
        .find(section_pattern)
        .ok_or_else(|| "[agent] section not found in mesh.toml".to_string())?;

    let section_end = content[section_start + section_pattern.len()..]
        .find('[')
        .map(|i| section_start + section_pattern.len() + i)
        .unwrap_or(content.len());

    let before = &content[..section_start];
    let section_content = &content[section_start..section_end];
    let after = &content[section_end..];

    // For booleans, write without quotes: in_place = true
    let key_pattern = format!("{} = ", key);
    let new_line = format!("{} = {}", key, bool_str);

    let updated_section = if let Some(key_start) = section_content.find(&key_pattern) {
        // Key exists - check if it has quotes and remove them, or replace value
        let key_line_end = section_content[key_start..]
            .find('\n')
            .map(|e| key_start + e)
            .unwrap_or(section_content.len());
        let before_key = &section_content[..key_start];
        let after_key = &section_content[key_line_end..];
        format!("{}{}{}", before_key, new_line, after_key)
    } else {
        // Key doesn't exist - insert it
        let insert_pos = section_content.trim_end().trim_end_matches('\n').len();
        let mut s = section_content[..insert_pos].to_string();
        s.push_str(&format!("\n{} = {}", key, bool_str));
        if insert_pos < section_content.len() {
            s.push_str(&section_content[insert_pos..]);
        }
        s
    };

    let updated = format!("{}{}{}", before, updated_section, after);
    std::fs::write(&config_path, updated)
        .map_err(|e| format!("failed to write mesh.toml: {}", e))?;
    Ok(())
}
