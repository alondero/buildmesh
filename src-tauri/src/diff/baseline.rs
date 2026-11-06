//! Which baseline a change set is measured against.
//!
//! The rule is ADR 0005's: a base ref is resolved to the **merge-base** of
//! HEAD and the ref, so the diff covers everything the agent changed since
//! branching — committed *and* uncommitted. The resolution itself lives in
//! [`crate::git::diff::resolve_base_tree`], the crate's single definition of
//! that seam; this module owns the *node-facing* half — which repository to
//! read and which ref to read it against.

use crate::db;
use crate::models::AgentNode;

/// The base ref used when the mesh row cannot be read.
pub const DEFAULT_BASE_REF: &str = "origin/main";

/// The baseline of one Agent Node's change set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeBaseline {
    /// The host path of the node's repository — its worktree when it has one.
    pub repo_path: String,
    /// The ref the change set is measured against.
    pub base_ref: String,
}

/// Resolve a node's baseline: the repository to read, and the ref to read it
/// against.
///
/// The repository is the node's *resolved* path — its worktree when it has
/// one — because a worktree agent's edits live there, not at the project root
/// (the regression the review surface hit when it diffed `node.path`).
pub fn node_baseline(node: &AgentNode) -> NodeBaseline {
    NodeBaseline {
        repo_path: crate::env::node_git_path(node).host_path,
        base_ref: node_base_ref(node),
    }
}

/// The node's mesh's base ref, defaulting to `origin/main` when the mesh row
/// cannot be read.
fn node_base_ref(node: &AgentNode) -> String {
    db::get_mesh_by_id(node.mesh_id)
        .map(|mesh| mesh.base_ref)
        .unwrap_or_else(|_| DEFAULT_BASE_REF.to_string())
}
