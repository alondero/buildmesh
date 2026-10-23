//! The pre-spawn pool seam and the "usable warm worktree" rule (issue #2533).
//!
//! Two things live here.
//!
//! ## The claim seam
//!
//! [`ClaimedWarmEntry`] and [`WarmClaim`] are the narrow, pool-agnostic
//! interface `git/worktree` uses to talk to the pre-spawn pool. Before issue
//! #2533 `git/worktree` named the warm-pool service module directly, so the two
//! modules depended on each other and the pool could not be reused by a third
//! warm-worktree producer without importing the whole service layer. The seam
//! is declared by the *consumer* (`git/`), implemented by the *producer*
//! (the warm pool), and points one way: `services/` → `git/`.
//!
//! It is deliberately **not** the `PoolBackend` trait ADR 0019 rejected. That
//! trait put the pool's own operations behind an injected backend — the
//! rejected shape, which grew a `dyn` cascade and a mock-only indirection
//! with no observable benefit. This seam is the opposite: it exposes the
//! handful of side effects the pool owns (its row lifecycle, its inventory,
//! its UI notification) and no git operations at all.
//!
//! ## The usability rule
//!
//! [`warm_checkout_plan`] and [`warm_entry_is_fresh`] together are the rule
//! "what makes a warm worktree usable". It used to be encoded twice — once by
//! the pool's fill (which cuts an entry detached at the mesh's base and stamps
//! the SHA it landed on) and once by the spawn's claim (which flips the entry
//! into the mesh's mode, moves it onto the required SHA, and re-applies
//! `.worktreeinclude`). The warm pool service owns the row lifecycle,
//! `git/worktree` owns the checkout, and the *rule* is owned here so each
//! side encodes it once.

use std::path::Path;

/// The mesh's `worktree_mode` value that means "create a branch".
pub const BRANCHED_MODE: &str = "branched";

/// The mesh's `worktree_mode` value that means "stay detached".
pub const DETACHED_MODE: &str = "detached";

/// What the spawn path needs to adopt a warm entry as a fresh node.
///
/// `path` and `preassigned_name` flow into `agent_nodes.path` and
/// `agent_nodes.worktree_name` respectively; `id` is the DB row id so the
/// spawn can drop the row after a successful claim (the row outlives its
/// purpose once the directory becomes a node's worktree). `base_sha` is
/// informational for the spawn-time freshness check.
#[derive(Debug, Clone)]
pub struct ClaimedWarmEntry {
    pub id: i64,
    pub path: String,
    pub preassigned_name: String,
    pub base_sha: Option<String>,
}

/// The narrow seam `git/worktree` consumes to talk to the pre-spawn pool.
///
/// Each method is either a side effect the pool owns (its row lifecycle) or
/// a fact only the pool knows (its inventory). `git/worktree` does not name the
/// pool's module, so the dependency direction stays one way.
pub trait WarmClaim: Send + Sync {
    /// The entry this claim holds, while the claim is live.
    fn entry(&self) -> Option<&ClaimedWarmEntry>;

    /// Whether the pool's inventory is one short because of this claim, so the
    /// post-spawn refill has to run. The pool owns the target size; `git/`
    /// only asks.
    fn needs_refill(&self) -> bool;

    /// Forget a claimed row: its directory has become an agent's worktree.
    fn mark_forgotten(&self, id: i64);

    /// Announce that the pool's ready count changed.
    fn emit_pool_changed(&self, mesh_id: i64);

    /// Run the pool's post-spawn maintenance (refresh stale entries, refill)
    /// off the spawn thread.
    fn schedule_maintenance(&self, mesh_id: i64, do_refresh: bool, do_refill: bool);
}

/// What a claimed warm worktree needs before an agent can run in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarmCheckout {
    /// No checkout needed — the entry is already in the requested mode.
    LeaveAsIs,
    /// Create the node's branch at the entry's current HEAD.
    BranchAtHead,
    /// Create the node's branch at `sha`.
    BranchAt(String),
    /// Detach the entry at `sha`.
    DetachAt(String),
}

/// The single rule for "what must happen to a claimed warm entry".
///
/// `target_sha` is the commit the entry must land on:
///
///   * `None` — keep the entry's own HEAD. A Manual claim adopts the
///     pre-warmed directory where it is; the pool already cut it at the
///     mesh's base, so the only thing left is the mode.
///   * `Some(sha)` — the entry must be moved onto that commit. An Issue/PR
///     claim adopts it onto the mesh base or the freshly fetched PR head.
///
/// The mode rules differ between the two cases: a Manual
/// claim leaves a `detached` entry alone but creates the node's branch for
/// each other mode, while an Issue/PR claim creates the branch only in
/// `branched` mode and detaches otherwise. Stating both here keeps the two
/// call sites from re-deriving the difference as bare string comparisons.
pub fn warm_checkout_plan(mode: &str, target_sha: Option<&str>) -> WarmCheckout {
    match target_sha {
        Some(sha) if mode == BRANCHED_MODE => WarmCheckout::BranchAt(sha.to_string()),
        Some(sha) => WarmCheckout::DetachAt(sha.to_string()),
        None if mode == DETACHED_MODE => WarmCheckout::LeaveAsIs,
        None => WarmCheckout::BranchAtHead,
    }
}

/// Is a warm entry still usable for `required_sha`?
///
/// The freshness half of the "is this warm worktree usable?" rule. A pool
/// entry is usable for a SHA only while it sits at that SHA. An entry with no
/// recorded SHA (`base_sha IS NULL` — the fill could not read the worktree's
/// HEAD) is **not** usable: the pool has no proof that it is on the right
/// commit, so it re-cuts the entry rather than trusting it.
pub fn warm_entry_is_fresh(recorded_sha: Option<&str>, required_sha: &str) -> bool {
    matches!(recorded_sha, Some(recorded) if recorded == required_sha)
}

/// Make a claimed warm worktree usable for the spawn that claimed it.
///
/// Applies the mode rule, the base-SHA move and the `.worktreeinclude`
/// re-application — the whole "the entry is now ready for an agent" step, in
/// one place. Both claim paths (the Manual mode-upgrade and the Issue/PR
/// adoption) go through it, so the two stay in step.
///
/// `target_sha` is the commit the entry must land on; `None` keeps the
/// entry's own HEAD (see [`warm_checkout_plan`]).
pub(crate) fn prepare_claimed_worktree(
    project_root: &str,
    host_path: &str,
    branch_name: &str,
    mode: &str,
    target_sha: Option<&str>,
) -> Result<(), String> {
    match warm_checkout_plan(mode, target_sha) {
        WarmCheckout::LeaveAsIs => {
            // The pool already cut the entry as detached — no ref flip needed.
        }
        WarmCheckout::BranchAtHead => {
            // `-b` refuses an existing branch instead of resetting sibling
            // work.
            run_git_checkout(host_path, &["-b", branch_name])?;
        }
        WarmCheckout::BranchAt(sha) => {
            // `-b <branch> <sha>` refuses an existing branch too, so a
            // re-spawn does not silently force-reset a deterministic
            // `gh{N}-`/`pr{N}-` branch and orphans the agent's earlier commits.
            run_git_checkout(host_path, &["-b", branch_name, &sha])?;
        }
        WarmCheckout::DetachAt(sha) => {
            run_git_checkout(host_path, &["--detach", &sha])?;
        }
    }
    // Re-apply `.worktreeinclude` so a claimed entry matches what
    // `create_git_worktree` already does (issue #639 gap 1). The prewarm-time
    // copy is stale by the time a user manually spawns — typical edits to a
    // `.worktreeinclude` source (`.env`, build cache, `node_modules/`) would
    // otherwise leave the agent on the prewarm snapshot. Best-effort like the
    // other call sites: a copy failure here is logged inside
    // `apply_worktree_include` but doesn't fail the spawn — the worktree is
    // already usable without the extras.
    super::apply_worktree_include(project_root, Path::new(host_path));
    Ok(())
}

/// Shared `git -C <host_path> checkout <args…>` runner for the warm-pool
/// checkout paths (manual mode-upgrade and Issue/PR adoption). Centralises the
/// `command_no_window` plumbing and the stderr-surfacing error shape so a
/// future fix (arg quoting, lock-retry) lands for both callers at once; the
/// deliberate flag differences (`-b` vs `--detach`) stay explicit at the call sites.
fn run_git_checkout(host_path: &str, args: &[&str]) -> Result<(), String> {
    use crate::process_util::git_command;
    let mut cmd = git_command();
    cmd.arg("-C").arg(host_path).arg("checkout").args(args);
    let output = super::run_bounded_git(cmd, "git checkout", super::GIT_TREE_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!(
            "git checkout {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "warm_tests.rs"]
mod tests;
