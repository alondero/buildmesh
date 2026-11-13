//! The diff engine: line diffing, hunk grouping, and the baseline walk.
//!
//! This is the machinery the IPC boundary used to own. The line diff,
//! `CONTEXT_LINES` grouping, escaping rules and hunk headers match what
//! `commands/diff.rs` produced before the move (issue #2527). The hunk
//! rendering is now lean: production output leaves the legacy whole-hunk
//! HTML fields empty (issue #2726).

use std::path::Path;
use std::sync::atomic::AtomicBool;

use difference_rs::{Changeset, Difference};

use crate::env::to_host_path;
use crate::models::{DiffCounts, DiffHunk, DiffLine, DiffResult, FileDiff};

use super::cancel::cancel_requested;
#[cfg(test)] // allow-test-in-production: the legacy whole-hunk renderer is a test seam.
use super::highlight::highlight_content;
use super::highlight::highlight_group_lines;
use super::DiffError;

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

/// Number of unchanged context lines to show on each side of a change
/// region. Mirrors the default in `git diff -U3` and GitHub's collapsed
/// diff view, so the user sees the change with enough surrounding code to
/// orient themselves but is not flooded with the whole file.
pub const CONTEXT_LINES: usize = 3;

/// Compute a diff between two strings, returning DiffLine structs
pub fn compute_file_diff(old_content: &str, new_content: &str) -> Vec<DiffLine> {
    let changeset = Changeset::new(old_content, new_content, "\n");
    let mut lines = Vec::new();
    let mut old_num = 1usize;
    let mut new_num = 1usize;

    for diff in changeset.diffs {
        match diff {
            Difference::Same(text) => {
                for line in text.lines() {
                    lines.push(DiffLine {
                        line_type: "context".to_string(),
                        content: line.to_string(),
                        old_num: Some(old_num),
                        new_num: Some(new_num),
                    });
                    old_num += 1;
                    new_num += 1;
                }
            }
            Difference::Add(text) => {
                for line in text.lines() {
                    lines.push(DiffLine {
                        line_type: "add".to_string(),
                        content: line.to_string(),
                        old_num: None,
                        new_num: Some(new_num),
                    });
                    new_num += 1;
                }
            }
            Difference::Rem(text) => {
                for line in text.lines() {
                    lines.push(DiffLine {
                        line_type: "remove".to_string(),
                        content: line.to_string(),
                        old_num: Some(old_num),
                        new_num: None,
                    });
                    old_num += 1;
                }
            }
        }
    }
    lines
}

/// Count the added and removed lines between two texts.
///
/// Uses the same `difference_rs` line split as [`compute_file_diff`], so a
/// caller that only needs the two totals gets exactly the counts the full
/// diff reports — without allocating a `DiffLine` for every line.
pub fn count_file_changes(old_content: &str, new_content: &str) -> (usize, usize) {
    let changeset = Changeset::new(old_content, new_content, "\n");
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for diff in changeset.diffs {
        match diff {
            Difference::Same(_) => {}
            Difference::Add(text) => additions += text.lines().count(),
            Difference::Rem(text) => deletions += text.lines().count(),
        }
    }
    (additions, deletions)
}

/// Build highlighted old/new content strings from diff lines.
///
/// Test-only: the production hunk path emits per-line HTML instead of these
/// whole-hunk strings (issue #2726).
#[cfg(test)] // allow-test-in-production: the legacy whole-hunk renderer is a test seam.
pub fn build_sides(lines: &[DiffLine]) -> (String, String) {
    let mut old_lines: Vec<&str> = Vec::new();
    let mut new_lines: Vec<&str> = Vec::new();

    for line in lines {
        match line.line_type.as_str() {
            "context" => {
                old_lines.push(&line.content);
                new_lines.push(&line.content);
            }
            "add" => {
                old_lines.push("");
                new_lines.push(&line.content);
            }
            "remove" => {
                old_lines.push(&line.content);
                new_lines.push("");
            }
            _ => {}
        }
    }

    (old_lines.join("\n"), new_lines.join("\n"))
}

/// Split a flat diff into hunks with bounded context. Long stretches of
/// unchanged lines that fall outside the context window around any change
/// are dropped. Adjacent change regions whose context windows overlap
/// (i.e. the gap between them is ≤ 2 * CONTEXT_LINES) are merged into a
/// single hunk. Returns an empty vec if the diff contains no changes.
pub fn group_into_hunks(lines: &[DiffLine], context: usize) -> Vec<Vec<DiffLine>> {
    let change_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.line_type != "context")
        .map(|(i, _)| i)
        .collect();

    if change_indices.is_empty() {
        return vec![];
    }

    // Window of [i - context, i + context] around each change, clamped to
    // the diff line range. End is exclusive (Rust range convention).
    let mut windows: Vec<(usize, usize)> = change_indices
        .iter()
        .map(|&i| {
            let start = i.saturating_sub(context);
            let end = (i + context + 1).min(lines.len());
            (start, end)
        })
        .collect();

    // Merge overlapping / touching windows so that close changes share
    // their context lines and render as a single hunk.
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for w in windows.drain(..) {
        if let Some(last) = merged.last_mut() {
            if w.0 <= last.1 {
                last.1 = last.1.max(w.1);
                continue;
            }
        }
        merged.push(w);
    }

    merged
        .iter()
        .map(|(start, end)| lines[*start..*end].to_vec())
        .collect()
}

/// Which whole-hunk HTML a caller needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkHighlight {
    /// Per-line HTML only. The desktop diff view renders `lines_highlighted`;
    /// the mobile web and Android clients render `lines[].content` as text.
    /// The legacy whole-hunk fields stay empty, so no unused HTML is built or
    /// sent (issue #2726).
    LinesOnly,
    /// Also build the legacy whole-hunk `old_highlighted` / `new_highlighted`
    /// HTML. This is a test-only seam: the payload/work comparison pins the
    /// old representation, while no production path can request it.
    #[cfg(test)]
    // allow-test-in-production: the payload test requests the legacy representation.
    Sides,
}

/// Build a single DiffHunk for a group of diff lines, computing per-line
/// syntax highlighting and hunk header metadata (old_start / new_start).
///
/// This is the lean path: the whole-hunk HTML fields stay empty because no
/// current client reads them (issue #2726). The legacy whole-hunk renderer is
/// a test-only seam behind [`HunkHighlight::Sides`], which does not exist in
/// production builds.
pub fn build_hunk(group: &[DiffLine], highlight_path: &str) -> DiffHunk {
    build_hunk_with(group, highlight_path, HunkHighlight::LinesOnly)
}

/// Build a single DiffHunk with an explicit hunk-rendering seam.
///
/// Production constructs only [`HunkHighlight::LinesOnly`]; the `Sides`
/// branch exists for tests that measure the legacy payload.
pub fn build_hunk_with(
    group: &[DiffLine],
    highlight_path: &str,
    whole_hunk_html: HunkHighlight,
) -> DiffHunk {
    #[cfg(test)]
    // allow-test-in-production: the hunk counter is the seam the counts-only read is measured with.
    super::work_metrics::hunk();
    let (old_hl, new_hl) = match whole_hunk_html {
        HunkHighlight::LinesOnly => (String::new(), String::new()),
        #[cfg(test)]
        // allow-test-in-production: the payload test requests the legacy representation.
        HunkHighlight::Sides => {
            let (old_highlighted, new_highlighted) = build_sides(group);
            (
                highlight_content(&old_highlighted, highlight_path),
                highlight_content(&new_highlighted, highlight_path),
            )
        }
    };
    // hunk header: oldest new/old line number in this group, plus the
    // count of remove/add lines (matches the previous single-hunk
    // convention; consumers that want total hunk length can read lines.len()).
    let old_start = group.iter().find_map(|l| l.old_num).unwrap_or(0);
    let new_start = group.iter().find_map(|l| l.new_num).unwrap_or(0);
    let old_lines = group.iter().filter(|l| l.line_type == "remove").count();
    let new_lines = group.iter().filter(|l| l.line_type == "add").count();
    let lines_highlighted = highlight_group_lines(group, highlight_path);
    DiffHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        old_highlighted: old_hl,
        new_highlighted: new_hl,
        lines: group.to_vec(),
        lines_highlighted,
    }
}

/// Read a changed file's working-tree bytes and decide whether the delta is
/// binary.
///
/// Both the full diff and the counts-only read use this, so they classify the
/// same files as binary from the same three signals: libgit2's own verdict,
/// a NUL in the current bytes, and a NUL in the baseline text.
fn entry_content(root: &Path, entry: &crate::git::diff::DiffEntry) -> (Option<Vec<u8>>, bool) {
    let raw = std::fs::read(root.join(&entry.path)).ok();
    let is_binary = entry.binary
        || raw.as_ref().map(|b| b.contains(&0)).unwrap_or(false)
        || entry.old_content.as_bytes().contains(&0);
    (raw, is_binary)
}

/// Diff the working directory against the merge-base with `base_ref`, so the
/// result is everything the agent changed since branching — committed *and*
/// uncommitted (ADR 0005). `only`, when set, restricts to one repo-relative
/// path. Files are returned sorted by path for a stable review order.
///
/// `cancel`, when set, is polled between deltas and between per-file hunks;
/// a flipped flag returns the cancellation sentinel so the calling
/// `#[command]` wrapper can short-circuit the blocking pool task
/// (issue #1181).
pub fn diff_against_base(
    repo_path: &str,
    base_ref: &str,
    only: Option<&str>,
    cancel: Option<&AtomicBool>,
) -> Result<DiffResult, DiffError> {
    diff_against_base_impl(repo_path, base_ref, only, cancel, HunkHighlight::LinesOnly)
}

/// [`diff_against_base`] with an explicit hunk-rendering seam.
///
/// The production read is the lean `LinesOnly` path. The test-only `Sides`
/// variant pins the legacy whole-hunk representation for the payload/work
/// comparison (issue #2726).
fn diff_against_base_impl(
    repo_path: &str,
    base_ref: &str,
    only: Option<&str>,
    cancel: Option<&AtomicBool>,
    hunk_html: HunkHighlight,
) -> Result<DiffResult, DiffError> {
    // Reject path traversal at the core so every caller inherits it — the
    // desktop IPC path and the mobile /diff route both land here. The HTTP
    // route keeps its own check to answer with a 400 before we ever open
    // the repo.
    if let Some(p) = only {
        if p.split(['/', '\\'])
            .any(|seg| seg == ".." || seg.is_empty())
        {
            return Err(DiffError::InvalidPath);
        }
    }
    let host = to_host_path(repo_path);
    let plan = crate::git::diff::diff_plan(
        &host,
        &crate::git::state::Baseline::BaseRef(base_ref.to_string()),
        only,
    )
    .map_err(|e| DiffError::Git(e.to_string()))?;
    let root = plan.root;
    let mut files = Vec::new();

    for entry in plan.entries {
        // Poll cancellation at the file boundary — cheap load on every
        // delta, and the walk is delta-bounded so we can't park here
        // long. Pairs with the per-hunk poll below for files whose
        // highlight passes dominate the cost.
        if cancel_requested(cancel) {
            return Err(DiffError::Cancelled);
        }
        let (raw, is_binary) = entry_content(&root, &entry);
        let rel = entry.path;
        let status = entry.status;
        let old_path = entry.old_path;
        let old_content = entry.old_content;

        if is_binary {
            files.push(FileDiff {
                path: rel,
                status,
                old_path,
                binary: true,
                ..Default::default()
            });
            continue;
        }

        let new_content = raw
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .unwrap_or_default();

        let lines = compute_file_diff(&old_content, &new_content);
        let additions = lines.iter().filter(|l| l.line_type == "add").count();
        let deletions = lines.iter().filter(|l| l.line_type == "remove").count();
        let groups = group_into_hunks(&lines, CONTEXT_LINES);
        // Per-hunk poll — the per-line syntect pass is the costliest step
        // on a busy worktree, so a single huge file would otherwise pin a
        // worker for seconds after cancellation. The check is one atomic
        // load; syntect dominates the cost by orders of magnitude.
        let hunks = groups
            .iter()
            .map(|g| {
                if cancel_requested(cancel) {
                    return Err(());
                }
                Ok(build_hunk_with(g, &rel, hunk_html))
            })
            .collect::<Result<Vec<_>, ()>>()
            .map_err(|_| DiffError::Cancelled)?;

        files.push(FileDiff {
            path: rel,
            hunks,
            status: status.to_string(),
            old_path,
            additions,
            deletions,
            binary: false,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(DiffResult { files })
}

/// Count the added and removed lines against the merge-base with `base_ref`.
///
/// This is [`diff_against_base`] without the hunk construction and syntax
/// highlighting: same baseline, same delta walk, same binary classification,
/// same `difference_rs` line split and counts. It exists for the
/// turn-finished banner, which displays only the two totals (issue #2726).
///
/// `cancel` is polled at every file boundary. The full walk also polls
/// between hunks, which this read never reaches.
pub fn diff_counts_against_base(
    repo_path: &str,
    base_ref: &str,
    cancel: Option<&AtomicBool>,
) -> Result<DiffCounts, DiffError> {
    let host = to_host_path(repo_path);
    let plan = crate::git::diff::diff_plan(
        &host,
        &crate::git::state::Baseline::BaseRef(base_ref.to_string()),
        None,
    )
    .map_err(|e| DiffError::Git(e.to_string()))?;
    let root = plan.root;
    let mut counts = DiffCounts::default();

    for entry in plan.entries {
        if cancel_requested(cancel) {
            return Err(DiffError::Cancelled);
        }
        let (raw, is_binary) = entry_content(&root, &entry);
        if is_binary {
            continue;
        }
        let new_content = raw
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .unwrap_or_default();
        let (additions, deletions) = count_file_changes(&entry.old_content, &new_content);
        counts.additions += additions;
        counts.deletions += deletions;
    }

    Ok(counts)
}
