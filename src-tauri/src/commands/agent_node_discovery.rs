//! Agent node discovery commands — find resumable Claude Code sessions on disk
//!
//! Renamed from `session_discovery` in issue #490: the public IPC surface
//! uses "Agent Node" vocabulary; the on-disk Claude Code CLI session id
//! stays as `cli_session_id` per CONTEXT.md ambiguity #1.
//!
//! `#[blocking_command]` (PR #1388 review point 3) wraps the body in
//! `crate::commands::run_blocking` so we don't hand-write the
//! offload boilerplate.

use crate::models::AgentNode;
use crate::services::agent_node_discovery::{self, ArchivedAgentNode};
use buildmesh_macros::blocking_command;
use tauri::command;

#[command]
#[blocking_command]
pub async fn discover_agent_nodes(
    mesh_id: i64,
    mesh_path: String,
) -> Result<Vec<ArchivedAgentNode>, String> {
    // Offload: discovery walks every `~/.claude/projects/<mesh>*` directory
    // and opens each session's JSONL transcript looking for the first real
    // user message — unbounded filesystem I/O (slow on WSL UNC paths) that
    // must not park a Tauri async worker while the Archive tab loads.
    agent_node_discovery::discover(mesh_id, &mesh_path)
}

/// Import a discovered session as a new agent node with the correct worktree/path settings,
/// then set its cli_session_id so it's ready for resume.
#[command]
pub async fn import_discovered_agent_node(
    mesh_id: i64,
    #[allow(unused)] mesh_path: String,
    cli_session_id: String,
    branch: String,
    worktree_name: Option<String>,
    provider: Option<String>,
) -> Result<AgentNode, String> {
    // `mesh_path` stays on the IPC surface. The runner attaches the node
    // to the path stored on the mesh row.
    crate::commands::run_blocking("import_discovered_agent_node", move || {
        crate::services::agent_node::import_discovered_session(
            mesh_id,
            &cli_session_id,
            &branch,
            worktree_name.as_deref(),
            provider.as_deref(),
        )
        .map_err(|error| error.to_string())
    })
    .await
}
