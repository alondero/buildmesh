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

/// The host-accessible Codex rollout directory for an agent environment.
/// CLI-home selection belongs to `environment`; this module owns the WSL path
/// conversion needed before the host reads that directory.
pub(crate) fn codex_sessions_dir(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    let home = super::environment::codex_dir_for_env(env_type, spawn_path)?;
    match env_type {
        EnvType::Windows => Some(home.join("sessions")),
        EnvType::Wsl => Some(
            PathBuf::from(to_host_path(&home.to_string_lossy())).join("sessions"),
        ),
    }
}

/// The host-accessible Command Code projects root for an agent environment
/// (`<home>/.commandcode/projects`). WSL homes are converted to host-readable
/// paths by the shared environment path module. Per-project slug formatting
/// lives with the transcript format (`transcript_reader::commandcode_project_slug`);
/// this module only owns the environment boundary (home lookup + WSL translation).
pub(crate) fn commandcode_projects_dir(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    let home = super::environment::commandcode_dir_for_env(env_type, spawn_path)?;
    match env_type {
        EnvType::Windows => Some(home.join("projects")),
        EnvType::Wsl => Some(
            PathBuf::from(to_host_path(&home.to_string_lossy())).join("projects"),
        ),
    }
}

/// The host-accessible Antigravity brain directory for an agent environment
/// (`<agy-home>/brain`, one subdirectory per conversation). CLI-home
/// selection belongs to `environment`; this module owns the WSL path
/// conversion needed before the host reads that directory (issue #1499).
pub(crate) fn agy_brain_dir_for_env(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    let home = super::environment::agy_dir_for_env(env_type, spawn_path)?;
    match env_type {
        EnvType::Windows => Some(home.join("brain")),
        EnvType::Wsl => Some(
            PathBuf::from(to_host_path(&home.to_string_lossy())).join("brain"),
        ),
    }
}

/// Normalize a raw path that may be a WSL UNC path (`\\wsl$\<distro>\...` or
/// `//wsl$/<distro>/...`) into WSL spawn form (`/...`). Non-UNC paths are
/// returned unchanged (borrowed — no allocation on the hot path).
///
/// Command Code runs inside WSL with cwd `/home/user/repo` and slugs that
/// form, but Buildmesh may hold the same mesh as a Windows host UNC path.
/// Slugging the UNC form would yield `wsl-ubuntu-home-user-repo` and silently
/// match nothing; normalize before slugging (issue #1500).
pub(crate) fn normalize_unc_to_wsl(path: &str) -> std::borrow::Cow<'_, str> {
    let bytes = path.as_bytes();
    if bytes.len() < 8 {
        return std::borrow::Cow::Borrowed(path);
    }
    let sep = |b: u8| b == b'\\' || b == b'/';
    if !sep(bytes[0]) || !sep(bytes[1]) {
        return std::borrow::Cow::Borrowed(path);
    }
    if !path.get(2..6).is_some_and(|s| s.eq_ignore_ascii_case("wsl$")) {
        return std::borrow::Cow::Borrowed(path);
    }
    if !sep(bytes[6]) {
        return std::borrow::Cow::Borrowed(path);
    }
    let rest = &path[7..];
    let distro_end = rest.find(['\\', '/']).map(|i| i + 7).unwrap_or(path.len());
    let after = if distro_end < path.len() {
        &path[distro_end..]
    } else {
        ""
    };
    if after.is_empty() {
        return std::borrow::Cow::Owned("/".to_string());
    }
    let mut out = String::with_capacity(after.len() + 1);
    out.push('/');
    let mut last_was_slash = true;
    for c in after.chars() {
        if c == '\\' || c == '/' {
            if !last_was_slash {
                out.push('/');
                last_was_slash = true;
            }
        } else {
            out.push(c);
            last_was_slash = false;
        }
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    std::borrow::Cow::Owned(out)
}

/// Compare CLI-recorded working directories across Windows, WSL, and native
/// Unix path syntax. APFS/HFS+ is usually case-insensitive too.
pub fn directories_match(recorded: &str, spawn: &str) -> bool {
    let recorded = normalize_directory(recorded);
    let spawn = normalize_directory(spawn);
    if case_insensitive_fs(&recorded) || case_insensitive_fs(&spawn) {
        recorded.eq_ignore_ascii_case(&spawn)
    } else {
        recorded == spawn
    }
}

/// Normalize dot components without requiring the path to exist.
///
/// Transcript stores record the CLI's normalized working directory, while a
/// relative Worktree Node preference is intentionally persisted in its raw
/// form. Discovery uses this helper to consider both spellings when encoding
/// project directories, including Windows drive and UNC prefixes.
pub fn normalize_path_lexically(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let (prefix, rest, absolute) = if normalized.starts_with("//") {
        ("//", &normalized[2..], true)
    } else if normalized.starts_with('/') {
        ("/", &normalized[1..], true)
    } else if normalized.len() >= 3
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[2] == b'/'
    {
        (&normalized[..3], &normalized[3..], true)
    } else if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        (&normalized[..2], &normalized[2..], false)
    } else {
        ("", normalized.as_str(), false)
    };

    let mut components = Vec::new();
    for component in rest.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.last().is_some_and(|last| *last != "..") {
                    components.pop();
                } else if !absolute {
                    components.push("..");
                }
            }
            value => components.push(value),
        }
    }
    let joined = components.join("/");
    match (prefix, joined.as_str()) {
        ("", value) => value.to_string(),
        ("/", "") => "/".to_string(),
        ("//", "") => "//".to_string(),
        ("/", value) | ("//", value) => format!("{prefix}{value}"),
        (drive, value) if drive.ends_with(":/") && value.is_empty() => drive.to_string(),
        (drive, value) if drive.ends_with(":/") => format!("{drive}{value}"),
        (drive, value) if value.is_empty() => drive.to_string(),
        (drive, value) => format!("{drive}{value}"),
    }
}

fn normalize_directory(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn case_insensitive_fs(path: &str) -> bool {
    cfg!(target_os = "macos") || looks_windows_volume(path)
}

fn looks_windows_volume(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[1] == b':')
        || path.starts_with("//")
        || (path.len() >= 7
            && path
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("/mnt/"))
            && path.as_bytes().get(6) == Some(&b'/'))
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
    /// The exact persisted Worktree Node path, or the legacy `base_path` plus
    /// `.claude/worktrees/{trimmed_name}` derivation for an upgraded row. The
    /// "raw" form is input-pass-through: POSIX `/home/...`, Windows native
    /// `C:\...`, or WSL UNC `\\wsl$\...`. This is the input to
    /// `to_host_path` / `to_spawn_path` and
    /// the form the frontend's `getNodeGitPath` (in `src/lib/paths.ts`)
    /// mirrors for the GIT_CHANGED subscription. Exposed so callers like
    /// `file_watcher` can stop re-spelling the worktree rule and consume
    /// the single canonical authority (issue #409).
    pub raw_path: String,
    /// The detected environment type for this path.
    pub env_type: EnvType,
}

/// Resolve one already-composed raw path into its host and spawn forms.
/// Configured Worktree Node paths use this same conversion seam as the
/// legacy layout (issue #1519).
pub fn resolve_path(raw_path: &str) -> ResolvedPath {
    let path_buf = PathBuf::from(raw_path);
    let env_internal = env_for_path(&path_buf);
    let env_type = EnvType::from(env_internal);
    let (host_path, spawn_path) = if !cfg!(target_os = "windows") {
        (raw_path.to_string(), raw_path.to_string())
    } else {
        (
            to_host_path(raw_path),
            to_spawn_path(&path_buf).to_string_lossy().to_string(),
        )
    };

    ResolvedPath {
        host_path,
        spawn_path,
        raw_path: raw_path.to_string(),
        env_type,
    }
}

/// Normalize an optional Worktree Node directory setting. Blank input clears
/// the setting and restores inheritance.
pub fn normalize_worktree_directory(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn is_absolute_in_any_supported_environment(path: &str) -> bool {
    let bytes = path.as_bytes();
    Path::new(path).is_absolute()
        || path.starts_with('/')
        || path.starts_with("\\\\")
        || path.starts_with("//")
        || (bytes.len() >= 3
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

fn join_raw(base: &str, child: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches(['/', '\\']),
        child.trim_matches(['/', '\\']).replace('\\', "/")
    )
}

/// Resolve the directory that contains a Mesh's Worktree Nodes.
///
/// `configured` is already selected by the caller using the precedence
/// `mesh override -> application default`. Relative values are rooted at the
/// Mesh; absolute values must stay in the Mesh's native/WSL environment so a
/// directory preference cannot silently switch which harness runtime starts.
pub fn resolve_worktree_directory(
    mesh_path: &str,
    configured: Option<&str>,
) -> Result<String, String> {
    let root = match normalize_worktree_directory(configured) {
        Some(value) if is_absolute_in_any_supported_environment(&value) => value,
        Some(value) => join_raw(mesh_path, &value),
        None => join_raw(mesh_path, ".claude/worktrees"),
    };

    let mesh_env = EnvType::from(env_for_path(Path::new(mesh_path)));
    let root_env = EnvType::from(env_for_path(Path::new(&root)));
    if mesh_env != root_env {
        return Err(format!(
            "worktree directory '{}' is in {:?}, but mesh '{}' is in {:?}; choose a path in the same environment",
            root, root_env, mesh_path, mesh_env
        ));
    }
    Ok(root)
}

/// Resolve a Mesh's effective Worktree Node directory using the shared
/// precedence rule: per-Mesh override, then Buildmesh-wide default, then the
/// legacy `.claude/worktrees` layout. Keeping this selection beside the path
/// resolver prevents spawn, discovery, and warm-pool callers from drifting.
pub fn effective_worktree_directory(
    mesh_path: &str,
    mesh_override: Option<&str>,
    app_default: Option<&str>,
) -> Result<String, String> {
    let configured = normalize_worktree_directory(mesh_override)
        .or_else(|| normalize_worktree_directory(app_default));
    resolve_worktree_directory(mesh_path, configured.as_deref())
}

/// Resolve the exact raw path for one Worktree Node beneath the effective
/// directory. The worktree name is a validated slug at this seam.
pub fn resolve_worktree_path(
    mesh_path: &str,
    configured: Option<&str>,
    worktree_name: &str,
) -> Result<String, String> {
    let worktree_name = worktree_name.trim();
    if worktree_name.is_empty()
        || worktree_name == "."
        || worktree_name == ".."
        || worktree_name.contains('/')
        || worktree_name.contains('\\')
    {
        return Err(format!(
            "invalid Worktree Node name '{}': expected a single directory name",
            worktree_name
        ));
    }
    Ok(join_raw(
        &resolve_worktree_directory(mesh_path, configured)?,
        worktree_name,
    ))
}

/// Resolve a discovered/imported Worktree Node path while preserving an exact
/// transcript cwd when it is one of the paths Buildmesh permits. A historical
/// session may still live in the legacy `.claude/worktrees` directory after a
/// user changes the configured parent, so both the effective and legacy path
/// for the validated name are accepted. Arbitrary cwd values are rejected.
pub fn resolve_imported_worktree_path(
    mesh_path: &str,
    effective_directory: &str,
    worktree_name: &str,
    cwd: Option<&str>,
) -> Result<String, String> {
    let configured_raw = resolve_worktree_path(mesh_path, Some(effective_directory), worktree_name)?;
    let legacy_raw = resolve_worktree_path(mesh_path, None, worktree_name)?;
    let chosen = cwd
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| configured_raw.clone());
    if !paths_equivalent(&chosen, &configured_raw) && !paths_equivalent(&chosen, &legacy_raw) {
        return Err(format!(
            "discovered Worktree Node cwd '{}' does not match the configured or legacy path for '{}'",
            chosen, worktree_name
        ));
    }
    Ok(chosen)
}

fn paths_equivalent(left: &str, right: &str) -> bool {
    let left_forms = normalized_path_forms(left);
    let right_forms = normalized_path_forms(right);
    left_forms.iter().any(|left| {
        right_forms
            .iter()
            .any(|right| directories_match(left, right))
    })
}

fn normalized_path_forms(path: &str) -> Vec<String> {
    let unc = normalize_unc_to_wsl(path);
    let mut forms = vec![normalize_path_lexically(path)];
    let unc_form = normalize_path_lexically(&unc);
    if unc_form != forms[0] {
        forms.push(unc_form);
    }
    forms
}

/// Return true when `child` is equal to or below `parent` using the same
/// separator/case rules as [`directories_match`]. Both paths are normalized
/// lexically so this works for paths that do not exist yet.
pub fn path_is_within_directory(parent: &str, child: &str) -> bool {
    let parent = normalize_path_lexically(parent).trim_end_matches('/').to_string();
    let child = normalize_path_lexically(child).trim_end_matches('/').to_string();
    if directories_match(&parent, &child) {
        return true;
    }
    let parent_cmp = if case_insensitive_fs(&parent) {
        parent.to_lowercase()
    } else {
        parent.clone()
    };
    let child_cmp = if case_insensitive_fs(&child) {
        child.to_lowercase()
    } else {
        child
    };
    child_cmp.starts_with(&format!("{parent_cmp}/"))
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

    resolve_path(&raw_path)
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
    match worktree_segment(node) {
        Some(name) => node
            .worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(resolve_path)
            .unwrap_or_else(|| resolve_agent_path(&node.path, Some(name))),
        None => resolve_agent_path(&node.path, None),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commandcode_projects_dir_resolves_base_without_slug() {
        // The base carries no slug; slug formatting lives in transcript_reader
        // (issue #1500 review). The composer test lives there too.
        let dir = commandcode_projects_dir(
            EnvType::Windows,
            r"F:\src\buildmesh\.claude\worktrees\saucy-thunderous-cove",
        )
        .expect("windows projects dir should resolve");
        let dir_str = dir.to_string_lossy().replace('\\', "/");
        assert!(
            dir_str.ends_with(".commandcode/projects"),
            "projects base should end with .commandcode/projects, got {dir_str}"
        );
        assert!(
            !dir_str.contains("saucy"),
            "base must not contain a slug, got {dir_str}"
        );
    }

    #[test]
    fn normalize_unc_to_wsl_maps_host_unc_into_spawn_form() {
        // Issue #1500 review: discovery may hold a Windows host UNC path for a
        // WSL mesh; the CLI slugged the in-WSL cwd, so normalize first.
        assert_eq!(
            normalize_unc_to_wsl(r"\\wsl$\Ubuntu\home\user\repo"),
            "/home/user/repo"
        );
        assert_eq!(
            normalize_unc_to_wsl("//wsl$/Ubuntu/home/user/repo"),
            "/home/user/repo"
        );
        assert_eq!(
            normalize_unc_to_wsl(r"\\WSL$\ubuntu\HOME\user\repo\"),
            "/HOME/user/repo"
        );
        assert_eq!(normalize_unc_to_wsl(r"\\wsl$\Ubuntu"), "/");
        // Non-UNC paths pass through borrowed (no allocation).
        assert!(matches!(
            normalize_unc_to_wsl("/home/user/repo"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert!(matches!(
            normalize_unc_to_wsl(r"F:\src\buildmesh"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(normalize_unc_to_wsl(""), "");
    }

    #[test]
    fn normalize_path_lexically_collapses_dot_components() {
        assert_eq!(
            normalize_path_lexically("/repo/mesh/../shared-worktrees/./node"),
            "/repo/shared-worktrees/node"
        );
        assert_eq!(
            normalize_path_lexically(r"C:\repo\mesh\..\shared-worktrees\node"),
            "C:/repo/shared-worktrees/node"
        );
    }

    #[test]
    fn imported_worktree_path_accepts_current_or_legacy_root_only() {
        let current = resolve_imported_worktree_path(
            "/repo/mesh",
            "/repo/mesh/../shared-worktrees",
            "gentle-fox",
            Some("/repo/shared-worktrees/gentle-fox"),
        )
        .unwrap();
        assert_eq!(current, "/repo/shared-worktrees/gentle-fox");

        let legacy = resolve_imported_worktree_path(
            "/repo/mesh",
            "/repo/shared-worktrees",
            "gentle-fox",
            Some("/repo/mesh/.claude/worktrees/gentle-fox"),
        )
        .unwrap();
        assert_eq!(legacy, "/repo/mesh/.claude/worktrees/gentle-fox");

        let error = resolve_imported_worktree_path(
            "/repo/mesh",
            "/repo/shared-worktrees",
            "gentle-fox",
            Some("/tmp/unrelated/gentle-fox"),
        )
        .expect_err("an imported cwd outside both allowed roots must be rejected");
        assert!(error.contains("does not match"));
    }

    #[test]
    fn imported_worktree_path_matches_wsl_unc_spelling() {
        let path = resolve_imported_worktree_path(
            "/home/user/repo",
            "/home/user/shared-worktrees",
            "gentle-fox",
            Some(r"\\wsl$\Ubuntu\home\user\shared-worktrees\gentle-fox"),
        )
        .unwrap();
        assert_eq!(
            path,
            r"\\wsl$\Ubuntu\home\user\shared-worktrees\gentle-fox"
        );
    }
}
