//! Worktree Node lifecycle: create, inspect (close-safety), and remove.
//!
//! Buildmesh owns each Agent Node's git worktree (ADR 0003). This module is the
//! one home for that ownership — it used to be spread across `env` (create),
//! `services::agent_node` (close-safety), and `commands::prune` (remove). The
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
//! `sanitize_git_worktree`, `prune_stale_worktrees`, `resolve_base_commit`,
//! `apply_worktree_include`) that the Provisioner drives on every spawn.
//! Issue #248 deepened the seam:
//! - `base_ref` (`SpawnContext.base_ref`) reaches the primitive via
//!   `provision_for_spawn` — pinned by `provision_for_spawn_cold_created_uses_spawn_context_base_ref_not_local_head`.
//! - `.worktreeinclude` directory entries are now recursively copied via
//!   `copy_dir_all` — pinned by `apply_worktree_include_copies_directory_recursively`.

pub mod provision;
mod trust;

#[cfg(test)]
mod ownership_tests;

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
) -> Result<(), AddWorktreeFailure> {
    let base_sha = resolve_base_commit(repo, base_ref)
        .map_err(AddWorktreeFailure::other)?
        .id()
        .to_string();
    let workdir = repo
        .workdir()
        .ok_or_else(|| AddWorktreeFailure::other("cannot add a worktree to a bare repository"))?;

    // `command_no_window` applies CREATE_NO_WINDOW on Windows (no per-OS cfg here).
    let mut cmd = crate::process_util::git_command();
    cmd.arg("-C").arg(workdir).arg("worktree").arg("add");
    if use_branched {
        cmd.arg("-b").arg(branch_name);
    } else {
        cmd.arg("--detach");
    }
    cmd.arg(host_path).arg(&base_sha);

    let output = run_bounded_git(cmd, "git worktree add", GIT_TREE_TIMEOUT)
        .map_err(AddWorktreeFailure::other)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AddWorktreeFailure::new(
            classify_worktree_add_failure(&stderr),
            format!("Git worktree add failed: {stderr}"),
        ));
    }
    Ok(())
}

/// Which of the two decisions [`create_git_worktree`] has to make about a failed
/// `git worktree add` (issue #2195).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddWorktreeFailureKind {
    /// The target path is already **registered as a worktree** in git's
    /// metadata — the disk/metadata disagreement that prune-and-retry repairs.
    RegisteredWorktree,
    /// Everything else: a branch-name clash, an unresolvable base ref, a failed
    /// spawn, an unwritable path. Pruning cannot fix any of these, so they are
    /// returned to the user unchanged.
    Other,
}

/// A failed `git worktree add`, carrying the decision the caller owes it.
///
/// The message is still the exact text users saw before (`Git worktree add
/// failed: <git's stderr>`) — only the *decision* moved off prose onto
/// [`AddWorktreeFailureKind`], so no consumer has to read English git output to
/// know whether to retry.
#[derive(Debug, Clone)]
pub struct AddWorktreeFailure {
    kind: AddWorktreeFailureKind,
    message: String,
}

impl AddWorktreeFailure {
    fn new(kind: AddWorktreeFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// A failure prune-and-retry cannot help. Every non-git-source error (base
    /// ref, bare repo, spawn) lands here without being classified against git's
    /// wording at all.
    fn other(message: impl Into<String>) -> Self {
        Self::new(AddWorktreeFailureKind::Other, message)
    }

    pub fn kind(&self) -> AddWorktreeFailureKind {
        self.kind
    }

    /// The user-facing text, unchanged from the `Result<(), String>` this
    /// replaced.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for AddWorktreeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AddWorktreeFailure {}

/// Decide what a failed `git worktree add` was actually complaining about.
///
/// Only one conflict is repairable: the target path is **already registered as a
/// worktree** whose directory is gone (or otherwise disagrees with the metadata).
/// `prune_stale_worktrees` clears exactly that registration, which is why a
/// conflict is worth one retry.
///
/// The wording is git's, so it moves between versions and locales — hence the
/// classifier lives here, in the one function that shells out, rather than in
/// the caller (issue #2195). Verified against git 2.30.0.windows.2:
///
/// | git's stderr | kind |
/// |---|---|
/// | `fatal: '<path>' is a missing but already registered worktree;` (git ≥ 2.30) | [`RegisteredWorktree`] — **contains neither "already exists" nor "already checked out"**, so the pre-#2195 caller never repaired it |
/// | `fatal: '<path>' already exists` (older git, same conflict) | [`RegisteredWorktree`] |
/// | `fatal: '<path>' already used by worktree at '<other>'` (older git, same conflict) | [`RegisteredWorktree`] |
/// | `fatal: '<branch>' is already checked out at '<path>'` | [`RegisteredWorktree`] |
/// | `fatal: A branch named '<branch>' already exists.` | [`Other`] — a *branch* clash: pruning removes worktree admin entries, never branches, so the retry fails with this identical error |
/// | `fatal: Not a valid object name: '<ref>'.` | [`Other`] |
///
/// The branch-clash exclusion is the load-bearing case: it is the one failure
/// this repo can produce from git that contains "already exists" as part of a
/// *different* failure, and it is exactly what the old substring match fired
/// on — pruning stale worktrees that had nothing to do with the clash and then
/// failing anyway. The check runs first so a message naming both reads as the
/// branch clash (git reports a branch clash before it looks at worktree
/// registrations).
fn classify_worktree_add_failure(stderr: &str) -> AddWorktreeFailureKind {
    let text = stderr.to_lowercase();

    if text.contains("already exists") && text.contains("branch named") {
        return AddWorktreeFailureKind::Other;
    }

    if text.contains("already registered worktree")
        || text.contains("already used by worktree")
        || text.contains("already checked out")
        || text.contains("already exists")
    {
        return AddWorktreeFailureKind::RegisteredWorktree;
    }

    AddWorktreeFailureKind::Other
}

fn owned_worktree_name(
    name: Result<Option<&str>, git2::Error>,
) -> Result<Option<String>, git2::Error> {
    name.map(|name| name.map(str::to_string))
}

/// Remove worktrees whose on-disk path no longer exists.
fn prune_stale_worktrees(repo: &git2::Repository) {
    if let Ok(worktree_names) = repo.worktrees() {
        prune_stale_worktree_names(
            repo,
            (0..worktree_names.len()).map(|index| worktree_names.get(index)),
        );
    }
}

fn prune_stale_worktree_names<'a>(
    repo: &git2::Repository,
    names: impl IntoIterator<Item = Result<Option<&'a str>, git2::Error>>,
) {
    for (index, result) in names.into_iter().enumerate() {
        let name = match result {
            Ok(Some(name)) => name,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(index, %error, "Could not read a registered worktree name; skipping it during stale-worktree pruning");
                continue;
            }
        };
        if let Ok(wt) = repo.find_worktree(name) {
            if wt.validate().is_err() {
                let mut prune_opts = git2::WorktreePruneOptions::new();
                prune_opts.valid(false);
                let _ = wt.prune(Some(&mut prune_opts));
            }
        }
    }
}

fn has_registered_worktree_path<'a>(
    repo: &git2::Repository,
    host_path: &std::path::Path,
    names: impl IntoIterator<Item = Result<Option<&'a str>, git2::Error>>,
) -> bool {
    for (index, result) in names.into_iter().enumerate() {
        let name = match result {
            Ok(Some(name)) => name,
            Ok(None) => continue,
            Err(error) => {
                // This entry provides no readable path to compare; keep scanning
                // for a later readable match.
                tracing::warn!(index, %error, "Could not read a registered worktree name; skipping it during linked-worktree ownership check");
                continue;
            }
        };
        if let Ok(wt) = repo.find_worktree(name) {
            // `wt.path()` returns git's stored path verbatim — the same form we
            // passed in when we cut the worktree, so direct equality is the
            // safe shape (no canonicalization that could miss symlinks/UNC).
            // In the normal create flow, the same path spelling is passed to
            // Git and retained in the admin entry, so compare those paths
            // directly instead of canonicalizing them.
            if wt.path() == host_path {
                return true;
            }
        }
    }
    false
}

/// Create a new Git worktree at the specified path and honor .worktreeinclude.
pub fn create_git_worktree(
    project_root: &str,
    worktree_host_path: &str,
    branch_name: &str,
    worktree_mode: &str,
    base_ref: &str,
) -> Result<(), String> {
    create_git_worktree_with_timeout(
        project_root,
        worktree_host_path,
        branch_name,
        worktree_mode,
        base_ref,
        crate::git::sync::SPAWN_SYNC_LOCK_TIMEOUT,
    )
}

fn create_git_worktree_with_timeout(
    project_root: &str,
    worktree_host_path: &str,
    branch_name: &str,
    worktree_mode: &str,
    base_ref: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let project = super::project::resolve(project_root)?;
    let project_root = project.working_root.as_str();
    crate::services::sync_lock::try_with_mesh_sync_lock_timeout(project_root, timeout, || {
        create_git_worktree_at_root(
            project_root,
            worktree_host_path,
            branch_name,
            worktree_mode,
            base_ref,
        )
    })
    .ok_or_else(|| {
        "Timed out waiting for repository synchronization; retry the launch".to_string()
    })?
}

fn create_git_worktree_at_root(
    project_root: &str,
    worktree_host_path: &str,
    branch_name: &str,
    worktree_mode: &str,
    base_ref: &str,
) -> Result<(), String> {
    let host_path = std::path::Path::new(worktree_host_path);

    if host_path.exists() {
        trust::ensure(project_root, worktree_host_path)?;
        // Issue #1230: a pre-existing directory is only a valid resume if it
        // is already a linked worktree of `project_root`. The old
        // `return Ok(())` accepted ANY directory — empty husks from
        // interrupted `git worktree add`, stray user folders, even unrelated
        // sub-repos — and the provisioner would report `Reused` and spawn the
        // agent on a non-worktree directory. Distinguish the three failure
        // shapes explicitly:
        //   (a) valid linked worktree of project_root → resume (intentional)
        //   (b) empty directory at the target        → rename aside, proceed
        //   (c) non-empty, non-worktree directory    → refuse, user data untouched
        if is_valid_linked_worktree(project_root, host_path) {
            return Ok(());
        }
        match classify_pre_existing_directory(host_path) {
            PreExistingDir::Empty => {
                let aside = generate_husk_stash_path(host_path);
                tracing::warn!(
                    "create_git_worktree: empty husk at {} is not a valid linked \
                     worktree of {} - renaming aside to {}",
                    host_path.display(),
                    project_root,
                    aside.display()
                );
                std::fs::rename(host_path, &aside).map_err(|e| {
                    format!(
                        "refusing to overwrite empty husk at {}; rename aside to {} failed: {}",
                        host_path.display(),
                        aside.display(),
                        e
                    )
                })?;
                // Fall through to the normal create path.
            }
            PreExistingDir::NonEmpty => {
                return Err(format!(
                    "refusing to overwrite non-empty, non-worktree directory at {} \
                     (not registered as a linked worktree of {}); move the \
                     directory aside manually to recover",
                    host_path.display(),
                    project_root
                ));
            }
            PreExistingDir::Unreadable(e) => {
                return Err(format!(
                    "refusing worktree path {} (not a valid linked worktree of {}): {}",
                    host_path.display(),
                    project_root,
                    e
                ));
            }
        }
    }

    // Ensure parent directory exists
    if let Some(parent) = host_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create worktrees directory: {}", e))?;
    }

    tracing::info!(
        "Creating git worktree: {} at {} (mode: {}, base_ref: {})",
        branch_name,
        worktree_host_path,
        worktree_mode,
        base_ref
    );

    let repo = git2::Repository::open(project_root)
        .map_err(|e| format!("Failed to open repository at {}: {}", project_root, e))?;

    let use_branched = worktree_mode == "branched";

    match add_worktree_impl(&repo, branch_name, host_path, use_branched, base_ref) {
        Ok(()) => {}
        // Issue #2195: the repair decision reads the failure's *kind*, not the
        // English in git's stderr. Only a target path that is already
        // registered as a worktree is repairable by pruning; every other
        // failure (including a branch-name clash, which also says "already
        // exists") is returned to the user untouched.
        Err(err) if err.kind() == AddWorktreeFailureKind::RegisteredWorktree => {
            tracing::warn!("Worktree already registered in git metadata, attempting to repair...");
            prune_stale_worktrees(&repo);
            add_worktree_impl(&repo, branch_name, host_path, use_branched, base_ref)
                .map_err(|e| e.message().to_string())?;
        }
        Err(err) => return Err(err.message().to_string()),
    }

    apply_worktree_include(project_root, host_path);
    if let Some(container) = host_path.parent() {
        ensure_container_excluded(project_root, &container.to_string_lossy());
    }

    trust::ensure(project_root, worktree_host_path)?;
    Ok(())
}

/// True when `host_path` opens as a Repository AND is registered as a linked
/// worktree of `project_root` in `<project_root>/.git/worktrees/`. Both halves
/// are load-bearing: an unrelated sub-repo opens as a Repository but isn't a
/// linked worktree of this parent; a freshly-initialised bare directory opens
/// as neither. Mirrors git's own notion — a worktree is not just any repo, it's
/// a repo whose admin entry points back through the parent's worktrees dir.
fn is_valid_linked_worktree(project_root: &str, host_path: &std::path::Path) -> bool {
    // 1. The candidate path must open as a Repository.
    let Ok(wt_repo) = Repository::open(host_path) else {
        return false;
    };
    // Defensive: an openable-but-not-actually-a-repo shouldn't pass. A bare
    // gitdir or a corrupt object store both produce errors here.
    if wt_repo.workdir().is_none() {
        return false;
    }
    // 2. The candidate path must be in project_root's linked-worktrees list.
    let Ok(parent_repo) = Repository::open(project_root) else {
        return false;
    };
    let Ok(names) = parent_repo.worktrees() else {
        return false;
    };
    has_registered_worktree_path(
        &parent_repo,
        host_path,
        (0..names.len()).map(|index| names.get(index)),
    )
}

/// Classification of a directory that exists at a target worktree path but
/// is NOT a valid linked worktree (issue #1230). Drives the recovery /
/// refusal branch in [`create_git_worktree`].
enum PreExistingDir {
    /// Directory exists but contains no entries — safe to rename aside and
    /// replace with a fresh worktree.
    Empty,
    /// Directory exists and has at least one entry — likely user data we must
    /// not touch. Refuse the create with a descriptive error.
    NonEmpty,
    /// Directory exists but cannot be enumerated (permissions, I/O error).
    /// Refuse rather than guess; the caller can chmod and retry.
    Unreadable(String),
}

fn classify_pre_existing_directory(host_path: &std::path::Path) -> PreExistingDir {
    match std::fs::read_dir(host_path) {
        Ok(mut entries) => match entries.next() {
            None => PreExistingDir::Empty,
            Some(_) => PreExistingDir::NonEmpty,
        },
        Err(e) => PreExistingDir::Unreadable(e.to_string()),
    }
}

/// Pick a sibling name to stash an empty husk under, alongside (not inside)
/// the worktree path so the rename is always on the same filesystem. We avoid
/// `std::time` clock skew by combining the unix-epoch millis with the pid —
/// a parallel test's husk on the same millisecond would otherwise collide.
fn generate_husk_stash_path(host_path: &std::path::Path) -> std::path::PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let stem = host_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "worktree".to_string());
    let parent = host_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    parent.join(format!("{stem}.husk-{millis}-{}", std::process::id()))
}

/// Keep the parent repository's `git status` clean when the worktree
/// container lives inside it (issue #1519).
///
/// Linked worktrees are NOT auto-ignored by git: a relative container like
/// `custom-wt/` shows up as an untracked directory, `primitives::is_dirty`
/// flips true, spawn-time auto-sync aborts, and health reports permanently
/// dirty. Appending `<rel>/` to the
/// repo-local `.git/info/exclude` — never the committed `.gitignore` — keeps
/// the user's committed ignore rules untouched.
///
/// `container_host_path` is the HOST-form container dir itself (not a worktree
/// path — callers pass the parent). No-op when the container is the root
/// itself, outside the root (absolute custom dirs), or when `.git` is
/// missing. Linked worktrees and submodules use the repository's common dir.
/// Idempotent: an existing matching line is never duplicated. Best-effort
/// like `apply_worktree_include` — filesystem errors log and return so a
/// strict exclude write can never fail a spawn.
pub(crate) fn ensure_container_excluded(project_root: &str, container_host_path: &str) {
    fn squash(p: &str) -> String {
        let mut n = p.replace('\\', "/");
        while n.len() > 1 && n.ends_with('/') {
            n.pop();
        }
        n
    }
    let Ok(project) = super::project::resolve(project_root) else {
        return;
    };
    let root = squash(&project.working_root);
    let container = squash(container_host_path);
    // Prefix check is case-insensitive on Windows (case-insensitive FS);
    // the written pattern keeps its original case (git honors
    // core.ignorecase there, and exact case is required elsewhere).
    let under_root = container.len() > root.len()
        && container.as_bytes().get(root.len()) == Some(&b'/')
        && if cfg!(target_os = "windows") {
            container[..root.len()].eq_ignore_ascii_case(&root)
        } else {
            container.starts_with(root.as_str())
        };
    if !under_root {
        return;
    }
    let rel = container[root.len() + 1..].to_string();
    if rel.is_empty() {
        return;
    }
    let Ok(repo) = super::project::open(project_root) else {
        return;
    };
    let exclude = repo.commondir().join("info").join("exclude");
    let pattern = format!("{rel}/");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == pattern) {
        return;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("# buildmesh: worktree container (issue #1519)\n");
    out.push_str(&pattern);
    out.push('\n');
    if let Some(parent) = exclude.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Err(e) = std::fs::write(&exclude, out) {
        tracing::warn!(
            "ensure_worktree_container_excluded: failed to update {}: {}",
            exclude.to_string_lossy(),
            e
        );
    }
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
    let include_root = super::project::resolve(project_root)
        .map(|project| project.working_root)
        .unwrap_or_else(|_| to_host_path(project_root));
    let project_root = include_root.as_str();
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
                    tracing::warn!(
                        "Failed to copy included file {} -> {}: {}",
                        src.display(),
                        dest.display(),
                        e
                    );
                } else {
                    tracing::info!("Copied included file: {}", trimmed);
                }
            } else if src.is_dir() {
                // #248: directory entries in `.worktreeinclude` are now
                // recursively copied rather than log-and-skipped. The previous
                // "not yet implemented" branch silently dropped `config/`,
                // `build/`, `.env/` and similar untracked subtrees the agent
                // needed, forcing users into per-file `.worktreeinclude`
                // entries. `copy_dir_all` is std::fs-only (no `walkdir`/`fs_extra`
                // dep — the tree doesn't carry either). Pinned by
                // `apply_worktree_include_copies_directory_recursively` and
                // `apply_worktree_include_directory_missing_source_is_noop`.
                if let Err(e) = copy_dir_all(&src, &dest) {
                    tracing::warn!(
                        ".worktreeinclude: dir copy {} -> {} failed: {}",
                        trimmed,
                        dest.display(),
                        e
                    );
                } else {
                    tracing::info!("Copied included dir: {}", trimmed);
                }
            }
        }
    }
}

/// Recursive `cp -r` for the `.worktreeinclude` directory-copy case (#248).
///
/// The `env::apply_worktree_include` walker hits `src.is_dir()` for entries
/// like `config/`, `build/`, or `.env/` — previously these were log-and-skipped
/// (mod.rs:215), forcing users into per-file entries. `copy_dir_all` walks the
/// source subtree and mirrors it under `dest`, copying regular files and
/// recursing into subdirectories. Symlinks, FIFOs, and sockets are
/// intentionally skipped (carrying those into a freshly-checked-out worktree
/// would be a surprise — a symlink to `../../secrets.env` is a leakage vector,
/// not a feature).
///
/// **No `walkdir` dep:** the tree doesn't depend on it (`Cargo.toml`), and the
/// handful of entries in any real `.worktreeinclude` (typically 1-3 dirs, 10-50
/// files total) don't justify a new crate. `std::fs::read_dir` + a small
/// recursive helper is the right shape.
///
/// Errors propagate up: a single failed entry bubbles; the caller
/// (`apply_worktree_include`) logs and continues rather than failing the worktree
/// materialisation (the agent can still operate on the parts that did copy).
pub(crate) fn copy_dir_all(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    if !dest.exists() {
        std::fs::create_dir_all(dest)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let src_child = entry.path();
        let dest_child = dest.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_all(&src_child, &dest_child)?;
        } else if ft.is_file() {
            std::fs::copy(&src_child, &dest_child)?;
        }
        // symlinks / fifos / sockets: silently skipped (see fn doc).
    }
    Ok(())
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
    let repo = super::primitives::open_from_host_path(&host_root)
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
    super::project::resolve(&host_root)?;
    let old_trust_key = trust::key(&old)?;

    // Guard against `git worktree move`'s `mv`-style nesting: when the target
    // directory already exists, git moves the source *inside* it
    // (`<target>/<src-basename>`) and exits 0 rather than refusing. The spawn
    // path only calls this when the target is free, but refusing explicitly
    // keeps a stray leftover from silently producing a nested worktree.
    if std::path::Path::new(&new).exists() {
        return Err(format!("git worktree move target already exists: {}", new));
    }

    // Revoke before changing the path. A configuration-write failure must
    // leave the old checkout in place, never an obsolete trusted path that
    // the warm fallback can lose track of. User-owned entries are untouched.
    trust::forget_key(&old_trust_key)
        .map_err(|error| format!("Cannot revoke Git trust before moving {old}: {error}"))?;

    let mut cmd = crate::process_util::git_command();
    cmd.arg("-C")
        .arg(&host_root)
        .arg("worktree")
        .arg("move")
        .arg(&old)
        .arg(&new);

    let moved = run_bounded_git(cmd, "git worktree move", GIT_LOCAL_TIMEOUT).and_then(|output| {
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "git worktree move failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    });
    if let Err(error) = moved {
        trust::ensure(&host_root, &old)
            .map_err(|restore| format!("{error}; cannot restore source Git trust: {restore}"))?;
        return Err(error);
    }
    trust::ensure(&host_root, &new)
}

/// Hard-reset a warm pool worktree onto `sha` so it tracks a freshly-fetched
/// base ref (issue #613 ref-freshness). Runs `git -C <worktree> reset --hard
/// <sha>` — a detached-HEAD warm worktree owns no branch and has no local work
/// to preserve, so a hard reset is exactly right: it moves HEAD and the working
/// tree to the new commit, discarding the (identical-up-to-the-old-base) tree.
///
/// Shells out to the git CLI rather than libgit2 for parity with the other
/// worktree mutators here (`move_git_worktree`) and because the CLI's
/// `reset --hard` is the canonical, well-understood operation. `worktree_path`
/// is normalised to host format first so a WSL-style path never reaches
/// git.exe unconverted.
pub fn reset_warm_worktree(worktree_path: &str, sha: &str) -> Result<(), String> {
    let host = to_host_path(worktree_path);

    let mut cmd = crate::process_util::git_command();
    cmd.arg("-C").arg(&host).arg("reset").arg("--hard").arg(sha);

    let output = run_bounded_git(cmd, "git reset --hard", GIT_LOCAL_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!(
            "git reset --hard {} failed in {}: {}",
            sha,
            host,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Convert a worktree link-file path to the target environment's format
/// (Windows drive path vs WSL `/mnt/...`). Shared by both halves of
/// [`sanitize_git_worktree`] so the gitlink and the back-pointer can't drift
/// onto different conversion rules.
///
/// Both arms delegate to the single canonical converter in `env::host_path`:
/// - WSL → `windows_to_wsl`, which knows about generic drives (`F:\...`,
///   `D:\...`) and Git-Bash drive style (`/f/...`) and preserves case.
/// - Windows → `to_host_path`, which handles `/mnt/`, `/home/`, and
///   `/<letter>/` shapes.
///
/// This is a thin dispatcher; building WSL `/mnt/` or UNC strings here
/// would violate the CLAUDE.md hard rule that `env::host_path` is the
/// SOLE module allowed to construct them (issue #1226).
fn convert_link_path_for_env(path: &str, env_type: EnvType) -> String {
    match env_type {
        EnvType::Wsl => windows_to_wsl(path),
        EnvType::Windows | EnvType::WindowsInterop => to_host_path(path),
    }
}

/// Sanitize a worktree's git link files to the correct path format for the
/// target environment (Windows vs WSL). Repairs BOTH sides of the link:
///
/// - the worktree's `.git` gitlink (`gitdir: <admin dir>`), and
/// - the repo-side back-pointer (`<admin dir>/gitdir` → `<worktree>/.git`).
///
/// The back-pointer matters because libgit2 resolves it when opening a
/// worktree: an MSYS-flavoured git writes Git-Bash-style `/f/...` paths that
/// the Git CLI tolerates but `git2::Repository::open` reports as NotFound.
/// In the 2026-07-17 gh252 incident that mismatch made the autopilot wrap-up
/// verifier see a healthy, already-PR'd worktree as unopenable; fixing only
/// the gitlink (this function's pre-incident behaviour) left libgit2 locked
/// out through the admin side.
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

    let new_path = convert_link_path_for_env(git_dir_path, env_type);

    if new_path != git_dir_path {
        tracing::info!("Sanitizing .git file: {} -> {}", git_dir_path, new_path);
        // Ensure we use Unix line endings for the .git file as Git expects
        std::fs::write(&git_file_path, format!("gitdir: {}\n", new_path))
            .map_err(|e| format!("Failed to write sanitized .git file: {}", e))?;
    }

    // Repo-side back-pointer. The admin dir itself is a host filesystem
    // access, so resolve it host-side regardless of the target env format.
    let backpointer_path = std::path::Path::new(&to_host_path(&new_path)).join("gitdir");
    if backpointer_path.is_file() {
        let existing = std::fs::read_to_string(&backpointer_path)
            .map_err(|e| format!("Failed to read worktree gitdir back-pointer: {}", e))?;
        let existing = existing.trim();
        let converted = convert_link_path_for_env(existing, env_type);
        if converted != existing {
            tracing::info!(
                "Sanitizing worktree gitdir back-pointer: {} -> {}",
                existing,
                converted
            );
            std::fs::write(&backpointer_path, format!("{}\n", converted))
                .map_err(|e| format!("Failed to write sanitized gitdir back-pointer: {}", e))?;
        }
    }

    Ok(())
}

/// A cross-runtime worktree keeps administration on the host. Older Git
/// accepts relative forward links, but treats relative backpointers as missing
/// worktrees. Lock the administrative entry so guest pruning cannot delete it.
/// Buildmesh's removal path already explicitly prunes locked entries.
pub(crate) fn prepare_cross_runtime_worktree(worktree_host_path: &str) -> Result<(), String> {
    if !std::path::Path::new(worktree_host_path)
        .join(".git")
        .is_file()
    {
        return Ok(());
    }
    sanitize_git_worktree(worktree_host_path, EnvType::Windows)?;
    let repo = Repository::open(worktree_host_path).map_err(|e| e.to_string())?;
    if !repo.is_worktree() {
        return Ok(());
    }
    let root = std::fs::canonicalize(worktree_host_path).map_err(|e| e.to_string())?;
    let admin = std::fs::canonicalize(repo.path()).map_err(|e| e.to_string())?;
    let root_parts = root.components().collect::<Vec<_>>();
    let admin_parts = admin.components().collect::<Vec<_>>();
    let shared = root_parts
        .iter()
        .zip(&admin_parts)
        .take_while(|(a, b)| a == b)
        .count();
    if shared == 0 {
        return Err("Cross-runtime worktrees must share a filesystem with their repository".into());
    }
    let mut relative = std::path::PathBuf::new();
    for _ in shared..root_parts.len() {
        relative.push("..");
    }
    for part in &admin_parts[shared..] {
        relative.push(part.as_os_str());
    }
    let worktree = git2::Worktree::open_from_repository(&repo).map_err(|e| e.to_string())?;
    if matches!(worktree.is_locked(), Ok(git2::WorktreeLockStatus::Unlocked)) {
        worktree
            .lock(Some(
                "Buildmesh cross-runtime worktree; manage through Buildmesh",
            ))
            .map_err(|e| e.to_string())?;
    }
    std::fs::write(
        root.join(".git"),
        format!(
            "gitdir: {}\n",
            relative.to_string_lossy().replace('\\', "/")
        ),
    )
    .map_err(|e| e.to_string())
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
    let current_refname = head.name().ok();

    if !is_detached {
        if let Some(refname) = current_refname {
            if let Ok(upstream_buf) = repo.branch_upstream_name(refname) {
                // Each step short-circuits on success only. A missing or
                // unreadable tracking ref is the everyday post-merge state
                // (`git status` shows `[gone]` after a squash-merge + remote
                // auto-delete); the reachability walk below is the better
                // safety net there, so we fall through instead of warning
                // unconditionally (#252).
                if let Ok(upstream_refname) = upstream_buf.as_str() {
                    if let Ok(upstream_ref) = repo.find_reference(upstream_refname) {
                        if let Some(upstream_oid) = upstream_ref.target() {
                            return primitives::ahead_behind(repo, head_oid, upstream_oid)
                                .map(|(ahead, _)| ahead > 0)
                                .unwrap_or(true);
                        }
                    }
                }
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
        let Ok(name) = reference.name() else {
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
const WORKTREE_REMOVE_ATTEMPTS: u32 = 60;
const WORKTREE_REMOVE_BACKOFF_MS: u64 = 100;

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
pub const OP_REVOKE_TRUST: &str = "revoke-worktree-git-trust";

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
    #[cfg(test)]
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

fn remove_worktree_inner(path: &str, delete_branch: bool) -> Result<(), WorktreeRemovalFailure> {
    let host_path = to_host_path(path);
    let dir = std::path::Path::new(&host_path);
    let trust_key =
        trust::key(&host_path).map_err(|e| WorktreeRemovalFailure::new(OP_REVOKE_TRUST, e))?;

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
        trust::forget_key(&trust_key)
            .map_err(|e| WorktreeRemovalFailure::new(OP_REVOKE_TRUST, e))?;
        return Ok(());
    }

    let repo = Repository::open(&host_path)
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
    trust::forget_key(&trust_key).map_err(|e| WorktreeRemovalFailure::new(OP_REVOKE_TRUST, e))?;

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
fn staging_path(path: &str) -> String {
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
fn remove_worktree_dir_with_retry(path: &str) -> Result<(), WorktreeRemovalFailure> {
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
fn prune_orphaned_worktree_entry(host_path: &str) -> Result<(), WorktreeRemovalFailure> {
    prune_orphaned_worktree_entry_with(host_path, |parent| Repository::discover(parent))
}

/// [`prune_orphaned_worktree_entry`] with repository discovery injected, so the
/// "no repository above this path" and "cannot read the repository" branches
/// can be told apart by a test (issue #2139 review round 2).
fn prune_orphaned_worktree_entry_with(
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
fn admin_entry_points_at(admin_dir: &std::path::Path, target: &std::path::Path) -> bool {
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
fn normalize_parent(path: &std::path::Path) -> std::path::PathBuf {
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
fn inode_same(a: &std::path::Path, target: &std::path::Path) -> bool {
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
fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
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

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
