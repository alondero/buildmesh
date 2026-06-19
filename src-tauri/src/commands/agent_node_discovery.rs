//! Agent node discovery commands — find resumable Claude Code sessions on disk
//!
//! Renamed from `session_discovery` in issue #490: the public IPC surface
//! uses "Agent Node" vocabulary; the on-disk Claude Code CLI session id
//! stays as `cli_session_id` per CONTEXT.md ambiguity #1.

use crate::db;
use crate::env;
use crate::models::{AgentNode, Provider};
use crate::services::agent_node_discovery::{self, DiscoveredAgentNode};
use tauri::command;

#[command]
pub async fn discover_agent_nodes(mesh_id: i64, mesh_path: String) -> Result<Vec<DiscoveredAgentNode>, String> {
    agent_node_discovery::discover(mesh_id, &mesh_path)
}

/// Import a discovered session as a new agent node with the correct worktree/path settings,
/// then set its cli_session_id so it's ready for resume.
#[command]
pub async fn import_discovered_agent_node(
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

    let use_worktree = worktree_name.is_some();
    let node = db::create_agent_node(
        mesh_id,
        &session_name,
        &mesh_path,
        &branch,
        env_type,
        provider_enum,
        worktree_name.as_deref(),
        None,
        None,
        None, // source_pr_pinned_sha — agent-node-discovery path doesn't pin a PR SHA
        use_worktree,
        None,
        None,
    ).map_err(|e| e.to_string())?;

    db::update_cli_session_id(node.id, &cli_session_id)
        .map_err(|e| e.to_string())?;

    db::get_agent_node_by_id(node.id).map_err(|e| e.to_string())
}

// --- Deprecation shims (issue #490). Forward old `discover_sessions` /
// `import_discovered_session` IPC names to the new `*_agent_node` commands.
// The OLD param names are preserved on the shim signatures so the wire shape
// stays byte-identical for one release. Removed in the release after next. ---

#[command]
pub async fn discover_sessions(mesh_id: i64, mesh_path: String) -> Result<Vec<DiscoveredAgentNode>, String> {
    tracing::warn!(
        target: "ipc_deprecation",
        "discover_sessions is deprecated; use discover_agent_nodes"
    );
    discover_agent_nodes(mesh_id, mesh_path).await
}

#[command]
pub async fn import_discovered_session(
    mesh_id: i64,
    mesh_path: String,
    cli_session_id: String,
    branch: String,
    worktree_name: Option<String>,
    provider: Option<String>,
) -> Result<AgentNode, String> {
    tracing::warn!(
        target: "ipc_deprecation",
        "import_discovered_session is deprecated; use import_discovered_agent_node"
    );
    import_discovered_agent_node(mesh_id, mesh_path, cli_session_id, branch, worktree_name, provider).await
}
