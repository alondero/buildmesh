//! Per-node project overrides selected through OPENCODE_CONFIG. V1 never reads
//! these V2-only keys, and concurrent nodes cannot overwrite each other's route.
use crate::agent::managed_config;
use std::path::Path;

const PLUGIN: &str = include_str!("../opencode2_attention_plugin.ts");
const PACKAGE: &str = "../../buildmesh-attention2";
pub(super) const BACKGROUND_CONFIG: &str = r#"{"permissions":[{"action":"*","resource":"*","effect":"deny"},{"action":"external_directory","resource":"*","effect":"deny"}],"policies":[{"action":"tool.use","resource":"*","effect":"deny"}]}"#;
pub(super) fn config_path(directory: &Path, node_id: i64) -> std::path::PathBuf {
    directory
        .join(".opencode/buildmesh2")
        .join(node_id.to_string())
        .join("opencode.json")
}

pub(super) fn provision(
    directory: &Path,
    node_id: i64,
    identity: Option<(&str, u16)>,
) -> Result<(), String> {
    let plugin_path = directory.join(".opencode/buildmesh-attention2/index.ts");
    let path = config_path(directory, node_id);
    std::fs::create_dir_all(path.parent().ok_or("OpenCode 2 config has no parent")?)
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(
        plugin_path
            .parent()
            .ok_or("OpenCode 2 plugin has no parent")?,
    )
    .map_err(|e| e.to_string())?;
    let mut doc = managed_config::read_document(&path).map_err(|e| e.to_string())?;
    let before = doc.root.clone();
    let root = managed_config::root_object(&mut doc, "OpenCode 2 config")?;
    let plugins = root
        .entry("plugins")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or("OpenCode 2 plugins must be an array")?;
    let index = plugins
        .iter()
        .position(|p| p["package"] == PACKAGE || p.as_str() == Some(PACKAGE));
    let index = index.unwrap_or_else(|| {
        plugins.push(serde_json::json!({"package":PACKAGE,"options":{"sessions":{}}}));
        plugins.len() - 1
    });
    if plugins[index].is_string() {
        plugins[index] = serde_json::json!({"package":PACKAGE,"options":{"sessions":{}}});
    }
    if let Some((id, port)) = identity {
        let options = plugins[index]
            .as_object_mut()
            .ok_or("Invalid OpenCode 2 plugin entry")?
            .entry("options")
            .or_insert_with(|| serde_json::json!({}));
        let sessions = options
            .as_object_mut()
            .ok_or("Invalid OpenCode 2 plugin options")?
            .entry("sessions")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or("Invalid OpenCode 2 session routes")?;
        sessions.retain(|_, route| route["nodeId"].as_i64() != Some(node_id));
        sessions.insert(id.into(), serde_json::json!({"nodeId":node_id,"port":port}));
    }
    let rules = root
        .entry("permissions")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or("OpenCode 2 permissions must be an array")?;
    // Keep one configured deny last; actual path enforcement still needs acceptance.
    let rule = serde_json::json!({"action":"external_directory","resource":"*","effect":"deny"});
    rules.retain(|existing| existing != &rule);
    rules.push(rule);
    if std::fs::read_to_string(&plugin_path).ok().as_deref() != Some(PLUGIN) {
        managed_config::atomic_write(&plugin_path, PLUGIN).map_err(|e| e.to_string())?;
    }
    if doc.root != before {
        managed_config::atomic_write(&path, &doc.to_pretty_json().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
