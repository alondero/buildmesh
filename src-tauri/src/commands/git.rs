//! Git operations via git2 crate

use std::collections::HashMap;

use git2::{DiffOptions, Patch, Repository, StatusOptions};
use serde::{Deserialize, Serialize};
use tauri::command;
use tauri::Emitter;

use crate::db;
use crate::env::to_host_path;
use crate::models::{HoldingWorktree, MeshHealth};
use crate::process_util::command_no_window;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitSummary {
    pub total: usize,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatus {
    pub path: String,
    pub status: String, // "modified" | "added" | "deleted" | "renamed" | "untracked"
    pub additions: usize,
    pub deletions: usize,
}

/// Build a map of `relative path -> (additions, deletions)` covering every
/// uncommitted change (HEAD → index + working tree), so each status entry can
/// be annotated with its line-level diff stats.
///
/// Untracked files require `show_untracked_content` so their new lines are
/// counted as additions; without it git2 emits an empty patch for them.
fn line_stats_by_path(repo: &Repository) -> HashMap<String, (usize, usize)> {
    let mut diff_opts = DiffOptions::new();
    diff_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);

    // No HEAD yet (repo with no commits) → diff against an empty tree.
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());

    let mut stats: HashMap<String, (usize, usize)> = HashMap::new();

    let diff = match repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut diff_opts)) {
        Ok(diff) => diff,
        Err(_) => return stats,
    };

    let num_deltas = diff.deltas().len();
    for idx in 0..num_deltas {
        // Binary files / errors yield no patch — leave them at (0, 0).
        let patch = match Patch::from_diff(&diff, idx) {
            Ok(Some(p)) => p,
            _ => continue,
        };

        let (_context, additions, deletions) = match patch.line_stats() {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Key on both old and new path so deleted (old) and added (new) files
        // are both found by the status loop's relative path.
        let delta = patch.delta();
        for file in [delta.new_file().path(), delta.old_file().path()].into_iter().flatten() {
            stats.insert(file.to_string_lossy().to_string(), (additions, deletions));
        }
    }

    stats
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBranchStatus {
    pub name: String,
    pub ahead: u32,
    pub behind: u32,
    /// Abbreviated HEAD OID (7 chars by default, matches `git rev-parse --short HEAD`).
    /// Empty string when HEAD is unborn. Useful for showing a stable identifier on
    /// detached-HEAD worktrees (e.g. after `free_base_branch` recovery detaches a
    /// branched worktree — see ADR 0006) where `name == "HEAD"` would otherwise be
    /// uninformative.
    pub short_sha: String,
}

/// Report the current branch and how far ahead/behind its upstream it is.
///
/// Returns `None` when `path` is not a git repository or HEAD is unborn (no
/// commits yet). A detached HEAD reports as `name = "HEAD"` with no upstream,
/// but `short_sha` is still populated so the UI can render e.g.
/// `detached @ a064f55`. Ahead/behind are `0` when no upstream is configured.
///
/// Uses git2's `graph_ahead_behind` rather than `git rev-list HEAD..@{u}`: the
/// brace syntax is silently mangled by `Command::args` on Windows
/// (see commands/prune.rs for the same pattern).
#[command]
pub fn get_git_branch_status(path: String) -> Result<Option<GitBranchStatus>, String> {
    let host_path = to_host_path(&path);
    let repo = match Repository::open(&host_path) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return Ok(None), // unborn HEAD (no commits yet)
    };

    // shorthand() yields the branch name, "HEAD" when detached, and None only
    // for a non-UTF8 ref name (which we can't represent, so treat as no branch).
    let name = match head.shorthand() {
        Some(n) => n.to_string(),
        None => return Ok(None),
    };

    let local_oid = head.target();

    // short_id() respects the repo's `core.abbreviate` (default 7). For unborn
    // HEAD the OID is None; we leave short_sha empty rather than fabricate.
    let short_sha = local_oid
        .and_then(|oid| repo.find_object(oid, None).ok())
        .and_then(|obj| obj.short_id().ok())
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
        .unwrap_or_default();

    let mut ahead = 0u32;
    let mut behind = 0u32;
    let refname = format!("refs/heads/{}", name);
    if let Ok(upstream_buf) = repo.branch_upstream_name(&refname) {
        if let Some(upstream_ref) = upstream_buf.as_str() {
            if let Ok(up_ref) = repo.find_reference(upstream_ref) {
                if let (Some(local), Some(up)) = (local_oid, up_ref.target()) {
                    if let Ok((a, b)) = repo.graph_ahead_behind(local, up) {
                        ahead = a as u32;
                        behind = b as u32;
                    }
                }
            }
        }
    }

    Ok(Some(GitBranchStatus {
        name,
        ahead,
        behind,
        short_sha,
    }))
}

/// Get git status for a directory — returns list of changed files with per-file
/// line additions/deletions for all uncommitted changes.
#[command]
pub fn get_git_status(path: String) -> Result<Vec<GitStatus>, String> {
    let host_path = to_host_path(&path);
    let repo = Repository::open(&host_path).map_err(|e| e.to_string())?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut opts))
        .map_err(|e| e.to_string())?;

    let line_stats = line_stats_by_path(&repo);

    let mut changed_files: Vec<GitStatus> = Vec::new();

    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }

        let status_flag = entry.status();
        let status_str = if status_flag.is_index_new() || status_flag.is_wt_new() {
            "added"
        } else if status_flag.is_index_modified() || status_flag.is_wt_modified() {
            "modified"
        } else if status_flag.is_index_deleted() || status_flag.is_wt_deleted() {
            "deleted"
        } else if status_flag.is_index_renamed() || status_flag.is_wt_renamed() {
            "renamed"
        } else if status_flag.is_ignored() {
            continue;
        } else {
            "untracked"
        };

        let (additions, deletions) = line_stats.get(&path).copied().unwrap_or((0, 0));

        changed_files.push(GitStatus {
            path,
            status: status_str.to_string(),
            additions,
            deletions,
        });
    }

    Ok(changed_files)
}

/// Get aggregate git change summary for a directory
#[command]
pub fn get_git_summary(path: String) -> Result<GitSummary, String> {
    let host_path = to_host_path(&path);
    let repo = Repository::open(&host_path).map_err(|e| e.to_string())?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut opts))
        .map_err(|e| e.to_string())?;

    let mut total = 0usize;
    let mut added = 0usize;
    let mut modified = 0usize;
    let mut deleted = 0usize;

    for entry in statuses.iter() {
        let status_flag = entry.status();
        if status_flag.is_ignored() {
            continue;
        }

        total += 1;
        if status_flag.is_index_new() || status_flag.is_wt_new() {
            added += 1;
        } else if status_flag.is_index_modified() || status_flag.is_wt_modified() {
            modified += 1;
        } else if status_flag.is_index_deleted() || status_flag.is_wt_deleted() {
            deleted += 1;
        }
        // renamed and untracked don't affect the count display but contribute to total
    }

    Ok(GitSummary {
        total,
        added,
        modified,
        deleted,
    })
}

/// Check whether a path is a valid git repository
#[command]
pub fn check_is_git_repo(path: String) -> bool {
    git2::Repository::open(&path).is_ok()
}

/// Get the default branch name for the remote named "origin".
/// Reads the local symbolic ref (populated by clone/fetch) to avoid a network round-trip.
/// Falls back to "main" if no remote is configured or HEAD ref is missing.
#[command]
pub fn get_default_branch(path: String) -> String {
    let repo = match Repository::open(&path) {
        Ok(r) => r,
        Err(_) => return "main".to_string(),
    };

    // Try the local symbolic ref first (no network needed)
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(target) = reference.symbolic_target() {
            if let Some(branch) = target.strip_prefix("refs/remotes/origin/") {
                return branch.to_string();
            }
        }
    }

    "main".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitSyncResult {
    pub fetched: bool,
    pub pulled: bool,
    pub new_commits: u32,
    pub message: String,
}

/// Fetch from the current branch's default remote and attempt a fast-forward pull.
/// Returns a structured result with feedback about what happened.
#[command]
pub async fn git_sync(path: String) -> Result<GitSyncResult, String> {
    let host_path = to_host_path(&path);

    // Step 1: git fetch
    let fetch_output = command_no_window("git")
        .args(["fetch"])
        .current_dir(&host_path)
        .output()
        .map_err(|e| format!("Failed to run git fetch: {}", e))?;

    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        return Ok(GitSyncResult {
            fetched: false,
            pulled: false,
            new_commits: 0,
            message: format!("Fetch failed: {}", stderr.trim()),
        });
    }

    // Step 2: Count how many commits we're behind. We compute this via
    // git2's `graph_ahead_behind` rather than shelling out to
    // `git rev-list --count HEAD..@{u}` because the `@{u}` syntax fails
    // on Windows when spawned via `std::process::Command::args` — the
    // curly braces are stripped during command-line construction and
    // git sees `HEAD..@u`, which it rejects as ambiguous. Using git2
    // (already imported in this file) sidesteps the brace issue at its
    // source rather than working around it in the subprocess arg.
    let behind_count: u32 = count_commits_behind(&host_path).unwrap_or_else(|e| {
        tracing::warn!("git_sync: failed to count commits behind upstream: {}", e);
        0
    });

    if behind_count == 0 {
        return Ok(GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits: 0,
            message: "Already up to date".to_string(),
        });
    }

    // Step 3: git pull --ff-only
    let pull_output = command_no_window("git")
        .args(["pull", "--ff-only"])
        .current_dir(&host_path)
        .output()
        .map_err(|e| format!("Failed to run git pull: {}", e))?;

    if !pull_output.status.success() {
        let stderr = String::from_utf8_lossy(&pull_output.stderr);
        return Ok(GitSyncResult {
            fetched: true,
            pulled: false,
            new_commits: behind_count,
            message: format!(
                "Fetched {} new commit{} but fast-forward failed: {}",
                behind_count,
                if behind_count == 1 { "" } else { "s" },
                stderr.trim()
            ),
        });
    }

    Ok(GitSyncResult {
        fetched: true,
        pulled: true,
        new_commits: behind_count,
        message: format!(
            "Pulled {} new commit{}",
            behind_count,
            if behind_count == 1 { "" } else { "s" }
        ),
    })
}

/// How many commits the current branch is behind its upstream.
/// Returns `Ok(0)` when the branch has no upstream configured.
/// Uses git2 to avoid the `@{u}` brace issue with `std::process::Command::args` on Windows.
fn count_commits_behind(host_path: &str) -> Result<u32, String> {
    let repo = Repository::open(host_path)
        .map_err(|e| format!("Failed to open repository at {}: {}", host_path, e))?;

    let head_oid = repo
        .head()
        .map_err(|e| format!("Failed to read HEAD: {}", e))?
        .peel_to_commit()
        .map_err(|e| format!("HEAD is not a commit: {}", e))?
        .id();

    // Find the upstream remote-tracking ref for the current branch.
    // `branch_upstream_name` returns something like "refs/remotes/main/main"
    // when the local branch tracks `main/main`.
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
    // `graph_ahead_behind` returns usize; on a 32-bit platform this could
    // overflow u32, but on any realistic repo it won't. Saturate rather
    // than panic.
    Ok(behind.try_into().unwrap_or(u32::MAX))
}

// ── Mesh health detection (issue #231) ──────────────────────────────────────

/// Result of a successful `restore_mesh_to_base` invocation. `restored` is
/// `true` only when HEAD actually moved; an already-on-base call returns
/// `restored = false` with a "no-op" message so the UI can distinguish
/// the two outcomes for telemetry / toast wording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    pub restored: bool,
    pub message: String,
}

/// Result of a successful `free_base_branch` invocation. `detached_at_sha`
/// is the 7-char short OID the worktree was detached at, useful for the
/// "freed at a064f55" toast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreeResult {
    pub detached_at_sha: String,
}

/// Derive the local branch name from a configured `base_ref` string.
///
/// Accepts the four observed forms:
///   - `main`                        → `Some("main")`
///   - `origin/main`                 → `Some("main")`
///   - `refs/heads/main`             → `Some("main")`
///   - `origin/feature/auth`         → `Some("feature/auth")` (strip the remote, keep nested branch)
///   - `refs/heads/feature/auth`     → `Some("feature/auth")` (branch can contain slashes)
///   - `refs/remotes/origin/feature/auth` → `Some("feature/auth")`
///
/// Returns `None` for:
///   - empty / whitespace-only strings
///   - `HEAD` (case-insensitive) — anywhere it appears as the branch part
///   - `FETCH_HEAD`
///
/// **Limitation:** when `base_ref` is a plain nested branch name like
/// `feature/auth` (no remote prefix, no `refs/heads/` prefix), this helper
/// can't tell the difference between a remote prefix and a nested branch
/// — it strips `feature/` and returns `auth`. To disambiguate, use the
/// full `refs/heads/feature/auth` form. The buildmesh DB default
/// (`origin/main`) and the common short forms (`main`, `develop`,
/// `origin/develop`) all resolve correctly.
///
/// Callers must treat `None` as "no base branch configured" — the badge
/// and the recovery button are both suppressed in that case.
pub(crate) fn parse_local_branch(base_ref: &str) -> Option<String> {
    let trimmed = base_ref.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Path 1: full branch ref `refs/heads/<branch>` — the branch can
    // contain slashes (e.g. `refs/heads/feature/auth`).
    if let Some(branch) = trimmed.strip_prefix("refs/heads/") {
        return finalize_branch(branch);
    }
    // Path 2: full remote-tracking ref `refs/remotes/<remote>/<branch>` —
    // strip the remote (first segment) and keep the rest, which may
    // contain slashes.
    if let Some(after) = trimmed.strip_prefix("refs/remotes/") {
        let branch = after.split_once('/').map(|(_, b)| b).unwrap_or(after);
        return finalize_branch(branch);
    }
    // Path 3: short form. Either `<branch>` (no slashes — easy) or
    // `<remote>/<branch>` (the buildmesh DB default `origin/main`).
    // Without a repo we can't tell them apart, so we split on the first
    // slash and treat the first segment as the remote. See the function-
    // level doc for the nested-branch-no-remote case.
    let local = match trimmed.split_once('/') {
        Some((_remote, rest)) if !rest.is_empty() => rest,
        _ => trimmed,
    };
    finalize_branch(local)
}

/// Reject empty / HEAD / FETCH_HEAD and lowercase-normalize the result.
fn finalize_branch(local: &str) -> Option<String> {
    if local.is_empty()
        || local.eq_ignore_ascii_case("HEAD")
        || local.eq_ignore_ascii_case("FETCH_HEAD")
    {
        return None;
    }
    Some(local.to_string())
}

/// Pure helper: compute a MeshHealth snapshot from an already-open `Repository`.
/// No DB access, no Tauri command. `base_ref` is the mesh's configured
/// `base_ref` (typically `origin/main`); pass an empty string or `HEAD`
/// when the mesh has no base configured and every field will reflect that.
///
/// `base_branch_holder` is always `None` from this helper — the Tauri
/// command fills it in via `find_base_branch_holder` (which needs the
/// agent-node active-paths list, not available here).
pub(crate) fn compute_mesh_health(
    repo: &Repository,
    base_ref: &str,
) -> Result<MeshHealth, String> {
    let local_base_branch = parse_local_branch(base_ref);

    // Read HEAD state. `head()` errors on an unborn HEAD (no commits yet).
    let (current_branch, current_short_sha, is_detached) = match repo.head() {
        Ok(_) => {
            let detached = repo.head_detached().unwrap_or(false);
            let name = if detached {
                Some("HEAD".to_string())
            } else {
                repo.head()
                    .ok()
                    .and_then(|h| h.shorthand().map(|s| s.to_string()))
            };
            let oid = repo.head().ok().and_then(|h| h.target());
            let short_sha = oid
                .and_then(|o| repo.find_object(o, None).ok())
                .and_then(|obj| obj.short_id().ok())
                .map(|buf| String::from_utf8_lossy(&buf).into_owned())
                .unwrap_or_default();
            (name, short_sha, detached)
        }
        Err(_) => (None, String::new(), false),
    };

    let is_dirty = repo_is_dirty(repo);
    let (has_upstream, unpushed_ahead) =
        compute_unpushed(repo, current_branch.as_deref(), local_base_branch.as_deref());
    let is_drifted = compute_is_drifted(
        repo,
        current_branch.as_deref(),
        is_detached,
        local_base_branch.as_deref(),
    );

    Ok(MeshHealth {
        base_ref: base_ref.to_string(),
        local_base_branch,
        current_branch,
        current_short_sha,
        is_detached,
        is_dirty,
        unpushed_ahead,
        has_upstream,
        is_drifted,
        base_branch_holder: None,
    })
}

/// `unpushed_ahead` semantics: the number of commits on the current
/// branch's tip that would be stranded by a `git checkout base_branch`.
///
/// - When the branch has an upstream configured: standard ahead-of-upstream
///   count via `graph_ahead_behind`.
/// - When it has no upstream: ahead-of-local-base-branch count. A branch
///   with the same tip as the local base (a fresh branch with no local
///   commits) reports 0 — it has nothing to lose.
///
/// Returns `(has_upstream, unpushed_ahead)`.
fn compute_unpushed(
    repo: &Repository,
    current_branch: Option<&str>,
    local_base_branch: Option<&str>,
) -> (bool, u32) {
    let Some(branch) = current_branch else {
        return (false, 0);
    };
    let refname = format!("refs/heads/{}", branch);

    // Try the configured upstream first. We use git2's `graph_ahead_behind`
    // for the same Windows-brace-stripping reason as `get_git_branch_status`:
    // `git rev-list --count HEAD..@{u}` is mangled by `Command::args` on
    // Windows, so we go through libgit2.
    if let Ok(upstream_buf) = repo.branch_upstream_name(&refname) {
        if let Some(upstream_ref) = upstream_buf.as_str() {
            if let Ok(up_ref) = repo.find_reference(upstream_ref) {
                if let (Some(head_oid), Some(up_oid)) = (
                    repo.head().ok().and_then(|h| h.target()),
                    up_ref.target(),
                ) {
                    if let Ok((a, _)) = repo.graph_ahead_behind(head_oid, up_oid) {
                        return (true, a.try_into().unwrap_or(u32::MAX));
                    }
                }
            }
        }
        // Upstream configured but we couldn't resolve OIDs.
        return (true, 0);
    }

    // No upstream: count commits on the current tip not on the local base.
    if let Some(base) = local_base_branch {
        let base_oid = repo
            .find_branch(base, git2::BranchType::Local)
            .ok()
            .and_then(|b| b.get().peel_to_commit().ok())
            .map(|c| c.id());
        let head_oid = repo.head().ok().and_then(|h| h.target());
        if let (Some(b), Some(h)) = (base_oid, head_oid) {
            if b == h {
                return (false, 0);
            }
            if let Ok((a, _)) = repo.graph_ahead_behind(h, b) {
                return (false, a.try_into().unwrap_or(u32::MAX));
            }
        }
    }

    (false, 0)
}

/// `is_drifted` rules, in priority order:
/// 1. `local_base_branch.is_none()` (no base configured) → `false`
/// 2. `current_branch == Some(local_base_branch)`        → `false` (on base)
/// 3. Detached HEAD at the base branch's OID             → `false` (close enough)
/// 4. Otherwise                                          → `true`
///
/// Returns `true` (drifted) when we cannot determine equality, as a
/// conservative default — it's better to surface a possibly-false badge
/// than to miss a real drift.
fn compute_is_drifted(
    repo: &Repository,
    current_branch: Option<&str>,
    is_detached: bool,
    local_base_branch: Option<&str>,
) -> bool {
    let Some(local) = local_base_branch else {
        return false;
    };
    if !is_detached {
        return current_branch != Some(local);
    }
    // Detached: compare OIDs. A detached HEAD at the same OID as the local
    // base branch is "close enough" — no badge.
    let base_oid = repo
        .find_branch(local, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.get().peel_to_commit().ok())
        .map(|c| c.id());
    let head_oid = repo.head().ok().and_then(|h| h.target());
    match (base_oid, head_oid) {
        (Some(b), Some(h)) => b != h,
        _ => true,
    }
}

/// Read the symbolic branch name from a worktree repo's HEAD. Returns
/// `None` for a detached HEAD (no symbolic target). Mirrors
/// `commands::prune::head_branch_name` — kept private here to avoid
/// cross-module coupling.
fn head_branch_name_local(repo: &Repository) -> Option<String> {
    let head = repo.find_reference("HEAD").ok()?;
    let target = head.symbolic_target()?;
    target.strip_prefix("refs/heads/").map(|s| s.to_string())
}

/// Path comparison helper. `canonicalize` is best-effort: on Windows a
/// held-handle error falls back to a normalised string compare. The
/// `to_host_path` conversion first lets WSL-stored paths compare
/// correctly against host-side paths.
fn normalize_holder_path(p: &str) -> String {
    let host = to_host_path(p);
    let trimmed = host.trim_end_matches(['/', '\\']);
    std::fs::canonicalize(trimmed)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| trimmed.replace('\\', "/"))
}

/// Which worktree (main or linked) currently has the Base Ref's branch
/// checked out? Returns the first match.
///
/// `active_paths` is the set of agent-node worktree paths; a holder whose
/// path matches an entry in the list is marked `is_active = true` so
/// the UI can warn the user before detaching a live agent's worktree.
pub(crate) fn find_base_branch_holder(
    repo: &Repository,
    local_base_branch: &str,
    active_paths: &[String],
) -> Option<HoldingWorktree> {
    // Check the main worktree first. The main worktree's workdir may
    // differ from the path the repo was opened with, so we reopen it.
    if let Some(main_workdir) = repo.workdir() {
        if let Ok(main_repo) = Repository::open(main_workdir) {
            if head_branch_name_local(&main_repo).as_deref() == Some(local_base_branch) {
                return Some(make_holder(main_workdir.to_string_lossy().as_ref(), active_paths));
            }
        }
    }
    // Then linked worktrees.
    if let Ok(names) = repo.worktrees() {
        for wt_name in names.iter().flatten() {
            if let Ok(wt) = repo.find_worktree(wt_name) {
                let path = wt.path();
                if let Ok(wt_repo) = Repository::open(path) {
                    if head_branch_name_local(&wt_repo).as_deref() == Some(local_base_branch) {
                        return Some(make_holder(&path.to_string_lossy(), active_paths));
                    }
                }
            }
        }
    }
    None
}

fn make_holder(path: &str, active_paths: &[String]) -> HoldingWorktree {
    let norm = normalize_holder_path(path);
    let is_active = active_paths.iter().any(|p| normalize_holder_path(p) == norm);
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string();
    HoldingWorktree {
        path: path.to_string(),
        name,
        is_active,
    }
}

/// Pure helper for the "is the working tree dirty?" check. Mirrors the
/// pattern at `commands::prune::repo_has_uncommitted` (5 lines, kept
/// private rather than re-exported to keep modules decoupled).
fn repo_is_dirty(repo: &Repository) -> bool {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    match repo.statuses(Some(&mut opts)) {
        Ok(statuses) => statuses.iter().any(|e| !e.status().is_ignored()),
        Err(_) => false,
    }
}

// ── Recovery (issue #231) ───────────────────────────────────────────────────

/// Pure helper for `restore_mesh_to_base`. Runs the full guard chain
/// then shells out to `git checkout <base_branch>` when all guards
/// pass. Returns `Ok(true)` when HEAD actually moved and `Ok(false)`
/// for the already-on-base no-op case.
///
/// Guard order (first failure short-circuits with a user-readable `Err`):
/// 1. `is_dirty`                       — uncommitted changes block
/// 2. `unpushed_ahead > 0`             — local commits would be lost
/// 3. `find_base_branch_holder` is Some — branch is checked out elsewhere
/// 4. `is_detached` on a non-base OID  — silent-data-loss via reflog only
/// 5. already on base                  — no-op, returns `Ok(false)`
pub(crate) fn restore_to_base_impl(
    repo: &Repository,
    base_branch: &str,
) -> Result<bool, String> {
    // Use base_branch as the base_ref too — `parse_local_branch` is idempotent
    // for both `main` and `origin/main`, and we need `local_base_branch` set
    // for the unpushed-ahead and drift calculations to be correct.
    let health = compute_mesh_health(repo, base_branch)?;

    if health.is_dirty {
        return Err("root has uncommitted changes — commit or stash first".to_string());
    }
    if health.unpushed_ahead > 0 {
        let branch = health.current_branch.as_deref().unwrap_or("HEAD");
        let hint = if health.has_upstream { "push" } else { "push or branch" };
        return Err(format!(
            "{} unpushed commit(s) on {} — {}, branch, or reset first",
            health.unpushed_ahead, branch, hint
        ));
    }
    // Already on base → no-op. This must come BEFORE the hostage check:
    // the root worktree itself "holds" the base branch whenever it's on it,
    // and that's not a hostage — it's the desired state.
    if health.current_branch.as_deref() == Some(base_branch) {
        return Ok(false);
    }
    // Hostage check uses empty active_paths; the Tauri command refines
    // `is_active` separately. Detecting the hostage at all is what blocks
    // the restore — the user is told which worktree holds the branch.
    if let Some(holder) = find_base_branch_holder(repo, base_branch, &[]) {
        return Err(format!(
            "branch {} is held by worktree at {} — free it first",
            base_branch, holder.path
        ));
    }

    // All guards pass — run `git checkout`.
    let host_path = repo
        .workdir()
        .ok_or_else(|| "repo has no working directory".to_string())?
        .to_string_lossy()
        .to_string();
    let output = command_no_window("git")
        .args(["checkout", base_branch])
        .current_dir(&host_path)
        .output()
        .map_err(|e| format!("failed to run git checkout: {}", e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(true)
}

/// Pure helper for `free_base_branch`. Detaches the worktree's HEAD at
/// its current commit (preserves the commit in reflog, releases the
/// branch lock). Idempotent: re-running on a worktree that is already
/// detached returns the current HEAD OID without making any changes.
/// Returns the 7-char short SHA of the detached commit on success.
pub(crate) fn free_base_branch_impl(
    host_worktree_path: &str,
    local_base_branch: &str,
) -> Result<String, String> {
    let repo = Repository::open(host_worktree_path)
        .map_err(|e| format!("failed to open worktree at {}: {}", host_worktree_path, e))?;

    let current_branch = head_branch_name_local(&repo);
    let already_detached = current_branch.is_none();

    if !already_detached && current_branch.as_deref() != Some(local_base_branch) {
        return Err(format!(
            "worktree at {} does not hold base branch '{}' (current branch: {})",
            host_worktree_path,
            local_base_branch,
            current_branch.unwrap_or_else(|| "detached".to_string())
        ));
    }

    let head_oid = repo
        .head()
        .map_err(|e| format!("worktree HEAD is unborn: {}", e))?
        .peel_to_commit()
        .map_err(|e| format!("worktree HEAD is not a commit: {}", e))?
        .id();

    if !already_detached {
        repo.set_head_detached(head_oid)
            .map_err(|e| format!("set_head_detached failed: {}", e))?;
    }

    let obj = repo
        .find_object(head_oid, None)
        .map_err(|e| format!("find_object failed: {}", e))?;
    let short = obj
        .short_id()
        .map_err(|e| format!("short_id failed: {}", e))?;
    Ok(String::from_utf8_lossy(&short).into_owned())
}

// ── Tauri commands (issue #231) ─────────────────────────────────────────────

/// Compute a `MeshHealth` snapshot for the given mesh. The snapshot is
/// what powers the sidebar `!` badge and the health block in
/// `BranchesWorktreesSection`. The active-paths list is sourced from
/// the live agent-nodes table so the holder's `is_active` reflects
/// whether a real agent is using the worktree.
#[command]
pub async fn get_mesh_health(mesh_id: i64) -> Result<MeshHealth, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let host_path = to_host_path(&mesh.path);
    let repo = Repository::open(&host_path)
        .map_err(|e| format!("failed to open repo at {}: {}", host_path, e))?;

    let mut health = compute_mesh_health(&repo, &mesh.base_ref)?;

    // Refine the holder: compute the holder with live active-paths so
    // `is_active` reflects whether an agent node points at the worktree.
    if let Some(local_base) = health.local_base_branch.clone() {
        let active_paths: Vec<String> = db::list_agent_nodes()
            .map_err(|e| format!("failed to list agent nodes: {}", e))?
            .into_iter()
            .map(|n| n.path)
            .collect();
        health.base_branch_holder =
            find_base_branch_holder(&repo, &local_base, &active_paths);
    }

    Ok(health)
}

/// Restore the Mesh root to its base branch. Guarded by the same
/// `restore_to_base_impl` rules: refuses if the root is dirty, has
/// unpushed commits, or the base branch is held by a worktree.
///
/// On success, emits a `git-changed` event for the mesh path so the
/// sidebar `!` badge clears and the file explorer refreshes.
#[command]
pub async fn restore_mesh_to_base(
    mesh_id: i64,
    app: tauri::AppHandle,
) -> Result<RestoreResult, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let host_path = to_host_path(&mesh.path);
    let repo = Repository::open(&host_path)
        .map_err(|e| format!("failed to open repo at {}: {}", host_path, e))?;
    let local_base = parse_local_branch(&mesh.base_ref)
        .ok_or_else(|| format!("base_ref '{}' is not a valid branch", mesh.base_ref))?;

    let moved = restore_to_base_impl(&repo, &local_base)?;
    let message = if moved {
        format!("checked out {}", local_base)
    } else {
        format!("already on {}", local_base)
    };

    // Notify the frontend so the badge clears and the panel refreshes.
    let _ = app.emit(
        "git-changed",
        serde_json::json!({ "path": host_path, "internal_path": host_path }),
    );

    Ok(RestoreResult { restored: moved, message })
}

/// Detach the holding worktree's HEAD at its current commit, releasing
/// the branch lock so the root can `git checkout <base>` next. Idempotent
/// and non-destructive — the worktree's working tree, index, and HEAD
/// commit are all preserved; only the symbolic `HEAD → refs/heads/<base>`
/// link is replaced with a detached HEAD at the same OID.
///
/// On success, emits a `git-changed` event for the freed worktree's path
/// so any open panel re-fetches.
#[command]
pub async fn free_base_branch(
    mesh_id: i64,
    worktree_path: String,
    app: tauri::AppHandle,
) -> Result<FreeResult, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let local_base = parse_local_branch(&mesh.base_ref)
        .ok_or_else(|| format!("base_ref '{}' is not a valid branch", mesh.base_ref))?;

    let host_wt_path = to_host_path(&worktree_path);
    let detached_at_sha = free_base_branch_impl(&host_wt_path, &local_base)?;

    // Notify the frontend so the panel refreshes. The freed worktree's
    // `path` is what `get_git_prune_info` and the worktree list use to
    // identify the worktree.
    let _ = app.emit(
        "git-changed",
        serde_json::json!({
            "path": to_host_path(&mesh.path),
            "internal_path": host_wt_path,
        }),
    );

    Ok(FreeResult { detached_at_sha })
}

#[cfg(test)]
#[path = "mesh_health_tests.rs"]
mod mesh_health_tests;
