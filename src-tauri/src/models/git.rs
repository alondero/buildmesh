//! Git, diff, and worktree wire types.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// File change event from the watcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: String, // "created" | "modified" | "deleted"
}

/// Diff result — a set of per-file diffs.
///
/// Generated to src/types/generated/DiffResult.ts (issue #404). `usize`
/// counters carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "DiffResult.ts")]
pub struct DiffResult {
    pub files: Vec<FileDiff>,
}

/// A single file diff.
///
/// Generated to src/types/generated/FileDiff.ts (issue #404). `usize`
/// counters carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[ts(export, export_to = "FileDiff.ts")]
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<DiffHunk>,
    /// Change kind: "added" | "modified" | "deleted" | "renamed" | "untracked".
    /// Matches the vocabulary `GitStatus.status` uses on the frontend.
    #[serde(default)]
    pub status: String,
    /// For renames, the path the file moved *from*; `None` otherwise.
    #[serde(default)]
    pub old_path: Option<String>,
    /// Added / removed line counts across the whole file (not just the
    /// context-bounded hunks), so summaries match `git diff --stat`.
    #[serde(default)]
    #[ts(as = "i32")]
    pub additions: usize,
    #[serde(default)]
    #[ts(as = "i32")]
    pub deletions: usize,
    /// True for binary files — `hunks` is empty and the UI shows a placeholder
    /// instead of dumping bytes.
    #[serde(default)]
    pub binary: bool,
}

/// A hunk within a diff.
///
/// Generated to src/types/generated/DiffHunk.ts (issue #404). `usize` hunk
/// line counters carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[ts(export, export_to = "DiffHunk.ts")]
pub struct DiffHunk {
    #[ts(as = "i32")]
    pub old_start: usize,
    #[ts(as = "i32")]
    pub old_lines: usize,
    #[ts(as = "i32")]
    pub new_start: usize,
    #[ts(as = "i32")]
    pub new_lines: usize,
    /// Full highlighted HTML for the old version of this hunk (side-by-side view)
    pub old_highlighted: String,
    /// Full highlighted HTML for the new version of this hunk (side-by-side view)
    pub new_highlighted: String,
    pub lines: Vec<DiffLine>,
    /// Per-line highlighted inline HTML, aligned 1:1 with `lines`. Lets the
    /// unified view colour each row with syntax highlighting while keeping its
    /// own add/remove background and gutter. Empty for producers that only feed
    /// the side-by-side view.
    #[serde(default)]
    pub lines_highlighted: Vec<String>,
}

/// A single line in a diff.
///
/// Generated to src/types/generated/DiffLine.ts (issue #404). `Option<usize>`
/// line numbers carry `#[ts(as = "Option<i32>")]` so they emit `number | null`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "DiffLine.ts")]
pub struct DiffLine {
    pub line_type: String,   // "context" | "add" | "remove"
    pub content: String,
    #[ts(as = "Option<i32>")]
    pub old_num: Option<usize>,
    #[ts(as = "Option<i32>")]
    pub new_num: Option<usize>,
}

/// Per-repo git prune info — branches, worktrees, and remote-tracking refs.
///
/// Generated to src/types/generated/GitRepoPruneInfo.ts (issue #404).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitRepoPruneInfo.ts")]
pub struct GitRepoPruneInfo {
    pub path: String,
    pub local_branches: Vec<BranchInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub remote_tracking_branches: Vec<String>,
}

/// A local branch and its prune-relevant metadata.
///
/// Generated to src/types/generated/BranchInfo.ts (issue #404). `u64` counters
/// carry `#[ts(as = "i32")]` so they emit `number`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "BranchInfo.ts")]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    /// None when the repo has no main/master branch to compare against.
    pub is_merged_into_main: Option<bool>,
    pub is_orphan: bool,
    /// `true` when a non-archived agent node has this branch checked out
    /// (sibling of `WorktreeInfo.is_active`). Mirrors the worktree's
    /// protection in the prune UI: the row's checkbox is disabled and a
    /// backend guard in `commands::prune::delete_branches` refuses to
    /// drop the branch — the user must close the node first.
    pub is_active: bool,
    /// `Some(worktree_path)` when this branch is the HEAD of *any* working
    /// tree on disk — main or linked worktree, agent-attached or orphan.
    /// Orthogonal to `is_active`: that field is about a live agent node;
    /// this one is about the on-disk git state. An orphan agent worktree
    /// (its node was deleted/archived but its directory survives) reads
    /// `is_active: false` here, so the user can no longer hit libgit2's
    /// "current HEAD of a linked repository" error from the prune UI.
    /// The UI uses this to disable the branch checkbox and direct the
    /// user to the worktree row above (whose delete already cascades to
    /// the branch via `remove_one_worktree_and_branch`).
    pub checked_out_in_worktree: Option<String>,
    pub has_uncommitted: bool,
    pub last_commit_date: Option<String>,
    #[ts(as = "i32")]
    pub ahead: u64,
    #[ts(as = "i32")]
    pub behind: u64,
}

/// A worktree (main or linked) and its prune-relevant metadata.
///
/// Generated to src/types/generated/WorktreeInfo.ts (issue #404).
///
/// `is_pool` distinguishes a pre-spawn pool entry from a normal worktree
/// (issue #611). Pool entries are detached-HEAD worktrees under
/// `{mesh.path}/.claude/worktrees/<slug>` whose path matches a row in
/// the `warm_worktrees` table; the Worktree Manager tab shows them with
/// a "Pre-spawn Pool" badge and disables the delete action so the
/// background worker can refill on demand. Always `false` for the
/// primary (repo-root) worktree — pool entries are always linked.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "WorktreeInfo.ts")]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub is_active: bool,
    pub is_stale: bool,
    pub is_pool: bool,
}
/// A worktree that currently holds the Base Ref's branch checked out,
/// blocking `git checkout <base>` from the Mesh root. Returned by
/// `get_mesh_health` as part of `MeshHealth.base_branch_holder`.
///
/// `path` is the on-disk worktree path (host-normalised). `name` is the
/// basename for display. `is_active` reflects whether a non-archived
/// agent node currently points at the path (active worktrees can't be
/// freely deleted but can be safely detached via `free_base_branch`).
///
/// Generated to src/types/generated/HoldingWorktree.ts (issue #404).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "HoldingWorktree.ts")]
pub struct HoldingWorktree {
    pub path: String,
    pub name: String,
    pub is_active: bool,
}

/// A single-snapshot read of a Mesh's Git health — what the Base Ref
/// resolved to, where HEAD actually is, and whether recovery is
/// currently safe. Computed by `commands::git::compute_mesh_health` and
/// returned over IPC by the `get_mesh_health` Tauri command.
///
/// Detection rules (see the `MeshHealth` doc on the command side for the
/// full reasoning):
/// - `local_base_branch` is derived from `base_ref` (e.g. `origin/main` → `main`).
/// - `is_drifted = true` when `current_branch != local_base_branch` or HEAD
///   is detached on a non-base OID. Detached at the base branch's OID is
///   not drifted — close enough to base that no badge is needed.
/// - `unpushed_ahead` is 0 when there is no upstream; a no-upstream branch
///   with local commits still triggers the "unpushed" guard because the
///   branch ref is the only handle to those commits.
/// - `base_branch_holder` is `Some` when the Base Ref's branch is checked
///   out in any of the Mesh's worktrees (main or linked).
///
/// Generated to src/types/generated/MeshHealth.ts (issue #404). `u32`
/// counter carries `#[ts(as = "i32")]` so it emits `number`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "MeshHealth.ts")]
pub struct MeshHealth {
    pub base_ref: String,
    pub local_base_branch: Option<String>,
    pub current_branch: Option<String>,
    pub current_short_sha: String,
    pub is_detached: bool,
    pub is_dirty: bool,
    #[ts(as = "i32")]
    pub unpushed_ahead: u32,
    pub has_upstream: bool,
    pub is_drifted: bool,
    pub base_branch_holder: Option<HoldingWorktree>,
}
