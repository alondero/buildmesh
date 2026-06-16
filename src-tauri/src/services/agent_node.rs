//! Agent Node service — creation, deletion, and lifecycle orchestration

use crate::db;
use crate::env;
use crate::git::worktree::{self, WorktreeCloseSafety};
use crate::models::{AgentNode, PendingWorktreeRemoval, Provider, SessionStatus};

/// Error type for agent node service operations
#[derive(Debug)]
pub enum AgentNodeError {
    Db(rusqlite::Error),
}

impl std::fmt::Display for AgentNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentNodeError::Db(e) => write!(f, "{}", e),
        }
    }
}

impl From<rusqlite::Error> for AgentNodeError {
    fn from(e: rusqlite::Error) -> Self {
        AgentNodeError::Db(e)
    }
}

/// Create a new agent node with auto-generated name, environment detection,
/// and provider resolution.
pub fn create(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    // Hand off to the explicit-source version with `source_pr = None` so the
    // single create() call site for issue-spawn / handover / generic paths
    // stays binary-compatible. The PR-spawn flow uses `create_with_source_pr`
    // (or `create_pending_with_source_pr` for the two-stage variant) to
    // record the originating PR number — see issue #420.
    create_with_source_pr(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        None, // source_pr
        None, // source_pr_pinned_sha — issue-spawn / generic paths don't pin
        use_worktree_override,
        name_override,
    )
}

/// Like [`create`], but accepts a `source_pr` so PR-spawned nodes (#420) can
/// record the originating PR number. Used by `commands::agent::create_pr_node`
/// and (transitively) by the stage-1 split of the PR-spawn flow. For all
/// other spawn paths the `create` wrapper passes `None` here.
///
/// `source_pr_pinned_sha` (issue #444) is the exact-pinning handle — the PR's
/// head commit SHA at the moment the user clicked Spawn. The spawn path
/// verifies the local `origin/<head_ref>` SHA matches it after `git fetch`
/// and emits a `pr_sha_drift` warning if not. `None` is fine: a row without
/// a pinned SHA simply skips the drift check (same fail-open semantics as
/// `pr_head_unfetchable`).
///
/// For fork PRs (issue #443), prefer [`create_with_source_pr_fork`] — it
/// also records the fork's owner login + clone URL so `spawn_agent_inner`
/// can add the fork as a remote and fetch the head ref. This entry point
/// is the binary-compatible trampoline for every other call site
/// (issue-spawn, handover, hand-spawn): it forwards to the fork-aware
/// variant with `head_repo_owner = None, head_repo_clone_url = None` and
/// `source_pr_pinned_sha` threaded through unchanged.
#[allow(clippy::too_many_arguments)]
pub fn create_with_source_pr(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    // Hand off to the explicit-fork variant with the fork fields set to None.
    // Every pre-#443 call site stays binary-compatible; the fork-aware
    // variant is the single insert path that knows how to record both the
    // PR number and (when applicable) the fork remote metadata.
    create_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree_override,
        name_override,
        None,
        None,
    )
}

/// Like [`create_with_source_pr`], but also records the fork's owner login
/// and clone URL when the PR is from a fork (issue #443, follow-up to #36
/// worktree adoption), and the PR's head commit SHA (issue #444) for
/// exact-pinning. `spawn_agent_inner` reads the fork fields to register
/// `fork-<owner>` as a remote and fetch the head ref from it, and the SHA
/// to verify the local SHA after `git fetch` (drift check). For same-repo
/// PRs the head's `repo.owner.login` is the destination owner — the caller
/// should still pass `None` for both fork fields (the spawn path then takes
/// the #420 `origin/<head_ref>` branch).
#[allow(clippy::too_many_arguments)]
pub fn create_with_source_pr_fork(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    use_worktree_override: Option<bool>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    let mesh = db::get_mesh_by_id(mesh_id)?;
    let use_worktree = use_worktree_override.unwrap_or(mesh.use_worktree);

    // Caller may supply a pre-derived name (e.g. `slugify_issue_title` for the
    // GitHub-issue spawn path). Validation/fallback is the caller's job — by
    // the time we get here, the name is assumed to be a valid `SLUG_REGEX`
    // match, which is also a safe directory name.
    let session_name = match name_override {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => crate::session_naming::on_spawn(),
    };
    tracing::debug!(
        "agent_node::create: mesh_id={}, name={}, path={}, branch={}, provider={:?}, use_worktree={}, source_pr={:?}, source_pr_pinned_sha={:?}, head_repo_owner={:?}",
        mesh_id, session_name, path, branch, provider, use_worktree, source_pr, source_pr_pinned_sha, head_repo_owner
    );

    let worktree_db_name = if use_worktree {
        Some(session_name.as_str())
    } else {
        None
    };

    let resolved = env::resolve_agent_path(path, worktree_db_name);
    let env_type = resolved.env_type;
    let provider_enum = provider
        .map(Provider::from_db_str)
        .unwrap_or(Provider::Anthropic);

    let node = db::create_agent_node(
        mesh_id,
        &session_name,
        path,
        branch,
        env_type,
        provider_enum,
        worktree_db_name,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        use_worktree,
        head_repo_owner,
        head_repo_clone_url,
    )?;

    Ok(node)
}

/// Like `create`, but sets the initial status to [`SessionStatus::Pending`]
/// so the frontend can distinguish "node row exists, stage-2 not yet
/// started" from "node row exists, agent is idle and ready to re-spawn".
///
/// This is the fast stage-1 of the two-stage issue-spawn flow. The caller
/// is expected to invoke `start_node_background` (or a future
/// fire-and-forget equivalent) to do the slow work — git fetch, worktree
/// create, PTY spawn — and update the status to `Running` on success or
/// `Error` on failure.
pub fn create_pending(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    // Hand off to the explicit-source variant with `source_pr = None` so the
    // existing issue-spawn call sites stay binary-compatible.
    create_pending_with_source_pr(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        None, // source_pr
        None, // source_pr_pinned_sha — issue-spawn doesn't pin
        name_override,
    )
}

/// Like [`create_pending`], but accepts a `source_pr` and `source_pr_pinned_sha`
/// for the PR-spawn flow (issues #420 + #444). See `create_with_source_pr` for
/// the underlying mechanics. This is the binary-compatible trampoline every
/// pre-#443 caller uses — it forwards to [`create_pending_with_source_pr_fork`]
/// with the fork fields set to `None`.
#[allow(clippy::too_many_arguments)]
pub fn create_pending_with_source_pr(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    create_pending_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        name_override,
        None,
        None,
    )
}

/// Like [`create_pending_with_source_pr`], but also records the fork's
/// owner login + clone URL when the PR is from a fork (issue #443) and
/// the PR's head commit SHA for exact-pinning (issue #444). The PR-spawn
/// flow (`commands::agent::create_pr_node`) calls this with the two fork
/// fields populated for fork PRs and `None, None` for same-repo PRs.
#[allow(clippy::too_many_arguments)]
pub fn create_pending_with_source_pr_fork(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
    source_pr: Option<i64>,
    source_pr_pinned_sha: Option<&str>,
    name_override: Option<&str>,
    head_repo_owner: Option<&str>,
    head_repo_clone_url: Option<&str>,
) -> Result<AgentNode, AgentNodeError> {
    let mut node = create_with_source_pr_fork(
        mesh_id,
        path,
        branch,
        provider,
        source_issue,
        source_pr,
        source_pr_pinned_sha,
        None,
        name_override,
        head_repo_owner,
        head_repo_clone_url,
    )?;
    // Two writes (insert + status update) is one extra ~1ms SQLite round
    // trip. Acceptable: this function is on the fast path, and the second
    // write is the whole point — without it the new node would look
    // identical to a freshly-closed, ready-to-resume idle node.
    db::update_agent_node_status(node.id, SessionStatus::Pending)?;
    node.status = SessionStatus::Pending;
    Ok(node)
}

/// Return whether closing the node can remove its worktree without risking work.
pub fn get_worktree_close_safety(session_id: i64) -> Result<WorktreeCloseSafety, AgentNodeError> {
    let node = db::get_agent_node_by_id(session_id)?;
    let Some(worktree_path) = env::node_worktree_path(&node).map(|r| r.host_path) else {
        return Ok(WorktreeCloseSafety {
            worktree_path: None,
            has_uncommitted: false,
            has_unpushed: false,
            is_detached: false,
        });
    };

    Ok(worktree::close_safety(&worktree_path))
}

/// Close an agent node (Phase 1 — fast and authoritative).
///
/// This kills the agent process tree and removes the node from the database
/// immediately, but deliberately does **not** delete the worktree directory:
/// that recursive delete is slow (a `node_modules`-heavy tree is tens of
/// thousands of files) and retry-prone on Windows, so blocking the close on it
/// is what makes the UI feel frozen. Instead, when a worktree should be removed
/// we record it in the durable `pending_worktree_removals` queue — atomically
/// with deleting the row — and let `process_pending_removals` reclaim the disk
/// in the background or on next launch. The kill stays synchronous here: it's
/// fast, and it's the one step we must finish before parting with the node so we
/// never leave a live agent running or fighting the eventual removal (#239).
pub fn delete(session_id: i64, remove_worktree: bool) -> Result<(), AgentNodeError> {
    let node = db::get_agent_node_by_id(session_id)?;

    let removal_path = if remove_worktree {
        crate::agent::process::PROCESS_REGISTRY.kill_session(session_id);
        crate::agent::process::PROCESS_REGISTRY.remove(&session_id);
        env::node_worktree_path(&node).map(|r| r.host_path)
    } else {
        None
    };

    crate::session_naming::cleanup(session_id);

    let removal = removal_path.as_deref().map(|p| (p, node.name.as_str()));
    db::delete_agent_node_enqueueing_removal(session_id, removal)?;
    Ok(())
}

/// Serializes drains. Each drain processes the *whole* queue, so two running at
/// once (e.g. closing two nodes in quick succession, or a close overlapping the
/// startup reconcile) would both try to remove the same just-enqueued worktree
/// and race on its `.removing` staging directory. Holding this for the drain's
/// duration means a second drain waits, then re-lists and finds the first
/// already dequeued — no wasted work, no spurious cleanup-failed warnings.
static DRAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Drain the pending worktree-removal queue: remove each queued worktree's
/// directory, dequeuing only the ones that succeed. Removals that still fail
/// (e.g. a handle is somehow held) stay queued for the next drain or next
/// launch, and are returned so the caller can warn the user. Safe to run from
/// the close path and from startup reconcile — `remove_one_worktree` is
/// idempotent (a missing directory counts as already removed).
pub fn process_pending_removals() -> Vec<(PendingWorktreeRemoval, String)> {
    let _guard = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let pending = match db::list_pending_worktree_removals() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("could not read pending worktree removals: {}", e);
            return Vec::new();
        }
    };

    process_removals(
        pending,
        crate::git::worktree::remove_one_worktree_and_branch,
        db::delete_pending_worktree_removal,
    )
}

/// Pure orchestration for the drain: for each removal, attempt the directory
/// remove; on success dequeue it; on failure keep it queued and collect it.
/// Dependencies are injected so the "dequeue only on success" invariant — the
/// thing that guarantees failed cleanups get retried rather than lost — is
/// testable without the global DB or a real filesystem.
fn process_removals(
    pending: Vec<PendingWorktreeRemoval>,
    remove: impl Fn(&str) -> Result<(), String>,
    dequeue: impl Fn(&str) -> db::SqlResult<()>,
) -> Vec<(PendingWorktreeRemoval, String)> {
    let mut failures = Vec::new();
    for removal in pending {
        match remove(&removal.worktree_path) {
            Ok(()) => {
                if let Err(e) = dequeue(&removal.worktree_path) {
                    tracing::error!(
                        "removed worktree {} but failed to dequeue it: {}",
                        removal.worktree_path, e
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "worktree removal for {} failed, will retry: {}",
                    removal.node_name, e
                );
                failures.push((removal, e));
            }
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let tmp =
                std::env::temp_dir().join(format!("buildmesh_agent_node_test_{}_{}", pid, id));
            Self(tmp)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sig() -> git2::Signature<'static> {
        git2::Signature::now("test", "test@example.com").unwrap()
    }

    fn init_repo(path: &Path) -> git2::Repository {
        fs::create_dir_all(path).unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(path, &opts).unwrap();

        fs::write(path.join("file.txt"), "initial").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let s = sig();
        repo.commit(Some("HEAD"), &s, &s, "initial commit", &tree, &[])
            .unwrap();
        drop(tree);

        repo
    }

    fn add_worktree(root_repo: &git2::Repository, root: &TempDir, name: &str) -> PathBuf {
        let head = root_repo.head().unwrap().peel_to_commit().unwrap();
        let branch = root_repo.branch(name, &head, false).unwrap();
        let reference = branch.into_reference();
        let worktree_path = root.path().join(".claude").join("worktrees").join(name);
        fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
        root_repo
            .worktree(
                name,
                &worktree_path,
                Some(git2::WorktreeAddOptions::new().reference(Some(&reference))),
            )
            .unwrap();
        worktree_path
    }

    #[test]
    fn drain_removes_real_worktree_and_dequeues_only_on_success() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let good = add_worktree(&root_repo, &root, "wt-good");

        let pending = vec![
            PendingWorktreeRemoval {
                worktree_path: good.to_string_lossy().to_string(),
                node_name: "wt-good".to_string(),
            },
            PendingWorktreeRemoval {
                worktree_path: "/nonexistent/wt-bad".to_string(),
                node_name: "wt-bad".to_string(),
            },
        ];

        let dequeued = std::cell::RefCell::new(Vec::<String>::new());
        let failures = process_removals(
            pending,
            |path| {
                // A missing parent repo can't be opened → genuine failure.
                if path.starts_with("/nonexistent") {
                    return Err("not a removable worktree".to_string());
                }
                crate::git::worktree::remove_one_worktree(path)
            },
            |path| {
                dequeued.borrow_mut().push(path.to_string());
                Ok(())
            },
        );

        assert!(!good.exists(), "the real worktree directory must be removed");
        assert_eq!(
            dequeued.into_inner(),
            vec![good.to_string_lossy().to_string()],
            "only the successful removal is dequeued"
        );
        assert_eq!(failures.len(), 1, "the failed removal is returned for warning");
        assert_eq!(failures[0].0.node_name, "wt-bad");
    }

}
