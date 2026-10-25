//! Import discovered conversations and validate their resumable harness provenance.

use super::*;
/// Import a discovered CLI session as a node and store its session id.
///
/// `provider` is the conversation's originating harness, carried from
/// `ArchivedAgentNode::provider` (issue #2332). It is required: the archived
/// conversation only exists inside that harness, so an absent or unusable
/// value is refused with [`AgentNodeError::UnresumableHarness`] rather than
/// defaulted to `anthropic`, which used to launch a Cursor/AGY/Command Code
/// conversation through Claude Code.
///
/// The id is stored verbatim once validated, so a Proxied Provider row
/// (`claude:minimax`) keeps working. This path does not resolve a launch
/// configuration: an import is a resume of an existing conversation, not a new
/// launch. The mesh path comes from the mesh row. `use_worktree` follows
/// whether a name was supplied, including an empty one; the stored path is set
/// only when that name trims to something.
pub fn import_discovered_session(
    mesh_id: i64,
    cli_session_id: &str,
    branch: &str,
    worktree_name: Option<&str>,
    provider: &str,
) -> Result<AgentNode, AgentNodeError> {
    // Validate before the first DB call: an unknown executor must leave no row
    // behind at all, and this reads preferences rather than holding a
    // connection.
    resolve_resumable_harness(provider)?;
    let mesh = match db::get_mesh_by_id(mesh_id) {
        Ok(mesh) => mesh,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(AgentNodeError::MeshNotFound(mesh_id));
        }
        Err(error) => return Err(AgentNodeError::Db(error)),
    };
    let session_name = crate::session_naming::on_spawn();
    let use_worktree = worktree_name.is_some();
    let (mut worktree_path, mut env_type) = place_worktree(
        &mesh.path,
        mesh.worktree_directory.as_deref(),
        worktree_name,
    );
    // Import resumes an existing checkout. Built-in layouts remain valid
    // discovery locations even after the user changes the configured dir.
    if let Some(name) = worktree_name.map(str::trim).filter(|name| !name.is_empty()) {
        let current_exists = worktree_path
            .as_deref()
            .is_some_and(crate::git::project::is_materialized_checkout);
        if !current_exists {
            let default_dir = env::effective_worktree_dir_raw(&mesh.path, None, None);
            let built_in_paths = [
                env::resolve_agent_path_in_dir(&mesh.path, &default_dir, Some(name)),
                env::resolve_agent_path(&mesh.path, Some(name)),
            ];
            if let Some(existing) = built_in_paths
                .into_iter()
                .find(|path| crate::git::project::is_materialized_checkout(&path.host_path))
            {
                worktree_path = Some(existing.raw_path);
                env_type = existing.env_type;
            }
        }
    }
    let provider_id = provider;
    let node = db::create_agent_node(db::NewAgentNode {
        mesh_id,
        name: &session_name,
        path: &mesh.path,
        branch,
        env: env_type,
        provider: provider_id,
        worktree_name,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: worktree_path.as_deref(),
        spawn_configuration: None,
    })?;
    db::update_cli_session_id(node.id, cli_session_id)?;
    db::get_agent_node_by_id(node.id).map_err(AgentNodeError::from)
}

/// Sentence returned when a discovery-based resume names no harness, or a
/// harness that cannot resume. Both the mobile route and the desktop command
/// surface it verbatim (issue #2332).
pub const UNRESUMABLE_HARNESS: &str =
    "Buildmesh does not know which agent harness created this conversation, so it cannot resume it. \
Update Buildmesh to scan this harness, or start a new session instead.";

/// Refuse a discovery-based resume whose executor is missing or unusable, and
/// otherwise hand back the executor that will run it.
///
/// The value may be an adapter id (`cursor`) *or* a Spawn Option id
/// (`claude:minimax`) — a Proxied Provider row is stored verbatim on the node,
/// so import keeps accepting both. Only the harness half decides anything, so
/// the id is split with the existing [`crate::agent::provider::SpawnOptionId`]
/// parser and the harness half is validated with `harness_capabilities_for`,
/// the existing known-harness check (issue #1148): `None` for an unknown id,
/// which is what keeps an absent or unrecognised value from falling through to
/// `Provider::from_db_str`'s Anthropic default. A plain Terminal or a
/// non-resumable harness (DSH) passes the known-id question and fails the
/// second: there is a CLI session id to resume, but nothing to resume it with.
///
/// The typed [`crate::models::Provider`] is what the HTTP route needs to pick
/// the provider-specific session-id gate
/// (`http::request::parse_session_id_for_provider`), so this is the one place
/// that answers "which executor, and can it resume at all".
pub fn resolve_resumable_harness(
    provider: &str,
) -> Result<crate::models::Provider, AgentNodeError> {
    let trimmed = provider.trim();
    if trimmed.is_empty() {
        return Err(AgentNodeError::UnresumableHarness(
            UNRESUMABLE_HARNESS.to_string(),
        ));
    }
    let harness_id = crate::agent::provider::SpawnOptionId::from(trimmed)
        .harness_id()
        .to_string();
    let Some(capabilities) = crate::preferences::harness_capabilities_for(&harness_id) else {
        return Err(AgentNodeError::UnresumableHarness(
            UNRESUMABLE_HARNESS.to_string(),
        ));
    };
    if !capabilities.supports_resume {
        let label = crate::agent::harness_catalog::inspector_label_for_adapter(&harness_id)
            .unwrap_or(harness_id.as_str());
        return Err(AgentNodeError::UnresumableHarness(format!(
            "{label} cannot resume an archived conversation."
        )));
    }
    Ok(crate::preferences::resolve_harness_provider(trimmed))
}
