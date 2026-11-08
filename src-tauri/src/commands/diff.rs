//! The diff commands — thin adapters over the [`crate::diff`] module.
//!
//! The engine this file used to own (the line diff, the hunk grouping, the
//! syntect highlighting and the cancellation registry) moved into
//! [`crate::diff`] (issue #2527). What is left is the adapter the module map
//! asks for: validate, call one `diff::` or `git::` function, map the error
//! to `String`. There is no `syntect` import, no `difference_rs` import, no
//! `DIFF_NODE_CANCEL` and no `resolve_base_tree` under `commands/`.
//!
//! The git2 side of the diff lives in [`crate::git::diff`] and
//! [`crate::git::state`]: this module asks for a baseline and gets plain
//! data back.

use crate::commands::git::{GitStatus, GitSummary};
use crate::models::{DiffCounts, DiffResult};
use tauri::command;

/// Diff two files on disk.
// Offloaded via `run_blocking` (issue #762 convention): file reads + the
// line diff + three syntect highlight passes per hunk are CPU/IO work that
// would otherwise park a Tauri async worker.
#[command]
pub async fn diff_files(old_path: String, new_path: String) -> Result<DiffResult, String> {
    crate::commands::run_blocking("diff_files", move || {
        crate::diff::diff_files(&old_path, &new_path).map_err(|error| error.to_string())
    })
    .await
}

/// Diff a single file against HEAD.
// Offloaded via `run_blocking`: repo open + tree lookup + highlighting.
#[command]
pub async fn diff_file_against_head(
    session_path: String,
    file_path: String,
) -> Result<DiffResult, String> {
    crate::commands::run_blocking("diff_file_against_head", move || {
        crate::diff::file_against_head(&session_path, &file_path).map_err(|error| error.to_string())
    })
    .await
}

/// Diff every file an Agent Node changed since it branched (ADR 0005). One call
/// returns the whole change set the review panel renders as a stacked view.
// Offloaded via `run_blocking`: the walk reads the whole working directory
// with libgit2, reads every changed file, and runs three syntect highlight
// passes per hunk — seconds of CPU on a busy worktree. The review panel
// re-fetches it on every `git-changed` event while an agent edits, so an
// inline body is the single biggest pool-parker in the app (the #761/#762
// starvation class: UI alive, keystrokes and probes frozen).
//
// Issue #1181: a fresh arrival flips the prior in-flight flag so the
// stale task exits at its next per-delta / per-hunk poll. This bounds
// pool pressure to one task per `node_id` regardless of the frontend's
// fetch rate.
#[command]
pub async fn diff_node_against_base(node_id: i64) -> Result<DiffResult, String> {
    crate::commands::run_blocking("diff_node_against_base", move || {
        crate::diff::node_change_set(node_id)
            .map(|files| DiffResult { files })
            .map_err(|error| error.to_string())
    })
    .await
}

/// Diff a single file of an Agent Node against its merge-base — the per-file
/// entry point the mobile changes view taps into.
// Offloaded via `run_blocking` — same rationale as `diff_node_against_base`.
//
// Issue #1181: same per-node cancel plumbing as `diff_node_against_base`.
// Rapid file-switching in `CenterDiffOverlay` would otherwise pile up
// per-file `run_blocking` tasks for the same `node_id`; a fresh tap
// flips the prior flag so the stale task exits at its next poll.
#[command]
pub async fn diff_node_file_against_base(
    node_id: i64,
    file_path: String,
) -> Result<DiffResult, String> {
    crate::commands::run_blocking("diff_node_file_against_base", move || {
        crate::diff::node_file_change_set(node_id, &file_path)
            .map(|files| DiffResult { files })
            .map_err(|error| error.to_string())
    })
    .await
}

/// The total added/removed lines in an Agent Node's change set.
///
/// The turn-finished banner's read (issue #2726): same merge-base baseline,
/// file walk, binary classification and line counts as
/// `diff_node_against_base`, without constructing hunks or running syntect.
// Offloaded via `run_blocking` — the walk still reads repository content.
// Issue #1181: same per-node cancel plumbing; the banner's AbortSignal drops
// a stale local result, and a fresh request for the node stops a prior walk.
#[command]
pub async fn diff_node_counts_against_base(node_id: i64) -> Result<DiffCounts, String> {
    crate::commands::run_blocking("diff_node_counts_against_base", move || {
        crate::diff::node_change_counts(node_id).map_err(|error| error.to_string())
    })
    .await
}

/// List the files an Agent Node changed since it branched (ADR 0005) — the
/// tree the mobile Changes view renders. Base_ref-relative so it agrees with
/// the per-file diff endpoint; committed *and* uncommitted work both show.
// Offloaded via `run_blocking` — same libgit2/pool rationale as
// `diff_node_against_base`.
#[command]
pub async fn node_changed_files(node_id: i64) -> Result<Vec<GitStatus>, String> {
    crate::commands::run_blocking("node_changed_files", move || {
        node_changed_files_blocking(node_id)
    })
    .await
}

pub(crate) fn node_changed_files_blocking(node_id: i64) -> Result<Vec<GitStatus>, String> {
    crate::diff::node_changed_files(node_id)
        .map(|files| {
            files
                .into_iter()
                .map(|file| GitStatus {
                    project_path: None,
                    path: file.path,
                    status: file.status,
                    additions: file.additions,
                    deletions: file.deletions,
                })
                .collect()
        })
        .map_err(|error| error.to_string())
}

/// The `{total, added, modified, deleted}` header for the mobile Changes view,
/// folded from the same since-branch list as [`node_changed_files`] so the
/// header and the tree can never disagree.
// Offloaded via `run_blocking` — same rationale as `node_changed_files`.
#[command]
pub async fn node_changed_summary(node_id: i64) -> Result<GitSummary, String> {
    crate::commands::run_blocking("node_changed_summary", move || {
        node_changed_summary_blocking(node_id)
    })
    .await
}

pub(crate) fn node_changed_summary_blocking(node_id: i64) -> Result<GitSummary, String> {
    crate::diff::node_changed_summary(node_id)
        .map(|summary| GitSummary {
            total: summary.total,
            added: summary.added,
            modified: summary.modified,
            deleted: summary.deleted,
        })
        .map_err(|error| error.to_string())
}
