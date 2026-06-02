//! Agent Node service — creation, deletion, and lifecycle orchestration

use crate::db;
use crate::env;
use crate::models::{AgentNode, Provider, SessionStatus};
use git2::{Oid, Repository, StatusOptions};
use serde::Serialize;

/// Error type for agent node service operations
#[derive(Debug)]
pub enum AgentNodeError {
    Db(rusqlite::Error),
    Git(String),
}

impl std::fmt::Display for AgentNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentNodeError::Db(e) => write!(f, "{}", e),
            AgentNodeError::Git(e) => write!(f, "{}", e),
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

/// Delete an agent node, cleaning up associated runtime state (node_namer buffers).
pub fn delete(session_id: i64, remove_worktree: bool) -> Result<(), AgentNodeError> {
    let node = db::get_agent_node_by_id(session_id)?;

    if remove_worktree {
        // A live agent keeps file handles open inside its worktree, which pins
        // the directory and blocks removal. Kill the whole process tree before
        // touching the worktree so removal isn't fighting a running agent — the
        // frontend kills first too, but this keeps removal correct for any other
        // caller (HTTP, tests) and makes the kill→remove ordering explicit (#239).
        crate::agent::process::PROCESS_REGISTRY.kill_session(session_id);
        crate::agent::process::PROCESS_REGISTRY.remove(&session_id);
    }

    delete_worktree_for_node(&node, remove_worktree)?;
    crate::session_naming::cleanup(session_id);
    db::delete_agent_node(session_id)?;
    Ok(())
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

fn delete_worktree_for_node(node: &AgentNode, remove_worktree: bool) -> Result<(), AgentNodeError> {
    if !remove_worktree {
        return Ok(());
    }

    let Some(worktree_path) = worktree_path_for_node(node) else {
        return Ok(());
    };

    crate::commands::prune::remove_one_worktree(&worktree_path).map_err(AgentNodeError::Git)
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
    fn delete_worktree_for_node_removes_named_worktree_when_requested() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let worktree_path = add_worktree(&root_repo, &root, "wt-remove");
        let node = make_node(&root, Some("wt-remove"));

        delete_worktree_for_node(&node, true).unwrap();

        assert!(
            !worktree_path.exists(),
            "linked worktree directory should be pruned"
        );
        assert!(root.path().exists(), "mesh root must remain");
    }

    #[test]
    fn delete_worktree_for_node_keeps_named_worktree_when_not_requested() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let worktree_path = add_worktree(&root_repo, &root, "wt-keep");
        let node = make_node(&root, Some("wt-keep"));

        delete_worktree_for_node(&node, false).unwrap();

        assert!(
            worktree_path.exists(),
            "linked worktree should be left on disk"
        );
        assert!(root.path().exists(), "mesh root must remain");
    }

    #[test]
    fn delete_worktree_for_node_never_removes_mesh_root_without_worktree_name() {
        let root = TempDir::new();
        init_repo(root.path());
        let node = make_node(&root, None);

        delete_worktree_for_node(&node, true).unwrap();

        assert!(root.path().exists(), "mesh root must not be pruned");
    }

    #[test]
    fn delete_worktree_for_node_succeeds_when_working_dir_already_gone() {
        let root = TempDir::new();
        let root_repo = init_repo(root.path());
        let worktree_path = add_worktree(&root_repo, &root, "wt-gone");
        // A previous interrupted close left the working directory missing (#239).
        fs::remove_dir_all(&worktree_path).unwrap();
        let node = make_node(&root, Some("wt-gone"));

        // Must not error: there is nothing left to remove, so the node closes.
        delete_worktree_for_node(&node, true).unwrap();
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
