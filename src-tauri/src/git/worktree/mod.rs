//! Worktree Node lifecycle: create, inspect (close-safety), and remove.
//!
//! Buildmesh owns each Agent Node's git worktree (ADR 0003). This module is the
//! one home for that ownership — it used to be spread across `env` (create),
//! the agent-node service (close-safety), and `commands::prune` (remove). The
//! *path* of a worktree (the configurable container and WSL/host
//! conversion) still belongs to `env`; this module takes an already-resolved
//! host path and makes / inspects / removes the git worktree there.
//!
//! The `provision` submodule is the **Worktree Provisioner** — the four-branch
//! decision (Reused / Adopted / Upgraded / Created) that turns a Spawn Context
//! into an on-disk worktree. ADR 0003's "Buildmesh Owns Worktree Creation" is
//! realised end-to-end in [`provision::provision_for_spawn`]; the orchestrator
//! (`agent/spawn.rs`) builds the Spawn Context and matches the outcome. This
//! module provides the primitives (`create_git_worktree`, `add_worktree_impl`,
//! `sanitize_git_worktree`, `prune_stale_worktrees`,
//! `apply_worktree_include`) that the Provisioner drives on every spawn.
//!
//! The `warm` submodule owns the pre-spawn pool seam and the "usable warm
//! worktree" rule. It is the reason this module does not name the service
//! layer: the pool is reached through the consumer-owned [`warm::WarmClaim`]
//! interface, so the dependency direction is one way (`services/` → `git/`).
//! See issue #2533.
//!
//! Issue #248 deepened the seam:
//! - `base_ref` (`SpawnContext.base_ref`) reaches the primitive via
//!   `provision_for_spawn` — pinned by `provision_for_spawn_cold_created_uses_spawn_context_base_ref_not_local_head`.
//! - `.worktreeinclude` directory entries are now recursively copied via
//!   `copy_dir_all` — pinned by `apply_worktree_include_copies_directory_recursively`.

mod include;
pub(crate) use include::*;
mod maintenance;
pub use maintenance::*;

mod create;
pub use create::*;
mod remove;
pub use remove::*;
mod safety;
pub use safety::*;

pub mod provision;
pub mod warm;

use std::time::Duration;

use git2::{Oid, Reference, Repository};
use serde::Serialize;
use ts_rs::TS;

use crate::env::to_host_path;
use crate::env::windows_to_wsl;
use crate::git::primitives;
use crate::models::EnvType;

// ── Spawn-path git CLI budgets (issue #1229) ───────────────────────────────
//
// Each of the shell-outs in this module and `provision` is reached only
// from the provisioner, the PR-creation command (`commands/pr.rs`), or the
// warm-pool refresh — the click-to-terminal path and the pool that feeds
// it. A bare `cmd.output()` waits forever, and two
// wedges are real — `git worktree add` runs a full checkout, so a slow
// smudge/clean filter (git-LFS pulls over the network) can stall it
// indefinitely; and checkout/reset can block on a `.git/index.lock`
// another process holds. One wedged filter hangs the spawn task with no
// feedback and no recovery, and a wedged `reset --hard` strands its
// warm-pool entry in `refreshing` forever. That is why the network-facing
// calls were already bounded (issue #762: `git::sync::SPAWN_FETCH_TIMEOUT`,
// `git::health::CHECKOUT_TIMEOUT`).
//
// A timeout is not merely a shorter wait: `process_util::run_command_with_timeout`
// kills the whole process tree (a hung git-LFS/credential helper would
// otherwise outlive its parent), records the wedge on the diagnostics timeline
// via `diagnostics::record_git_subprocess_timeout`, and returns a message
// naming the command — so the spawn failure toast says which command gave up
// rather than showing a bare git error.

// Budget for a call that can materialise a working tree — `git worktree add`
// and `git checkout -b/--detach <sha>`. 120s is generous headroom for a
// legitimately slow large checkout, because these run on the user's click.
const GIT_TREE_TIMEOUT: Duration = Duration::from_secs(120);

// Budget for the remaining spawn-path calls — `git worktree move`,
// `git reset --hard`, the `git remote get-url/add/set-url` trio, and
// `git rev-parse`. All are local and normally sub-second; 30s only has to
// outlast a contended index lock. `reset --hard` is deliberately on this
// cheaper tier despite writing a tree: a wedged reset leaves a warm-pool
// entry stuck in `refreshing` with no path back to `available`, so a
// bounded failure the pool retries beats an unbounded one it cannot.
const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(30);

/// The single choke point for git CLI calls on the spawn-critical worktree
/// path. Delegates to [`crate::process_util::run_command_with_timeout`], whose
/// contract (kill the tree on deadline, record diagnostics, name the command in
/// the error) is what makes a wedge recoverable; keeping the indirection here
/// gives the test suite one seam to pin the budgets against, and
/// `spawn_path_git_shellouts_are_all_budgeted` guards that every call site in
/// this module and `provision` actually goes through it.
///
/// Module-private: the only callers are this module and its `provision`
/// child, and a wider surface would invite a shell-out elsewhere to reach for
/// the seam without also picking up one of the budgets above.
fn run_bounded_git(
    cmd: std::process::Command,
    op_name: &str,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    crate::process_util::run_command_with_timeout(cmd, op_name, timeout)
}

// ── Inspect (close-safety) ──────────────────────────────────────────────────

/// Whether closing an Agent Node can remove its worktree without risking work.
///
/// Wire type — generated to `src/types/generated/WorktreeCloseSafety.ts` and
/// imported by `src/lib/worktreeClose.ts` (issue #1248). The previous hand-
/// declared TS interface in that file drifted from this struct; deriving `TS`
/// here and removing the hand-declared one keeps both sides in lockstep.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "WorktreeCloseSafety.ts")]
pub struct WorktreeCloseSafety {
    pub worktree_path: Option<String>,
    pub has_uncommitted: bool,
    pub has_unpushed: bool,
    pub is_detached: bool,
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
