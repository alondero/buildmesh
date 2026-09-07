//! Agent node discovery commands — find resumable Claude Code sessions on disk
//!
//! Renamed from `session_discovery` in issue #490: the public IPC surface
//! uses "Agent Node" vocabulary; the on-disk Claude Code CLI session id
//! stays as `cli_session_id` per CONTEXT.md ambiguity #1.
//!
//! `#[blocking_command]` (PR #1388 review point 3) wraps the body in
//! `crate::commands::run_blocking` so we don't hand-write the
//! offload boilerplate.

use crate::db;
use crate::env;
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
    mesh_path: String,
    cli_session_id: String,
    branch: String,
    worktree_name: Option<String>,
    provider: Option<String>,
) -> Result<AgentNode, String> {
    crate::commands::run_blocking("import_discovered_agent_node", move || {
        let session_name = crate::session_naming::on_spawn();
        // Issue #1519: resolve the effective dir so the imported node
        // persists the same `worktree_path` a fresh spawn would.
        let mesh_row = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
        let app_dir = crate::preferences::worktree_directory();
        let effective_dir = env::effective_worktree_dir_raw(
            &mesh_path,
            mesh_row.worktree_directory.as_deref(),
            app_dir.as_deref(),
        );
        let use_worktree = worktree_name.is_some();
        // Imported nodes keep the discovered `worktree_name` when set;
        // otherwise they are Root Nodes. Persist the effective path for
        // worktree imports so later setting changes don't move them.
        let worktree_path_owned: Option<String> = worktree_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|n| env::resolve_worktree_node_raw(&effective_dir, n));
        let resolved =
            env::resolve_agent_path_in_dir(&mesh_path, &effective_dir, worktree_name.as_deref());
        let env_type = resolved.env_type;
        // Store the harness/profile id verbatim (issue #535); resolution to a
        // concrete executor happens at the spawn seam. Absent → "anthropic".
        let provider_id = provider.as_deref().unwrap_or("anthropic");

        let node = db::create_agent_node(
            mesh_id,
            &session_name,
            &mesh_path,
            &branch,
            env_type,
            provider_id,
            worktree_name.as_deref(),
            None,
            None,
            None, // source_pr_pinned_sha — agent-node-discovery path doesn't pin a PR SHA
            use_worktree,
            None,
            None,
            worktree_path_owned.as_deref(),
        )
        .map_err(|e| e.to_string())?;

        db::update_cli_session_id(node.id, &cli_session_id).map_err(|e| e.to_string())?;

        db::get_agent_node_by_id(node.id).map_err(|e| e.to_string())
    })
    .await
}
