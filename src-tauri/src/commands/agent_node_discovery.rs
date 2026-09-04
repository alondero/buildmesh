//! Agent node discovery commands — find resumable Claude Code sessions on disk
//!
//! Renamed from `session_discovery` in issue #490: the public IPC surface
//! uses "Agent Node" vocabulary; the on-disk Claude Code CLI session id
//! stays as `cli_session_id` per CONTEXT.md ambiguity #1.
//!
//! `#[blocking_command]` (PR #1388 review point 3) wraps the body in
//! `crate::commands::run_blocking` so we don't hand-write the
//! offload boilerplate.

use buildmesh_macros::blocking_command;
use crate::db;
use crate::env;
use crate::models::AgentNode;
use crate::services::agent_node_discovery::{self, ArchivedAgentNode};
use tauri::command;

#[command]
#[blocking_command]
pub async fn discover_agent_nodes(mesh_id: i64, mesh_path: String) -> Result<Vec<ArchivedAgentNode>, String> {
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
    mesh_path: String,
    cli_session_id: String,
    branch: String,
    worktree_name: Option<String>,
    cwd: Option<String>,
    provider: Option<String>,
) -> Result<AgentNode, String> {
    crate::commands::run_blocking("import_discovered_agent_node", move || {
        // Keep the wire argument for API compatibility; the database row is
        // authoritative for the repository path used below.
        let _caller_mesh_path = mesh_path;
        let session_name = crate::session_naming::on_spawn();
        let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
        // The Mesh row is authoritative; do not let a caller-supplied path
        // redirect an imported node to a different repository.
        let mesh_root = mesh.path.as_str();
        let app_directory = crate::preferences::worktree_directory();
        let configured_directory = env::effective_worktree_directory(
            mesh_root,
            mesh.worktree_directory.as_deref(),
            app_directory.as_deref(),
        )?;
        let worktree_path = worktree_name
            .as_deref()
            .map(|name| {
                env::resolve_imported_worktree_path(
                    mesh_root,
                    &configured_directory,
                    name,
                    cwd.as_deref(),
                )
            })
            .transpose()?;
        let resolved = worktree_path
            .as_deref()
            .map(env::resolve_path)
            .unwrap_or_else(|| env::resolve_agent_path(mesh_root, None));
        let env_type = resolved.env_type;
        // Store the harness/profile id verbatim (issue #535); resolution to a
        // concrete executor happens at the spawn seam. Absent → "anthropic".
        let provider_id = provider.as_deref().unwrap_or("anthropic");

        let use_worktree = worktree_name.is_some();
        let node = db::create_agent_node(
            mesh_id,
            &session_name,
            mesh_root,
            &branch,
            env_type,
            provider_id,
            worktree_name.as_deref(),
            worktree_path.as_deref(),
            None,
            None,
            None, // source_pr_pinned_sha — agent-node-discovery path doesn't pin a PR SHA
            use_worktree,
            None,
            None,
        )
        .map_err(|e| e.to_string())?;

        db::update_cli_session_id(node.id, &cli_session_id)
            .map_err(|e| e.to_string())?;

        db::get_agent_node_by_id(node.id).map_err(|e| e.to_string())
    })
    .await
}
