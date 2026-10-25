//! The baseline-relative diff walk (issue #2526).
//!
//! One place that knows how a *baseline* becomes a *tree*, how a tree becomes
//! a diff, and what a delta means. Callers that used to open a
//! `git2::Repository` themselves and re-derive the merge-base rule now say
//! which baseline they mean and get plain data back.
//!
//! The baseline rule is unchanged from ADR 0005: a base ref is resolved to the
//! **merge-base** of HEAD and the ref, so the diff shows everything the agent
//! changed since branching — committed *and* uncommitted.

use std::path::PathBuf;

use git2::{Diff, DiffFindOptions, Repository, Tree};

use super::state::{Baseline, GitQueryError};

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;

/// One changed file in a diff, with the baseline side of its content.
///
/// The *new* side of the file is not carried here: it lives in the working
/// tree, and the caller reads it from [`DiffPlan::root`]. That keeps the
/// expensive per-file read next to the work that needs it, so a cancelled
/// walk does not read files it will never show.
pub struct DiffEntry {
    /// The repository-relative path of the file, as the delta names it.
    pub path: String,
    /// The previous path, for a rename.
    pub old_path: Option<String>,
    /// `added`, `deleted`, `renamed`, `untracked` or `modified`.
    pub status: String,
    /// The file's content at the baseline. Empty for an added or untracked file.
    pub old_content: String,
    /// libgit2's own binary verdict for either side of the delta.
    pub binary: bool,
}

/// A resolved diff: the entries plus the working-tree root they are relative
/// to.
pub struct DiffPlan {
    /// The working-tree root of the repository.
    pub root: PathBuf,
    /// The changed files, in the order the diff reported them.
    pub entries: Vec<DiffEntry>,
}

/// The tree a baseline resolves to, or `None` when there is nothing to diff
/// against yet — a repository with no commits, or an unresolvable base ref.
///
/// `None` means "diff against the empty tree", which is how a brand-new
/// repository's files all read as additions.
pub(crate) fn baseline_tree<'r>(repo: &'r Repository, baseline: &Baseline) -> Option<Tree<'r>> {
    match baseline {
        // The working-tree baseline is HEAD itself. A repository with no
        // commits has an unborn HEAD, and there is no tree to compare
        // against — the empty tree is the only honest baseline.
        Baseline::WorkingTree => repo.head().ok().and_then(|head| head.peel_to_tree().ok()),
        Baseline::BaseRef(base_ref) => resolve_base_tree(repo, base_ref),
    }
}

/// The merge-base of HEAD and `base_ref`, as a tree.
///
/// ADR 0005: the base ref resolves to the merge-base of HEAD and the ref, so
/// the diff covers everything the agent changed since branching — committed
/// *and* uncommitted. The fallback chain is merge-base → the `base_ref`
/// commit (unrelated histories) → `HEAD` (no resolvable base) → `None`, i.e.
/// the empty tree (a repo with no commits, where everything in the working
/// dir reads as an addition).
pub(crate) fn resolve_base_tree<'r>(repo: &'r Repository, base_ref: &str) -> Option<Tree<'r>> {
    let head_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let base_commit = repo
        .revparse_single(base_ref)
        .ok()
        .and_then(|obj| obj.peel_to_commit().ok());

    match (head_commit, base_commit) {
        (Some(head), Some(base)) => {
            if let Ok(mb) = repo.merge_base(head.id(), base.id()) {
                if let Some(tree) = repo.find_commit(mb).ok().and_then(|c| c.tree().ok()) {
                    return Some(tree);
                }
            }
            base.tree().ok()
        }
        (Some(head), None) => head.tree().ok(),
        (None, Some(base)) => base.tree().ok(),
        (None, None) => None,
    }
}

/// Whether a diff collapses a moved file into one rename entry.
///
/// The two changed-file readers disagree here, and the disagreement is
/// deliberate — it predates this module:
///
/// - The **status** reader ([`crate::git::state`]) reports a moved file as
///   the delete and the add the index actually holds, so a staged rename
///   reads as "deleted" plus "added" and each side carries its own line
///   counts.
/// - The **change-set** reader collapses the pair into one "renamed" entry,
///   because the review panel shows a move as a move.
///
/// The line counts themselves come from one implementation either way
/// ([`crate::git::status_stats::delta_counts`]); only the grouping differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameDetection {
    /// Collapse a delete+add of the same content into one rename entry.
    Collapse,
    /// Report a moved file as a delete plus an add.
    Separate,
}

/// Build the diff for a baseline.
///
/// `only` restricts the diff to one repository-relative path.
pub(crate) fn build_diff<'r>(
    repo: &'r Repository,
    baseline: &Baseline,
    only: Option<&str>,
    renames: RenameDetection,
) -> Result<(Diff<'r>, Option<Tree<'r>>), GitQueryError> {
    let base_tree = baseline_tree(repo, baseline);
    let mut opts = super::status_stats::diff_options();
    if let Some(path) = only {
        opts.pathspec(path);
    }
    let mut diff = repo
        .diff_tree_to_workdir_with_index(base_tree.as_ref(), Some(&mut opts))
        .map_err(GitQueryError::Read)?;
    if renames == RenameDetection::Collapse {
        let mut find = DiffFindOptions::new();
        find.renames(true);
        // A failure here only costs rename detection, not the diff itself.
        let _ = diff.find_similar(Some(&mut find));
    }
    Ok((diff, base_tree))
}

/// The change kind of a delta, in Buildmesh's vocabulary.
pub(crate) fn delta_status_str(status: git2::Delta) -> &'static str {
    match status {
        git2::Delta::Added => "added",
        git2::Delta::Deleted => "deleted",
        git2::Delta::Renamed | git2::Delta::Copied => "renamed",
        git2::Delta::Untracked => "untracked",
        _ => "modified",
    }
}

/// The content of a blob in a tree, as text. Empty when the path is not in
/// the tree or its content is not representable as text.
pub(crate) fn tree_blob_string(repo: &Repository, tree: Option<&Tree<'_>>, path: &str) -> String {
    let Some(tree) = tree else {
        return String::new();
    };
    let entry = match tree.get_path(std::path::Path::new(path)) {
        Ok(entry) => entry,
        Err(_) => return String::new(),
    };
    let blob = match repo.find_blob(entry.id()) {
        Ok(blob) => blob,
        Err(_) => return String::new(),
    };
    match std::str::from_utf8(blob.content()) {
        Ok(text) => text.to_string(),
        Err(_) => String::new(),
    }
}

/// A file's committed content, and the working-tree root it is relative to.
pub struct HeadFile {
    /// The working-tree root of the repository.
    pub root: PathBuf,
    /// The file's content at `HEAD`, or `None` when HEAD has no such file —
    /// an unborn HEAD, a path that was never committed, or a path that is not
    /// a blob.
    pub content: Option<String>,
}

/// The committed content of one file, read from `HEAD`.
///
/// The per-file diff view needs two sides: what `HEAD` has, and what the
/// working tree has. This is the `HEAD` side.
pub fn head_file_at(host_path: &str, path: &str) -> Result<HeadFile, GitQueryError> {
    let repo = super::primitives::open_at(host_path).map_err(GitQueryError::Open)?;
    let root = repo
        .workdir()
        .ok_or(GitQueryError::NoWorkingDirectory)?
        .to_path_buf();
    let content = repo
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok())
        .and_then(|commit| commit.tree().ok())
        .and_then(|tree| tree.get_path(std::path::Path::new(path)).ok())
        .and_then(|entry| entry.to_object(&repo).ok())
        .and_then(|object| object.as_blob().map(|blob| blob.content().to_vec()))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    Ok(HeadFile { root, content })
}

/// Resolve a baseline into the changed files it describes.
///
/// This is the read behind the per-file diff view: the caller gets the
/// baseline side of every changed file and reads the working-tree side
/// itself, from [`DiffPlan::root`].
pub fn diff_plan(
    host_path: &str,
    baseline: &Baseline,
    only: Option<&str>,
) -> Result<DiffPlan, GitQueryError> {
    let repo = super::primitives::open_at(host_path)?;
    let root = repo
        .workdir()
        .ok_or(GitQueryError::NoWorkingDirectory)?
        .to_path_buf();
    let (diff, base_tree) = build_diff(&repo, baseline, only, RenameDetection::Collapse)?;
    let mut entries = Vec::new();
    for idx in 0..diff.deltas().len() {
        let Some(delta) = diff.get_delta(idx) else {
            continue;
        };
        let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) else {
            continue;
        };
        let rel = path.to_string_lossy().into_owned();
        let status = delta_status_str(delta.status());
        let old_path = if status == "renamed" {
            delta
                .old_file()
                .path()
                .map(|old| old.to_string_lossy().into_owned())
        } else {
            None
        };
        let old_content = if matches!(status, "added" | "untracked") {
            String::new()
        } else {
            tree_blob_string(
                &repo,
                base_tree.as_ref(),
                old_path.as_deref().unwrap_or(&rel),
            )
        };
        entries.push(DiffEntry {
            path: rel,
            old_path,
            status: status.to_string(),
            old_content,
            binary: delta.new_file().is_binary() || delta.old_file().is_binary(),
        });
    }
    Ok(DiffPlan { root, entries })
}
