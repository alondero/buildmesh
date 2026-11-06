//! The diff engine, off the IPC boundary (issue #2527).
//!
//! A `#[command]` is an adapter: validate, call one service or `db::`
//! function, map the error to `String`. The diff view needed more than that
//! — it needed a diff engine — so the engine moved here, out of
//! `commands/diff.rs`, and the command layer became the adapter it is
//! supposed to be.
//!
//! The split:
//!
//! - [`engine`] — the line diff, the hunk grouping and the baseline walk.
//! - [`highlight`] — the syntect themes and the HTML escaping rules.
//! - [`baseline`] — which repository and which ref a change set is read
//!   against.
//! - [`cancel`] — the per-node cancellation registry.
//!
//! The **counts** over a change set are not here: they are a git fact, so
//! they live with the git read that produces the list —
//! [`crate::git::state::summarize_statuses`] is the one implementation of the
//! change-set counting policy, and [`node_changed_summary`] folds the very
//! same list the file tree renders (issue #2527, Move B).
//!
//! The git side of the diff — the baseline tree, the delta walk and the
//! line counts — stays in [`crate::git`], which owns every `git2` use. The
//! merge-base rule of ADR 0005 has exactly one definition in the crate,
//! [`crate::git::diff::resolve_base_tree`], and both readers hang off that
//! one seam.
//!
//! Nothing here changes the diff's output: the same `difference_rs` line
//! diff, the same `CONTEXT_LINES` grouping, the same hunk headers and the
//! same escaping. The wire types (`models::DiffResult` and friends) are
//! unchanged.

pub mod baseline;
pub mod cancel;
pub mod engine;
pub mod highlight;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::git::state::{ChangedFile, StatusSummary};
use crate::models::{DiffResult, FileDiff};

use cancel::DIFF_CANCELLED;

/// Why a diff read failed.
#[derive(Debug)]
pub enum DiffError {
    /// The node or mesh row could not be read.
    Db(String),
    /// The repository could not be read.
    Git(String),
    /// A fresher request for the same node cancelled this one.
    Cancelled,
    /// The requested path was not a repository-relative path.
    InvalidPath,
}

impl std::fmt::Display for DiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiffError::Db(message) => write!(f, "{message}"),
            DiffError::Git(message) => write!(f, "{message}"),
            // The sentinel the frontend's race guard matches. It is a fixed
            // string, not a rendered message: the guard compares it exactly.
            DiffError::Cancelled => write!(f, "{DIFF_CANCELLED}"),
            DiffError::InvalidPath => write!(f, "invalid path"),
        }
    }
}

impl std::error::Error for DiffError {}

/// Diff two files on disk.
///
/// Offloaded by the caller: the file reads and the three syntect highlight
/// passes per hunk are CPU/IO work that would otherwise park a Tauri async
/// worker (issue #762).
pub fn diff_files(old_path: &str, new_path: &str) -> Result<DiffResult, DiffError> {
    let old_content = std::fs::read_to_string(old_path).unwrap_or_default();
    let new_content = std::fs::read_to_string(new_path).unwrap_or_default();
    let lines = engine::compute_file_diff(&old_content, &new_content);
    let groups = engine::group_into_hunks(&lines, engine::CONTEXT_LINES);
    let hunks = groups
        .iter()
        .map(|group| engine::build_hunk(group, new_path))
        .collect();

    Ok(DiffResult {
        files: vec![FileDiff {
            path: new_path.to_string(),
            hunks,
            ..Default::default()
        }],
    })
}

/// Diff a single file against `HEAD`.
pub fn file_against_head(repo_path: &str, file_path: &str) -> Result<DiffResult, DiffError> {
    let head = crate::git::diff::head_file_at(repo_path, file_path)
        .map_err(|e| DiffError::Git(e.to_string()))?;

    let current_content = std::fs::read_to_string(head.root.join(file_path)).unwrap_or_default();

    let old_content = head.content.as_deref().unwrap_or("");
    let lines = engine::compute_file_diff(old_content, &current_content);
    let groups = engine::group_into_hunks(&lines, engine::CONTEXT_LINES);
    let hunks = groups
        .iter()
        .map(|group| engine::build_hunk(group, file_path))
        .collect();

    Ok(DiffResult {
        files: vec![FileDiff {
            path: file_path.to_string(),
            hunks,
            ..Default::default()
        }],
    })
}

/// Every file an Agent Node changed since it branched — committed *and*
/// uncommitted (ADR 0005) — with its per-file diff.
///
/// This is the **Node Change Set** (CONTEXT.md): the files the node touched
/// since it branched, each with the hunks that changed.
///
/// The walk is cancellable: a fresher request for the same node flips the
/// prior flag, so a stale walk exits at its next poll (issue #1181).
pub fn node_change_set(node_id: i64) -> Result<Vec<FileDiff>, DiffError> {
    node_change_set_of(node_id, None)
}

/// One file of an Agent Node's change set.
pub fn node_file_change_set(node_id: i64, file_path: &str) -> Result<Vec<FileDiff>, DiffError> {
    node_change_set_of(node_id, Some(file_path))
}

/// The files a repository changed against a base ref — committed *and*
/// uncommitted — as lightweight status entries.
///
/// The read is [`crate::git::state::changed_files_at`], the same one the
/// Changed Files Section uses, and both views count lines through the shared
/// implementation, so their line counts agree. Their labels stay each view's
/// own vocabulary — the mobile Changes view stitches both into one view. It
/// skips the hunk building and syntect highlighting the full diff pays for —
/// a status entry needs only path, classification, and line counts.
pub fn changed_files_at(repo_path: &str, base_ref: &str) -> Result<Vec<ChangedFile>, DiffError> {
    crate::git::state::changed_files_at(
        repo_path,
        crate::git::state::Baseline::BaseRef(base_ref.to_string()),
    )
    .map_err(|e| DiffError::Git(e.to_string()))
}

/// The files an Agent Node changed since it branched — committed *and*
/// uncommitted — as lightweight status entries.
pub fn node_changed_files(node_id: i64) -> Result<Vec<ChangedFile>, DiffError> {
    let baseline = node_baseline(node_id)?;
    changed_files_at(&baseline.repo_path, &baseline.base_ref)
}

/// The counts over the files an Agent Node changed since it branched.
///
/// Folded from the very same list [`node_changed_files`] returns, so the
/// header and the tree can never disagree. The policy is
/// [`crate::git::state::changed_files_summary`]'s — a git fact, counted
/// once.
pub fn node_changed_summary(node_id: i64) -> Result<StatusSummary, DiffError> {
    let files = node_changed_files(node_id)?;
    Ok(crate::git::state::changed_files_summary(&files))
}

/// The change set of a node, optionally restricted to one path.
fn node_change_set_of(node_id: i64, only: Option<&str>) -> Result<Vec<FileDiff>, DiffError> {
    let token = cancel::acquire_diff_cancel(node_id);
    let _guard = cancel::DiffCancelGuard::new(node_id, Arc::clone(&token));
    if token.load(Ordering::SeqCst) {
        return Err(DiffError::Cancelled);
    }
    let baseline = node_baseline(node_id)?;
    let result = engine::diff_against_base(
        &baseline.repo_path,
        &baseline.base_ref,
        only,
        Some(&token),
    )?;
    Ok(result.files)
}

/// The node's repository and the ref its change set is measured against.
fn node_baseline(node_id: i64) -> Result<baseline::NodeBaseline, DiffError> {
    let node = crate::db::get_agent_node_by_id(node_id).map_err(|e| DiffError::Db(e.to_string()))?;
    Ok(baseline::node_baseline(&node))
}
