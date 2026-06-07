//! Environment detection and path handling for Windows + WSL hybrid setup

mod mesh_config;

pub use mesh_config::read_mesh_config;

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

use crate::models::EnvType;

/// A fully-resolved set of paths for an agent node, ready for use by callers
/// without needing to compose env detection + host conversion + worktree logic.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    /// Host-accessible path for file system operations (e.g. Windows UNC path for WSL,
    /// or native path on macOS/Windows).
    pub host_path: String,
    /// Path to use as CWD when spawning agent/shell processes.
    pub spawn_path: String,
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
    let effective_path = match worktree_name {
        Some(wt_name) if !wt_name.is_empty() => {
            format!("{}/.claude/worktrees/{}", base_path, wt_name)
        }
        _ => base_path.to_string(),
    };

    // Detect environment from the effective path
    let path_buf = PathBuf::from(&effective_path);
    let env_internal = env_for_path(&path_buf);
    let env_type = EnvType::from(env_internal);

    // On macOS, paths are always native — no WSL conversion needed.
    // On Windows, convert based on detected environment.
    let (host_path, spawn_path) = if cfg!(target_os = "macos") {
        (effective_path.clone(), effective_path)
    } else {
        let host = to_host_path(&effective_path);
        let spawn = to_spawn_path(&path_buf).to_string_lossy().to_string();
        (host, spawn)
    };

    ResolvedPath {
        host_path,
        spawn_path,
        env_type,
    }
}

/// Sanitize the .git file in a worktree to ensure it uses the correct path format
/// for the target environment (Windows vs WSL).
pub fn sanitize_git_worktree(worktree_host_path: &str, env_type: EnvType) -> Result<(), String> {
    let git_file_path = std::path::Path::new(worktree_host_path).join(".git");
    
    if !git_file_path.is_file() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&git_file_path)
        .map_err(|e| format!("Failed to read .git file: {}", e))?;

    if !content.starts_with("gitdir: ") {
        return Ok(());
    }

    let git_dir_path = content.trim_start_matches("gitdir: ").trim();
    if git_dir_path.is_empty() {
        return Err("invalid .git file: empty gitdir path".to_string());
    }

    // Convert the path to the target environment's format
    let new_path = match env_type {
        EnvType::Wsl => {
            // Ensure it's a WSL-friendly path
            if git_dir_path.contains(':') || git_dir_path.starts_with("\\\\") {
                // Convert Windows path to WSL (/mnt/c/...)
                let path_str = git_dir_path.replace('\\', "/");
                if let Some(pos) = path_str.find(':') {
                    let drive = path_str[..pos].to_lowercase();
                    format!("/mnt/{}{}", drive, &path_str[pos + 1..])
                } else {
                    path_str
                }
            } else {
                git_dir_path.to_string()
            }
        }
        EnvType::Windows => {
            // Target is Windows. Use to_host_path which now handles /mnt/, /home/, and /c/ styles.
            to_host_path(git_dir_path)
        }
    };

    if new_path != git_dir_path {
        tracing::info!("Sanitizing .git file: {} -> {}", git_dir_path, new_path);
        // Ensure we use Unix line endings for the .git file as Git expects
        std::fs::write(&git_file_path, format!("gitdir: {}\n", new_path))
            .map_err(|e| format!("Failed to write sanitized .git file: {}", e))?;
    }

    Ok(())
}

/// Resolve the base commit a new worktree should be created from.
///
/// `"HEAD"` (or empty) uses the Mesh root's current HEAD — the explicit
/// "Head" option. Any other ref (e.g. `"origin/main"`) is resolved via
/// revparse; callers must run [`fetch_origin`] first so remote-tracking refs
/// are current. If the ref cannot be resolved (offline, missing), this falls
/// back to HEAD with a warning rather than failing the spawn, consistent with
/// ADR 0001's offline-fallback intent.
fn resolve_base_commit<'r>(
    repo: &'r git2::Repository,
    base_ref: &str,
) -> Result<git2::Commit<'r>, String> {
    let head_commit = || {
        repo.head()
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| format!("Failed to resolve HEAD commit: {}", e))
    };

    if base_ref.is_empty() || base_ref == "HEAD" {
        return head_commit();
    }

    match repo
        .revparse_single(base_ref)
        .and_then(|obj| obj.peel_to_commit())
    {
        Ok(commit) => Ok(commit),
        Err(e) => {
            tracing::warn!(
                "resolve_base_commit: could not resolve base_ref '{}' ({}); falling back to HEAD",
                base_ref,
                e
            );
            head_commit()
        }
    }
}

/// Add a worktree to the repo — branched (new branch) or detached — based on
/// the commit resolved from `base_ref` (see [`resolve_base_commit`]).
fn add_worktree_impl(
    repo: &git2::Repository,
    branch_name: &str,
    host_path: &std::path::Path,
    use_branched: bool,
    base_ref: &str,
) -> Result<(), String> {
    let base_commit = resolve_base_commit(repo, base_ref)?;

    // Both modes create a branch at the resolved base so the worktree checks
    // out that commit (not the Mesh root's current HEAD). Detached mode then
    // detaches the worktree's HEAD and removes the temporary branch.
    let branch = repo
        .branch(branch_name, &base_commit, false)
        .map_err(|e| format!("Failed to create branch '{}': {}", branch_name, e))?;
    let reference = branch.into_reference();
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    repo.worktree(branch_name, host_path, Some(&opts))
        .map_err(|e| format!("Git worktree add failed: {}", e))?;

    if !use_branched {
        let wt_repo = git2::Repository::open(host_path)
            .map_err(|e| format!("Failed to open worktree repo: {}", e))?;
        wt_repo
            .set_head_detached(base_commit.id())
            .map_err(|e| format!("Failed to detach HEAD in worktree: {}", e))?;

        if let Ok(mut branch) = repo.find_branch(branch_name, git2::BranchType::Local) {
            let _ = branch.delete();
        }
    }
    Ok(())
}

/// Remove worktrees whose on-disk path no longer exists.
fn prune_stale_worktrees(repo: &git2::Repository) {
    if let Ok(worktree_names) = repo.worktrees() {
        for i in 0..worktree_names.len() {
            if let Some(name) = worktree_names.get(i) {
                if let Ok(wt) = repo.find_worktree(name) {
                    if wt.validate().is_err() {
                        let mut prune_opts = git2::WorktreePruneOptions::new();
                        prune_opts.valid(false);
                        let _ = wt.prune(Some(&mut prune_opts));
                    }
                }
            }
        }
    }
}

/// Outcome of a single `fetch_origin` invocation. The variants let the
/// caller (spawn.rs) decide whether to surface a warning toast without
/// having to reparse strings — every non-`Skipped*` non-`UpToDate` outcome
/// is something the user might want to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Parent repo has uncommitted changes; the pull is skipped per
    /// issue #213's "skip if dirty" criterion. This is silent from the
    /// user's perspective — the user clearly knows the working tree is
    /// dirty, and we don't want to nag them on every spawn.
    SkippedDirty,
    /// No `origin` remote is configured. A purely local Mesh is a
    /// valid setup; we don't surface a toast for that.
    SkippedNoRemote,
    /// Fetched and there were no new commits to pull — already up to
    /// date.
    UpToDate,
    /// Fetched and fast-forwarded `new_commits` commits onto the
    /// current branch. The Agent Node will start from the latest
    /// upstream HEAD.
    Synced { new_commits: u32 },
    /// Fetched `new_commits` commits but the fast-forward was rejected
    /// (local history has diverged from the remote). The Agent Node
    /// still spawns — from the local HEAD — but the user should be
    /// told the auto-sync was partial, so this is the case that
    /// surfaces a warning toast.
    FetchedButDiverged { new_commits: u32, reason: String },
}

/// Failure modes for `fetch_origin` that the caller should treat as
/// "auto-sync unavailable, proceed with local HEAD anyway". The
/// variants carry enough context to compose a user-readable toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The path exists but isn't a git repository (or can't be
    /// opened as one). Unusual — a Mesh without a repo is a
    /// misconfiguration — but we still proceed to spawn since the
    /// agent's behaviour with a non-repo path is a separate concern.
    RepoUnusable(String),
    /// `git fetch` itself failed (most commonly: no network, DNS
    /// failure, or `origin` was removed between the no-remote check
    /// and the fetch). Carries stderr for the toast.
    FetchFailed(String),
}

/// Fetch from `origin` and fast-forward the parent **Mesh**'s current
/// branch onto the remote tip.
///
/// Behaviour (issue #213):
/// 1. **Dirty parent → skip silently.** A repo with uncommitted
///    changes returns `Ok(SkippedDirty)`. The user is in the middle
///    of something; we don't want to surface a warning on every spawn.
/// 2. **No `origin` remote → skip silently.** A purely local Mesh is
///    valid; this returns `Ok(SkippedNoRemote)`.
/// 3. **Clean repo with remote → fetch, then `git pull --ff-only`.**
///    `Ok(UpToDate)` if nothing to pull, `Ok(Synced)` if commits were
///    pulled, `Ok(FetchedButDiverged)` if the history has diverged
///    (caller surfaces a warning toast), `Err(FetchFailed)` if the
///    fetch itself failed (caller surfaces a warning toast).
///
/// **This function never blocks the spawn.** The caller (`spawn.rs`)
/// is expected to call it as a best-effort step and continue with
/// local-HEAD fallback on any non-`Ok(Synced|_UpToDate|_Skipped*)`
/// outcome.
pub fn fetch_origin(project_root: &str) -> Result<FetchOutcome, FetchError> {
    let host_root = to_host_path(project_root);

    // Step 1: open the repo. If the path isn't a git repo, we can't
    // decide dirty / has-remote / pull — bail with a typed error.
    let repo = match git2::Repository::open(&host_root) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "fetch_origin: path {} is not a usable git repo ({}); skipping auto-sync",
                host_root,
                e
            );
            return Err(FetchError::RepoUnusable(e.to_string()));
        }
    };

    // Step 2: skip if the working tree is dirty. Untracked files and
    // staged-but-uncommitted changes both count. We deliberately do
    // this BEFORE shelling out — no point spending a network round
    // trip on a sync that we'd refuse to apply. The
    // `unwrap_or(true)` is the safer fallback: if the status call
    // itself fails (corrupt index, permission denied on .git), we
    // treat the repo as dirty and skip rather than try to pull into
    // a state we couldn't even read.
    let mut status_opts = git2::StatusOptions::new();
    status_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true);
    let is_dirty = repo
        .statuses(Some(&mut status_opts))
        .map(|s| s.iter().any(|e| !e.status().is_ignored()))
        .unwrap_or(true);
    if is_dirty {
        tracing::info!(
            "fetch_origin: {} has uncommitted changes; skipping auto-sync",
            host_root
        );
        return Ok(FetchOutcome::SkippedDirty);
    }

    // Step 3: skip if there's no `origin` remote. A Mesh may be a
    // purely local repo (not yet pushed) and that's a valid state.
    let has_origin = repo.find_remote("origin").is_ok();
    if !has_origin {
        tracing::info!(
            "fetch_origin: {} has no 'origin' remote; skipping auto-sync",
            host_root
        );
        return Ok(FetchOutcome::SkippedNoRemote);
    }

    // Step 4: `git fetch origin`. If this fails (network down,
    // auth, remote deleted between the has-remote check and the
    // fetch) we return FetchFailed and the caller surfaces a toast.
    tracing::info!("fetch_origin: running git fetch origin in {}", host_root);
    let fetch_output = match command_no_window("git")
        .args(["fetch", "origin"])
        .current_dir(&host_root)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("fetch_origin: failed to spawn git fetch: {}", e);
            return Err(FetchError::FetchFailed(e.to_string()));
        }
    };
    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        let trimmed = stderr.trim();
        tracing::warn!("fetch_origin: git fetch origin failed: {}", trimmed);
        return Err(FetchError::FetchFailed(trimmed.to_string()));
    }

    // Step 5: count how many commits the local branch is behind the
    // upstream. If 0, we're already in sync. We use git2's
    // `graph_ahead_behind` (not `git rev-list HEAD..@{u}`) to avoid
    // the Windows `@{u}` brace-stripping bug — see the comment in
    // commands/git.rs for the same pattern.
    let new_commits = match count_commits_behind_upstream(&repo) {
        Ok(n) => n,
        Err(e) => {
            // No upstream configured is the most common cause here
            // (e.g. a fresh local-only branch that just gained a
            // remote via `git remote add` but never pushed). Treat
            // it as "nothing to pull" rather than a hard error.
            tracing::info!(
                "fetch_origin: no upstream to compare against ({}); treating as up-to-date",
                e
            );
            return Ok(FetchOutcome::UpToDate);
        }
    };
    if new_commits == 0 {
        return Ok(FetchOutcome::UpToDate);
    }

    // Step 6: `git pull --ff-only --no-rebase`. We pass --no-rebase
    // explicitly because a user's global `pull.rebase=true` would
    // otherwise turn this into a rebase — and rebase on a diverged
    // history produces a merge conflict (write conflict markers to
    // the working tree) instead of the clean "not fast-forwardable"
    // rejection we want. The auto-sync is read-only by policy
    // (issue #213: spawn never blocks, never mutates the local
    // branch on failure), so we must never silently rebase.
    tracing::info!(
        "fetch_origin: running git pull --ff-only --no-rebase ({} new commit{} behind)",
        new_commits,
        if new_commits == 1 { "" } else { "s" }
    );
    let pull_output = match command_no_window("git")
        .args(["pull", "--ff-only", "--no-rebase"])
        .current_dir(&host_root)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Ok(FetchOutcome::FetchedButDiverged {
                new_commits,
                reason: format!("git pull --ff-only failed to start: {}", e),
            });
        }
    };
    if !pull_output.status.success() {
        let stderr = String::from_utf8_lossy(&pull_output.stderr);
        let trimmed = stderr.trim();
        tracing::warn!(
            "fetch_origin: git pull --ff-only rejected: {}",
            trimmed
        );
        return Ok(FetchOutcome::FetchedButDiverged {
            new_commits,
            reason: if trimmed.is_empty() {
                "fast-forward rejected (likely local history diverged)".to_string()
            } else {
                trimmed.to_string()
            },
        });
    }

    Ok(FetchOutcome::Synced { new_commits })
}

/// How many commits the current branch is behind its upstream. Used
/// by `fetch_origin` to decide whether a `git pull --ff-only` would do
/// anything. Mirrors the helper of the same name in
/// `commands::git::count_commits_behind` but takes an already-open
/// `Repository` so we don't pay the cost of re-opening it here.
fn count_commits_behind_upstream(repo: &git2::Repository) -> Result<u32, String> {
    let head_oid = repo
        .head()
        .map_err(|e| format!("Failed to read HEAD: {}", e))?
        .peel_to_commit()
        .map_err(|e| format!("HEAD is not a commit: {}", e))?
        .id();

    let branch_name = repo
        .head()
        .map_err(|e| format!("Failed to read HEAD: {}", e))?
        .shorthand()
        .ok_or_else(|| "HEAD is not on a branch".to_string())?
        .to_string();
    let local_refname = format!("refs/heads/{}", branch_name);
    let upstream_name = repo
        .branch_upstream_name(&local_refname)
        .map_err(|e| format!("no upstream configured for {}: {:?}", local_refname, e))?
        .as_str()
        .ok_or_else(|| "upstream name is not valid UTF-8".to_string())?
        .to_string();
    let upstream_oid = repo
        .find_reference(&upstream_name)
        .map_err(|e| format!("Failed to find upstream ref {}: {}", upstream_name, e))?
        .target()
        .ok_or_else(|| "upstream ref has no target".to_string())?;

    let (_ahead, behind) = repo
        .graph_ahead_behind(head_oid, upstream_oid)
        .map_err(|e| format!("graph_ahead_behind failed: {}", e))?;
    Ok(behind.try_into().unwrap_or(u32::MAX))
}

/// Create a new Git worktree at the specified path and honor .worktreeinclude.
pub fn create_git_worktree(
    project_root: &str,
    worktree_host_path: &str,
    branch_name: &str,
    worktree_mode: &str,
    base_ref: &str,
) -> Result<(), String> {
    let host_path = std::path::Path::new(worktree_host_path);

    if host_path.exists() {
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = host_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create worktrees directory: {}", e))?;
    }

    tracing::info!("Creating git worktree: {} at {} (mode: {}, base_ref: {})", branch_name, worktree_host_path, worktree_mode, base_ref);

    let repo = git2::Repository::open(project_root)
        .map_err(|e| format!("Failed to open repository at {}: {}", project_root, e))?;

    let use_branched = worktree_mode == "branched";

    match add_worktree_impl(&repo, branch_name, host_path, use_branched, base_ref) {
        Ok(()) => {}
        Err(err) if err.contains("already exists") || err.contains("already checked out") => {
            tracing::warn!("Worktree already exists in git metadata, attempting to repair...");
            prune_stale_worktrees(&repo);
            add_worktree_impl(&repo, branch_name, host_path, use_branched, base_ref)?;
        }
        Err(err) => return Err(err),
    }

    // Honor .worktreeinclude if it exists in the project root
    let include_file_path = std::path::Path::new(project_root).join(".worktreeinclude");
    if include_file_path.exists() {
        tracing::info!("Found .worktreeinclude, copying included files...");
        if let Ok(content) = std::fs::read_to_string(&include_file_path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                let src = std::path::Path::new(project_root).join(trimmed);
                let dest = host_path.join(trimmed);

                if src.exists() {
                    if let Some(parent) = dest.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }

                    if src.is_file() {
                        if let Err(e) = std::fs::copy(&src, &dest) {
                            tracing::warn!("Failed to copy included file {} -> {}: {}", src.display(), dest.display(), e);
                        } else {
                            tracing::info!("Copied included file: {}", trimmed);
                        }
                    } else if src.is_dir() {
                        tracing::warn!(".worktreeinclude: directory copying not yet implemented: {}", trimmed);
                    }
                }
            }
        }
    }

    Ok(())
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

#[cfg(test)]
#[path = "fetch_origin_tests.rs"]
mod fetch_origin_tests;

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
    use crate::env::test_helpers::{
        init_repo_with_commit, repo_is_dirty, repo_with_drifted_head, TestDir,
    };
    use std::fs;

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

    // -----------------------------------------------------------------
    // Regression tests for #210 — branched worktrees must be creatable
    // from a dirty parent. The git worktree model isolates checkouts by
    // design, so the previous "source must be clean" gate was an
    // artificial constraint. These tests document the underlying
    // capability and guard against it being re-broken.
    // -----------------------------------------------------------------

    /// Acceptance criterion for #210: `create_git_worktree` must succeed in
    /// `branched` mode even when the parent repo has uncommitted changes.
    /// This test guards the underlying git capability that the spawn-time
    /// gate used to block.
    #[test]
    fn create_branched_worktree_succeeds_with_dirty_parent() {
        let td = TestDir::new("dirty_branched");
        let parent = td.path();

        // Clean initial commit with one tracked file.
        init_repo_with_commit(parent, &[("README.md", "# project\n")]);

        // Make the working tree dirty: modify the tracked file and add a
        // brand-new untracked file. Both must be ignored by worktree add.
        fs::write(parent.join("README.md"), "# project (in-progress edit)\n").unwrap();
        fs::write(parent.join("scratch.txt"), "untracked local note").unwrap();

        // Sanity: parent is actually dirty (would have failed the old gate).
        assert!(
            repo_is_dirty(parent),
            "precondition: parent repo must be dirty for this test to be meaningful"
        );

        // The actual capability: create a branched worktree.
        let wt_path = parent.join(".claude").join("worktrees").join("wt-1");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-1",
            "branched",
            "HEAD",
        )
        .expect("branched worktree creation must succeed with a dirty parent");

        // The worktree directory exists and is a real git checkout.
        assert!(wt_path.exists(), "worktree directory should exist");
        let wt_repo = git2::Repository::open(&wt_path).expect("worktree should be a valid repo");

        // The new branch must be checked out in the worktree.
        let head = wt_repo.head().unwrap();
        assert_eq!(
            head.shorthand().unwrap_or(""),
            "wt-1",
            "worktree HEAD should be on the new branch"
        );

        // Critical: the parent's uncommitted changes must NOT leak into the
        // new worktree (worktree add checks out the commit, not the
        // working tree of the parent).
        let wt_readme = fs::read_to_string(wt_path.join("README.md")).unwrap();
        assert_eq!(
            wt_readme, "# project\n",
            "worktree must contain the committed version, not the parent's dirty edit"
        );
        assert!(
            !wt_path.join("scratch.txt").exists(),
            "parent's untracked file must not appear in the worktree"
        );

        // The parent is still dirty after the worktree add.
        assert!(
            repo_is_dirty(parent),
            "creating a worktree must not modify the parent's working tree"
        );
    }

    /// Detached mode must keep working — unchanged by #210.
    #[test]
    fn create_detached_worktree_succeeds_with_dirty_parent() {
        let td = TestDir::new("dirty_detached");
        let parent = td.path();

        init_repo_with_commit(parent, &[("README.md", "# project\n")]);
        fs::write(parent.join("README.md"), "# project (in-progress edit)\n").unwrap();

        assert!(repo_is_dirty(parent), "precondition: parent repo must be dirty");

        let wt_path = parent.join(".claude").join("worktrees").join("wt-det");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-det",
            "detached",
            "HEAD",
        )
        .expect("detached worktree creation must succeed with a dirty parent");

        assert!(wt_path.exists(), "worktree directory should exist");

        // The worktree HEAD must be detached (not on the auto-created branch).
        let wt_repo =
            git2::Repository::open(&wt_path).expect("worktree should be a valid repo");
        assert!(
            wt_repo.head_detached().unwrap_or(false),
            "worktree HEAD should be detached in detached mode"
        );

        // The parent's dirty edit must not leak into the worktree.
        let wt_readme = fs::read_to_string(wt_path.join("README.md")).unwrap();
        assert_eq!(
            wt_readme, "# project\n",
            "worktree must contain the committed version, not the parent's dirty edit"
        );

        // The parent is still dirty after the worktree add.
        assert!(
            repo_is_dirty(parent),
            "creating a worktree must not modify the parent's working tree"
        );
    }

    // -----------------------------------------------------------------
    // Regression tests for #230 — worktrees must be based on the
    // configured base_ref (default origin/main), not the Mesh root's
    // local HEAD. Without this, root drift (parked on a feature branch,
    // behind origin) is inherited by every new worktree.
    // -----------------------------------------------------------------

    #[test]
    fn resolve_base_commit_head_returns_mesh_root_head() {
        let td = TestDir::new("resolve_head");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let head = repo.head().unwrap().peel_to_commit().unwrap().id();
        assert_eq!(resolve_base_commit(&repo, "HEAD").unwrap().id(), head);
        assert_eq!(resolve_base_commit(&repo, "").unwrap().id(), head);
    }

    #[test]
    fn resolve_base_commit_resolves_remote_ref() {
        let td = TestDir::new("resolve_remote");
        let (repo, origin_oid) = repo_with_drifted_head(td.path());
        assert_eq!(
            resolve_base_commit(&repo, "origin/main").unwrap().id(),
            origin_oid,
            "origin/main must resolve to the remote-tracking commit, not HEAD"
        );
    }

    #[test]
    fn resolve_base_commit_falls_back_to_head_when_unresolvable() {
        let td = TestDir::new("resolve_fallback");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let head = repo.head().unwrap().peel_to_commit().unwrap().id();
        assert_eq!(
            resolve_base_commit(&repo, "origin/does-not-exist")
                .unwrap()
                .id(),
            head,
            "an unresolvable ref must fall back to HEAD rather than error"
        );
    }

    #[test]
    fn branched_worktree_bases_off_base_ref_not_head() {
        let td = TestDir::new("base_ref_branched");
        let parent = td.path();
        let (_repo, origin_oid) = repo_with_drifted_head(parent);

        let wt_path = parent.join(".claude").join("worktrees").join("wt-br");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-br",
            "branched",
            "origin/main",
        )
        .expect("branched worktree creation must succeed");

        let content = fs::read_to_string(wt_path.join("f.txt")).unwrap();
        assert_eq!(
            content, "from-origin-main\n",
            "worktree must be based on origin/main, not the drifted Mesh root HEAD"
        );
        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert_eq!(
            wt_repo.head().unwrap().peel_to_commit().unwrap().id(),
            origin_oid
        );
    }

    #[test]
    fn detached_worktree_bases_off_base_ref_not_head() {
        let td = TestDir::new("base_ref_detached");
        let parent = td.path();
        let (_repo, origin_oid) = repo_with_drifted_head(parent);

        let wt_path = parent.join(".claude").join("worktrees").join("wt-det");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-det",
            "detached",
            "origin/main",
        )
        .expect("detached worktree creation must succeed");

        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert!(
            wt_repo.head_detached().unwrap_or(false),
            "worktree HEAD should be detached in detached mode"
        );
        assert_eq!(
            wt_repo.head().unwrap().peel_to_commit().unwrap().id(),
            origin_oid,
            "detached worktree must point at origin/main, not the drifted HEAD"
        );
        let content = fs::read_to_string(wt_path.join("f.txt")).unwrap();
        assert_eq!(content, "from-origin-main\n");
    }

    #[test]
    fn worktree_base_ref_head_reproduces_mesh_root_head() {
        let td = TestDir::new("base_ref_head");
        let parent = td.path();
        let (repo, origin_oid) = repo_with_drifted_head(parent);
        let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

        let wt_path = parent.join(".claude").join("worktrees").join("wt-head");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-head",
            "branched",
            "HEAD",
        )
        .expect("worktree creation must succeed");

        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        let wt_oid = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
        assert_eq!(
            wt_oid, head_oid,
            "base_ref=HEAD must reproduce today's behaviour (based on Mesh root HEAD)"
        );
        assert_ne!(wt_oid, origin_oid, "HEAD has drifted away from origin/main");
        let content = fs::read_to_string(wt_path.join("f.txt")).unwrap();
        assert_eq!(content, "local-drift\n");
    }

    #[test]
    fn worktree_unresolvable_base_ref_falls_back_to_head() {
        let td = TestDir::new("base_ref_offline");
        let parent = td.path();
        let repo = init_repo_with_commit(parent, &[("f.txt", "only-commit\n")]);
        let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

        // No origin/main ref exists (simulates offline / missing remote).
        let wt_path = parent.join(".claude").join("worktrees").join("wt-off");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-off",
            "branched",
            "origin/main",
        )
        .expect("spawn must not fail when base_ref is unresolvable");

        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert_eq!(
            wt_repo.head().unwrap().peel_to_commit().unwrap().id(),
            head_oid,
            "unresolvable base_ref must fall back to HEAD"
        );
    }
}
