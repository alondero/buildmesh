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

    // Step 2: Count how many commits we're behind
    let rev_list_output = command_no_window("git")
        .args(["rev-list", "--count", "HEAD..@{u}"])
        .current_dir(&host_path)
        .output();

    let behind_count: u32 = rev_list_output
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0);

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
