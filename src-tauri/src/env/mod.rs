//! Environment detection and path handling for Windows + WSL hybrid setup

mod mesh_row;

pub use mesh_row::mesh_row;

use std::path::{Path, PathBuf};
use once_cell::sync::Lazy;
use std::env;

use crate::process_util::command_no_window;

/// The default WSL distro name (e.g., "Ubuntu"), cached after first detection
static DETECTED_DISTRO: Lazy<Option<String>> = Lazy::new(get_default_wsl_distro_impl);

/// The Windows username, cached after first lookup
#[allow(dead_code)]
static WINDOWS_USERNAME: Lazy<Option<String>> = Lazy::new(get_windows_username_impl);

/// Get the default WSL distro name by parsing `wsl.exe -l -v` output.
/// Returns the distro marked as (default) or the first one if none marked.
fn get_default_wsl_distro_impl() -> Option<String> {
    let output = command_no_window("wsl.exe")
        .args(["-l", "-v"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.contains('(') && line.contains("Default") {
            // Line format: "  Ubuntu    Active          2"
            return line.split_whitespace().next().map(|s| s.to_string());
        }
    }
    // No default marked, use first distro
    stdout
        .lines()
        .skip(1) // skip header
        .find(|l| !l.trim().is_empty())
        .and_then(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
}

/// Get the cached default WSL distro name
fn get_default_wsl_distro() -> Option<String> {
    DETECTED_DISTRO.clone()
}

/// Parse the login shell (field 7) out of a `getent passwd <user>` line.
///
/// `getent passwd` formats a line as `name:pw:uid:gid:gecos:home:shell` —
/// colons in any field are not escaped by glibc's NSS, so plain `split(':')`
/// is correct in practice (a GECOS field containing `:` is a malformed
/// entry by spec). Returns `None` for the no-login shells
/// (`/usr/sbin/nologin`, `/bin/false`) and any line with fewer than 7
/// fields, so the cached lookup can fall through to a plain `sh` default
/// rather than launching a shell that exits immediately.
fn parse_login_shell_from_passwd(line: &str) -> Option<String> {
    let shell = line.split(':').nth(6)?.trim();
    if shell.is_empty() || shell == "/usr/sbin/nologin" || shell == "/bin/false" {
        return None;
    }
    Some(shell.to_string())
}

/// Resolve the WSL user's login shell by running `getent passwd $(whoami)`
/// inside the default distro. Returns `None` if WSL is unavailable, the
/// passwd entry can't be read, or the entry points at a no-login shell —
/// the caller is expected to fall back to a POSIX-`sh` default in that case.
///
/// The returned `&'static str` is leaked from a one-shot `String`; the leak
/// happens at most once per Buildmesh session (the result is cached in
/// [`DETECTED_WSL_LOGIN_SHELL`]). The same one-shot leak pattern is used
/// for the tracing `_guard` in `lib.rs`.
fn get_default_wsl_login_shell_impl() -> Option<&'static str> {
    let distro = get_default_wsl_distro().unwrap_or_else(|| "Ubuntu".to_string());
    let output = command_no_window("wsl.exe")
        .args(["-d", &distro, "--", "sh", "-c", "getent passwd $(whoami)"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?;
    parse_login_shell_from_passwd(line).map(|s| Box::leak(s.into_boxed_str()) as &'static str)
}

/// The user's WSL login shell (e.g. `/usr/bin/zsh`), cached after first
/// detection. `None` when WSL isn't available or the login shell isn't
/// usable as an interactive terminal.
static DETECTED_WSL_LOGIN_SHELL: Lazy<Option<&'static str>> =
    Lazy::new(get_default_wsl_login_shell_impl);

/// Get the cached WSL login shell, if any. `SpawnRecipe::binary` needs
/// `&'static str`, so the cached value is leaked once at first detection
/// (see [`get_default_wsl_login_shell_impl`]).
pub fn wsl_login_shell() -> Option<&'static str> {
    *DETECTED_WSL_LOGIN_SHELL
}

/// Get the Windows username (used for path construction)
#[allow(dead_code)]
fn get_windows_username_impl() -> Option<String> {
    env::var("USERNAME").ok()
}

/// Get the cached Windows username
#[allow(dead_code)]
fn get_windows_username() -> Option<String> {
    WINDOWS_USERNAME.clone()
}

/// The detected runtime environment for this process
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// Running on native Windows (Git Bash/MSYS2)
    Windows,
    /// Running inside WSL (Windows Subsystem for Linux)
    Wsl,
}

impl Environment {
    /// Detect the current environment by checking for WSL signature
    pub fn detect() -> Self {
        if cfg!(target_os = "windows") {
            // On Windows, check if /proc/version contains "microsoft" (WSL signature)
            if let Ok(versions) = std::fs::read_to_string("/proc/version") {
                if versions.to_lowercase().contains("microsoft") {
                    return Environment::Wsl;
                }
            }
            // Check via wsl.exe detection
            if let Ok(output) = command_no_window("wsl.exe")
                .args(["--detect-nested"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim() == "1" {
                    return Environment::Wsl;
                }
            }
            Environment::Windows
        } else {
            // Non-Windows (Linux/WSL)
            if let Ok(versions) = std::fs::read_to_string("/proc/version") {
                if versions.to_lowercase().contains("microsoft") {
                    Environment::Wsl
                } else {
                    Environment::Windows // treat native Linux as "Windows" for our purposes
                }
            } else {
                Environment::Windows
            }
        }
    }

    /// Returns true if we're running inside WSL
    pub fn is_wsl(&self) -> bool {
        matches!(self, Environment::Wsl)
    }
}

static CURRENT_ENV: Lazy<Environment> = Lazy::new(Environment::detect);

/// Get the current environment (cached)
pub fn current_env() -> Environment {
    *CURRENT_ENV
}

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

/// Get the path to the cc wrapper script
#[allow(dead_code)]
pub fn cc_path() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("/mnt/c/Users/alond/.local/bin/cc"),
        Environment::Windows => {
            // Try to find cc in PATH
            if let Ok(output) = command_no_window("where")
                .arg("cc")
                .output()
            {
                let path_str = String::from_utf8_lossy(&output.stdout);
                let path = path_str.trim().lines().next().unwrap_or("");
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
            PathBuf::from("C:/Users/alond/.local/bin/cc")
        }
    }
}

/// Get the Git binary path for the correct environment
#[allow(dead_code)]
pub fn git_path() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("git"),
        Environment::Windows => {
            if let Ok(output) = command_no_window("where")
                .arg("git")
                .output()
            {
                let path_str = String::from_utf8_lossy(&output.stdout);
                let path = path_str.trim().lines().next().unwrap_or("");
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
            PathBuf::from("git")
        }
    }
}

/// Determine the environment for a given session path
pub fn env_for_path(path: &Path) -> Environment {
    if cfg!(target_os = "macos") {
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
    if path.starts_with('/') {
        if path.starts_with("/mnt/") {
            // /mnt/c/Users -> C:\Users
            let drive = path.chars().nth(5).unwrap_or('c').to_uppercase().next().unwrap();
            format!("{}:{}", drive, path[6..].replace('/', "\\"))
        } else if path.starts_with("/home/") {
            // /home/user -> \\wsl$\Ubuntu\home\user
            let distro = get_default_wsl_distro().unwrap_or_else(|| "Ubuntu".to_string());
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

// ---------------------------------------------------------------------------
// ResolvedPath — high-level path resolution for agent operations
// ---------------------------------------------------------------------------

use crate::models::{AgentNode, EnvType, SessionStatus};

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

    // On macOS, paths are always native — no WSL conversion needed.
    // On Windows, convert based on detected environment.
    let (host_path, spawn_path) = if cfg!(target_os = "macos") {
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
///    calling this. `delete_branches` does this; `get_git_prune_info` does
///    NOT (it uses `db::list_agent_nodes`) — that asymmetry is intentional
///    because path collisions don't happen for branches the user is
///    actively viewing in their currently-selected mesh.
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

/// Get the .claude directory for session storage in the correct environment
pub fn claude_dir() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("/mnt/c/Users/alond/.claude"),
        Environment::Windows => {
            if let Ok(home) = env::var("USERPROFILE") {
                PathBuf::from(home).join(".claude")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".claude")
            } else {
                PathBuf::from("C:/Users/alond/.claude")
            }
        }
    }
}


/// Shared test fixtures used by both `mod tests` (worktree / base_ref
/// regression suites) and `fetch_origin_tests` (issue #213). Lifted
/// out of `mod tests` so the sibling fetch_origin module can reach
/// them — `mod tests` items are private to that scope. Kept inside
/// env/mod.rs rather than a standalone file so the helpers stay
/// co-located with the production code they exercise.
#[cfg(test)]
pub(crate) mod test_helpers {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

    /// Per-test scratch directory under %TEMP%, named uniquely so parallel
    /// cargo test invocations don't collide. Removed on drop.
    pub(crate) struct TestDir(PathBuf);
    impl TestDir {
        pub(crate) fn new(suffix: &str) -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "buildmesh_wt_test_{}_{}_{}",
                suffix,
                std::process::id(),
                id
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Init a repo with one initial commit containing the given files.
    /// `path` is expected to exist (callers pass a `TestDir`).
    pub(crate) fn init_repo_with_commit(
        path: &Path,
        files: &[(&str, &str)],
    ) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();

        let mut index = repo.index().unwrap();
        for (name, content) in files {
            let full = path.join(name);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(&full, content).unwrap();
            index.add_path(Path::new(name)).unwrap();
        }
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        {
            // Scope the Tree borrow so it's dropped before we return `repo`.
            let tree = repo.find_tree(tree_oid).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    /// Returns true if the repo at `path` has any non-ignored working-tree
    /// changes. Used as a precondition assertion by the dirty-parent tests
    /// so a silent failure to dirty the repo doesn't make the test pass
    /// for the wrong reason.
    pub(crate) fn repo_is_dirty(path: &Path) -> bool {
        let repo = git2::Repository::open(path).unwrap();
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let statuses = repo.statuses(Some(&mut opts)).unwrap();
        statuses.iter().any(|entry| !entry.status().is_ignored())
    }

    /// Add a commit on top of current HEAD with the given file content,
    /// advancing HEAD. Returns the new commit oid.
    pub(crate) fn commit_file(
        repo: &git2::Repository,
        root: &Path,
        name: &str,
        content: &str,
    ) -> git2::Oid {
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        fs::write(root.join(name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "more", &tree, &[&parent])
            .unwrap()
    }

    /// Set up a repo where `origin/main` (a remote-tracking ref) points at the
    /// initial commit, then drift the local HEAD forward to a second commit.
    /// Returns (repo, origin_main_oid).
    pub(crate) fn repo_with_drifted_head(root: &Path) -> (git2::Repository, git2::Oid) {
        let repo = init_repo_with_commit(root, &[("f.txt", "from-origin-main\n")]);
        let origin_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference("refs/remotes/origin/main", origin_oid, false, "test")
            .unwrap();
        commit_file(&repo, root, "f.txt", "local-drift\n");
        assert_ne!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            origin_oid,
            "precondition: HEAD must differ from origin/main"
        );
        (repo, origin_oid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: when worktree_name is None, resolve_agent_path returns base_path directly
    /// (i.e., no .claude/worktrees/ subdirectory)
    #[test]
    fn resolve_agent_path_with_none_worktree_returns_base_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should NOT contain worktrees subdirectory
        assert!(!resolved.host_path.contains("worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.host_path);
        assert!(!resolved.spawn_path.contains("worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.spawn_path);
    }

    /// Test: when worktree_name is Some("foo"), resolve_agent_path returns
    /// {base}/.claude/worktrees/foo
    #[test]
    fn resolve_agent_path_with_some_worktree_returns_worktree_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, Some("foo"));

        // Path should contain worktrees subdirectory and the specific worktree name
        assert!(resolved.host_path.contains("worktrees") && resolved.host_path.contains("foo"),
            "Expected worktree subdir, got: {}", resolved.host_path);
        assert!(resolved.spawn_path.contains("worktrees") && resolved.spawn_path.contains("foo"),
            "Expected worktree subdir, got: {}", resolved.spawn_path);
    }

    /// Test: when worktree_name is Some(""), it's treated as no worktree
    #[test]
    fn resolve_agent_path_with_empty_worktree_returns_base_path() {
        let base = "/home/user/my-repo";
        let resolved = resolve_agent_path(base, Some(""));

        assert!(!resolved.host_path.contains(".claude/worktrees"),
            "Expected base path without worktree subdir, got: {}", resolved.host_path);
    }

    /// Test: resolve_agent_path works with Windows paths too
    #[test]
    fn resolve_agent_path_with_windows_path() {
        let base = "C:\\Users\\user\\my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should return a valid path without crashing
        assert!(!resolved.host_path.is_empty());
        assert!(!resolved.spawn_path.is_empty());
    }

    /// Test: resolve_agent_path works with WSL paths
    #[test]
    fn resolve_agent_path_with_wsl_path() {
        let base = "/mnt/c/Users/user/my-repo";
        let resolved = resolve_agent_path(base, None);

        // Should return a valid path without crashing
        assert!(!resolved.host_path.is_empty());
        assert!(!resolved.spawn_path.is_empty());
    }

    /// Minimal Agent Node fixture; `use_worktree`/`worktree_name` are the only
    /// fields the Node Working Directory rule reads. `..Default::default()`
    /// covers the rest so future optional columns don't reopen this fixture
    /// (issue #457).
    fn node(use_worktree: bool, worktree_name: Option<&str>) -> AgentNode {
        AgentNode {
            path: "/home/user/my-repo".to_string(),
            worktree_name: worktree_name.map(str::to_string),
            use_worktree,
            ..Default::default()
        }
    }

    /// A Worktree Node resolves into its `.claude/worktrees/<name>` dir.
    #[test]
    fn node_working_path_for_worktree_node_resolves_worktree_dir() {
        let resolved = node_working_path(&node(true, Some("gentle-fox")));
        assert!(
            resolved.host_path.contains("worktrees") && resolved.host_path.contains("gentle-fox"),
            "expected worktree dir, got: {}",
            resolved.host_path
        );
    }

    /// A Root Node resolves to the Mesh root — never a worktree subdir.
    #[test]
    fn node_working_path_for_root_node_resolves_mesh_root() {
        let resolved = node_working_path(&node(false, Some("ignored")));
        assert!(
            !resolved.host_path.contains("worktrees"),
            "root node must not resolve into a worktree, got: {}",
            resolved.host_path
        );
    }

    /// The canonical rule trims the worktree name. This is the behaviour
    /// `pr::node_working_path` lacked (it fed an untrimmed name straight to
    /// `resolve_agent_path`), so a name with stray whitespace resolved to a
    /// different directory than diff/close-safety used. One resolver, one rule.
    #[test]
    fn node_working_path_trims_worktree_name() {
        let trimmed = node_working_path(&node(true, Some("foo")));
        let padded = node_working_path(&node(true, Some("  foo  ")));
        assert_eq!(padded.host_path, trimmed.host_path);
    }

    /// A whitespace-only worktree name collapses to "no worktree".
    #[test]
    fn node_working_path_blank_worktree_name_is_root() {
        let resolved = node_working_path(&node(true, Some("   ")));
        assert!(!resolved.host_path.contains("worktrees"));
    }

    /// `node_worktree_path` is `Some` only for a Worktree Node; the `None` for a
    /// Root Node is what close-safety and removal lean on to skip root nodes.
    #[test]
    fn node_worktree_path_is_some_for_worktree_none_for_root() {
        assert!(node_worktree_path(&node(true, Some("gentle-fox"))).is_some());
        assert!(node_worktree_path(&node(false, Some("gentle-fox"))).is_none());
        assert!(node_worktree_path(&node(true, None)).is_none());
        assert!(node_worktree_path(&node(true, Some("   "))).is_none());
    }

    /// When present, the worktree path agrees with the working path (it's the
    /// same dir — `node_worktree_path` is just the `Option` view of it).
    #[test]
    fn node_worktree_path_agrees_with_working_path() {
        let n = node(true, Some("gentle-fox"));
        assert_eq!(
            node_worktree_path(&n).map(|r| r.host_path),
            Some(node_working_path(&n).host_path)
        );
    }

    // ----- active_node_paths (#607 / #621) -----
    //
    // `n.path` alone is the mesh root. A Worktree Node's work lives at
    // `<mesh>/.claude/worktrees/<name>` — that subdir must also enter the
    // active set, or `path_is_active` matches every linked worktree against
    // the mesh root alone and flags them all `is_active: false` in both
    // the Worktree Manager (#607) and Mesh Health (#621). Delegating to
    // `node_worktree_path` keeps the one-rule invariant intact.

    /// Regression for #607 / #621: a Worktree Node must contribute BOTH its
    /// mesh path AND its resolved worktree dir, so the linked worktree on
    /// disk matches against the active set instead of being flagged
    /// inactive/stale.
    #[test]
    fn active_node_paths_includes_resolved_worktree_dir_for_worktree_nodes() {
        let paths = active_node_paths(&[node(true, Some("gentle-fox"))]);

        assert!(
            paths.iter().any(|p| p == "/home/user/my-repo"),
            "mesh path must be present so the main worktree still matches: {:?}",
            paths
        );
        assert!(
            paths.iter().any(|p| p.contains("gentle-fox")),
            "Worktree Node must contribute its resolved worktree dir (#607 / #621): {:?}",
            paths
        );
    }

    /// A Root Node has no worktree dir to add — only its mesh path participates.
    #[test]
    fn active_node_paths_root_node_contributes_only_mesh_path() {
        let paths = active_node_paths(&[node(false, None)]);

        assert_eq!(
            paths.len(),
            1,
            "root node contributes exactly one path: {:?}",
            paths
        );
        assert_eq!(paths[0], "/home/user/my-repo");
    }

    /// A whitespace-only `worktree_name` collapses to "no worktree" per the
    /// canonical rule in `node_worktree_path`, so it contributes only the
    /// mesh path — same as a Root Node.
    #[test]
    fn active_node_paths_blank_worktree_name_contributes_only_mesh_path() {
        let paths = active_node_paths(&[node(true, Some("   "))]);

        assert_eq!(
            paths.len(),
            1,
            "blank worktree name is treated as no worktree: {:?}",
            paths
        );
        assert_eq!(paths[0], "/home/user/my-repo");
    }

    // ----- raw_path contract (issue #409) -----
    //
    // `raw_path` is the POSIX-style effective path (input to `to_host_path` /
    // `to_spawn_path`) and the string the GIT_CHANGED payload carries to the
    // frontend for `getNodeGitPath()` to subscribe on. These assertions pin
    // that `env::node_working_path` is now the SOLE Rust definition of the
    // worktree rule — `file_watcher::node_internal_path` was deleted and
    // consumes `raw_path` instead. If any case here drifts, the GIT_CHANGED
    // match contract breaks and changed-files go stale (issue #387).

    /// Worktree Node: raw_path is `<base>/.claude/worktrees/<trimmed_name>`.
    #[test]
    fn raw_path_for_worktree_node_is_worktree_subdir() {
        assert_eq!(
            node_working_path(&node(true, Some("gentle-fox"))).raw_path,
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// Root Node: raw_path is the Mesh root regardless of a stale
    /// `worktree_name`. Regression: without the `use_worktree` gate, a Root
    /// Node with a stale `worktree_name` emitted a worktree subdir the
    /// frontend never subscribed to (issue #383).
    #[test]
    fn raw_path_for_root_node_ignores_stale_worktree_name() {
        assert_eq!(
            node_working_path(&node(false, Some("gentle-fox"))).raw_path,
            "/home/user/my-repo"
        );
    }

    /// No worktree name → Mesh root.
    #[test]
    fn raw_path_without_worktree_name_is_mesh_root() {
        assert_eq!(
            node_working_path(&node(true, None)).raw_path,
            "/home/user/my-repo"
        );
    }

    /// Padded `worktree_name` is trimmed (parity with the frontend's
    /// `getNodeGitPath()` in `src/lib/paths.ts`, issue #387).
    #[test]
    fn raw_path_for_padded_worktree_name_is_trimmed() {
        assert_eq!(
            node_working_path(&node(true, Some("  gentle-fox  "))).raw_path,
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// Whitespace-only worktree name trims to empty → Mesh root.
    #[test]
    fn raw_path_for_whitespace_only_worktree_name_is_mesh_root() {
        assert_eq!(
            node_working_path(&node(true, Some("   "))).raw_path,
            "/home/user/my-repo"
        );
    }

    /// `raw_path` is the input to `to_host_path` / `to_spawn_path` — the
    /// "pre-transform" form. The raw/host/spawn triple is internally
    /// consistent in that `raw_path` does NOT go through `to_host_path`; a
    /// regression that routed `raw_path` through that conversion would
    /// produce a UNC-shaped string on Windows and break the GIT_CHANGED
    /// match contract (which depends on `raw_path` matching
    /// `getNodeGitPath()` in the TS layer). This single value assertion
    /// pins that contract for the standard Worktree Node fixture: if
    /// `raw_path` ever diverges from `{base}/.claude/worktrees/{trimmed}`,
    /// the regression is caught here.
    #[test]
    fn raw_path_is_effective_path_not_host_or_spawn_form() {
        let n = node(true, Some("gentle-fox"));
        let resolved = node_working_path(&n);
        assert_eq!(resolved.raw_path, "/home/user/my-repo/.claude/worktrees/gentle-fox");
    }

    // ----- wsl_login_shell helper (issue #548) -----
    //
    // `parse_login_shell_from_passwd` is the pure function behind
    // `wsl_login_shell()`. The impure wrapper runs `wsl.exe` once per session
    // (cached via `Lazy`); on a Linux CI host it always returns `None`
    // because `wsl.exe` doesn't exist. These tests pin the parsing rules so
    // a regression in the helper can't silently hand the Terminal adapter
    // `/usr/sbin/nologin` and crash the spawn.

    /// Real-world `getent passwd` line for an ohmyzsh user. Field 7 is the
    /// absolute path to the login shell; everything before it must be ignored.
    #[test]
    fn parse_login_shell_extracts_field_7_with_gecos() {
        assert_eq!(
            parse_login_shell_from_passwd("alice:x:1000:1000:Alice Smith:/home/alice:/usr/bin/zsh"),
            Some("/usr/bin/zsh".to_string())
        );
    }

    /// `getent passwd` line without a GECOS field (the 5th field is empty).
    /// Some distros / NSS backends omit GECOS for system accounts; the parser
    /// must not require it.
    #[test]
    fn parse_login_shell_extracts_field_7_without_gecos() {
        assert_eq!(
            parse_login_shell_from_passwd("user:x:1000:1000::/home/user:/bin/bash"),
            Some("/bin/bash".to_string())
        );
    }

    /// Trailing newline (and any other trailing whitespace) must be trimmed.
    #[test]
    fn parse_login_shell_trims_trailing_whitespace() {
        assert_eq!(
            parse_login_shell_from_passwd("user:x:1000:1000::/home/user:/usr/bin/fish\n"),
            Some("/usr/bin/fish".to_string())
        );
    }

    /// Service accounts whose login shell is `/usr/sbin/nologin` must
    /// collapse to `None` — spawning that would exit immediately.
    #[test]
    fn parse_login_shell_rejects_nologin() {
        assert_eq!(
            parse_login_shell_from_passwd("ftp:x:114:120:ftp daemon:/srv/ftp:/usr/sbin/nologin"),
            None
        );
    }

    /// `nobody` is conventionally `/bin/false` and must also collapse to `None`.
    #[test]
    fn parse_login_shell_rejects_false() {
        assert_eq!(
            parse_login_shell_from_passwd("nobody:x:65534:65534::/:/bin/false"),
            None
        );
    }

    /// An empty 7th field (a malformed passwd entry) must collapse to `None`,
    /// not the empty string.
    #[test]
    fn parse_login_shell_rejects_empty_shell() {
        assert_eq!(
            parse_login_shell_from_passwd("user:x:1000:1000::/home/user:"),
            None
        );
    }

    /// Lines with fewer than 7 fields are malformed — return `None` rather
    /// than panic on the missing `nth(6)`.
    #[test]
    fn parse_login_shell_rejects_too_few_fields() {
        assert_eq!(parse_login_shell_from_passwd(""), None);
        assert_eq!(parse_login_shell_from_passwd("user"), None);
        assert_eq!(parse_login_shell_from_passwd("user:x:1000:1000::/home/user"), None);
    }

    /// The cached lookup must be safe to call and must return an `Option<&'static str>`
    /// (not panic) — on a host where WSL is unavailable it is `None`, on a
    /// Windows+WSL host it is `Some("/usr/bin/zsh")`. We only assert the type
    /// and that it doesn't panic; behavioural pinning lives in the
    /// `parse_login_shell_from_passwd` tests above.
    #[test]
    fn wsl_login_shell_returns_option_without_panicking() {
        let _ = wsl_login_shell();
    }
}
