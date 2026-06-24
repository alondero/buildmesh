//! Worktree Node lifecycle: create, inspect (close-safety), and remove.
//!
//! Buildmesh owns each Agent Node's git worktree (ADR 0003). This module is the
//! one home for that ownership — it used to be spread across `env` (create),
//! `services::agent_node` (close-safety), and `commands::prune` (remove). The
//! *path* of a worktree (the `.claude/worktrees/<name>` layout and WSL/host
//! conversion) still belongs to `env`; this module takes an already-resolved
//! host path and makes / inspects / removes the git worktree there.

use git2::{Oid, Reference, Repository};
use serde::Serialize;

use crate::env::to_host_path;
use crate::git::primitives;
use crate::models::EnvType;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
// Suppress the transient console window git.exe would otherwise pop under a
// GUI-subsystem Tauri process (CREATE_NO_WINDOW). No winapi dep needed.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ── Create ──────────────────────────────────────────────────────────────────

/// Resolve the base commit a new worktree should be created from.
///
/// `"HEAD"` (or empty) uses the Mesh root's current HEAD — the explicit
/// "Head" option. Any other ref (e.g. `"origin/main"`) is resolved via
/// revparse; callers must run `env::fetch_origin` first so remote-tracking refs
/// are current. If the ref cannot be resolved (offline, missing), this falls
/// back to HEAD with a warning rather than failing the spawn, consistent with
/// ADR 0001's offline-fallback intent.
fn resolve_base_commit<'r>(
    repo: &'r git2::Repository,
    base_ref: &str,
) -> Result<git2::Commit<'r>, String> {
    let head_commit = || {
        repo.head()
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| format!("Failed to resolve HEAD commit: {}", e))
    };

    if base_ref.is_empty() || base_ref == "HEAD" {
        return head_commit();
    }

    match repo
        .revparse_single(base_ref)
        .and_then(|obj| obj.peel_to_commit())
    {
        Ok(commit) => Ok(commit),
        Err(e) => {
            tracing::warn!(
                "resolve_base_commit: could not resolve base_ref '{}' ({}); falling back to HEAD",
                base_ref,
                e
            );
            head_commit()
        }
    }
}

/// Add a worktree to the repo — branched (new branch) or detached — based on
/// the commit resolved from `base_ref` (see [`resolve_base_commit`]).
///
/// This is the one deliberate exception to ADR 0007's "all git access via
/// libgit2" rule: it shells out to `git worktree add`. libgit2's per-file
/// checkout write path is ~8× slower than git's on Windows, and that checkout
/// is 97% of worktree-create — 324 files took 3–12s via `repo.worktree()`
/// versus ~0.4s via the CLI (PR #308 follow-up). No git2 checkout flag closes
/// the gap (the write-everything path is the slow one), so the only fix that
/// makes a freshly-spawned Agent Node usable in ~2s instead of ~14s is the
/// CLI. See docs/adr/0007 for the amendment.
///
/// We still resolve `base_ref` → a concrete commit via libgit2 (fast, and it
/// keeps `resolve_base_commit`'s offline-fallback semantics) and hand git the
/// 40-char hex SHA. Passing a SHA — never a ref like `@{u}` — also sidesteps
/// the Windows `Command::args` `{}`-stripping trap (see the curly-brace-arg
/// memory). `-b <name>` gives branched mode a real branch at the base; detached
/// mode uses `--detach` and creates no branch at all (cleaner than the old
/// create-branch-then-detach-and-delete dance).
fn add_worktree_impl(
    repo: &git2::Repository,
    branch_name: &str,
    host_path: &std::path::Path,
    use_branched: bool,
    base_ref: &str,
) -> Result<(), String> {
    let base_sha = resolve_base_commit(repo, base_ref)?.id().to_string();
    let workdir = repo
        .workdir()
        .ok_or_else(|| "cannot add a worktree to a bare repository".to_string())?;

    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(workdir).arg("worktree").arg("add");
    if use_branched {
        cmd.arg("-b").arg(branch_name);
    } else {
        cmd.arg("--detach");
    }
    cmd.arg(host_path).arg(&base_sha);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `git worktree add`: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "Git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Remove worktrees whose on-disk path no longer exists.
fn prune_stale_worktrees(repo: &git2::Repository) {
    if let Ok(worktree_names) = repo.worktrees() {
        for i in 0..worktree_names.len() {
            if let Some(name) = worktree_names.get(i) {
                if let Ok(wt) = repo.find_worktree(name) {
                    if wt.validate().is_err() {
                        let mut prune_opts = git2::WorktreePruneOptions::new();
                        prune_opts.valid(false);
                        let _ = wt.prune(Some(&mut prune_opts));
                    }
                }
            }
        }
    }
}

/// Create a new Git worktree at the specified path and honor .worktreeinclude.
pub fn create_git_worktree(
    project_root: &str,
    worktree_host_path: &str,
    branch_name: &str,
    worktree_mode: &str,
    base_ref: &str,
) -> Result<(), String> {
    let host_path = std::path::Path::new(worktree_host_path);

    if host_path.exists() {
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = host_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create worktrees directory: {}", e))?;
    }

    tracing::info!("Creating git worktree: {} at {} (mode: {}, base_ref: {})", branch_name, worktree_host_path, worktree_mode, base_ref);

    let repo = git2::Repository::open(project_root)
        .map_err(|e| format!("Failed to open repository at {}: {}", project_root, e))?;

    let use_branched = worktree_mode == "branched";

    match add_worktree_impl(&repo, branch_name, host_path, use_branched, base_ref) {
        Ok(()) => {}
        Err(err) if err.contains("already exists") || err.contains("already checked out") => {
            tracing::warn!("Worktree already exists in git metadata, attempting to repair...");
            prune_stale_worktrees(&repo);
            add_worktree_impl(&repo, branch_name, host_path, use_branched, base_ref)?;
        }
        Err(err) => return Err(err),
    }

    apply_worktree_include(project_root, host_path);

    Ok(())
}

/// Copy the files listed in `<project_root>/.worktreeinclude` into a freshly
/// materialised worktree. Best-effort: a missing manifest is a no-op and a
/// failed copy is logged, never fatal — the worktree is already usable without
/// the extras.
///
/// Shared by the cold `create_git_worktree` path AND the warm-pool adoption
/// path (issue #612). The warm pool pre-copies these at prewarm time, but the
/// referenced files (local config, env files, build artifacts) can change
/// before an Issue/PR spawn adopts the entry, so re-applying on adoption keeps
/// an adopted worktree byte-for-byte equivalent to a cold-spawned one.
pub(crate) fn apply_worktree_include(project_root: &str, host_path: &std::path::Path) {
    let include_file_path = std::path::Path::new(project_root).join(".worktreeinclude");
    if !include_file_path.exists() {
        return;
    }
    tracing::info!("Found .worktreeinclude, copying included files...");
    let Ok(content) = std::fs::read_to_string(&include_file_path) else {
        return;
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let src = std::path::Path::new(project_root).join(trimmed);
        let dest = host_path.join(trimmed);

        if src.exists() {
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            if src.is_file() {
                if let Err(e) = std::fs::copy(&src, &dest) {
                    tracing::warn!("Failed to copy included file {} -> {}: {}", src.display(), dest.display(), e);
                } else {
                    tracing::info!("Copied included file: {}", trimmed);
                }
            } else if src.is_dir() {
                tracing::warn!(".worktreeinclude: directory copying not yet implemented: {}", trimmed);
            }
        }
    }
}

/// Resolve `base_ref` to a concrete 40-char commit SHA against the mesh repo,
/// with the same offline/HEAD fallback [`add_worktree_impl`] gets from
/// [`resolve_base_commit`]. The warm-pool adoption path (issue #612) hands the
/// resulting SHA — never a symbolic ref like `origin/main` or `@{u}` — to
/// `git checkout`, so it inherits both the offline resilience (an unfetched
/// remote ref degrades to HEAD instead of hard-failing the checkout) and the
/// Windows `Command::args` brace-strip protection the cold path relies on.
pub fn resolve_base_ref_sha(project_root: &str, base_ref: &str) -> Result<String, String> {
    let host_root = to_host_path(project_root);
    let repo = git2::Repository::open(&host_root)
        .map_err(|e| format!("Failed to open repository at {}: {}", host_root, e))?;
    // Bind before returning so the borrowed `Commit` temporary is dropped
    // while `repo` is still alive (the SHA itself is owned).
    let sha = resolve_base_commit(&repo, base_ref)?.id().to_string();
    Ok(sha)
}

/// Move (and rename) an existing worktree to a new directory via
/// `git worktree move`. This relocates the working tree on disk AND rewrites
/// git's internal references — the per-worktree admin `gitdir` pointer and the
/// worktree's own `.git` file — so every git command keeps working at the new
/// path. The warm pool (issue #612) uses it to adopt a pre-warmed plain-slug
/// worktree under an Issue/PR node's `gh{N}-`/`pr{N}-` target name without
/// paying the cold `git worktree add` checkout cost.
///
/// Shells out to the git CLI for the same reason [`add_worktree_impl`] does —
/// libgit2 exposes no worktree-move API. `project_root` is the mesh repo; the
/// two paths are the worktree's current and target host directories. Git
/// refuses to overwrite an existing target and needs the target's parent to
/// exist; both hold for the pool's sibling layout under `.claude/worktrees/`.
pub fn move_git_worktree(
    project_root: &str,
    old_host_path: &str,
    new_host_path: &str,
) -> Result<(), String> {
    // Normalise all three to host format before handing them to the Windows
    // git CLI — a WSL-style path would otherwise reach git.exe unconverted.
    // `to_host_path` is idempotent on an already-host path, so today's callers
    // (which pass host paths) are unaffected.
    let host_root = to_host_path(project_root);
    let old = to_host_path(old_host_path);
    let new = to_host_path(new_host_path);

    // Guard against `git worktree move`'s `mv`-style nesting: when the target
    // directory already exists, git moves the source *inside* it
    // (`<target>/<src-basename>`) and exits 0 rather than refusing. The spawn
    // path only calls this when the target is free, but refusing explicitly
    // keeps a stray leftover from silently producing a nested worktree.
    if std::path::Path::new(&new).exists() {
        return Err(format!("git worktree move target already exists: {}", new));
    }

    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(&host_root)
        .arg("worktree")
        .arg("move")
        .arg(&old)
        .arg(&new);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `git worktree move`: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git worktree move failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Sanitize the .git file in a worktree to ensure it uses the correct path format
/// for the target environment (Windows vs WSL).
pub fn sanitize_git_worktree(worktree_host_path: &str, env_type: EnvType) -> Result<(), String> {
    let git_file_path = std::path::Path::new(worktree_host_path).join(".git");

    if !git_file_path.is_file() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&git_file_path)
        .map_err(|e| format!("Failed to read .git file: {}", e))?;

    if !content.starts_with("gitdir: ") {
        return Ok(());
    }

    let git_dir_path = content.trim_start_matches("gitdir: ").trim();
    if git_dir_path.is_empty() {
        return Err("invalid .git file: empty gitdir path".to_string());
    }

    // Convert the path to the target environment's format
    let new_path = match env_type {
        EnvType::Wsl => {
            // Ensure it's a WSL-friendly path
            if git_dir_path.contains(':') || git_dir_path.starts_with("\\\\") {
                // Convert Windows path to WSL (/mnt/c/...)
                let path_str = git_dir_path.replace('\\', "/");
                if let Some(pos) = path_str.find(':') {
                    let drive = path_str[..pos].to_lowercase();
                    format!("/mnt/{}{}", drive, &path_str[pos + 1..])
                } else {
                    path_str
                }
            } else {
                git_dir_path.to_string()
            }
        }
        EnvType::Windows => {
            // Target is Windows. to_host_path handles /mnt/, /home/, and /c/ styles.
            to_host_path(git_dir_path)
        }
    };

    if new_path != git_dir_path {
        tracing::info!("Sanitizing .git file: {} -> {}", git_dir_path, new_path);
        // Ensure we use Unix line endings for the .git file as Git expects
        std::fs::write(&git_file_path, format!("gitdir: {}\n", new_path))
            .map_err(|e| format!("Failed to write sanitized .git file: {}", e))?;
    }

    Ok(())
}

// ── Inspect (close-safety) ──────────────────────────────────────────────────

/// Whether closing an Agent Node can remove its worktree without risking work.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeCloseSafety {
    pub worktree_path: Option<String>,
    pub has_uncommitted: bool,
    pub has_unpushed: bool,
    pub is_detached: bool,
}

/// Inspect a worktree to decide whether closing its node can remove it.
///
/// A worktree we can't even open as a repository — its directory is gone or its
/// git metadata is unreadable — has no detectable work to protect and nothing
/// removable, so it reports `worktree_path: None` ("nothing to remove, safe to
/// close") rather than failing. Blocking the close on it would leave the node
/// permanently un-closable from the UI (#239).
pub fn close_safety(path: &str) -> WorktreeCloseSafety {
    let host_path = to_host_path(path);
    let nothing_to_remove = WorktreeCloseSafety {
        worktree_path: None,
        has_uncommitted: false,
        has_unpushed: false,
        is_detached: false,
    };

    let Ok(repo) = Repository::open(&host_path) else {
        return nothing_to_remove;
    };
    let Ok(head) = repo.head() else {
        return nothing_to_remove;
    };

    // Fail closed: an unreadable status means we can't prove the worktree is
    // safe to remove, so treat it as having work to protect.
    let has_uncommitted = primitives::is_dirty(&repo).unwrap_or(true);
    let is_detached = !head.is_branch();
    let has_unpushed = head
        .target()
        .map(|head_oid| head_has_unpushed_or_unmerged_commits(&repo, &head, head_oid, is_detached))
        .unwrap_or(false);

    WorktreeCloseSafety {
        worktree_path: Some(path.to_string()),
        has_uncommitted,
        has_unpushed,
        is_detached,
    }
}

fn head_has_unpushed_or_unmerged_commits(
    repo: &Repository,
    head: &Reference,
    head_oid: Oid,
    is_detached: bool,
) -> bool {
    let current_refname = head.name();

    if !is_detached {
        if let Some(refname) = current_refname {
            if let Ok(upstream_buf) = repo.branch_upstream_name(refname) {
                let Some(upstream_refname) = upstream_buf.as_str() else {
                    return true;
                };
                let Ok(upstream_ref) = repo.find_reference(upstream_refname) else {
                    return true;
                };
                let Some(upstream_oid) = upstream_ref.target() else {
                    return true;
                };
                return primitives::ahead_behind(repo, head_oid, upstream_oid)
                    .map(|(ahead, _)| ahead > 0)
                    .unwrap_or(true);
            }
        }
    }

    !head_is_reachable_from_another_branch_or_remote(repo, head_oid, current_refname)
}

fn head_is_reachable_from_another_branch_or_remote(
    repo: &Repository,
    head_oid: Oid,
    current_refname: Option<&str>,
) -> bool {
    let Ok(references) = repo.references() else {
        return false;
    };

    for reference in references.flatten() {
        let Some(name) = reference.name() else {
            continue;
        };
        if Some(name) == current_refname || name.ends_with("/HEAD") {
            continue;
        }
        if !name.starts_with("refs/heads/") && !name.starts_with("refs/remotes/") {
            continue;
        }
        let Some(ref_oid) = reference.target() else {
            continue;
        };

        if ref_oid == head_oid || repo.graph_descendant_of(ref_oid, head_oid).unwrap_or(false) {
            return true;
        }
    }

    false
}

// ── Remove ──────────────────────────────────────────────────────────────────

/// How many times to attempt the working-tree removal, and how long to wait
/// between tries. Windows releases a just-killed process's directory handles
/// asynchronously, so the removal can briefly fail with "being used by another
/// process" even after the agent is gone; a short backoff rides that out.
const WORKTREE_REMOVE_ATTEMPTS: u32 = 5;
const WORKTREE_REMOVE_BACKOFF_MS: u64 = 100;

/// Remove a single worktree: delete its working directory (with retry) then
/// prune its git admin entry, leaving the branch it checked out alone. Used by
/// the Worktree Manager, where branches are a separate, independently-selectable
/// list. Idempotent — a missing directory counts as already removed.
pub fn remove_one_worktree(path: &str) -> Result<(), String> {
    remove_worktree_inner(path, false)
}

/// Like [`remove_one_worktree`], but also deletes the local branch the worktree
/// had checked out. This is the *node close* path: a branched worktree's branch
/// exists only to back that node, so leaving it behind on close is what lets
/// dead branches pile up and slow git operations. The work-at-risk decision is
/// made upstream (the close-safety prompt) — by the time a removal is queued the
/// branch is meant to go, so the deletion is unconditional but best-effort.
pub fn remove_one_worktree_and_branch(path: &str) -> Result<(), String> {
    remove_worktree_inner(path, true)
}

fn remove_worktree_inner(path: &str, delete_branch: bool) -> Result<(), String> {
    let host_path = to_host_path(path);

    // An already-gone working directory is nothing to remove. The node may have
    // been gutted by a previous interrupted close (#239); treat it as success so
    // the close can still proceed rather than erroring on a missing repo.
    if !std::path::Path::new(&host_path).exists() {
        return Ok(());
    }

    let repo = Repository::open(&host_path).map_err(|e| e.to_string())?;
    let worktree = git2::Worktree::open_from_repository(&repo)
        .map_err(|e| format!("not a removable worktree: {}", e))?;

    // Capture what we need to tidy the branch *before* the worktree is gone: a
    // branch checked out in a live worktree can't be deleted, so the delete must
    // wait until after the prune — by which point this handle's working
    // directory (and its per-worktree gitdir) no longer exist, so we reopen via
    // the shared common dir, where the branch ref actually lives.
    let branch_to_delete = if delete_branch {
        primitives::head_branch_name(&repo).map(|b| (b, repo.commondir().to_path_buf()))
    } else {
        None
    };

    // Delete the working directory ourselves first. git2's prune removes the
    // admin gitdir before the working tree, so if its rmdir loses the race with
    // a just-killed agent's handle release it leaves a dangling, un-prunable
    // worktree. Owning the removal lets us retry through that window; git2 then
    // only does the admin-entry bookkeeping, which never touches locked files.
    remove_worktree_dir_with_retry(&host_path)?;

    let mut opts = git2::WorktreePruneOptions::new();
    opts.valid(true).locked(true);
    worktree.prune(Some(&mut opts)).map_err(|e| e.to_string())?;

    // Best-effort: the worktree (and its work) is already gone, so a branch
    // delete that fails — e.g. the branch was reused by another worktree — must
    // not fail the close.
    if let Some((branch, commondir)) = branch_to_delete {
        delete_local_branch_best_effort(&commondir, &branch);
    }

    Ok(())
}

/// Delete a local branch from the repository whose git dir is `commondir`,
/// logging rather than propagating any failure. The branch ref lives in the
/// shared common dir, not the (now-removed) per-worktree gitdir, so we open
/// there.
fn delete_local_branch_best_effort(commondir: &std::path::Path, branch: &str) {
    let Ok(repo) = Repository::open(commondir) else {
        tracing::warn!("could not reopen repo at {:?} to delete branch {}", commondir, branch);
        return;
    };
    // No such branch (detached, or already gone) — nothing to tidy.
    let Ok(mut branch_ref) = repo.find_branch(branch, git2::BranchType::Local) else {
        return;
    };
    if let Err(e) = branch_ref.delete() {
        tracing::warn!("could not delete branch {} after closing its worktree: {}", branch, e);
    }
}

/// Remove a worktree's working directory *all-or-nothing*, retrying with backoff
/// to absorb the brief window where a just-killed process's handles are still
/// being released by the OS (the Windows close-node failure mode).
///
/// We can't delete in place: `fs::remove_dir_all` walks entries one at a time,
/// so a live agent's locked file makes it gut everything it *can* delete (the
/// source tree and the `.git` gitlink) before it fails — leaving a broken stub
/// that can't be opened as a repo, so the node can never be closed (#239).
/// Instead we rename the whole directory to a sibling staging name first: on
/// Windows that rename fails atomically while any descendant handle is open, so
/// a failure leaves the worktree fully intact. Only once the rename succeeds —
/// proving nothing pins the tree — do we delete the staged copy, where a
/// partial failure can no longer harm the live worktree path.
fn remove_worktree_dir_with_retry(path: &str) -> Result<(), String> {
    if !std::path::Path::new(path).exists() {
        return Ok(());
    }

    let staging = format!("{}.removing", path.trim_end_matches(['/', '\\']));
    // Clear any staging dir left by a previous interrupted removal; by
    // definition it holds no live handles, so this can't gut the live worktree.
    let _ = std::fs::remove_dir_all(&staging);

    let mut last_err = String::new();
    for attempt in 0..WORKTREE_REMOVE_ATTEMPTS {
        match std::fs::rename(path, &staging) {
            Ok(()) => return std::fs::remove_dir_all(&staging).map_err(|e| e.to_string()),
            Err(e) => {
                last_err = e.to_string();
                if attempt + 1 < WORKTREE_REMOVE_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        WORKTREE_REMOVE_BACKOFF_MS,
                    ));
                }
            }
        }
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_helpers::{
        commit_file, init_repo_with_commit, repo_is_dirty, repo_with_drifted_head, TestDir,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Create a branched worktree under `<root>/.claude/worktrees/<name>` via
    /// the production `create_git_worktree`, returning its path.
    fn make_branched_worktree(root: &Path, name: &str) -> PathBuf {
        let wt = root.join(".claude").join("worktrees").join(name);
        create_git_worktree(
            root.to_str().unwrap(),
            wt.to_str().unwrap(),
            name,
            "branched",
            "HEAD",
        )
        .expect("worktree creation must succeed");
        wt
    }

    // End-to-end: time the *production* `create_git_worktree` (now a git-CLI
    // shell-out) against a real repo. Confirms the fix through the actual code
    // path, not a hand-inlined libgit2 reproduction.
    //   cargo test -p buildmesh --lib timing_create_git_worktree_e2e -- --ignored --nocapture
    #[test]
    #[ignore = "timing harness; set BUILDMESH_WT_TIMING_REPO=<repo> to run"]
    fn timing_create_git_worktree_e2e() {
        use std::time::Instant;

        let repo_root = match std::env::var("BUILDMESH_WT_TIMING_REPO") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("SKIP: set BUILDMESH_WT_TIMING_REPO to a real repo to run");
                return;
            }
        };
        let wt_name = "wt-e2e-probe";
        let wt_path = Path::new(&repo_root)
            .join(".claude")
            .join("worktrees")
            .join(wt_name);
        let wt_path_str = wt_path.to_str().unwrap().to_string();

        // Clean residue, then drop any stale branch/metadata of the same name.
        let _ = remove_worktree_dir_with_retry(&wt_path_str);
        let repo = git2::Repository::open(&repo_root).expect("open repo");
        prune_stale_worktrees(&repo);
        if let Ok(mut b) = repo.find_branch(wt_name, git2::BranchType::Local) {
            let _ = b.delete();
        }

        // Loop a few times to separate a systematic cost from disk/Defender
        // noise (the libgit2 numbers swung 3.7↔12.4s, so a single sample lies).
        for i in 0..3 {
            let t = Instant::now();
            create_git_worktree(&repo_root, &wt_path_str, wt_name, "branched", "HEAD")
                .expect("create_git_worktree must succeed");
            eprintln!("[e2e] run {} create_git_worktree (git CLI): {:?}", i, t.elapsed());

            // Cleanup between runs.
            let _ = remove_worktree_dir_with_retry(&wt_path_str);
            prune_stale_worktrees(&repo);
            if let Ok(wt) = repo.find_worktree(wt_name) {
                let mut po = git2::WorktreePruneOptions::new();
                po.valid(true).working_tree(true);
                let _ = wt.prune(Some(&mut po));
            }
            let branch_to_delete = repo.find_branch(wt_name, git2::BranchType::Local);
            if let Ok(mut b) = branch_to_delete {
                let _ = b.delete();
            }
        }
    }

    // ── create (#210: dirty parent allowed) ──────────────────────────────────

    #[test]
    fn create_branched_worktree_succeeds_with_dirty_parent() {
        let td = TestDir::new("dirty_branched");
        let parent = td.path();
        init_repo_with_commit(parent, &[("README.md", "# project\n")]);

        // Make the working tree dirty: modify a tracked file + add an untracked one.
        fs::write(parent.join("README.md"), "# project (in-progress edit)\n").unwrap();
        fs::write(parent.join("scratch.txt"), "untracked local note").unwrap();
        assert!(repo_is_dirty(parent), "precondition: parent repo must be dirty");

        let wt_path = parent.join(".claude").join("worktrees").join("wt-1");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-1",
            "branched",
            "HEAD",
        )
        .expect("branched worktree creation must succeed with a dirty parent");

        assert!(wt_path.exists(), "worktree directory should exist");
        let wt_repo = git2::Repository::open(&wt_path).expect("worktree should be a valid repo");
        let head = wt_repo.head().unwrap();
        assert_eq!(head.shorthand().unwrap_or(""), "wt-1");

        // The parent's uncommitted changes must NOT leak into the new worktree.
        let wt_readme = fs::read_to_string(wt_path.join("README.md")).unwrap();
        assert_eq!(wt_readme, "# project\n");
        assert!(!wt_path.join("scratch.txt").exists());
        assert!(repo_is_dirty(parent), "creating a worktree must not modify the parent");
    }

    #[test]
    fn create_detached_worktree_succeeds_with_dirty_parent() {
        let td = TestDir::new("dirty_detached");
        let parent = td.path();
        init_repo_with_commit(parent, &[("README.md", "# project\n")]);
        fs::write(parent.join("README.md"), "# project (in-progress edit)\n").unwrap();
        assert!(repo_is_dirty(parent), "precondition: parent repo must be dirty");

        let wt_path = parent.join(".claude").join("worktrees").join("wt-det");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-det",
            "detached",
            "HEAD",
        )
        .expect("detached worktree creation must succeed with a dirty parent");

        assert!(wt_path.exists());
        let wt_repo = git2::Repository::open(&wt_path).expect("worktree should be a valid repo");
        assert!(wt_repo.head_detached().unwrap_or(false), "HEAD should be detached");
        let wt_readme = fs::read_to_string(wt_path.join("README.md")).unwrap();
        assert_eq!(wt_readme, "# project\n");
        assert!(repo_is_dirty(parent));
    }

    // ── move (#612: warm-pool adoption rename) ───────────────────────────────

    /// Create a DETACHED worktree under `<root>/.claude/worktrees/<name>`,
    /// matching the on-disk shape the warm pool actually cuts (it pins
    /// `detached` mode so a claim can `git checkout -b` to upgrade). Move tests
    /// use this rather than `make_branched_worktree` for production fidelity.
    fn make_detached_worktree(root: &Path, name: &str) -> PathBuf {
        let wt = root.join(".claude").join("worktrees").join(name);
        create_git_worktree(
            root.to_str().unwrap(),
            wt.to_str().unwrap(),
            name,
            "detached",
            "HEAD",
        )
        .expect("detached worktree creation must succeed");
        wt
    }

    #[test]
    fn move_git_worktree_relocates_dir_and_keeps_git_working() {
        let td = TestDir::new("move_wt");
        init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
        // A pre-warmed worktree under a plain slug, the pool's on-disk shape
        // (detached, as the pool cuts it).
        let src = make_detached_worktree(td.path(), "warm-amber-fox");
        let dst = td
            .path()
            .join(".claude")
            .join("worktrees")
            .join("gh123-fix-this");
        assert!(src.exists(), "precondition: warm worktree exists");
        assert!(!dst.exists(), "precondition: target name is free");

        move_git_worktree(
            td.path().to_str().unwrap(),
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
        )
        .expect("git worktree move must succeed");

        assert!(!src.exists(), "source directory must be gone after the move");
        assert!(dst.exists(), "target directory must exist after the move");

        // Git's internal references must have followed the directory: the moved
        // worktree opens as a repo and its HEAD resolves (a `git worktree move`
        // that didn't rewrite the gitdir pointer would break both).
        let moved = git2::Repository::open(&dst)
            .expect("moved worktree must still open as a git repository");
        assert!(
            moved.head().is_ok(),
            "HEAD must resolve inside the moved worktree"
        );
        // And the root repo's admin list must point at the new path, not the old.
        let root = git2::Repository::open(td.path()).unwrap();
        let still_registered = root
            .worktrees()
            .unwrap()
            .iter()
            .flatten()
            .filter_map(|n| root.find_worktree(n).ok())
            .any(|wt| wt.path() == dst.as_path());
        assert!(
            still_registered,
            "the moved worktree must be registered at its new path"
        );
    }

    #[test]
    fn move_git_worktree_refuses_an_occupied_target() {
        let td = TestDir::new("move_wt_clash");
        init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
        let src = make_detached_worktree(td.path(), "warm-amber-fox");
        // An occupied target. Bare `git worktree move` would nest the source
        // INSIDE it (mv semantics) and exit 0; our existence guard must refuse
        // instead so a stray leftover never produces a nested worktree.
        let dst = make_branched_worktree(td.path(), "gh7-occupied");

        let err = move_git_worktree(
            td.path().to_str().unwrap(),
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
        )
        .expect_err("moving onto an existing target must be refused");
        assert!(
            err.contains("target already exists"),
            "error must name the occupied-target guard, got: {}",
            err
        );
        assert!(src.exists(), "source must be untouched after a refused move");
        // The target must NOT have gained a nested worktree.
        assert!(
            !dst.join("warm-amber-fox").exists(),
            "the source must not have been nested inside the occupied target"
        );
    }

    #[test]
    fn resolve_base_ref_sha_resolves_and_falls_back_to_head() {
        let td = TestDir::new("resolve_sha");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let head = repo.head().unwrap().peel_to_commit().unwrap().id().to_string();

        // A resolvable ref returns its concrete SHA.
        assert_eq!(
            resolve_base_ref_sha(td.path().to_str().unwrap(), "HEAD").unwrap(),
            head
        );
        // An unresolvable ref degrades to HEAD (offline resilience) rather than
        // erroring — the property the warm-pool adoption path depends on.
        assert_eq!(
            resolve_base_ref_sha(td.path().to_str().unwrap(), "origin/does-not-exist").unwrap(),
            head,
            "an unfetched ref must fall back to HEAD, not hard-fail the checkout"
        );
    }

    // ── resolve_base_commit + base_ref (#230) ────────────────────────────────

    #[test]
    fn resolve_base_commit_head_returns_mesh_root_head() {
        let td = TestDir::new("resolve_head");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let head = repo.head().unwrap().peel_to_commit().unwrap().id();
        assert_eq!(resolve_base_commit(&repo, "HEAD").unwrap().id(), head);
        assert_eq!(resolve_base_commit(&repo, "").unwrap().id(), head);
    }

    #[test]
    fn resolve_base_commit_resolves_remote_ref() {
        let td = TestDir::new("resolve_remote");
        let (repo, origin_oid) = repo_with_drifted_head(td.path());
        assert_eq!(
            resolve_base_commit(&repo, "origin/main").unwrap().id(),
            origin_oid,
            "origin/main must resolve to the remote-tracking commit, not HEAD"
        );
    }

    #[test]
    fn resolve_base_commit_falls_back_to_head_when_unresolvable() {
        let td = TestDir::new("resolve_fallback");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let head = repo.head().unwrap().peel_to_commit().unwrap().id();
        assert_eq!(
            resolve_base_commit(&repo, "origin/does-not-exist").unwrap().id(),
            head,
            "an unresolvable ref must fall back to HEAD rather than error"
        );
    }

    #[test]
    fn branched_worktree_bases_off_base_ref_not_head() {
        let td = TestDir::new("base_ref_branched");
        let parent = td.path();
        let (_repo, origin_oid) = repo_with_drifted_head(parent);

        let wt_path = parent.join(".claude").join("worktrees").join("wt-br");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-br",
            "branched",
            "origin/main",
        )
        .expect("branched worktree creation must succeed");

        let content = fs::read_to_string(wt_path.join("f.txt")).unwrap();
        assert_eq!(content, "from-origin-main\n");
        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert_eq!(wt_repo.head().unwrap().peel_to_commit().unwrap().id(), origin_oid);
    }

    #[test]
    fn detached_worktree_bases_off_base_ref_not_head() {
        let td = TestDir::new("base_ref_detached");
        let parent = td.path();
        let (_repo, origin_oid) = repo_with_drifted_head(parent);

        let wt_path = parent.join(".claude").join("worktrees").join("wt-det");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-det",
            "detached",
            "origin/main",
        )
        .expect("detached worktree creation must succeed");

        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert!(wt_repo.head_detached().unwrap_or(false));
        assert_eq!(wt_repo.head().unwrap().peel_to_commit().unwrap().id(), origin_oid);
        assert_eq!(fs::read_to_string(wt_path.join("f.txt")).unwrap(), "from-origin-main\n");
    }

    #[test]
    fn worktree_base_ref_head_reproduces_mesh_root_head() {
        let td = TestDir::new("base_ref_head");
        let parent = td.path();
        let (repo, origin_oid) = repo_with_drifted_head(parent);
        let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

        let wt_path = parent.join(".claude").join("worktrees").join("wt-head");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-head",
            "branched",
            "HEAD",
        )
        .expect("worktree creation must succeed");

        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        let wt_oid = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
        assert_eq!(wt_oid, head_oid);
        assert_ne!(wt_oid, origin_oid);
        assert_eq!(fs::read_to_string(wt_path.join("f.txt")).unwrap(), "local-drift\n");
    }

    #[test]
    fn worktree_unresolvable_base_ref_falls_back_to_head() {
        let td = TestDir::new("base_ref_offline");
        let parent = td.path();
        let repo = init_repo_with_commit(parent, &[("f.txt", "only-commit\n")]);
        let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

        let wt_path = parent.join(".claude").join("worktrees").join("wt-off");
        create_git_worktree(
            parent.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "wt-off",
            "branched",
            "origin/main",
        )
        .expect("spawn must not fail when base_ref is unresolvable");

        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert_eq!(
            wt_repo.head().unwrap().peel_to_commit().unwrap().id(),
            head_oid,
            "unresolvable base_ref must fall back to HEAD"
        );
    }

    // ── close-safety ─────────────────────────────────────────────────────────

    #[test]
    fn close_safety_reports_clean_merged_worktree_as_safe() {
        let td = TestDir::new("safe_clean");
        init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
        let wt = make_branched_worktree(td.path(), "wt-safe");

        let safety = close_safety(&wt.to_string_lossy());

        assert!(!safety.has_uncommitted);
        assert!(!safety.has_unpushed);
        assert!(!safety.is_detached);
    }

    #[test]
    fn close_safety_reports_uncommitted_changes() {
        let td = TestDir::new("safe_dirty");
        init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
        let wt = make_branched_worktree(td.path(), "wt-dirty");
        fs::write(wt.join("scratch.txt"), "not committed").unwrap();

        let safety = close_safety(&wt.to_string_lossy());

        assert!(safety.has_uncommitted);
        assert!(!safety.has_unpushed);
    }

    #[test]
    fn close_safety_reports_branch_commits_not_reachable_from_another_ref() {
        let td = TestDir::new("safe_ahead");
        init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
        let wt = make_branched_worktree(td.path(), "wt-ahead");
        let wt_repo = git2::Repository::open(&wt).unwrap();
        commit_file(&wt_repo, &wt, "work.txt", "committed locally");

        let safety = close_safety(&wt.to_string_lossy());

        assert!(!safety.has_uncommitted);
        assert!(safety.has_unpushed);
    }

    #[test]
    fn close_safety_reports_detached_head_without_extra_commits_as_safe() {
        let td = TestDir::new("safe_detached_safe");
        init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
        let wt = make_branched_worktree(td.path(), "wt-detached-safe");
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        wt_repo.set_head_detached(head.id()).unwrap();

        let safety = close_safety(&wt.to_string_lossy());

        assert!(safety.is_detached);
        assert!(!safety.has_unpushed);
    }

    #[test]
    fn close_safety_reports_detached_head_with_unique_commits_as_unpushed() {
        let td = TestDir::new("safe_detached_risk");
        init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
        let wt = make_branched_worktree(td.path(), "wt-detached-risk");
        let wt_repo = git2::Repository::open(&wt).unwrap();
        let head = wt_repo.head().unwrap().peel_to_commit().unwrap();
        wt_repo.set_head_detached(head.id()).unwrap();
        commit_file(&wt_repo, &wt, "detached.txt", "detached work");

        let safety = close_safety(&wt.to_string_lossy());

        assert!(safety.is_detached);
        assert!(safety.has_unpushed);
    }

    #[test]
    fn close_safety_treats_missing_worktree_as_nothing_to_remove() {
        let td = TestDir::new("safe_missing");
        let missing = td.path().join("does-not-exist");

        let safety = close_safety(&missing.to_string_lossy());

        assert_eq!(safety.worktree_path, None);
        assert!(!safety.has_uncommitted);
        assert!(!safety.has_unpushed);
        assert!(!safety.is_detached);
    }

    #[test]
    fn close_safety_treats_non_repo_directory_as_nothing_to_remove() {
        let td = TestDir::new("safe_nonrepo");
        fs::write(td.path().join("stray.txt"), "not a git repo").unwrap();

        let safety = close_safety(&td.path().to_string_lossy());

        assert_eq!(safety.worktree_path, None);
        assert!(!safety.has_uncommitted);
    }

    // ── remove ───────────────────────────────────────────────────────────────

    #[test]
    fn remove_one_worktree_removes_dir_and_prunes_admin_entry() {
        let td = TestDir::new("remove_wt");
        let root_repo = init_repo_with_commit(td.path(), &[("file.txt", "initial\n")]);
        let wt = make_branched_worktree(td.path(), "wt-remove");
        assert!(wt.exists());

        remove_one_worktree(&wt.to_string_lossy()).expect("removal succeeds");

        assert!(!wt.exists(), "working directory must be gone");
        // The git admin entry must be pruned too.
        assert!(
            root_repo.find_worktree("wt-remove").is_err(),
            "worktree admin entry must be pruned"
        );
    }

    #[test]
    fn remove_one_worktree_is_idempotent_for_missing_dir() {
        let td = TestDir::new("remove_missing");
        let missing = td.path().join("never-existed");
        remove_one_worktree(&missing.to_string_lossy())
            .expect("a missing worktree dir counts as already removed");
    }
}
