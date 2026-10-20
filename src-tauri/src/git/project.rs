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
    let root = repo
        .workdir()
        .ok_or_else(|| no_working_directory_reason(selected, &repo))?;
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

/// Why this repository offers no working directory.
///
/// A repository whose own config sets `core.bare=true` reports no working
/// directory even when the folder holds a full checkout: git then refuses
/// the commands that need a work tree ("fatal: this operation must be run in
/// a work tree") and the checkout is invisible to git. Telling the user their
/// project "is a bare repository" sends them hunting for a bare clone they do
/// not have, so name the mis-set flag and the one-line repair instead.
fn no_working_directory_reason(selected: &str, repo: &Repository) -> String {
    if working_tree_flagged_bare(repo) {
        return format!(
            "{selected} is marked as a bare repository (core.bare=true in its Git config) \
             even though it holds a working tree; repair it with \
             `git -C \"{selected}\" config core.bare false`"
        );
    }
    format!("{selected} is a bare repository, not a project working directory")
}

/// Whether the repository holds a working tree that its own config hides
/// behind `core.bare=true`.
///
/// The Git directory's *name* is not the answer: a checkout may keep its data
/// in a separate directory (`--separate-git-dir`), and a bare repository may
/// live in a folder called `.git`. The index is: a working tree **with tracked
/// files** leaves a non-empty one behind, a bare repository has none, and the
/// flag only stops git from *using* the index — it does not delete it. The
/// flag itself is read from the repository's config rather than assumed, so a
/// repository that reports no working directory for some other reason keeps
/// the plain bare-repository wording.
///
/// A working tree with an empty index — no tracked files — keeps the plain
/// wording too: there is no evidence it was ever a checkout and the repair
/// command would be a guess. It is the same wording it got before this rule
/// existed.
fn working_tree_flagged_bare(repo: &Repository) -> bool {
    let flagged = repo
        .config()
        .and_then(|config| config.get_bool("core.bare"))
        .unwrap_or(false);
    if !flagged {
        return false;
    }
    repo.index().map(|index| !index.is_empty()).unwrap_or(false)
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

/// Whether this folder has a repository that a Worktree Node could be cut
/// from — the question a new mesh's worktree strategy is decided by.
///
/// Deliberately [`resolve`], the *same* gate `create_git_worktree` runs, and
/// not a bare `.git` presence check: discovery means a subfolder of a
/// repository still resolves (its worktrees come from the repository root),
/// while a bare repository and a folder outside any working tree do not. Those
/// are exactly the folders whose Worktree Node spawns would fail later, so
/// answering with this predicate keeps the mesh's default honest instead of
/// promising a worktree the provisioner cannot cut. An unreadable path resolves
/// to `false` for the same reason: the conservative answer is the one that does
/// not promise a worktree.
///
/// Call outside DB locks: `resolve` canonicalizes and reads git metadata.
pub fn can_cut_worktrees(selected: &str) -> bool {
    match resolve(selected) {
        Ok(_) => true,
        Err(reason) => {
            tracing::debug!(
                path = selected,
                %reason,
                "no repository to cut worktrees from; a mesh created here starts with use worktree off"
            );
            false
        }
    }
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
        let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("is a bare repository, not a project working directory"),
            "a genuinely bare repository keeps the plain wording: {reason}"
        );
        assert!(
            !reason.contains("core.bare=true"),
            "a genuinely bare repository must not be told to repair a mis-set flag: {reason}"
        );
    }

    /// A checkout whose own config sets `core.bare=true` has a working tree
    /// that git refuses to use, and the user has no bare clone to look for.
    /// The message must name the mis-set flag and the repair, or the failure
    /// reads as a mystery ("why is my project bare?").
    #[test]
    fn names_the_mis_set_bare_flag_and_its_repair() {
        let td = TestDir::new("project-flagged-bare");
        init_repo_with_commit(td.path(), &[("file.txt", "content\n")]);
        let repo = Repository::open(td.path()).unwrap();
        repo.config().unwrap().set_bool("core.bare", true).unwrap();
        drop(repo);

        let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "the message must name the mis-set flag: {reason}"
        );
        assert!(
            reason.contains(&format!(
                "git -C \"{}\" config core.bare false",
                td.path().display()
            )),
            "the message must carry the repair command: {reason}"
        );
        // The repair restores the working tree the folder already had.
        assert!(resolve(td.path().to_str().unwrap()).is_err());
        std::process::Command::new("git")
            .args([
                "-C",
                &td.path().to_string_lossy(),
                "config",
                "core.bare",
                "false",
            ])
            .status()
            .expect("git config");
        assert!(
            resolve(td.path().to_str().unwrap()).is_ok(),
            "clearing core.bare must make the checkout resolvable again"
        );
    }

    /// A bare repository may live in a folder called `.git`, and a checkout
    /// may keep its Git data elsewhere (`--separate-git-dir`). The Git
    /// directory's name is not the discriminator; the index is.
    #[test]
    fn a_bare_repository_named_git_is_not_told_it_holds_a_working_tree() {
        let td = TestDir::new("project-bare-named-git");
        let bare = td.path().join(".git");
        Repository::init_bare(&bare).unwrap();
        let reason = resolve(bare.to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("is a bare repository, not a project working directory"),
            "a bare repository must keep the plain wording: {reason}"
        );
        assert!(
            !reason.contains("core.bare=true"),
            "a bare repository must not be told to repair a mis-set flag: {reason}"
        );
    }

    /// A checkout whose Git data lives in a separate directory still holds a
    /// working tree, so the mis-set flag — not the bare-repository wording —
    /// is what the user must be told.
    #[test]
    fn a_separate_git_dir_checkout_is_recognised_as_a_working_tree() {
        let td = TestDir::new("project-flagged-separate-git-dir");
        let checkout = td.path().join("checkout");
        let git_dir = td.path().join("gitdata");
        std::fs::create_dir_all(&checkout).unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&[
            "init",
            "--separate-git-dir",
            git_dir.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ]);
        std::fs::write(checkout.join("file.txt"), "content\n").unwrap();
        run(&["-C", checkout.to_str().unwrap(), "add", "file.txt"]);
        run(&[
            "-C",
            checkout.to_str().unwrap(),
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "initial",
        ]);
        run(&[
            "--git-dir",
            git_dir.to_str().unwrap(),
            "config",
            "core.bare",
            "true",
        ]);

        let reason = resolve(checkout.to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a separate-git-dir checkout still holds a working tree: {reason}"
        );
    }

    /// The worktree-capability predicate a new mesh's strategy is decided by
    /// must agree with what `create_git_worktree` accepts, so a mesh never
    /// boots with **Use worktree** on for a folder whose spawns would fail.
    #[test]
    fn worktree_capability_tracks_repository_resolution() {
        let td = TestDir::new("worktree-capability");
        let root = td.path().to_str().unwrap();
        // Fixture precondition: `resolve` walks *up* the tree, so a temp dir
        // nested inside some unrelated repository would make the first two
        // assertions pass for the wrong reason. Fail with that explanation
        // instead of a confusing message mismatch.
        assert!(
            Repository::discover(td.path()).is_err(),
            "fixture precondition: temp dir must not sit inside a git repository"
        );
        // A plain folder is not a project working directory.
        assert!(!can_cut_worktrees(root));
        init_repo_with_commit(td.path(), &[("file.txt", "content\n")]);
        assert!(can_cut_worktrees(root));
        // A subfolder resolves — worktrees are cut from the repository root —
        // so it keeps worktrees on, matching the provisioner.
        let sub = td.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(can_cut_worktrees(sub.to_str().unwrap()));
        // A path that does not exist cannot be resolved.
        assert!(!can_cut_worktrees(
            td.path().join("missing").to_str().unwrap()
        ));
        // A bare repository has no working tree to work in.
        let bare = TestDir::new("worktree-capability-bare");
        Repository::init_bare(bare.path()).unwrap();
        assert!(!can_cut_worktrees(bare.path().to_str().unwrap()));
    }
}
