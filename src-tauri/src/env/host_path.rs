//! Host-path conversion between the session-internal path forms and the
//! host-native form.
//!
//! This module is the **only** place in the Buildmesh tree that builds
//! `\\wsl$\...` UNC strings — the CLAUDE.md hard rule is structurally
//! enforced here, not by convention.
//!
//! It also owns the [`ResolvedPath`] machinery: the trio of `host_path` /
//! `spawn_path` / `raw_path` plus the [`Environment`] classification that
//! callers need to drive spawn commands, file operations, and filesystem
//! subscriptions on Windows / WSL hybrid hosts.
//!
//! ## Why a separate module from environment
//!
//! [`crate::env::Environment`] (in `environment.rs`) answers "what
//! environment are we in?". This module answers "given that environment, how
//! do I rewrite a path so the host tools can open it?". Splitting them keeps
//! detection silent on path conversion and conversion silent on detection —
//! a future change to either can't silently drag the other along.
//!
//! ## Adding a new path conversion
//!
//! Every `\\wsl$\`-shaped string and every `/mnt/c/` rewrite lands here, no
//! exceptions. The `.claude/hooks/guard-antipatterns.mjs` lint enforces this
//! for Windows-style paths via `// allow-wsl-path` per-line escape — that's
//! the side of the rule, this module is the open side.

use std::path::{Path, PathBuf};

use crate::models::{AgentNode, EnvType, SessionStatus};

use super::{current_env, Environment};

// ── Path-conversion primitives ─────────────────────────────────────────────

/// Convert a session path to the correct form for spawning commands
/// WSL paths are stored as Unix paths internally, Windows paths as Windows paths
pub fn to_spawn_path(path: &Path) -> PathBuf {
    match current_env() {
        Environment::Wsl => {
            if path.to_string_lossy().starts_with("/mnt/") {
                path.to_path_buf()
            } else if path.to_string_lossy().starts_with("C:\\") || path.to_string_lossy().starts_with("c:\\") {
                let path_str = path.to_string_lossy().to_lowercase();
                let drive = path_str.chars().next().unwrap_or('c');
                let rest = &path_str[2..].replace('\\', "/");
                PathBuf::from(format!("/mnt/{}{}", drive, rest))
            } else {
                path.to_path_buf()
            }
        }
        Environment::Windows => {
            path.to_path_buf()
        }
    }
}

/// Determine the environment for a given session path
pub fn env_for_path(path: &Path) -> Environment {
    // WSL only exists on a Windows host. On macOS and native Linux every path is
    // host-native, so a `/home/...` or `/mnt/...` path must NOT be classified as
    // WSL — doing so on Linux would route spawns through `wsl.exe` and rewrite
    // paths to bogus `\\wsl$\...` UNC form. (`Environment::Windows` is the
    // "native host, no translation" variant here, despite the name.)
    if !cfg!(target_os = "windows") {
        return Environment::Windows;
    }

    let path_str = path.to_string_lossy().to_lowercase();

    // WSL detection: paths starting with /mnt/, /home/, or \\wsl$
    if path_str.starts_with("/mnt/")
        || path_str.starts_with("/home/")
        || path_str.starts_with("\\\\wsl$")
    {
        Environment::Wsl
    } else {
        Environment::Windows
    }
}

/// Convert a path from session internal form to host-readable form
/// (e.g., /home/user -> \\wsl$\Ubuntu\home\user, /mnt/c/Users -> C:\Users, /c/Users -> C:\Users)
pub fn to_host_path(path: &str) -> String {
    // Only a Windows host has a WSL filesystem to translate into (`\\wsl$\...`
    // UNC paths, drive letters). On macOS and native Linux the path is already
    // host-native, so return it unchanged — otherwise a normal Linux
    // `/home/...` path would be rewritten to a `\\wsl$\...` UNC path that no
    // git2/file op on that host can open.
    if !cfg!(target_os = "windows") {
        return path.to_string();
    }

    if path.starts_with('/') {
        if path.starts_with("/mnt/") {
            // /mnt/c/Users -> C:\Users
            let drive = path.chars().nth(5).unwrap_or('c').to_uppercase().next().unwrap();
            format!("{}:{}", drive, path[6..].replace('/', "\\"))
        } else if path.starts_with("/home/") {
            // /home/user -> \\wsl$\Ubuntu\home\user
            let distro = super::environment::get_default_wsl_distro()
                .unwrap_or_else(|| "Ubuntu".to_string());
            format!("\\\\wsl$\\{}{}", distro, path.replace('/', "\\"))
        } else if path.len() >= 2 && path.chars().nth(1).unwrap().is_alphabetic() && (path.len() == 2 || path.chars().nth(2) == Some('/')) {
            // Handle Git Bash style /c/Users/ or /c
            let drive = path.chars().nth(1).unwrap().to_uppercase().next().unwrap();
            let rest = if path.len() > 2 { &path[2..] } else { "" };
            format!("{}:{}", drive, rest.replace('/', "\\"))
        } else {
            // Other Unix-style absolute path on Windows (e.g. /Users/...)
            // Return as-is, caller will handle if needed.
            path.to_string()
        }
    } else {
        path.to_string()
    }
}

// ── ResolvedPath — high-level path resolution for agent operations ─────────

/// A fully-resolved set of paths for an agent node, ready for use by callers
/// without needing to compose env detection + host conversion + worktree logic.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    /// Host-accessible path for file system operations (e.g. Windows UNC path for WSL,
    /// or native path on macOS/Windows).
    pub host_path: String,
    /// Path to use as CWD when spawning agent/shell processes.
    pub spawn_path: String,
    /// The input `base_path`, optionally with a `/`-separated
    /// `.claude/worktrees/{trimmed_name}` appended for a Worktree Node. The
    /// "raw" form is input-pass-through — `node.path` is whatever was stored
    /// in the DB (POSIX `/home/...`, Windows native `C:\...`, or WSL UNC
    /// `\\wsl$\...`), and the worktree subdir is appended with `/` regardless
    /// of host. This is the input to `to_host_path` / `to_spawn_path` and
    /// the form the frontend's `getNodeGitPath` (in `src/lib/paths.ts`)
    /// mirrors for the GIT_CHANGED subscription. Exposed so callers like
    /// `file_watcher` can stop re-spelling the worktree rule and consume
    /// the single canonical authority (issue #409).
    pub raw_path: String,
    /// The detected environment type for this path.
    pub env_type: EnvType,
}

/// Resolve the working directory for an agent node, accounting for worktree
/// layout and environment differences.
///
/// - `base_path`: The agent node's stored `path` field (project root).
/// - `worktree_name`: If set, the worktree subdirectory name under
///   `{base_path}/.claude/worktrees/{name}`.
///
/// Returns a `ResolvedPath` with host, spawn, and env fields populated.
pub fn resolve_agent_path(base_path: &str, worktree_name: Option<&str>) -> ResolvedPath {
    // Compute the effective path (with worktree if applicable)
    let raw_path = match worktree_name {
        Some(wt_name) if !wt_name.is_empty() => {
            format!("{}/.claude/worktrees/{}", base_path, wt_name)
        }
        _ => base_path.to_string(),
    };

    // Detect environment from the effective path
    let path_buf = PathBuf::from(&raw_path);
    let env_internal = env_for_path(&path_buf);
    let env_type = EnvType::from(env_internal);

    // On macOS and native Linux, paths are always host-native — no WSL
    // conversion needed. Only a Windows host translates based on the detected
    // environment (WSL agents need `\\wsl$\...` host paths + `/mnt/...` spawn
    // paths).
    let (host_path, spawn_path) = if !cfg!(target_os = "windows") {
        (raw_path.clone(), raw_path.clone())
    } else {
        let host = to_host_path(&raw_path);
        let spawn = to_spawn_path(&path_buf).to_string_lossy().to_string();
        (host, spawn)
    };

    ResolvedPath {
        host_path,
        spawn_path,
        raw_path,
        env_type,
    }
}

/// The trimmed, non-empty worktree name iff the node runs in a worktree — the
/// single definition of "does this Agent Node have a Worktree Node dir".
///
/// `pub(crate)` so command handlers that need to validate a worktree (e.g.
/// `commands::build_run` feeding the name into `validate_worktree_exists`'s
/// git2 worktree-list compare) read the SAME trimmed value the canonical
/// resolver consumed. Re-reading `node.worktree_name` directly bypasses the
/// trim invariant; reaching for `worktree_segment` keeps it in one place.
pub(crate) fn worktree_segment(node: &AgentNode) -> Option<&str> {
    if !node.use_worktree {
        return None;
    }
    node.worktree_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
}

/// The [Node Working Directory](../../CONTEXT.md): the directory an Agent Node's
/// work physically lives in — its Worktree Node dir for a worktree node, or the
/// Mesh root for a Root Node. This is the one canonical home of the
/// `use_worktree` + (trimmed, non-empty) `worktree_name` rule; diff, PR,
/// close-safety, removal, and the coordinator transcript reader all resolve
/// through here so every layer agrees on a node's directory. Callers pick the
/// `host_path` (Windows git2 / file ops), the `spawn_path` (the path as the
/// agent saw it — the form Claude Code encodes for its on-disk transcript
/// dir), or the `raw_path` (the POSIX-style effective path; the string the
/// GIT_CHANGED payload carries to the frontend's `getNodeGitPath()` for
/// subscription matching, issue #409).
///
/// Mirrors the frontend's `getNodeGitPath` in `src/lib/paths.ts` — keep the two
/// in sync if the worktree layout ever changes (paired cross-language defaults,
/// not a single source; see [[buildmesh-use-worktree-derivation]] and
/// [[feedback_cross-language-default-coupling]]).
pub fn node_working_path(node: &AgentNode) -> ResolvedPath {
    resolve_agent_path(&node.path, worktree_segment(node))
}

/// The node's Worktree Node dir — `Some` only for a Worktree Node, `None` for a
/// Root Node. The `None` is load-bearing: close-safety and worktree removal skip
/// Root Nodes through it (a Root Node has no worktree to inspect or delete).
pub fn node_worktree_path(node: &AgentNode) -> Option<ResolvedPath> {
    worktree_segment(node).map(|_| node_working_path(node))
}

/// The paths a node "owns" — its mesh root plus the resolved Worktree Node
/// dir, if any. Single canonical reader of the `use_worktree` +
/// `worktree_name` rule via `node_worktree_path`; the worktree-manager
/// (`get_git_prune_info`, issue #607) and mesh-health
/// (`find_base_branch_holder`, issue #621) both consume this so they
/// can't drift apart on the worktree-dir entry.
pub fn active_node_paths(nodes: &[AgentNode]) -> Vec<String> {
    let mut paths = Vec::new();
    for node in nodes {
        paths.push(node.path.clone());
        if let Some(resolved) = node_worktree_path(node) {
            paths.push(resolved.host_path);
        }
    }
    paths
}

/// The branches a node currently has checked out — the `branch` field of
/// every non-archived node. Sibling reader to [`active_node_paths`]: just
/// as a worktree is "active" when a node's path resolves to it, a branch
/// is "active" when a live node has it checked out.
///
/// Used by the Worktree Manager tab to (a) flag local branches with an
/// `is_active` badge in the prune list and (b) gate the delete checkbox
/// so the user can't accidentally drop a branch a live agent is using.
/// The corresponding backend guard in `commands::prune::delete_branches`
/// is defence-in-depth — a stale UI (or a direct API call) must not be
/// able to delete a branch a live node is on.
///
/// **Two filter axes the caller must handle**:
/// 1. **Mesh scope** — branch names are not filesystem-unique like paths,
///    so a `feature-a` in `/repo1` would collide with a `feature-a` in
///    `/repo2`. Callers that care about repo-scoped active-ness should
///    pre-filter `nodes` by mesh (`db::list_agent_nodes_by_mesh`) before
///    calling this. Both `delete_branches` and `get_git_prune_info`
///    mesh-scope via `db::list_agent_nodes_by_mesh(mesh_id)` — the
///    path asymmetry that previously existed (`get_git_prune_info` used
///    the global `db::list_agent_nodes`) was closed in PR #660, and
///    worktree path active-checks are now also mesh-scoped (defensive
///    symmetry even though filesystem paths are host-unique on disk).
/// 2. **Archived status** — enforced here. The function drops any node
///    whose `status == SessionStatus::Archived` so a closed agent node
///    doesn't keep its branch locked. This matches the contract
///    `db::list_agent_nodes()` provides on its own (`WHERE status !=
///    'archived'`); `db::list_agent_nodes_by_mesh` does NOT filter
///    archived, which is why the filter lives here.
pub fn active_node_branches(nodes: &[AgentNode]) -> Vec<String> {
    nodes
        .iter()
        .filter(|n| n.status != SessionStatus::Archived)
        .map(|n| n.branch.clone())
        .collect()
}
