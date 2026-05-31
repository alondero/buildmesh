//! Session discovery commands — find resumable Claude Code sessions on disk

use crate::db;
use crate::env;
use crate::models::{AgentNode, Provider};
use crate::services::session_discovery::{self, DiscoveredSession};
use tauri::command;

#[command]
pub async fn discover_sessions(mesh_id: i64, mesh_path: String) -> Result<Vec<DiscoveredSession>, String> {
    session_discovery::discover(mesh_id, &mesh_path)
}

/// Import a discovered session as a new agent node with the correct worktree/path settings,
/// then set its cli_session_id so it's ready for resume.
#[command]
pub async fn import_discovered_session(
    mesh_id: i64,
    mesh_path: String,
    cli_session_id: String,
    branch: String,
    worktree_name: Option<String>,
    provider: Option<String>,
) -> Result<AgentNode, String> {
    let session_name = crate::session_naming::on_spawn();
    let resolved = env::resolve_agent_path(&mesh_path, None);
    let env_type = resolved.env_type;
    let provider_enum = provider
        .as_deref()
        .map(Provider::from_db_str)
        .unwrap_or(Provider::Anthropic);

    let node = db::create_agent_node(
        mesh_id,
        &session_name,
        &mesh_path,
        &branch,
        env_type,
        provider_enum,
        worktree_name.as_deref(),
        None,
    ).map_err(|e| e.to_string())?;

    db::update_cli_session_id(node.id, &cli_session_id)
        .map_err(|e| e.to_string())?;

    db::get_agent_node_by_id(node.id).map_err(|e| e.to_string())
}
