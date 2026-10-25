//! Branch and worktree enumeration, and branch deletion (issue #2526).
//!
//! The Worktree Manager's prune view is a read of the repository: its local
//! branches, its worktrees, its remote-tracking refs, and — for each branch —
//! how far ahead or behind its upstream is and whether its work is already
//! on `main`. That read, and the branch deletion it enables, belong to `git/`
//! so the command layer never holds a `Repository` handle.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use git2::{BranchType, Oid, Repository};

use crate::models::{BranchInfo, GitRepoPruneInfo, WorktreeInfo};

use super::primitives;

#[cfg(test)]
#[path = "branches_tests.rs"]
mod tests;

/// Delete local branches by name from the repo at `repo_path`.
///
/// Active branches are rejected up-front with a single combined error so the
/// user sees the full blocked set in one toast, not multiple per-branch errors
/// mixed with successful deletes. Mirrors the pool-path rejection in the
/// worktree removal path.
///
/// Note on the "continue past failures" contract: the original function
/// accumulated per-branch git2 errors and still attempted the rest of the
/// batch. The active-branch carve-out intentionally departs from that — a
/// request containing any active branch is rejected atomically, because
/// silently deleting the deletable siblings would leave the user with a
/// partial state they may not want.
pub(crate) fn delete_branches_in_repo(
    repo_path: &str,
    branch_names: &[String],
    active_branches: &[String],
) -> Result<(), String> {
    let repo = primitives::open_at(repo_path).map_err(|e| e.to_string())?;

    let (active_blocked, deletable): (Vec<&String>, Vec<&String>) = branch_names
        .iter()
        .partition(|name| active_branches.iter().any(|b| b == *name));
    reject_blocked(
        &active_blocked,
        "cannot delete branches held by an active agent node",
    )?;

    let mut errors: Vec<String> = Vec::new();
    for name in deletable {
        match repo.find_branch(name, BranchType::Local) {
            Ok(mut branch) => {
                if let Err(e) = branch.delete() {
                    errors.push(format!("{}: {}", name, e));
                }
            }
            Err(e) => errors.push(format!("{}: {}", name, e)),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Pure enumeration: given a repo path, the set of active node paths and
/// branches, and an `is_pool_path` predicate, build the prune info. No DB
/// access — the caller supplies everything. The three siblings
/// (`active_paths` for worktrees, `active_branches` for branches,
/// `is_pool_path` for pre-spawn pool entries) feed the row flags; lifting
/// the pool check out of this function removes a latent test-isolation
/// race that surfaced when new branch-active tests started exercising
/// the worktree enumeration path.
///
/// Three passes, in dependency order:
/// 1. Cheap local-branch name list — feeds worktree `is_stale`.
/// 2. Worktrees (main + linked) + a `branch → worktree_path` map so the
///    branch pass below can stamp `checked_out_in_worktree` on every
///    branch that is HEAD of some working tree (live or orphan).
/// 3. Full `BranchInfo` list with worktree annotation.
///
/// The squash-merge classification runs under a wall-clock budget
/// ([`MERGE_SCAN_BUDGET`]). A budget that runs out truncates the
/// classification, not the read: the affected branches report
/// `is_merged_into_main: None` (unknown) and the returned info carries
/// `merge_scan_truncated: true` (issue #2580).
pub(crate) fn collect_prune_info(
    repo_path: &str,
    active_paths: &[String],
    active_branches: &[String],
    is_pool_path: &dyn Fn(&str) -> bool,
) -> Result<GitRepoPruneInfo, String> {
    let mut budget = MergeScanBudget::new(MERGE_SCAN_BUDGET);
    collect_prune_info_within(
        repo_path,
        active_paths,
        active_branches,
        is_pool_path,
        &mut budget,
    )
}

/// [`collect_prune_info`] with a caller-supplied budget.
///
/// [`collect_prune_info`] uses [`MERGE_SCAN_BUDGET`]; this variant exists so a
/// test can pin the budget (a zero-length one truncates the scan
/// immediately) and assert what the read reports when it runs out.
pub(crate) fn collect_prune_info_within(
    repo_path: &str,
    active_paths: &[String],
    active_branches: &[String],
    is_pool_path: &dyn Fn(&str) -> bool,
    budget: &mut MergeScanBudget,
) -> Result<GitRepoPruneInfo, String> {
    let repo = primitives::open_at(repo_path).map_err(|e| e.to_string())?;

    let main_oid = main_branch_oid(&repo);
    let head_dirty = primitives::is_dirty(&repo).unwrap_or(false);

    // ── Pass 1: cheap local-branch name list ────────────────────────────
    let mut local_names: Vec<String> = Vec::new();
    for entry in repo
        .branches(Some(BranchType::Local))
        .map_err(|e| e.to_string())?
    {
        let (branch, _) = entry.map_err(|e| e.to_string())?;
        if let Ok(Some(name)) = branch.name() {
            local_names.push(name.to_string());
        }
    }

    // ── Pass 2: worktrees (main + linked) + branch → worktree-path map ──
    //
    // The map captures every branch checked out as the HEAD of some
    // working tree, regardless of whether an agent node still points at
    // it. A branch's value here drives `BranchInfo.checked_out_in_worktree`
    // below, which is what stops the libgit2 "current HEAD of a linked
    // repository" error in the prune UI: orphan agent worktrees
    // (whose node was deleted/archived but whose directory survives)
    // don't trip `is_active`, but they *do* appear in this map.
    let mut worktrees: Vec<WorktreeInfo> = Vec::new();
    let mut branch_to_wt_path: HashMap<String, String> = HashMap::new();

    if let Some(workdir) = repo.workdir() {
        let path = workdir
            .to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .to_string();
        let branch = primitives::head_branch_name(&repo);
        if let Some(ref b) = branch {
            branch_to_wt_path.insert(b.clone(), path.clone());
        }
        worktrees.push(WorktreeInfo {
            is_active: path_is_active(&path, active_paths),
            is_stale: branch_is_stale(&branch, &local_names),
            // Primary (repo-root) worktree is never a pool entry — pool
            // entries are always linked worktrees under
            // `.claude/worktrees/<slug>`. The predicate comes from the
            // caller (production: `db::is_warm_pool_path`; tests: `|_| false`)
            // so this function stays DB-free.
            is_pool: is_pool_path(&path),
            branch,
            path,
        });
    }

    if let Ok(names) = repo.worktrees() {
        for wt_name in names.iter().filter_map(|name| name.ok().flatten()) {
            let wt = match repo.find_worktree(wt_name) {
                Ok(w) => w,
                Err(_) => continue,
            };
            let path = wt
                .path()
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_string();
            let branch = Repository::open(wt.path())
                .ok()
                .and_then(|r| primitives::head_branch_name(&r));
            if let Some(ref b) = branch {
                branch_to_wt_path.insert(b.clone(), path.clone());
            }
            worktrees.push(WorktreeInfo {
                is_active: path_is_active(&path, active_paths),
                is_stale: branch_is_stale(&branch, &local_names),
                is_pool: is_pool_path(&path),
                branch,
                path,
            });
        }
    }

    // ── Pass 3: local branches with worktree annotation ─────────────────
    //
    // `patch_ids` is shared across every branch in this pass: a commit's
    // patch-id does not depend on which branch asks for it, so computing it
    // once turns the scan from `O(branches x history)` into
    // `O(history + branches)` (issue #2580).
    let mut patch_ids = PatchIdCache::new(&repo);
    let mut local_branches: Vec<BranchInfo> = Vec::new();
    for entry in repo
        .branches(Some(BranchType::Local))
        .map_err(|e| e.to_string())?
    {
        let (branch, _) = entry.map_err(|e| e.to_string())?;
        let name = match branch.name().map_err(|e| e.to_string())? {
            Some(n) => n.to_string(),
            None => continue, // non-UTF8 branch name
        };

        let is_head = branch.is_head();
        let branch_oid = branch.get().peel_to_commit().ok().map(|c| c.id());
        let last_commit_date = branch
            .get()
            .peel_to_commit()
            .ok()
            .and_then(|c| format_commit_time(c.time()));

        // Upstream resolution → ahead/behind + orphan detection.
        let mut ahead = 0u64;
        let mut behind = 0u64;
        let mut is_orphan = false;
        let refname = format!("refs/heads/{}", name);
        if let Ok(upstream_buf) = repo.branch_upstream_name(&refname) {
            // An upstream is configured for this branch.
            if let Ok(upstream_ref) = upstream_buf.as_str() {
                match repo.find_reference(upstream_ref) {
                    Ok(up_ref) => {
                        if let (Some(local), Some(up)) = (branch_oid, up_ref.target()) {
                            if let Ok((a, b)) = primitives::ahead_behind(&repo, local, up) {
                                ahead = a as u64;
                                behind = b as u64;
                            }
                        }
                    }
                    // Configured upstream no longer exists → orphan.
                    Err(_) => is_orphan = true,
                }
            }
        }

        let is_merged_into_main = match (main_oid, branch_oid) {
            (Some(main), Some(b)) => {
                // `branch_is_merged_into_main` answers from the cheap
                // ancestry check even when the budget is spent, and returns
                // `None` (*unknown*) when the budget stopped the expensive
                // squash scan. A truncated scan must never claim a branch is
                // safe to delete.
                branch_is_merged_into_main(&repo, &mut patch_ids, budget, main, b)
            }
            _ => None,
        };

        // Cross-reference against the active-branch set: a branch is
        // "active" iff its name matches any non-archived agent node's
        // `branch` field. Mirrors the worktree `is_active` derivation
        // (path matching) — same intent, different identity key.
        let is_active = active_branches.iter().any(|b| b == &name);

        // Worktree HEAD annotation — independent of `is_active`. A branch
        // can be HEAD of an orphan worktree (no live node) or of a live
        // one (also `is_active: true`); the two flags overlap on the
        // live case but each covers a separate scenario on its own.
        let checked_out_in_worktree = branch_to_wt_path.get(&name).cloned();

        local_branches.push(BranchInfo {
            name,
            is_head,
            is_merged_into_main,
            is_orphan,
            is_active,
            checked_out_in_worktree,
            has_uncommitted: is_head && head_dirty,
            last_commit_date,
            ahead,
            behind,
        });
    }

    // ── Remote-tracking branches ────────────────────────────────────────
    let mut remote_tracking_branches: Vec<String> = Vec::new();
    if let Ok(branches) = repo.branches(Some(BranchType::Remote)) {
        for entry in branches.flatten() {
            if let Ok(Some(name)) = entry.0.name() {
                if name.ends_with("/HEAD") {
                    continue;
                }
                remote_tracking_branches.push(name.to_string());
            }
        }
    }

    Ok(GitRepoPruneInfo {
        path: repo_path.to_string(),
        local_branches,
        worktrees,
        remote_tracking_branches,
        merge_scan_truncated: budget.was_truncated(),
    })
}

/// Reject a batch request atomically when any of its members are blocked.
///
/// Returns `Err(<header>: <name1>, <name2>, …)` listing every blocked
/// item, or `Ok(())` when the batch is clean. Shared by the branch
/// (`delete_branches_in_repo`) and worktree (`remove_worktrees`) delete
/// paths — both reject any-blocked requests atomically and surface every
/// blocked item in one combined message rather than splitting the error
/// across multiple per-item failures interleaved with successful deletes
/// (issue #661). A stale-UI row or a misrouted direct API call must see
/// every blocked item, not just the first failure.
///
/// Caller responsibility: build `blocked` by filtering the request items
/// with the site-specific predicate first — branch-name membership for
/// `delete_branches`, warm-pool path for `delete_worktrees`. The
/// predicates genuinely differ between the two sites, so this helper is
/// purely the combined-error formatting; the partition itself stays at
/// each call site (typically via `iter().partition(...)`).
pub(crate) fn reject_blocked<T: AsRef<str>>(blocked: &[&T], header: &str) -> Result<(), String> {
    if blocked.is_empty() {
        return Ok(());
    }
    let names = blocked
        .iter()
        .map(|n| n.as_ref())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!("{header}: {names}"))
}

/// Whether a worktree path is one an agent node is using.
pub(crate) fn path_is_active(path: &str, active_paths: &[String]) -> bool {
    let norm = primitives::normalize_for_compare(path);
    active_paths
        .iter()
        .any(|p| primitives::normalize_for_compare(p) == norm)
}

/// A worktree is stale when it points at a branch that no longer exists
/// locally. Detached worktrees (no branch) are never stale.
fn branch_is_stale(branch: &Option<String>, local_names: &[String]) -> bool {
    matches!(branch, Some(b) if !local_names.iter().any(|n| n == b))
}

/// Resolve the tip commit of the repo's `main` (or `master`) branch, if any.
fn main_branch_oid(repo: &Repository) -> Option<Oid> {
    for candidate in ["main", "master"] {
        if let Ok(branch) = repo.find_branch(candidate, BranchType::Local) {
            if let Ok(commit) = branch.get().peel_to_commit() {
                return Some(commit.id());
            }
        }
    }
    None
}

/// Cap on how many commits to scan back from `main` when hunting for the squash
/// commit that integrated a branch.
///
/// A real integration sits within a handful of commits of the tip, so the scan
/// only needs the recent tail of `main`. The cap is what makes that true
/// rather than aspirational: the previous value (2,000) was larger than this
/// repository's entire `main` history, so on a repository of this shape the
/// cap never fired and every unmerged branch paid a walk over the whole
/// history.
///
/// The trade-off is deliberate and one-directional. A squash merge that
/// landed further back than the cap is no longer recognised, so the branch
/// reads as *not merged* and is not auto-recommended for cleanup. That is
/// the safe direction - the user keeps a stale branch instead of losing the
/// signal that told them to delete one - and it is the direction the squash
/// heuristic has always been documented to fail in.
const SQUASH_SCAN_CAP: usize = 200;

/// Wall-clock budget for the squash-merge scan of one prune read.
///
/// The scan is bounded so a repository with many branches over a long `main`
/// history cannot hold the Probe's Repository tab in its loading state for
/// tens of minutes (issue #2580). When the budget runs out the read still
/// returns: the branches it did not classify report `is_merged_into_main` as
/// unknown, and the read flags `merge_scan_truncated` so the UI can say so
/// instead of rendering a silently missing "merged" badge.
///
/// The budget is a safety net, not the normal path. A repository with a
/// normal number of branches classifies in well under a second; the
/// pathological case this repository happens to be (419 local branches over a
/// 1,861-commit `main`) measures at roughly 30-50 seconds, so the budget is
/// set above that: cutting it lower would truncate the read on the very
/// repository the fix is for, and the truncation is durable — the cached
/// answer keeps saying "unknown" until `main` moves.
///
/// A false negative is the safe failure direction here — an unclassified
/// branch is simply not recommended for cleanup — so cutting the scan short
/// can only under-report, never delete work that is still unmerged.
pub(crate) const MERGE_SCAN_BUDGET: Duration = Duration::from_secs(60);

/// The wall-clock budget for one read's merge scan, plus whether it ran out.
///
/// The budget is checked *inside* the commit walk as well as between
/// branches: the first branch to reach a long stretch of `main` is the one
/// that pays for computing its patch-ids, and it must not be able to run past
/// the budget in a single step.
pub(crate) struct MergeScanBudget {
    started: Instant,
    limit: Duration,
    truncated: bool,
}

impl MergeScanBudget {
    /// A budget that allows `limit` of wall-clock time for the scan.
    pub(crate) fn new(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
            truncated: false,
        }
    }

    /// Whether the budget is spent. The scan stops doing further work once
    /// this is true.
    pub(crate) fn expired(&self) -> bool {
        self.started.elapsed() >= self.limit
    }

    /// Record that the scan gave up before classifying every branch.
    fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// Whether the scan gave up before classifying every branch.
    pub(crate) fn was_truncated(&self) -> bool {
        self.truncated
    }
}

/// Per-commit patch-ids, memoised for the lifetime of one prune read.
///
/// A commit's patch-id is a pure function of `(parent, commit)` — it does not
/// depend on which branch asks for it. `main_contains_patch` used to re-diff
/// and re-hash the same commit of `main` once per unmerged branch, so a
/// repository with `N` unmerged branches over an `H`-commit history paid
/// `N x H` tree diffs for a single read: 131,600 diffs on this repository's
/// 418 branches and 1,861-commit `main` (issue #2580). Caching by commit OID
/// makes the cost `O(H + N)`.
///
/// `None` in the map means "this commit has no usable patch-id" — it is a
/// merge commit, or its diff could not be computed. That is the same
/// distinction the pre-memoisation code drew by skipping the commit.
struct PatchIdCache<'repo> {
    repo: &'repo Repository,
    ids: HashMap<Oid, Option<Oid>>,
}

impl<'repo> PatchIdCache<'repo> {
    fn new(repo: &'repo Repository) -> Self {
        Self {
            repo,
            ids: HashMap::new(),
        }
    }

    /// The patch-id of `oid`'s own diff against its first parent, or `None`
    /// when the commit has no single parent or the diff cannot be computed.
    fn patch_id(&mut self, oid: Oid) -> Option<Oid> {
        if let Some(cached) = self.ids.get(&oid) {
            return *cached;
        }
        let computed = self.compute(oid);
        self.ids.insert(oid, computed);
        computed
    }

    fn compute(&self, oid: Oid) -> Option<Oid> {
        let commit = self.repo.find_commit(oid).ok()?;
        // A squash lands as a single-parent commit; skip merge commits, whose
        // patch-id isn't comparable to a linear branch diff.
        if commit.parent_count() != 1 {
            return None;
        }
        let (Ok(tree), Ok(parent_tree)) = (commit.tree(), commit.parent(0).and_then(|p| p.tree()))
        else {
            return None;
        };
        let diff = self
            .repo
            .diff_tree_to_tree(Some(&parent_tree), Some(&tree), None)
            .ok()?;
        diff.patchid(None).ok()
    }
}

/// Whether `branch_oid`'s work is already present in `main_oid`.
///
/// Ancestry catches fast-forward and merge-commit integration. But GitHub's
/// default *squash and merge* rewrites the whole branch into a single new
/// commit on main with no ancestry link back — so an ancestry-only check
/// reports squash-merged branches as unmerged, and they pile up locally
/// forever. A squash commit's patch is, by construction, the branch's
/// *cumulative* diff against the merge base, so we compute that one patch-id
/// and look for a commit on main (since the merge base) whose own patch
/// matches. Rebase-and-merge that collapses to an equal patch is caught the
/// same way. False negatives are safe (the branch just isn't recommended);
/// a patch-id match is strong evidence the identical change is already on main.
///
/// `None` means *unknown* — the merge base could not be found, or the scan ran
/// out of budget before it could answer. It is deliberately not `false`: a
/// truncated scan must not claim a branch is unmerged.
fn branch_is_merged_into_main(
    repo: &Repository,
    patch_ids: &mut PatchIdCache<'_>,
    budget: &mut MergeScanBudget,
    main_oid: Oid,
    branch_oid: Oid,
) -> Option<bool> {
    if main_oid == branch_oid {
        return Some(true);
    }
    // The ancestry check and the merge base are the same question, and the
    // merge base is the cheap way to ask it.
    //
    // The previous code asked `graph_descendant_of(main, branch)` first and
    // then `merge_base(main, branch)`. The ancestry question was what made
    // the read expensive: libgit2 answers it by walking `main`'s *entire*
    // history to prove the branch is not in it, so a repository with
    // hundreds of branches paid a full history walk per branch — roughly
    // 70 seconds of this read on this repository. The merge base stops at
    // the first common ancestor, so it is far cheaper, and
    // `base == branch_oid` answers the ancestry question exactly: the merge
    // base of `main` and `branch` is the branch's own tip precisely when the
    // branch is an ancestor of `main`.
    //
    // The merge base is also what the squash scan needs next, so this
    // replaces one history walk with one, rather than adding a cheap check
    // in front of an expensive one.
    let Ok(base) = repo.merge_base(main_oid, branch_oid) else {
        return Some(false);
    };
    if base == branch_oid {
        return Some(true);
    }

    // From here on the work is a tree diff plus a commit walk, so the budget
    // decides whether it happens at all.
    if budget.expired() {
        budget.mark_truncated();
        return None;
    }

    let cumulative = match cumulative_patch_id(repo, base, branch_oid) {
        // Identical trees → the branch adds nothing main lacks → nothing to
        // lose, so treat it as merged.
        Ok(None) => return Some(true),
        Ok(Some(pid)) => pid,
        Err(()) => return Some(false),
    };

    main_contains_patch(repo, patch_ids, budget, main_oid, base, cumulative)
}

/// Patch-id of the cumulative diff from `from` to `to`. `Ok(None)` = the trees
/// are identical (empty diff); `Err(())` = it could not be computed.
fn cumulative_patch_id(repo: &Repository, from: Oid, to: Oid) -> Result<Option<Oid>, ()> {
    let from_tree = repo
        .find_commit(from)
        .and_then(|c| c.tree())
        .map_err(|_| ())?;
    let to_tree = repo
        .find_commit(to)
        .and_then(|c| c.tree())
        .map_err(|_| ())?;
    let diff = repo
        .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)
        .map_err(|_| ())?;
    if diff.deltas().len() == 0 {
        return Ok(None);
    }
    diff.patchid(None).map(Some).map_err(|_| ())
}

/// Scan commits added to `main` since `base` for one whose own patch equals
/// `target` — the fingerprint a squash merge of the branch leaves behind.
///
/// Returns `None` when the scan ran out of budget before it could answer;
/// `Some(false)` is a real answer ("no such commit on main").
fn main_contains_patch(
    repo: &Repository,
    patch_ids: &mut PatchIdCache<'_>,
    budget: &mut MergeScanBudget,
    main_oid: Oid,
    base: Oid,
    target: Oid,
) -> Option<bool> {
    let Ok(mut walk) = repo.revwalk() else {
        return Some(false);
    };
    if walk.push(main_oid).is_err() {
        return Some(false);
    }
    let _ = walk.hide(base);

    for oid in walk.flatten().take(SQUASH_SCAN_CAP) {
        // The budget is checked inside the walk as well as between branches:
        // the first branch to reach a long stretch of `main` is the one that
        // pays for computing its patch-ids, and it must not be able to run
        // past the budget in a single step.
        if budget.expired() {
            budget.mark_truncated();
            return None;
        }
        if patch_ids.patch_id(oid) == Some(target) {
            return Some(true);
        }
    }
    Some(false)
}

fn format_commit_time(time: git2::Time) -> Option<String> {
    chrono::DateTime::from_timestamp(time.seconds(), 0).map(|dt| dt.to_rfc3339())
}
