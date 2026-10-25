//! Create and validate owned linked worktrees, including stale registrations and husks.

use super::*;
// ── Create ──────────────────────────────────────────────────────────────────

/// Add a worktree to the repo — branched (new branch) or detached — based on
/// the commit resolved from `base_ref` (see [`crate::git::sha::resolve_sha`]).
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
/// keeps the one resolver's offline-fallback semantics) and hand git the
/// 40-char hex SHA. Passing a SHA — never a ref like `@{u}` — also sidesteps
/// the Windows `Command::args` `{}`-stripping trap (see the curly-brace-arg
/// memory). `-b <name>` gives branched mode a real branch at the base; detached
/// mode uses `--detach` and creates no branch at all (cleaner than the old
/// create-branch-then-detach-and-delete dance).
pub(super) fn add_worktree_impl(
    repo: &git2::Repository,
    branch_name: &str,
    host_path: &std::path::Path,
    use_branched: bool,
    base_ref: &str,
) -> Result<(), AddWorktreeFailure> {
    let base_sha = crate::git::sha::resolve_sha(
        repo,
        &crate::git::sha::ShaSource::BaseRef(base_ref.to_string()),
    )
    .map_err(|e| AddWorktreeFailure::other(e.to_string()))?
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
    pub(super) message: String,
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
pub(super) fn classify_worktree_add_failure(stderr: &str) -> AddWorktreeFailureKind {
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

pub(super) fn owned_worktree_name(
    name: Result<Option<&str>, git2::Error>,
) -> Result<Option<String>, git2::Error> {
    name.map(|name| name.map(str::to_string))
}

/// Remove worktrees whose on-disk path no longer exists.
pub(super) fn prune_stale_worktrees(repo: &git2::Repository) {
    if let Ok(worktree_names) = repo.worktrees() {
        prune_stale_worktree_names(
            repo,
            (0..worktree_names.len()).map(|index| worktree_names.get(index)),
        );
    }
}

pub(super) fn prune_stale_worktree_names<'a>(
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

pub(super) fn has_registered_worktree_path<'a>(
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
            // WSL has two equivalent Windows share names. Normalize only
            // those names, preserving the distribution and guest path case.
            let aliases = crate::env::wsl_host_path_aliases(wt.path());
            if wt.path() == host_path
                || (cfg!(windows)
                    && !aliases.is_empty()
                    && aliases == crate::env::wsl_host_path_aliases(host_path))
            {
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

pub(super) fn create_git_worktree_with_timeout(
    project_root: &str,
    worktree_host_path: &str,
    branch_name: &str,
    worktree_mode: &str,
    base_ref: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let project = crate::git::project::resolve(project_root)?;
    let project_root = project.working_root.as_str();
    crate::git::sync_lock::try_with_mesh_sync_lock_timeout(project_root, timeout, || {
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

pub(super) fn create_git_worktree_at_root(
    project_root: &str,
    worktree_host_path: &str,
    branch_name: &str,
    worktree_mode: &str,
    base_ref: &str,
) -> Result<(), String> {
    let host_path = std::path::Path::new(worktree_host_path);

    if host_path.exists() {
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
        if is_valid_linked_worktree(project_root, host_path)? {
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

    let repo = crate::git::project::open(project_root)
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

    crate::git::trust::ensure_linked_worktree(&repo, host_path).map_err(|e| e.to_string())?;

    apply_worktree_include(project_root, host_path);
    if let Some(container) = host_path.parent() {
        ensure_container_excluded(project_root, &container.to_string_lossy());
    }

    Ok(())
}

/// True when `host_path` opens as a Repository AND is registered as a linked
/// worktree of `project_root` in `<project_root>/.git/worktrees/`. Both halves
/// are load-bearing: an unrelated sub-repo opens as a Repository but isn't a
/// linked worktree of this parent; a freshly-initialised bare directory opens
/// as neither. Mirrors git's own notion — a worktree is not just any repo, it's
/// a repo whose admin entry points back through the parent's worktrees dir.
pub(super) fn is_valid_linked_worktree(
    project_root: &str,
    host_path: &std::path::Path,
) -> Result<bool, String> {
    let parent_repo = crate::git::project::open(project_root).map_err(|e| e.to_string())?;
    // Ownership/configuration failures are access failures, not evidence that
    // this directory is stray data. Repair trust only after proving linkage.
    if let Err(error) = Repository::open(host_path) {
        if error.code() == git2::ErrorCode::NotFound {
            return Ok(false);
        }
        if error.code() != git2::ErrorCode::Owner {
            return Err(error.to_string());
        }
        crate::git::trust::ensure_linked_worktree(&parent_repo, host_path)
            .map_err(|e| e.to_string())?;
    }
    // 1. The candidate path must open as a Repository.
    let wt_repo = Repository::open(host_path).map_err(|e| e.to_string())?;
    // Defensive: an openable-but-not-actually-a-repo shouldn't pass. A bare
    // gitdir or a corrupt object store both produce errors here.
    if wt_repo.workdir().is_none() {
        return Ok(false);
    }
    // 2. The candidate path must be in project_root's linked-worktrees list.
    let names = parent_repo.worktrees().map_err(|e| e.to_string())?;
    Ok(has_registered_worktree_path(
        &parent_repo,
        host_path,
        (0..names.len()).map(|index| names.get(index)),
    ))
}

/// Classification of a directory that exists at a target worktree path but
/// is NOT a valid linked worktree (issue #1230). Drives the recovery /
/// refusal branch in [`create_git_worktree`].
pub(super) enum PreExistingDir {
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

pub(super) fn classify_pre_existing_directory(host_path: &std::path::Path) -> PreExistingDir {
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
pub(super) fn generate_husk_stash_path(host_path: &std::path::Path) -> std::path::PathBuf {
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

impl std::error::Error for AddWorktreeFailure {}

impl std::fmt::Display for AddWorktreeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
