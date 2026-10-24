//! The capability-shaped read of a repository's state (issue #2526).
//!
//! Before this module, every command that needed to know something about a
//! repository opened a `git2::Repository` itself and re-derived the same
//! primitives — "is the repo dirty", "what changed and by how many lines",
//! "which branch, how far ahead". The raw handle was the interface, so the
//! primitives leaked out of `git/` and the copies drifted apart.
//!
//! Now a caller says **what it wants** and gets plain data back:
//!
//! - [`Baseline`] says *which* baseline a read is relative to — the working
//!   tree, or a base ref resolved through the merge-base rule of ADR 0005.
//! - [`state_at`] is the whole state in one read.
//! - [`branch_status_at`], [`summary_at`] and [`changed_files_at`] are the
//!   narrower reads for the callers that do not need all of it.
//!
//! Every changed-file view in Buildmesh derives its line counts from
//! [`crate::git::status_stats::delta_counts`], so the Changed Files Section
//! and the Node Change Set can no longer disagree about a file's line counts.

use git2::{Repository, Status, StatusOptions};

use super::diff;
use super::primitives;
use super::status_stats;

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;

/// Which baseline a read is relative to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Baseline {
    /// HEAD-relative: the uncommitted changes only — the index plus the
    /// working tree.
    WorkingTree,
    /// The merge-base with `base_ref`, then the working tree. This is what
    /// "everything the agent changed since branching" means (ADR 0005).
    BaseRef(String),
}

/// Why a state read failed.
#[derive(Debug)]
pub enum GitQueryError {
    /// The repository at the requested path could not be opened.
    Open(git2::Error),
    /// A read inside the repository failed.
    Read(git2::Error),
    /// The repository has no working directory — a bare repository.
    NoWorkingDirectory,
}

impl std::fmt::Display for GitQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitQueryError::Open(error) | GitQueryError::Read(error) => write!(f, "{error}"),
            GitQueryError::NoWorkingDirectory => {
                write!(f, "Git repository has no working directory")
            }
        }
    }
}

impl std::error::Error for GitQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GitQueryError::Open(error) | GitQueryError::Read(error) => Some(error),
            GitQueryError::NoWorkingDirectory => None,
        }
    }
}

impl From<git2::Error> for GitQueryError {
    /// A libgit2 failure inside an already-open repository is a read
    /// failure. An *open* failure is mapped explicitly by the caller, which
    /// knows the path the user gave.
    fn from(error: git2::Error) -> Self {
        GitQueryError::Read(error)
    }
}

/// Where HEAD points.
///
/// The cheap facts only — reading this never walks the commit graph, so it
/// is safe on a hot path. [`BranchStatus`] adds the distance to the upstream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadInfo {
    /// The branch name, or `HEAD` when the repository is detached. `None`
    /// when HEAD is unborn or its ref name is not representable as UTF-8 —
    /// both cases where there is no branch to report.
    pub branch: Option<String>,
    /// True when the repository has no commits yet.
    pub unborn: bool,
    /// True when HEAD is detached from any branch.
    pub detached: bool,
    /// The short SHA of HEAD's commit. Empty when there is no commit.
    pub short_sha: String,
    /// The upstream branch, when the current branch tracks one.
    pub upstream: Option<String>,
}

/// How far the current branch is from its upstream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchStatus {
    /// The branch name, or `HEAD` when the repository is detached.
    pub name: String,
    /// Commits on the current branch that the upstream does not have.
    pub ahead: u32,
    /// Commits on the upstream that the current branch does not have.
    pub behind: u32,
    /// The short SHA of HEAD's commit.
    pub short_sha: String,
}

/// One file's working-tree status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    /// The repository-relative path.
    pub path: String,
    /// `added`, `modified`, `deleted`, `renamed` or `untracked`.
    pub status: String,
    /// The change is in the index.
    pub staged: bool,
    /// The change is in the working tree.
    pub unstaged: bool,
    /// Lines added, relative to the baseline.
    pub additions: usize,
    /// Lines deleted, relative to the baseline.
    pub deletions: usize,
}

/// One file changed relative to a baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// The repository-relative path.
    pub path: String,
    /// The previous path, for a rename.
    pub old_path: Option<String>,
    /// `added`, `deleted`, `renamed`, `untracked` or `modified`.
    pub status: String,
    /// Lines added.
    pub additions: usize,
    /// Lines deleted.
    pub deletions: usize,
}

/// How many files changed, by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusSummary {
    pub total: usize,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

/// Everything a caller can know about a repository at one baseline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoState {
    /// Where HEAD points.
    pub head: HeadInfo,
    /// The working-tree status, with the line counts of the working-tree
    /// baseline.
    pub statuses: Vec<FileStatus>,
    /// The files changed relative to the baseline.
    pub changed_files: Vec<ChangedFile>,
    /// The counts over [`RepoState::statuses`].
    pub summary: StatusSummary,
}

/// The wrap-up verification's view of a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WrapupGitState {
    /// The working tree has uncommitted changes.
    pub dirty: bool,
    /// The branch HEAD points at.
    pub branch: Option<String>,
    /// The branch is fully pushed to its upstream.
    pub pushed: bool,
    /// The reason the repository could not be opened, when it could not be.
    pub repo_error: Option<String>,
}

/// The wrap-up verification's view of the repository at `host_path`.
///
/// The open is self-healing: an MSYS-flavoured git leaves Git-Bash-style
/// `/f/...` paths in the worktree link files that the agent's CLI reads
/// fine but libgit2 reports as NotFound (the 2026-07-17 gh252 incident —
/// the agent had to run `git worktree repair` itself). Sanitizing both link
/// sides and retrying once means a format-only mismatch never reaches the
/// caller as a failure.
pub fn wrapup_state_at(host_path: &str, env_type: crate::models::EnvType) -> WrapupGitState {
    let opened = Repository::open(host_path).or_else(|first_err| {
        tracing::info!(
            "open failed ({}); sanitizing worktree links and retrying",
            first_err
        );
        if let Err(error) = super::worktree::sanitize_git_worktree(host_path, env_type) {
            tracing::warn!("sanitize failed: {}", error);
        }
        Repository::open(host_path)
    });

    match opened {
        Ok(repo) => {
            let dirty = primitives::is_dirty(&repo).unwrap_or(true);
            let branch = primitives::head_branch_name(&repo);
            let pushed = branch
                .as_deref()
                .and_then(|name| {
                    let local = repo.find_branch(name, git2::BranchType::Local).ok()?;
                    let upstream = local.upstream().ok()?;
                    let local_oid = local.get().target()?;
                    let upstream_oid = upstream.get().target()?;
                    let (ahead, _behind) =
                        primitives::ahead_behind(&repo, local_oid, upstream_oid).ok()?;
                    Some(ahead == 0)
                })
                .unwrap_or(false);
            WrapupGitState {
                dirty,
                branch,
                pushed,
                repo_error: None,
            }
        }
        Err(error) => {
            tracing::warn!("could not open repo at {}: {}", host_path, error);
            WrapupGitState {
                dirty: true,
                branch: None,
                pushed: false,
                repo_error: Some(error.to_string()),
            }
        }
    }
}

/// The whole state of a repository at one baseline.
///
/// This is the read the issue asks for: one call, one baseline, plain data
/// back. The narrower reads below are the same computation with less of it,
/// for the callers that do not need the whole thing.
pub fn state_at(host_path: &str, baseline: Baseline) -> Result<RepoState, GitQueryError> {
    let repo = primitives::open_at(host_path).map_err(GitQueryError::Open)?;
    let head = head_info(&repo);
    let entries = status_entries(&repo)?;

    // The working-tree baseline is HEAD. Its diff is also the source of the
    // status rows' line counts, so one walk serves both. A clean working
    // tree never reaches the diff at all — there is nothing to count, and
    // skipping it keeps a clean repository off the patch path.
    let head_deltas = if entries.is_empty() {
        Vec::new()
    } else {
        deltas_of(
            &repo,
            &Baseline::WorkingTree,
            diff::RenameDetection::Separate,
        )?
    };
    let counts = status_stats::line_stats_map(&head_deltas);
    let statuses: Vec<FileStatus> = entries
        .into_iter()
        .map(|entry| {
            let (additions, deletions) = counts.get(&entry.path).copied().unwrap_or((0, 0));
            FileStatus {
                additions,
                deletions,
                ..entry
            }
        })
        .collect();
    let summary = summarize(&statuses);

    let changed_files = match &baseline {
        // The working-tree baseline is the diff the status rows came from,
        // so the walk happens once and both views read it.
        Baseline::WorkingTree => changed_files_from_deltas(&head_deltas),
        Baseline::BaseRef(_) => changed_files_in(&repo, &baseline)?,
    };

    Ok(RepoState {
        head,
        statuses,
        changed_files,
        summary,
    })
}

/// How far the current branch is from its upstream.
///
/// `None` when there is no branch to report: an unreadable repository, an
/// unborn HEAD, or a ref name that is not representable as UTF-8. All three
/// mean "no branch", and the caller shows nothing either way.
pub fn branch_status_at(host_path: &str) -> Result<Option<BranchStatus>, GitQueryError> {
    let repo = primitives::open_at(host_path).map_err(GitQueryError::Open)?;
    let head = head_info(&repo);
    let Some(name) = head.branch else {
        return Ok(None);
    };
    let Some((ahead, behind)) = ahead_behind_of(&repo, &name) else {
        return Ok(Some(BranchStatus {
            name,
            ahead: 0,
            behind: 0,
            short_sha: head.short_sha,
        }));
    };
    Ok(Some(BranchStatus {
        name,
        ahead,
        behind,
        short_sha: head.short_sha,
    }))
}

/// How many files changed in the working tree, by kind.
///
/// This is the cheap read: it walks the status and stops. It never opens the
/// line-stat diff, so a caller that only wants a badge does not pay for a
/// patch pass over the repository.
pub fn summary_at(host_path: &str) -> Result<StatusSummary, GitQueryError> {
    let repo = primitives::open_at(host_path).map_err(GitQueryError::Open)?;
    Ok(summarize(&status_entries(&repo)?))
}

/// The files changed relative to a baseline, with their line counts.
pub fn changed_files_at(
    host_path: &str,
    baseline: Baseline,
) -> Result<Vec<ChangedFile>, GitQueryError> {
    let repo = primitives::open_at(host_path).map_err(GitQueryError::Open)?;
    changed_files_in(&repo, &baseline)
}

/// Where HEAD points, read from an open repository.
fn head_info(repo: &Repository) -> HeadInfo {
    let Ok(head) = repo.head() else {
        // An unborn HEAD: a repository with no commits yet.
        return HeadInfo {
            unborn: true,
            ..HeadInfo::default()
        };
    };
    let detached = repo.head_detached().unwrap_or(false);
    let branch = head.shorthand().ok().map(|name| name.to_string());
    let short_sha = head
        .target()
        .map(|oid| primitives::short_sha(repo, oid))
        .unwrap_or_default();
    let upstream = branch.as_deref().and_then(|name| upstream_name(repo, name));
    HeadInfo {
        branch,
        unborn: false,
        detached,
        short_sha,
        upstream,
    }
}

/// The upstream branch name, when the branch tracks one.
fn upstream_name(repo: &Repository, branch: &str) -> Option<String> {
    repo.branch_upstream_name(&format!("refs/heads/{branch}"))
        .ok()
        .and_then(|name| name.as_str().ok().map(|name| name.to_string()))
}

/// The commits the branch is ahead and behind its upstream.
///
/// `None` when the branch has no upstream, or when the distance cannot be
/// computed — both mean "not comparable", which the caller reads as zero.
fn ahead_behind_of(repo: &Repository, branch: &str) -> Option<(u32, u32)> {
    let local = repo.head().ok()?.target()?;
    let upstream = primitives::upstream_oid_for_branch(repo, branch)?;
    primitives::ahead_behind(repo, local, upstream).ok()
}

/// The working-tree status walk, without the line counts.
fn status_entries(repo: &Repository) -> Result<Vec<FileStatus>, GitQueryError> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    #[cfg(test)]
    // allow-test-in-production: the counter's writer, called from the one status walk it counts.
    status_stats::status_metrics::walk();
    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(GitQueryError::Read)?;
    let mut entries = Vec::new();
    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        // `is_ignored_entry`, not `Status::is_ignored`: the latter leaves a
        // junctioned `src-tauri/target` listed as a changed file (#2193).
        if primitives::is_ignored_entry(repo, &entry) {
            continue;
        }
        let flag = entry.status();
        entries.push(FileStatus {
            status: classify_status(flag).to_string(),
            staged: is_staged(flag),
            unstaged: is_unstaged(flag),
            additions: 0,
            deletions: 0,
            path,
        });
    }
    Ok(entries)
}

/// The change kind of a status flag.
///
/// The order matters and is shared with the summary count, so the summary can
/// never report a different set than the rows it summarises (#2193).
///
/// The order and the fallback are unchanged from the pre-#2526 walk, because
/// the UI renders these labels. The fallback is not dead code: a file that
/// changed *kind* (a regular file replaced by a symlink, say) carries a
/// type-change flag, which none of the named branches match.
fn classify_status(flag: Status) -> &'static str {
    if flag.is_index_new() || flag.is_wt_new() {
        "added"
    } else if flag.is_index_modified() || flag.is_wt_modified() {
        "modified"
    } else if flag.is_index_deleted() || flag.is_wt_deleted() {
        "deleted"
    } else if flag.is_index_renamed() || flag.is_wt_renamed() {
        "renamed"
    } else {
        "untracked"
    }
}

/// The change is in the index.
fn is_staged(flag: Status) -> bool {
    flag.is_index_new()
        || flag.is_index_modified()
        || flag.is_index_deleted()
        || flag.is_index_renamed()
        || flag.is_index_typechange()
}

/// The change is in the working tree.
fn is_unstaged(flag: Status) -> bool {
    flag.is_wt_new()
        || flag.is_wt_modified()
        || flag.is_wt_deleted()
        || flag.is_wt_renamed()
        || flag.is_wt_typechange()
}

/// The counts over a status list.
fn summarize(entries: &[FileStatus]) -> StatusSummary {
    let mut summary = StatusSummary::default();
    for entry in entries {
        summary.total += 1;
        match entry.status.as_str() {
            "added" => summary.added += 1,
            "modified" => summary.modified += 1,
            "deleted" => summary.deleted += 1,
            // renamed and untracked do not affect the count display but do
            // contribute to the total.
            _ => {}
        }
    }
    summary
}

/// The files changed relative to a baseline, read from an open repository.
fn changed_files_in(
    repo: &Repository,
    baseline: &Baseline,
) -> Result<Vec<ChangedFile>, GitQueryError> {
    Ok(changed_files_from_deltas(&deltas_of(
        repo,
        baseline,
        diff::RenameDetection::Collapse,
    )?))
}

/// The walked deltas of a baseline.
///
/// `renames` decides whether a moved file is one entry or two — see
/// [`diff::RenameDetection`] for why the two readers differ.
fn deltas_of(
    repo: &Repository,
    baseline: &Baseline,
    renames: diff::RenameDetection,
) -> Result<Vec<status_stats::DeltaView>, GitQueryError> {
    let (diff, _) = diff::build_diff(repo, baseline, None, renames)?;
    Ok(status_stats::walk_deltas(repo, &diff))
}

/// The changed files a walked diff describes.
fn changed_files_from_deltas(deltas: &[status_stats::DeltaView]) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = deltas
        .iter()
        .map(|delta| ChangedFile {
            path: delta.path.clone(),
            old_path: delta.old_path.clone(),
            status: delta.status.to_string(),
            additions: delta.additions,
            deletions: delta.deletions,
        })
        .collect();
    // A stable order, so the review panel does not reshuffle between reads.
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}
