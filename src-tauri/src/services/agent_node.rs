//! Agent Node service — creation, deletion, and lifecycle orchestration

use crate::db;
use crate::env;
use crate::models::{AgentNode, Provider, SessionStatus};

/// Error type for agent node service operations
#[derive(Debug)]
pub enum AgentNodeError {
    Db(rusqlite::Error),
}

impl std::fmt::Display for AgentNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentNodeError::Db(e) => write!(f, "{}", e),
        }
    }
}

impl From<rusqlite::Error> for AgentNodeError {
    fn from(e: rusqlite::Error) -> Self {
        AgentNodeError::Db(e)
    }
}

/// Create a new agent node with auto-generated name, environment detection,
/// and provider resolution.
pub fn create(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
) -> Result<AgentNode, AgentNodeError> {
    let session_name = crate::session_naming::on_spawn();
    tracing::debug!(
        "agent_node::create: mesh_id={}, name={}, path={}, branch={}, provider={:?}",
        mesh_id, session_name, path, branch, provider
    );

    let resolved = env::resolve_agent_path(path, None);
    let env_type = resolved.env_type;
    let provider_enum = provider
        .map(Provider::from_db_str)
        .unwrap_or(Provider::Anthropic);

    let node = db::create_agent_node(
        mesh_id,
        &session_name,
        path,
        branch,
        env_type,
        provider_enum,
        Some(&session_name),
        source_issue,
    )?;

    Ok(node)
}

/// Delete an agent node, cleaning up associated runtime state (node_namer buffers).
pub fn delete(session_id: i64) -> Result<(), AgentNodeError> {
    crate::session_naming::cleanup(session_id);
    db::delete_agent_node(session_id)?;
    Ok(())
}

/// Update agent node status from a string representation.
pub fn update_status(session_id: i64, status: &str) -> Result<(), AgentNodeError> {
    let status = SessionStatus::from_db_str(status);
    db::update_agent_node_status(session_id, status)?;
    Ok(())
}
