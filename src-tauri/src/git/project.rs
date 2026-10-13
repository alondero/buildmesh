//! Git backing for a selected project folder. Call on a blocking thread,
//! outside DB connections; reconstructing a saved node cwd belongs to `env`.

use git2::Repository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRepository {
    pub working_root: String,
    pub relative_folder: String,
    pub identity: String,
}

pub fn open(selected: &str) -> Result<Repository, git2::Error> {
    Repository::discover(crate::env::to_host_path(selected))
}

pub fn resolve(selected: &str) -> Result<ProjectRepository, String> {
    let repo =
        open(selected).map_err(|e| format!("No working Git repository for {selected}: {e}"))?;
    let root = repo.workdir().ok_or_else(|| {
        format!("{selected} is a bare repository, not a project working directory")
    })?;
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let folder = std::fs::canonicalize(crate::env::to_host_path(selected))
        .map_err(|e| format!("Cannot resolve project folder {selected}: {e}"))?;
    if !folder.is_dir() {
        return Err(format!("Project folder {selected} is not a directory"));
    }
    let relative = folder
        .strip_prefix(&root)
        .map_err(|_| format!("Project folder {selected} is outside its Git working root"))?;
    let relative_folder = relative.to_string_lossy().replace('\\', "/");
    crate::env::validate_project_scope(&relative_folder)?;
    Ok(ProjectRepository {
        working_root: crate::env::canonical_host_path_string(&root),
        relative_folder,
        identity: repository_identity(&repo)?,
    })
}

fn repository_identity(repo: &Repository) -> Result<String, String> {
    let common = std::fs::canonicalize(repo.commondir()).map_err(|e| e.to_string())?;
    let key = crate::env::canonical_host_path_string(&common);
    Ok(if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    })
}

/// Non-Git callers retain their existing path-key behavior. Discovery happens
/// before taking the coordination map's lock.
pub fn coordination_key(path: &str) -> String {
    open(path)
        .ok()
        .and_then(|repo| repository_identity(&repo).ok())
        .unwrap_or_else(|| path.to_string())
}

/// Ownership is shared by Meshes backed by the same repository, while branch
/// names in unrelated repositories remain independent. Call outside DB locks.
pub fn repository_nodes(
    path: &str,
    nodes: Vec<crate::models::AgentNode>,
) -> Result<Vec<crate::models::AgentNode>, String> {
    let identity = repository_identity(&open(path).map_err(|e| e.to_string())?)?;
    Ok(nodes
        .into_iter()
        .filter(|node| {
            let owned = crate::env::node_git_path(node);
            let matches = [node.path.as_str(), owned.host_path.as_str()]
                .into_iter()
                .any(|path| {
                    open(path)
                        .ok()
                        .and_then(|repo| repository_identity(&repo).ok())
                        .as_ref()
                        == Some(&identity)
                });
            matches
        })
        .collect())
}

pub fn active_paths(nodes: &[crate::models::AgentNode]) -> Vec<String> {
    let mut paths = crate::env::active_node_paths(nodes);
    paths.extend(nodes.iter().filter_map(|node| {
        open(&node.path)
            .ok()
            .and_then(|repo| repo.workdir().map(crate::env::canonical_host_path_string))
    }));
    paths
}

/// Exact open: discovering an ancestor would mistake an empty checkout husk
/// inside the source repository for an already materialized worktree.
pub fn is_materialized_checkout(path: &str) -> bool {
    Repository::open(crate::env::to_host_path(path)).is_ok_and(|repo| repo.is_worktree())
}

/// Saved scope needs no source repository. Legacy resume preserves the root
/// cwd; only an explicit fresh launch may infer scope before materialization.
pub fn scope_for_checkout(
    selected: &str,
    checkout: &str,
    saved: Option<&str>,
    explicit_fresh: bool,
) -> Result<Option<String>, String> {
    if let Some(scope) = saved {
        crate::env::validate_project_scope(scope)?;
        return Ok(Some(scope.to_string()));
    }
    if !explicit_fresh || is_materialized_checkout(checkout) {
        return Ok(None);
    }
    resolve(selected).map(|project| Some(project.relative_folder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_helpers::{init_repo_with_commit, TestDir};

    #[test]
    fn legacy_resume_and_explicit_husk_retry_preserve_scope_intent() {
        let td = TestDir::new("legacy-scope-intent");
        init_repo_with_commit(td.path(), &[("family/parents/AGENTS.md", "Scoped")]);
        let selected = td.path().join("family/parents");
        let checkout = td.path().join("checkout");
        let scope = |fresh| {
            scope_for_checkout(
                selected.to_str().unwrap(),
                checkout.to_str().unwrap(),
                None,
                fresh,
            )
            .unwrap()
        };
        assert_eq!(scope(false), None); // missing legacy checkout on resume
        std::fs::create_dir(&checkout).unwrap();
        assert_eq!(scope(false), None);
        assert_eq!(scope(true).as_deref(), Some("family/parents")); // empty husk
        crate::git::worktree::create_git_worktree(
            td.path().to_str().unwrap(),
            checkout.to_str().unwrap(),
            "legacy",
            "detached",
            "HEAD",
        )
        .unwrap();
        assert_eq!(scope(true), None); // materialized legacy launch
        assert_eq!(
            scope_for_checkout(
                "missing-source",
                checkout.to_str().unwrap(),
                Some("family/parents"),
                false
            )
            .unwrap()
            .as_deref(),
            Some("family/parents")
        );
        assert!(scope_for_checkout(
            "missing-source",
            checkout.to_str().unwrap(),
            Some("../escape"),
            false
        )
        .is_err());
    }

    #[test]
    fn resolves_root_subfolder_nested_repository_and_linked_worktree() {
        let td = TestDir::new("project-discovery");
        init_repo_with_commit(
            td.path(),
            &[("family/parents/AGENTS.md", "Scoped instructions")],
        );
        let root = resolve(td.path().to_str().unwrap()).unwrap();
        let scoped = resolve(td.path().join("family/parents").to_str().unwrap()).unwrap();
        assert_eq!(root.relative_folder, "");
        assert_eq!(scoped.relative_folder, "family/parents");
        assert_eq!(root.identity, scoped.identity);
        assert_eq!(root.working_root, scoped.working_root);
        let linked = td.path().join("linked");
        crate::git::worktree::create_git_worktree(
            td.path().to_str().unwrap(),
            linked.to_str().unwrap(),
            "linked",
            "detached",
            "HEAD",
        )
        .unwrap();
        let in_linked = resolve(linked.join("family/parents").to_str().unwrap()).unwrap();
        assert_eq!(in_linked.identity, root.identity);
        assert_eq!(in_linked.relative_folder, "family/parents");
        assert_ne!(in_linked.working_root, root.working_root);
        let nested = td.path().join("family/parents/nested");
        std::fs::create_dir_all(&nested).unwrap();
        init_repo_with_commit(&nested, &[("README.md", "Nested repository")]);
        let inner = resolve(nested.to_str().unwrap()).unwrap();
        assert_eq!(inner.relative_folder, "");
        assert_ne!(inner.identity, root.identity);
    }

    #[test]
    fn rejects_non_git_and_bare_directories() {
        let td = TestDir::new("project-unavailable");
        assert!(resolve(td.path().to_str().unwrap()).is_err());
        Repository::init_bare(td.path()).unwrap();
        assert!(resolve(td.path().to_str().unwrap())
            .unwrap_err()
            .contains("bare"));
    }
}
