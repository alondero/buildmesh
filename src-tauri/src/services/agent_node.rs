//! Agent Node service — creation, deletion, and lifecycle orchestration

mod create;
pub use create::*;
mod discovery;
pub use discovery::*;
mod cleanup;
pub use cleanup::*;

mod lifecycle;
pub use lifecycle::*;

use crate::agent::provider::SpawnOptionId;
use crate::db;

/// Upgrade legacy history outside DB locks; the existing JSON column keeps the operation additive.
pub fn migrate_launch_history() -> Result<(), String> {
    let prefs = crate::preferences::load()?;
    let mut mappings: std::collections::HashMap<String, String> = prefs
        .spawn_configurations
        .iter()
        .filter(|c| c.generated.is_some())
        .filter(|c| {
            !prefs
                .spawn_configurations
                .iter()
                .any(|other| other.id == c.spawn_option_id)
        })
        .map(|c| (c.spawn_option_id.clone(), c.id.clone()))
        .collect();
    // Stored references to retired catalogue recipes (`launch/<harness>`)
    // heal to the equivalent bare Spawn Option; user-owned and explicitly
    // deleted ids are excluded by the helper.
    mappings
        .extend(crate::preferences::launch_configurations::retired_configuration_aliases(&prefs));
    db::migrate_launch_selections(&mappings).map_err(|e| e.to_string())?;
    let nodes = db::legacy_launch_nodes().map_err(|e| e.to_string())?;
    for node in nodes {
        let mut snapshot_prefs = prefs.clone();
        let selection = if let Some(configuration) = node.launch_configuration.as_ref() {
            snapshot_prefs
                .spawn_configurations
                .retain(|c| c.id != configuration.id);
            snapshot_prefs
                .spawn_configurations
                .push(configuration.clone());
            configuration.id.as_str()
        } else {
            node.provider.as_str()
        };
        match crate::preferences::launch_configurations::capture_legacy(&snapshot_prefs, selection)
        {
            Ok(plan) => db::set_node_launch_snapshot(
                node.id,
                &crate::preferences::launch_configurations::snapshot(plan),
            )?,
            Err(error) => tracing::warn!(
                node_id = node.id,
                "Legacy launch snapshot unavailable: {error}"
            ),
        }
    }
    Ok(())
}
use crate::env;
use crate::git::worktree::{self, WorktreeCloseSafety};
use crate::models::{AgentNode, PendingWorktreeRemoval, SessionStatus};
use crate::preferences::resolve_harness_provider_for;

/// Error type for agent node service operations
#[derive(Debug)]
pub enum AgentNodeError {
    Db(rusqlite::Error),
    /// Node's `SessionStatus` was not eligible for the requested
    /// operation (e.g. `regenerate` rejects `Spawning` / `Pending` /
    /// `Archived` / `Suspended`). The wrapped string is the user-
    /// facing reason.
    Status(String),
    /// The referenced `mesh_id` did not exist when this operation
    /// required it. Surfaced as a typed sentinel so callers can map
    /// to a precise HTTP status (the `/api/nodes/create` route uses
    /// this for its 400 "Mesh not found" response) instead of
    /// pattern-matching on a wrapped `String`. A regression that
    /// leaks `rusqlite::Error::QueryReturnedNoRows` past this
    /// boundary would surface as a 500 instead of a 400.
    ///
    /// Review-round-2 fix: replaces the prior string sentinel
    /// `"mesh not found"` carried inside `Status(String)`, which was
    /// brittle stringly-typed control flow.
    MeshNotFound(i64),
    /// The requested saved spawn configuration is invalid for this option.
    /// This remains typed so HTTP callers can return a client error without
    /// scraping the validation text.
    InvalidConfiguration(String),
    /// The caller included an initial prompt, and the harness that will
    /// launch does not accept one. Both surfaces return
    /// [`INITIAL_PROMPT_UNSUPPORTED`]; no row is inserted.
    InitialPromptUnsupported,
    /// A backend orchestrator call (`spawn_agent_inner` today, but
    /// the variant is intentionally generic so future downstream
    /// calls share the path) failed. The wrapped string is the
    /// underlying error message verbatim, so the frontend's toast
    /// surfaces the same text the Tauri command returns.
    Backend(String),
    /// A discovery-based resume named no harness, or one this build
    /// cannot resume. Import refuses to guess: a wrong executor turns a
    /// conversation from another harness into an empty or brand-new
    /// session (issue #2332). The wrapped string is the user-facing
    /// reason, and no row is inserted.
    UnresumableHarness(String),
    /// A remote create-and-start operation key (issue #2331) was reused for a
    /// *different* normalized request. The node INSERT is rolled back, so
    /// nothing was committed; the caller maps this to 409.
    OperationKeyConflict {
        key: String,
    },
}

impl std::fmt::Display for AgentNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentNodeError::Db(e) => write!(f, "{}", e),
            AgentNodeError::Status(e) => write!(f, "{}", e),
            AgentNodeError::MeshNotFound(mesh_id) => write!(f, "mesh {mesh_id} not found"),
            AgentNodeError::InvalidConfiguration(e) => write!(f, "{}", e),
            AgentNodeError::InitialPromptUnsupported => write!(f, "{INITIAL_PROMPT_UNSUPPORTED}"),
            AgentNodeError::Backend(e) => write!(f, "{}", e),
            AgentNodeError::UnresumableHarness(e) => write!(f, "{}", e),
            AgentNodeError::OperationKeyConflict { key } => write!(
                f,
                "operation key already used for a different request: {key}"
            ),
        }
    }
}

impl From<rusqlite::Error> for AgentNodeError {
    fn from(e: rusqlite::Error) -> Self {
        AgentNodeError::Db(e)
    }
}

/// Sentence returned when a create request includes an initial prompt and
/// the harness that will launch cannot accept one. HTTP and the desktop
/// commands share this exact text.
pub const INITIAL_PROMPT_UNSUPPORTED: &str = "Choose an agent that supports an initial prompt";

// Test-only modules are declared at the BOTTOM of this file on purpose.
// `agent::output`'s source-scan guard takes each file's production text as
// everything before its first `#[cfg(test)]` attribute line, so a gated item
// placed above production code would silently hide every call site after it.
// Keeping the gates last is what lets that guard stay a short scan instead of
// a lexer; `agent::output` asserts the invariant on every file it visits.
#[cfg(test)]
mod create_tests;
// Re-exported so `mod tests` keeps calling these bare; the
// `*_configured` / `create_blocking` functions above are the production surface.
// The two fork wrappers are called only from `create` / `create_pending` inside
// `create_tests`, so they stay private to that module.
#[cfg(test)]
pub(crate) use create_tests::{create, create_pending};

#[cfg(test)]
#[path = "agent_node_tests.rs"]
mod tests;
