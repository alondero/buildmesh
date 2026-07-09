//! Mesh health: drift, base-branch-hostage, and unpushed-commit detection plus
//! the one-click recovery actions (issue #231, ADR 0006). Pure functions over an
//! already-open `&Repository`; the thin `#[command]` adapters that open the repo
//! and emit events stay in commands/git.rs. See ADR 0007.

use std::time::Duration;

use git2::Repository;

use crate::git::primitives;
use crate::models::{HoldingWorktree, MeshHealth};
use crate::process_util::command_no_window;

/// Wall-clock timeout for the `git checkout` shell-out in
/// [`restore_to_base_impl`]. `checkout` is purely local (no network), so
/// 30s is generous headroom while still bounding the "stuck on
/// `.git/index.lock`" case — the diff's subprocess-timeout pass missed
/// this call site on the first sweep (issue #762 review).
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(30);

/// Derive the local branch name from a configured `base_ref` string.
///
/// Accepts the four observed forms:
///   - `main`                        → `Some("main")`
///   - `origin/main`                 → `Some("main")`
///   - `refs/heads/main`             → `Some("main")`
///   - `origin/feature/auth`         → `Some("feature/auth")` (strip the remote, keep nested branch)
///   - `refs/heads/feature/auth`     → `Some("feature/auth")` (branch can contain slashes)
///   - `refs/remotes/origin/feature/auth` → `Some("feature/auth")`
///
/// Returns `None` for:
///   - empty / whitespace-only strings
///   - `HEAD` (case-insensitive) — anywhere it appears as the branch part
///   - `FETCH_HEAD`
///
/// **Limitation:** when `base_ref` is a plain nested branch name like
/// `feature/auth` (no remote prefix, no `refs/heads/` prefix), this helper
/// can't tell the difference between a remote prefix and a nested branch
/// — it strips `feature/` and returns `auth`. To disambiguate, use the
/// full `refs/heads/feature/auth` form. The buildmesh DB default
/// (`origin/main`) and the common short forms (`main`, `develop`,
/// `origin/develop`) all resolve correctly.
///
/// Callers must treat `None` as "no Base Ref configured" — the badge
/// and the recovery button are both suppressed in that case.
pub(crate) fn parse_local_branch(base_ref: &str) -> Option<String> {
    let trimmed = base_ref.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Path 1: full branch ref `refs/heads/<branch>` — the branch can
    // contain slashes (e.g. `refs/heads/feature/auth`).
    if let Some(branch) = trimmed.strip_prefix("refs/heads/") {
        return finalize_branch(branch);
    }
    // Path 2: full remote-tracking ref `refs/remotes/<remote>/<branch>` —
    // strip the remote (first segment) and keep the rest, which may
    // contain slashes.
    if let Some(after) = trimmed.strip_prefix("refs/remotes/") {
        let branch = after.split_once('/').map(|(_, b)| b).unwrap_or(after);
        return finalize_branch(branch);
    }
    // Path 3: short form. Either `<branch>` (no slashes — easy) or
    // `<remote>/<branch>` (the buildmesh DB default `origin/main`).
    // Without a repo we can't tell them apart, so we split on the first
    // slash and treat the first segment as the remote. See the function-
    // level doc for the nested-branch-no-remote case.
    let local = match trimmed.split_once('/') {
        Some((_remote, rest)) if !rest.is_empty() => rest,
        _ => trimmed,
    };
    finalize_branch(local)
}

/// Reject empty / HEAD / FETCH_HEAD and lowercase-normalize the result.
fn finalize_branch(local: &str) -> Option<String> {
    if local.is_empty()
        || local.eq_ignore_ascii_case("HEAD")
        || local.eq_ignore_ascii_case("FETCH_HEAD")
    {
        return None;
    }
    Some(local.to_string())
}

/// Pure helper: compute a MeshHealth snapshot from an already-open `Repository`.
/// No DB access, no Tauri command. `base_ref` is the mesh's configured
/// `base_ref` (typically `origin/main`); pass an empty string or `HEAD`
/// when the mesh has no base configured and every field will reflect that.
///
/// `base_branch_holder` is always `None` from this helper — the Tauri
/// command fills it in via `find_base_branch_holder` (which needs the
/// agent-node active-paths list, not available here).
pub(crate) fn compute_mesh_health(
    repo: &Repository,
    base_ref: &str,
) -> Result<MeshHealth, String> {
    let local_base_branch = parse_local_branch(base_ref);

    // Read HEAD state. `head()` errors on an unborn HEAD (no commits yet).
    let (current_branch, current_short_sha, is_detached) = match repo.head() {
        Ok(_) => {
            let detached = repo.head_detached().unwrap_or(false);
            let name = if detached {
                Some("HEAD".to_string())
            } else {
                repo.head()
                    .ok()
                    .and_then(|h| h.shorthand().map(|s| s.to_string()))
            };
            let oid = repo.head().ok().and_then(|h| h.target());
            let short_sha = oid.map(|o| primitives::short_sha(repo, o)).unwrap_or_default();
            (name, short_sha, detached)
        }
        Err(_) => (None, String::new(), false),
    };

    let is_dirty = primitives::is_dirty(repo).unwrap_or(false);
    let (has_upstream, unpushed_ahead) =
        compute_unpushed(repo, current_branch.as_deref(), local_base_branch.as_deref());
    let is_drifted = compute_is_drifted(
        repo,
        current_branch.as_deref(),
        is_detached,
        local_base_branch.as_deref(),
        base_ref,
    );

    Ok(MeshHealth {
        base_ref: base_ref.to_string(),
        local_base_branch,
        current_branch,
        current_short_sha,
        is_detached,
        is_dirty,
        unpushed_ahead,
        has_upstream,
        is_drifted,
        base_branch_holder: None,
    })
}

/// `unpushed_ahead` semantics: the number of commits on the current
/// branch's tip that would be stranded by a `git checkout base_branch`.
///
/// - When the branch has an upstream configured: standard ahead-of-upstream
///   count via `graph_ahead_behind`.
/// - When it has no upstream: ahead-of-local-base-branch count. A branch
///   with the same tip as the local base (a fresh branch with no local
///   commits) reports 0 — it has nothing to lose.
///
/// Returns `(has_upstream, unpushed_ahead)`.
fn compute_unpushed(
    repo: &Repository,
    current_branch: Option<&str>,
    local_base_branch: Option<&str>,
) -> (bool, u32) {
    let Some(branch) = current_branch else {
        return (false, 0);
    };
    let refname = format!("refs/heads/{}", branch);

    // Try the configured upstream first. We use git2's `graph_ahead_behind`
    // for the same Windows-brace-stripping reason as `get_git_branch_status`:
    // `git rev-list --count HEAD..@{u}` is mangled by `Command::args` on
    // Windows, so we go through libgit2.
    // `branch_upstream_name` succeeding is what tells us an upstream is
    // *configured* (distinct from resolvable), so we keep that check here for
    // the `has_upstream` flag and use the primitive only for the count.
    if let Ok(upstream_buf) = repo.branch_upstream_name(&refname) {
        if let Some(upstream_ref) = upstream_buf.as_str() {
            if let Ok(up_ref) = repo.find_reference(upstream_ref) {
                if let (Some(head_oid), Some(up_oid)) = (
                    repo.head().ok().and_then(|h| h.target()),
                    up_ref.target(),
                ) {
                    if let Ok((ahead, _)) = primitives::ahead_behind(repo, head_oid, up_oid) {
                        return (true, ahead);
                    }
                }
            }
        }
        // Upstream configured but we couldn't resolve OIDs.
        return (true, 0);
    }

    // No upstream: count commits on the current tip not on the local base.
    if let Some(base) = local_base_branch {
        let base_oid = repo
            .find_branch(base, git2::BranchType::Local)
            .ok()
            .and_then(|b| b.get().peel_to_commit().ok())
            .map(|c| c.id());
        let head_oid = repo.head().ok().and_then(|h| h.target());
        if let (Some(b), Some(h)) = (base_oid, head_oid) {
            if b == h {
                return (false, 0);
            }
            if let Ok((ahead, _)) = primitives::ahead_behind(repo, h, b) {
                return (false, ahead);
            }
        }
    }

    (false, 0)
}

/// `is_drifted` rules, in priority order:
/// 1. `local_base_branch.is_none()` (no base configured)       → `false`
/// 2. Base Ref absent from the repo (e.g. default `origin/main`
///    against a `master`-trunk repo)                           → `false`
/// 3. `current_branch == Some(local_base_branch)`              → `false` (on base)
/// 4. Detached HEAD at the Base Ref's OID                      → `false` (close enough)
/// 5. Otherwise                                                → `true`
///
/// Rule 2 is the key guard against false positives: the buildmesh DB
/// defaults every Mesh's `base_ref` to `origin/main`, so a repo whose
/// trunk is `master` (or any other name) would otherwise be reported as
/// permanently "drifted" off a `main` branch that does not exist. We only
/// claim drift when there is a real base to drift *from*.
///
/// Among the remaining cases we still return `true` when OID equality for a
/// detached HEAD cannot be determined — better a possibly-false badge than
/// a missed real drift, once we know the base genuinely exists.
fn compute_is_drifted(
    repo: &Repository,
    current_branch: Option<&str>,
    is_detached: bool,
    local_base_branch: Option<&str>,
    base_ref: &str,
) -> bool {
    let Some(local) = local_base_branch else {
        return false;
    };
    if !base_branch_present(repo, local, base_ref) {
        return false;
    }
    if !is_detached {
        return current_branch != Some(local);
    }
    // Detached: compare OIDs. A detached HEAD at the same OID as the local
    // Base Ref is "close enough" — no badge.
    let base_oid = repo
        .find_branch(local, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.get().peel_to_commit().ok())
        .map(|c| c.id());
    let head_oid = repo.head().ok().and_then(|h| h.target());
    match (base_oid, head_oid) {
        (Some(b), Some(h)) => b != h,
        _ => true,
    }
}

/// Does the configured Base Ref actually exist in this repo? A Mesh
/// carrying the default `origin/main` base_ref against a repo whose trunk
/// is `master` has no `main` to compare against — drift detection must
/// treat that as "no base", not "permanently drifted".
///
/// Resolution order: a local branch with the parsed name is the strongest
/// signal; failing that, the raw `base_ref` may name a remote-tracking
/// branch (`origin/main`) that `revparse` can resolve even with no local
/// branch present.
fn base_branch_present(repo: &Repository, local_base_branch: &str, base_ref: &str) -> bool {
    if repo
        .find_branch(local_base_branch, git2::BranchType::Local)
        .is_ok()
    {
        return true;
    }
    repo.revparse_single(base_ref).is_ok()
}

/// Which *linked* worktree currently has the Base Ref's branch checked
/// out, holding it hostage from the Mesh root? Returns the first match.
///
/// The Mesh root (the main worktree) is deliberately NOT considered: the
/// root sitting on the Base Ref is the healthy, desired state, not a
/// hostage — git only blocks a `git checkout <base>` from the root when a
/// *different* worktree already holds that branch. Treating the root as a
/// holder produced a false "base held by <repo>" badge for every healthy
/// Mesh (#265 follow-up).
///
/// `active_paths` is the set of agent-node worktree paths; a holder whose
/// path matches an entry in the list is marked `is_active = true` so
/// the UI can warn the user before detaching a live agent's worktree.
pub(crate) fn find_base_branch_holder(
    repo: &Repository,
    local_base_branch: &str,
    active_paths: &[String],
) -> Option<HoldingWorktree> {
    // Linked worktrees only — the main worktree (root) is never a hostage.
    if let Ok(names) = repo.worktrees() {
        for wt_name in names.iter().flatten() {
            if let Ok(wt) = repo.find_worktree(wt_name) {
                let path = wt.path();
                if let Ok(wt_repo) = Repository::open(path) {
                    if primitives::head_branch_name(&wt_repo).as_deref() == Some(local_base_branch) {
                        return Some(make_holder(&path.to_string_lossy(), active_paths));
                    }
                }
            }
        }
    }
    None
}

fn make_holder(path: &str, active_paths: &[String]) -> HoldingWorktree {
    let norm = primitives::normalize_for_compare(path);
    let is_active = active_paths.iter().any(|p| primitives::normalize_for_compare(p) == norm);
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string();
    HoldingWorktree {
        path: path.to_string(),
        name,
        is_active,
    }
}

// ── Recovery (issue #231) ───────────────────────────────────────────────────

/// Pure helper for `restore_mesh_to_base`. Runs the full guard chain
/// then shells out to `git checkout <base_branch>` when all guards
/// pass. Returns `Ok(true)` when HEAD actually moved and `Ok(false)`
/// for the already-on-base no-op case.
///
/// Guard order (first failure short-circuits with a user-readable `Err`):
/// 1. `is_dirty`                       — uncommitted changes block
/// 2. `unpushed_ahead > 0`             — local commits would be lost
/// 3. `find_base_branch_holder` is Some — branch is checked out elsewhere
/// 4. `is_detached` on a non-base OID  — silent-data-loss via reflog only
/// 5. already on base                  — no-op, returns `Ok(false)`
pub(crate) fn restore_to_base_impl(
    repo: &Repository,
    base_branch: &str,
) -> Result<bool, String> {
    // Use base_branch as the base_ref too — `parse_local_branch` is idempotent
    // for both `main` and `origin/main`, and we need `local_base_branch` set
    // for the unpushed-ahead and drift calculations to be correct.
    let health = compute_mesh_health(repo, base_branch)?;

    if health.is_dirty {
        return Err("root has uncommitted changes — commit or stash first".to_string());
    }
    if health.unpushed_ahead > 0 {
        let branch = health.current_branch.as_deref().unwrap_or("HEAD");
        let hint = if health.has_upstream { "push" } else { "push or branch" };
        return Err(format!(
            "{} unpushed commit(s) on {} — {}, branch, or reset first",
            health.unpushed_ahead, branch, hint
        ));
    }
    // Already on base → no-op. This must come BEFORE the hostage check:
    // the root worktree itself "holds" the base branch whenever it's on it,
    // and that's not a hostage — it's the desired state.
    if health.current_branch.as_deref() == Some(base_branch) {
        return Ok(false);
    }
    // Hostage check uses empty active_paths; the Tauri command refines
    // `is_active` separately. Detecting the hostage at all is what blocks
    // the restore — the user is told which worktree holds the branch.
    if let Some(holder) = find_base_branch_holder(repo, base_branch, &[]) {
        return Err(format!(
            "branch {} is held by worktree at {} — free it first",
            base_branch, holder.path
        ));
    }

    // All guards pass — run `git checkout`.
    let host_path = repo
        .workdir()
        .ok_or_else(|| "repo has no working directory".to_string())?
        .to_string_lossy()
        .to_string();
    let mut checkout_builder = command_no_window("git");
    checkout_builder
        .args(["checkout", base_branch])
        .current_dir(&host_path);
    let output = crate::process_util::run_command_with_timeout(
        checkout_builder,
        "git checkout",
        CHECKOUT_TIMEOUT,
    )
    .map_err(|e| format!("failed to run git checkout: {}", e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(true)
}

/// Pure helper for `free_base_branch`. Detaches the worktree's HEAD at
/// its current commit (preserves the commit in reflog, releases the
/// branch lock). Idempotent: re-running on a worktree that is already
/// detached returns the current HEAD OID without making any changes.
/// Returns the 7-char short SHA of the detached commit on success.
pub(crate) fn free_base_branch_impl(
    host_worktree_path: &str,
    local_base_branch: &str,
) -> Result<String, String> {
    let repo = Repository::open(host_worktree_path)
        .map_err(|e| format!("failed to open worktree at {}: {}", host_worktree_path, e))?;

    let current_branch = primitives::head_branch_name(&repo);
    let already_detached = current_branch.is_none();

    if !already_detached && current_branch.as_deref() != Some(local_base_branch) {
        return Err(format!(
            "worktree at {} does not hold base branch '{}' (current branch: {})",
            host_worktree_path,
            local_base_branch,
            current_branch.unwrap_or_else(|| "detached".to_string())
        ));
    }

    let head_oid = repo
        .head()
        .map_err(|e| format!("worktree HEAD is unborn: {}", e))?
        .peel_to_commit()
        .map_err(|e| format!("worktree HEAD is not a commit: {}", e))?
        .id();

    if !already_detached {
        repo.set_head_detached(head_oid)
            .map_err(|e| format!("set_head_detached failed: {}", e))?;
    }

    Ok(primitives::short_sha(&repo, head_oid))
}


#[cfg(test)]
#[path = "mesh_health_tests.rs"]
mod mesh_health_tests;
