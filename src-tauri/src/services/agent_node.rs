//! Agent Node service — creation, deletion, and lifecycle orchestration

use crate::db;
use crate::env;
use crate::models::{AgentNode, PendingWorktreeRemoval, Provider, SessionStatus};
use git2::{Oid, Repository, StatusOptions};
use serde::Serialize;

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

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeCloseSafety {
    pub worktree_path: Option<String>,
    pub has_uncommitted: bool,
    pub has_unpushed: bool,
    pub is_detached: bool,
}

/// Create a new agent node with auto-generated name, environment detection,
/// and provider resolution.
pub fn create(
    mesh_id: i64,
    path: &str,
    branch: &str,
    provider: Option<&str>,
    source_issue: Option<i64>,
) -> Result<AgentNode, AgentNodeError> {
    let session_name = crate::session_naming::on_spawn();
    tracing::debug!(
        "agent_node::create: mesh_id={}, name={}, path={}, branch={}, provider={:?}",
        mesh_id, session_name, path, branch, provider
    );

    let resolved = env::resolve_agent_path(path, None);
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
        Some(&session_name),
        source_issue,
    )?;

    Ok(node)
}

/// Return whether closing the node can remove its worktree without risking work.
pub fn get_worktree_close_safety(session_id: i64) -> Result<WorktreeCloseSafety, AgentNodeError> {
    let node = db::get_agent_node_by_id(session_id)?;
    let Some(worktree_path) = worktree_path_for_node(&node) else {
        return Ok(WorktreeCloseSafety {
            worktree_path: None,
            has_uncommitted: false,
            has_unpushed: false,
            is_detached: false,
        });
    };

    Ok(close_safety_for_worktree_path(&worktree_path))
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
        worktree_path_for_node(&node)
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
        crate::commands::prune::remove_one_worktree,
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

/// Update agent node status from a string representation.
pub fn update_status(session_id: i64, status: &str) -> Result<(), AgentNodeError> {
    let status = SessionStatus::from_db_str(status);
    db::update_agent_node_status(session_id, status)?;
    Ok(())
}

fn worktree_path_for_node(node: &AgentNode) -> Option<String> {
    if !node.use_worktree {
        return None;
    }

    let worktree_name = node.worktree_name.as_deref()?.trim();
    if worktree_name.is_empty() {
        return None;
    }

    Some(env::resolve_agent_path(&node.path, Some(worktree_name)).host_path)
}

/// Inspect a worktree to decide whether closing its node can remove it.
///
/// A worktree we can't even open as a repository — its directory is gone or its
/// git metadata is unreadable — has no detectable work to protect and nothing
/// removable, so it reports `worktree_path: None` ("nothing to remove, safe to
/// close") rather than failing. Blocking the close on it would leave the node
/// permanently un-closable from the UI (#239).
fn close_safety_for_worktree_path(path: &str) -> WorktreeCloseSafety {
    let host_path = env::to_host_path(path);
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

    let has_uncommitted = repo_has_uncommitted(&repo);
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

fn repo_has_uncommitted(repo: &Repository) -> bool {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    match repo.statuses(Some(&mut opts)) {
        Ok(statuses) => statuses
            .iter()
            .any(|entry| !entry.status().is_ignored() && entry.status() != git2::Status::CURRENT),
        Err(_) => true,
    }
}

fn head_has_unpushed_or_unmerged_commits(
    repo: &Repository,
    head: &git2::Reference,
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
                return repo
                    .graph_ahead_behind(head_oid, upstream_oid)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AgentNode, EnvType, Provider};
    use chrono::Utc;
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

        fn path_str(&self) -> String {
            self.0.to_string_lossy().to_string()
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

    fn commit_file(repo: &git2::Repository, name: &str, content: &str) {
        let workdir = repo.workdir().unwrap();
        fs::write(workdir.join(name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let s = sig();
        repo.commit(Some("HEAD"), &s, &s, "commit", &tree, &[&parent])
            .unwrap();
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

    fn make_node(root: &TempDir, worktree_name: Option<&str>) -> AgentNode {
        AgentNode {
            id: 1,
            mesh_id: 1,
            name: worktree_name.unwrap_or("mesh-root").to_string(),
            path: root.path_str(),
            branch: "main".to_string(),
            env: EnvType::Windows,
            provider: Provider::Anthropic,
            status: SessionStatus::Idle,
            cli_session_id: None,
            worktree_name: worktree_name.map(str::to_string),
            use_worktree: worktree_name.is_some(),
            source_issue: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn close_safety_reports_clean_merged_worktree_as_safe() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let worktree_path = add_worktree(&root_repo, &root, "wt-safe");

        let safety = close_safety_for_worktree_path(&worktree_path.to_string_lossy());

        assert!(!safety.has_uncommitted);
        assert!(!safety.has_unpushed);
        assert!(!safety.is_detached);
    }

    #[test]
    fn close_safety_reports_uncommitted_changes() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let worktree_path = add_worktree(&root_repo, &root, "wt-dirty");
        fs::write(worktree_path.join("scratch.txt"), "not committed").unwrap();

        let safety = close_safety_for_worktree_path(&worktree_path.to_string_lossy());

        assert!(safety.has_uncommitted);
        assert!(!safety.has_unpushed);
    }

    #[test]
    fn close_safety_reports_branch_commits_not_reachable_from_another_ref() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let worktree_path = add_worktree(&root_repo, &root, "wt-ahead");
        let worktree_repo = git2::Repository::open(&worktree_path).unwrap();
        commit_file(&worktree_repo, "work.txt", "committed locally");

        let safety = close_safety_for_worktree_path(&worktree_path.to_string_lossy());

        assert!(!safety.has_uncommitted);
        assert!(safety.has_unpushed);
    }

    #[test]
    fn close_safety_reports_detached_head_without_extra_commits_as_safe() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let worktree_path = add_worktree(&root_repo, &root, "wt-detached-safe");
        let worktree_repo = git2::Repository::open(&worktree_path).unwrap();
        let head = worktree_repo.head().unwrap().peel_to_commit().unwrap();
        worktree_repo.set_head_detached(head.id()).unwrap();

        let safety = close_safety_for_worktree_path(&worktree_path.to_string_lossy());

        assert!(safety.is_detached);
        assert!(!safety.has_unpushed);
    }

    #[test]
    fn close_safety_reports_detached_head_with_unique_commits_as_unpushed() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let worktree_path = add_worktree(&root_repo, &root, "wt-detached-risk");
        let worktree_repo = git2::Repository::open(&worktree_path).unwrap();
        let head = worktree_repo.head().unwrap().peel_to_commit().unwrap();
        worktree_repo.set_head_detached(head.id()).unwrap();
        commit_file(&worktree_repo, "detached.txt", "detached work");

        let safety = close_safety_for_worktree_path(&worktree_path.to_string_lossy());

        assert!(safety.is_detached);
        assert!(safety.has_unpushed);
    }

    #[test]
    fn worktree_path_for_node_resolves_named_worktree() {
        let root = TempDir::new();
        let node = make_node(&root, Some("wt-remove"));

        let path = worktree_path_for_node(&node).expect("worktree node has a path");

        assert!(path.ends_with("wt-remove"), "path points at the named worktree");
    }

    #[test]
    fn worktree_path_for_node_is_none_without_worktree_name() {
        // The mesh-root node has no worktree_name — closing it must never derive
        // a removable path, or close would queue the mesh root for deletion.
        let root = TempDir::new();
        let node = make_node(&root, None);

        assert_eq!(worktree_path_for_node(&node), None);
    }

    #[test]
    fn worktree_path_for_node_is_none_when_use_worktree_false() {
        let root = TempDir::new();
        let mut node = make_node(&root, Some("wt-inplace"));
        node.use_worktree = false;

        assert_eq!(worktree_path_for_node(&node), None);
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
                crate::commands::prune::remove_one_worktree(path)
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

    #[test]
    fn close_safety_treats_missing_worktree_as_nothing_to_remove() {
        let root = TempDir::new();
        let missing = root.path().join("does-not-exist");

        let safety = close_safety_for_worktree_path(&missing.to_string_lossy());

        // A worktree we can't open has no work to protect and nothing to remove,
        // so it must never block the close (#239).
        assert_eq!(safety.worktree_path, None);
        assert!(!safety.has_uncommitted);
        assert!(!safety.has_unpushed);
        assert!(!safety.is_detached);
    }

    #[test]
    fn close_safety_treats_non_repo_directory_as_nothing_to_remove() {
        let root = TempDir::new();
        fs::create_dir_all(root.path()).unwrap();
        fs::write(root.path().join("stray.txt"), "not a git repo").unwrap();

        let safety = close_safety_for_worktree_path(&root.path_str());

        assert_eq!(safety.worktree_path, None);
        assert!(!safety.has_uncommitted);
    }
}
