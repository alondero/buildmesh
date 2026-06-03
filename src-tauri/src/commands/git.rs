//! Git operations via git2 crate

use std::collections::HashMap;

use git2::{DiffOptions, Patch, Repository, StatusOptions};
use serde::{Deserialize, Serialize};
use tauri::command;

use crate::env::to_host_path;
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
    /// detached-HEAD worktrees (the default buildmesh agent worktree mode) where
    /// `name == "HEAD"` would otherwise be uninformative.
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
