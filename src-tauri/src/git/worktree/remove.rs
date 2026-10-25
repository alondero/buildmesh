//! Staged worktree removal, bounded filesystem retries, and orphaned admin-entry pruning.

use super::*;
// ── Remove ──────────────────────────────────────────────────────────────────

/// How many times to attempt the rename-into-staging, and how long to wait
/// between tries. Windows releases a just-killed process's directory handles
/// asynchronously, so the rename can briefly fail with "being used by another
/// process" even after the agent is gone; the backoff rides that out. An open
/// handle *inside* the tree is what blocks it — probed on Windows, the rename
/// fails with os error 5 while the handle is held, so this budget is the whole
/// close-node window, and the window is load-dependent rather than bounded by
/// anything the caller can predict. The original 5 x 100 ms left a tombstone
/// queued for a worktree that was about to become removable; even 20 x 100 ms
/// was not enough under a four-way parallel test run (issue #2173). Only a
/// genuinely blocked removal waits this long, and that one is retried by the
/// cleanup worker either way.
pub(super) const WORKTREE_REMOVE_ATTEMPTS: u32 = 60;
pub(super) const WORKTREE_REMOVE_BACKOFF_MS: u64 = 100;

/// Stable identifier for the removal step that failed, persisted per blocked
/// cleanup (`pending_worktree_removals.last_operation`) so the UI can name
/// what Buildmesh was doing when it got stuck — a rename blocked by an open
/// handle is a completely different problem from a delete blocked by the same
/// thing (issue #2139).
pub const OP_OPEN_WORKTREE_REPO: &str = "open-worktree-repository";
pub const OP_OPEN_ADMIN_ENTRY: &str = "open-worktree-admin-entry";
pub const OP_RENAME_TO_STAGING: &str = "rename-worktree-to-staging";
pub const OP_DELETE_STAGING: &str = "delete-staged-worktree";
pub const OP_PRUNE_ADMIN_ENTRY: &str = "prune-worktree-admin-entry";

/// A removal failure that names the step that failed alongside the underlying
/// OS error. `detail` is exactly the error text the old `Result<(), String>`
/// carried, so every consumer that matched on message substrings keeps
/// working; `operation` is the addition that makes a blocked cleanup
/// diagnosable (issue #2139).
#[derive(Debug, Clone)]
pub struct WorktreeRemovalFailure {
    pub operation: &'static str,
    pub detail: String,
}

impl WorktreeRemovalFailure {
    fn new(operation: &'static str, detail: impl ToString) -> Self {
        Self {
            operation,
            detail: detail.to_string(),
        }
    }

    /// Test-only constructor: build a failure for a named step so a test can
    /// state exactly which step broke without forcing the real thing.
    #[cfg(test)] // allow-test-in-production: existing shared constructor retained with its moved failure type
    pub(crate) fn for_test(operation: &'static str, detail: &str) -> Self {
        Self::new(operation, detail)
    }

    /// The error text alone — the shape every pre-#2139 caller consumed.
    pub fn message(&self) -> String {
        self.detail.clone()
    }
}

/// Remove a single worktree: delete its working directory (with retry) then
/// prune its git admin entry, leaving the branch it checked out alone. Used by
/// the Worktree Manager, where branches are a separate, independently-selectable
/// list. Idempotent — a missing directory counts as already removed.
pub fn remove_one_worktree(path: &str) -> Result<(), String> {
    remove_worktree_inner(path, false).map_err(|f| f.message())
}

/// [`remove_one_worktree`] with the failure kept structured: the caller gets the
/// failed *operation* as well as the OS error, which is what a persisted blocked
/// cleanup needs (issue #2139). This is the node-close path — the pending-removal
/// drain calls it — and it also deletes the local branch the worktree had
/// checked out, because a branched worktree's branch exists only to back that
/// node and leaving it behind is what lets dead branches pile up and slow git
/// operations. The work-at-risk decision is made upstream (the close-safety
/// prompt); by the time a removal is queued the branch is meant to go, so the
/// deletion is unconditional but best-effort.
pub fn remove_one_worktree_and_branch_detailed(path: &str) -> Result<(), WorktreeRemovalFailure> {
    remove_worktree_inner(path, true)
}

pub(super) fn remove_worktree_inner(
    path: &str,
    delete_branch: bool,
) -> Result<(), WorktreeRemovalFailure> {
    let host_path = to_host_path(path);
    let dir = std::path::Path::new(&host_path);

    // An already-gone working directory is nothing to remove. The node may have
    // been gutted by a previous interrupted close (#239); treat it as success so
    // the close can still proceed rather than erroring on a missing repo.
    //
    // This return is load-bearing for #239 but it cannot tell "gone because we
    // removed it" from "this path never existed" — and the second case means
    // the caller derived the wrong path, so the REAL worktree is still on disk
    // and is now leaked with no error, no retry and no `worktree-cleanup-failed`
    // toast. That is exactly how #1080 stayed invisible for two days. We still
    // return `Ok` (a hard error here would strand the close), but a path whose
    // parent directory is intact while the path itself is absent is a strong
    // signal of a derived-path bug rather than a completed delete, so say so
    // loudly enough to show up in a log scan.
    //
    // Issue #2139: "absent" is not automatically "finished". An interrupted
    // removal can leave a `<path>.removing` staging directory (the rename
    // succeeded, the staged delete never ran) and a git admin entry that no
    // longer has a working directory. Both are ours to finish, and a failure
    // to finish them must keep the queue entry alive rather than reporting a
    // clean success.
    if !dir.exists() {
        let parent_intact = dir.parent().map(|p| p.exists()).unwrap_or(false);
        if parent_intact {
            tracing::warn!(
                "remove_worktree_inner: {} does not exist but its parent does — treating as \
                 already-removed, but this is also what a wrong derived removal path looks \
                 like (see #1080). If a worktree is leaking, suspect the caller's \
                 `worktree_name` rather than this function.",
                host_path
            );
        }
        // Disk half: reclaim any staged copy of this worktree left behind by
        // an interrupted removal.
        remove_worktree_dir_with_retry(&host_path)?;
        // Git half: prune the admin entry if it still points at this path, so
        // the queue entry is only retired once both halves are done.
        prune_orphaned_worktree_entry(&host_path)?;
        if let Err(error) = crate::git::trust::forget_worktree(std::path::Path::new(&host_path)) {
            tracing::warn!(%error, "Could not revoke absent worktree's Buildmesh Git trust");
        }
        return Ok(());
    }

    let repo = Repository::open(&host_path)
        .map_err(|e| WorktreeRemovalFailure::new(OP_OPEN_WORKTREE_REPO, e))?;
    let trust_path = std::fs::canonicalize(&host_path)
        .map_err(|e| WorktreeRemovalFailure::new(OP_OPEN_WORKTREE_REPO, e))?;
    let worktree = git2::Worktree::open_from_repository(&repo)
        .map_err(|e| WorktreeRemovalFailure::new(OP_OPEN_ADMIN_ENTRY, e))?;

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
    worktree
        .prune(Some(&mut opts))
        .map_err(|e| WorktreeRemovalFailure::new(OP_PRUNE_ADMIN_ENTRY, e))?;

    if let Err(error) = crate::git::trust::forget_worktree(&trust_path) {
        tracing::warn!(%error, "Could not revoke removed worktree's Buildmesh Git trust");
    }

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
pub(super) fn delete_local_branch_best_effort(commondir: &std::path::Path, branch: &str) {
    let Ok(repo) = Repository::open(commondir) else {
        tracing::warn!(
            "could not reopen repo at {:?} to delete branch {}",
            commondir,
            branch
        );
        return;
    };
    // No such branch (detached, or already gone) — nothing to tidy.
    let Ok(mut branch_ref) = repo.find_branch(branch, git2::BranchType::Local) else {
        return;
    };
    if let Err(e) = branch_ref.delete() {
        tracing::warn!(
            "could not delete branch {} after closing its worktree: {}",
            branch,
            e
        );
    }
}

/// The staging sibling a removal renames a worktree to before deleting it
/// (the atomicity gate from #239).
pub(super) fn staging_path(path: &str) -> String {
    format!("{}.removing", path.trim_end_matches(['/', '\\']))
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
///
/// Issue #2139 — the "original path absent" case is no longer a silent
/// success. A removal interrupted between the rename and the staged delete
/// leaves `<path>.removing` on disk with the original path gone; the old
/// `!path.exists() → Ok(())` early-return skipped the staging cleanup, so the
/// directory leaked for as long as the tombstone had been dequeued. Reclaiming
/// it here (and failing loudly if that delete is blocked) is what lets the
/// caller keep the cleanup queued until the disk really is clear.
pub(super) fn remove_worktree_dir_with_retry(path: &str) -> Result<(), WorktreeRemovalFailure> {
    let staging = staging_path(path);

    if !std::path::Path::new(path).exists() {
        // Nothing to rename, but a staged copy from an interrupted removal may
        // still be sitting here. It holds no live handles by definition (the
        // rename already proved that), so deleting it can't gut a live worktree
        // — and if the delete fails (e.g. a new holder appeared), the removal
        // must report failure rather than pretend it is complete.
        if std::path::Path::new(&staging).exists() {
            tracing::info!(
                "remove_worktree_dir_with_retry: original {} is absent; reclaiming leftover \
                 staging {} (issue #2139)",
                path,
                staging
            );
            return std::fs::remove_dir_all(&staging)
                .map_err(|e| WorktreeRemovalFailure::new(OP_DELETE_STAGING, e));
        }
        return Ok(());
    }

    // Clear any staging dir left by a previous interrupted removal; by
    // definition it holds no live handles, so this can't gut the live worktree.
    let _ = std::fs::remove_dir_all(&staging);

    let mut last_err = String::new();
    for attempt in 0..WORKTREE_REMOVE_ATTEMPTS {
        match std::fs::rename(path, &staging) {
            Ok(()) => {
                return std::fs::remove_dir_all(&staging)
                    .map_err(|e| WorktreeRemovalFailure::new(OP_DELETE_STAGING, e))
            }
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
    Err(WorktreeRemovalFailure::new(OP_RENAME_TO_STAGING, last_err))
}

/// Finish the git half of a removal whose working directory is already gone:
/// find the admin entry (in the repository that owns the worktree) that still
/// points at `host_path` and prune it. Returns `Ok(())` when there is no entry
/// to prune, so this is safe to run on every drain.
///
/// Without it, a worktree whose rename-into-staging succeeded but whose prune
/// never ran keeps a dead entry in `.git/worktrees/` forever: `git worktree
/// list` reports a phantom, and a later spawn reusing the name finds the name
/// taken (issue #2139).
///
/// A failure to look is a failure, not a success: returning `Ok(())` here would
/// dequeue a tombstone whose git half is unfinished, which is the leak this
/// function exists to prevent. The one legitimate `Ok` without a prune is
/// "there is no repository above this path" — a path that was never a worktree
/// of any repo we can find (a #1080-style derived path, a mesh root that was
/// deleted). Anything else — an I/O error, a repository that cannot be read —
/// is an error and keeps the tombstone (issue #2139 review round 2).
pub(super) fn prune_orphaned_worktree_entry(host_path: &str) -> Result<(), WorktreeRemovalFailure> {
    prune_orphaned_worktree_entry_with(host_path, |parent| Repository::discover(parent))
}

/// [`prune_orphaned_worktree_entry`] with repository discovery injected, so the
/// "no repository above this path" and "cannot read the repository" branches
/// can be told apart by a test (issue #2139 review round 2).
pub(super) fn prune_orphaned_worktree_entry_with(
    host_path: &str,
    discover: impl Fn(&std::path::Path) -> Result<Repository, git2::Error>,
) -> Result<(), WorktreeRemovalFailure> {
    let target = std::path::Path::new(host_path);
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    // The working directory is gone, so open the repository from above: it is
    // the one that holds the (still present) admin metadata. Only "not found"
    // means there is no repository above this path, and therefore no metadata
    // to reconcile; every other error is a failed read.
    let repo = match discover(parent) {
        Ok(repo) => repo,
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            tracing::debug!(
                "prune_orphaned_worktree_entry: no repository above {} ({}); nothing to \
                 reconcile",
                parent.display(),
                e
            );
            return Ok(());
        }
        Err(e) => {
            return Err(WorktreeRemovalFailure::new(
                OP_PRUNE_ADMIN_ENTRY,
                format!("could not open the repository above {host_path}: {e}"),
            ));
        }
    };
    // Everything below here *is* a repository we found, so a failure means we
    // could not finish the git half and the caller must keep the tombstone.
    let names = repo.worktrees().map_err(|e| {
        WorktreeRemovalFailure::new(
            OP_PRUNE_ADMIN_ENTRY,
            format!("could not list git worktrees for {host_path}: {e}"),
        )
    })?;
    for i in 0..names.len() {
        let Some(name) = owned_worktree_name(names.get(i)).map_err(|error| {
            WorktreeRemovalFailure::new(
                OP_PRUNE_ADMIN_ENTRY,
                format!("could not read a registered worktree name for {host_path}: {error}"),
            )
        })?
        else {
            continue;
        };
        let Ok(worktree) = repo.find_worktree(&name) else {
            continue;
        };
        // `worktree.path()` is the *working* directory, not the admin entry
        // we need to read and prune — build that path from the repo's own
        // git dir instead (issue #2139).
        let admin_dir = repo.path().join("worktrees").join(name);
        if !admin_entry_points_at(&admin_dir, target) {
            continue;
        }
        let mut opts = git2::WorktreePruneOptions::new();
        opts.valid(true).locked(true);
        if let Err(e) = worktree.prune(Some(&mut opts)) {
            return Err(WorktreeRemovalFailure::new(OP_PRUNE_ADMIN_ENTRY, e));
        }
    }
    Ok(())
}

/// Whether the admin entry in `admin_dir` (a `.git/worktrees/<name>` directory)
/// still refers to the working directory `target`. The entry's `gitdir` file
/// holds the path of the worktree's `.git` link, whose parent is the working
/// directory itself. Git may write that path as absolute *or* relative to the
/// admin directory, so resolve both spellings before comparing.
pub(super) fn admin_entry_points_at(admin_dir: &std::path::Path, target: &std::path::Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(admin_dir.join("gitdir")) else {
        return false;
    };
    let gitdir = contents.trim();
    if gitdir.is_empty() {
        return false;
    }
    let raw = std::path::Path::new(gitdir);
    let resolved = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        admin_dir.join(raw)
    };
    let Some(parent) = resolved.parent() else {
        return false;
    };
    let candidates = [parent.to_path_buf(), normalize_parent(parent)];
    candidates
        .iter()
        .any(|candidate| same_path(candidate, target) || inode_same(candidate, target))
}

/// Lexical fallback (`..` / . collapsing) for symlinked or non-normalised
/// paths where canonicalisation isn't available.
pub(super) fn normalize_parent(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Identity comparison that survives a component being a symlink or junction
/// (Windows dev machines have both, and `mklink /J` is the documented way to
/// keep a worktree off the SSD).
///
/// `target` is the worktree path, which is *absent* by the time this runs, so
/// it cannot be canonicalised itself: canonicalise the parent that does exist
/// and join the final component. A path whose parent is missing simply loses
/// the comparison rather than matching by accident.
pub(super) fn inode_same(a: &std::path::Path, target: &std::path::Path) -> bool {
    let Some(name) = target.file_name() else {
        return false;
    };
    let Some(parent) = target.parent() else {
        return false;
    };
    let Ok(parent) = std::fs::canonicalize(parent) else {
        return false;
    };
    let Ok(candidate) = std::fs::canonicalize(a) else {
        return false;
    };
    candidate == parent.join(name)
}

/// Path equality that tolerates the Windows verbatim/UNC and separator
/// differences a `gitdir` file can carry (git writes forward slashes, the
/// queue can hold either spelling). Case folding is Windows-only: on Unix two
/// paths that differ in case are different files.
pub(super) fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    let normalize = |p: &std::path::Path| {
        let unified = p
            .to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .replace('\\', "/");
        if cfg!(target_os = "windows") {
            unified.to_lowercase()
        } else {
            unified
        }
    };
    normalize(a) == normalize(b)
}
