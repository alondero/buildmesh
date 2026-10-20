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
    if working_tree_flagged_bare(repo, selected) {
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
/// live in a folder called `.git`. The **index** does not settle it either:
/// git only says an index is *usually* absent from a bare repository, and
/// `git read-tree` fills a bare repository's index with tree data without
/// writing a single file. Telling such a repository to set `core.bare=false`
/// would break the one thing it is: a repository with no working tree.
///
/// The evidence that settles it is the index's own record of the working
/// tree: an entry that carries stat data was staged from a file that existed
/// on disk, and that file is still there. A bare repository's index carries
/// no stat data, so it does not pass that check whatever its index says and
/// whatever file happens to sit beside it. The flag is read from the
/// repository's config rather than assumed, so a repository that reports no
/// working directory for some other reason keeps the plain bare-repository
/// wording.
fn working_tree_flagged_bare(repo: &Repository, selected: &str) -> bool {
    let flagged = repo
        .config()
        .and_then(|config| config.get_bool("core.bare"))
        .unwrap_or(false);
    if !flagged {
        return false;
    }
    let Some(root) = checkout_root(repo, selected) else {
        return false;
    };
    tracked_files_present(repo, &root)
}

/// The folder that holds the checkout, when the repository's layout names one.
///
/// The `.git` *file* relationship comes first. A checkout may keep its Git
/// data in a separate directory, and that directory may itself be named
/// `.git` — so the Git directory's name alone does not stand in for the
/// checkout relationship. The `.git` file is the only pointer back to the
/// checkout, and it names the repository it points at.
///
/// The standard layout is the fallback: the Git data lives in a `.git`
/// directory inside the checkout, so the checkout is the Git directory's
/// parent. A bare repository reaches that fallback only when no `.git` file
/// points at it, and it is then separated from a checkout by the stat check
/// in [`tracked_files_present`].
fn checkout_root(repo: &Repository, selected: &str) -> Option<std::path::PathBuf> {
    let git_dir = repo.path();
    if let Some(root) = gitlink_checkout_root(git_dir, selected) {
        return Some(root);
    }
    if git_dir.file_name() == Some(std::ffi::OsStr::new(".git")) {
        return git_dir.parent().map(std::path::Path::to_path_buf);
    }
    None
}

/// The checkout that reaches this repository through a `.git` file.
///
/// The file is looked up on the selected path and its ancestors, because a
/// checkout may be a subfolder of the folder that holds the `.git` file.
fn gitlink_checkout_root(git_dir: &std::path::Path, selected: &str) -> Option<std::path::PathBuf> {
    let git_dir = git_dir.canonicalize().ok()?;
    let mut candidate = std::path::PathBuf::from(crate::env::to_host_path(selected));
    loop {
        let link = candidate.join(".git");
        if link.is_file() && gitlink_points_at(&link, &git_dir) {
            return Some(candidate);
        }
        candidate = candidate.parent()?.to_path_buf();
    }
}

/// Whether a `.git` *file* (a gitlink) names `git_dir` as the repository.
///
/// A relative `gitdir:` target is resolved from the directory that holds the
/// link file, the way git resolves it (`setup.c`: `read_gitfile_gently`) —
/// `git worktree` writes one when `worktree.useRelativePaths` is set, and
/// resolving it against the process working directory instead would compare
/// the wrong path.
fn gitlink_points_at(link: &std::path::Path, git_dir: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(link) else {
        return false;
    };
    let Some(target) = text.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let target = std::path::Path::new(target.trim());
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        match link.parent() {
            Some(parent) => parent.join(target),
            None => target.to_path_buf(),
        }
    };
    let Ok(target) = target.canonicalize() else {
        return false;
    };
    target == git_dir
}

/// Whether the repository's tracked files are on disk under `root`.
///
/// The index is git's record of the working tree. An entry that carries stat
/// data — the size and timestamps of a file that existed on disk — was staged
/// from a real file, and that file is still there. A bare repository's index
/// carries no stat data (`git read-tree` writes tree data without touching
/// the filesystem), so a bare repository does not pass this check, whatever its
/// index says and whatever file happens to sit beside it.
///
/// The comparison is stat-based rather than content-based, on purpose. Git may
/// have converted line endings or applied a checkout filter when it wrote the
/// working tree, so the bytes on disk need not hash to the blob the index
/// records — git itself reports such a file as modified. The index records the
/// *working tree's* size, not the blob's, so the stat survives a smudge
/// filter. What this check asks about is presence, not identity.
///
/// The scan is bounded: a diagnostic must not walk a whole repository, and it
/// reads no file content at all, so a large tracked file is not loaded into memory.
fn tracked_files_present(repo: &Repository, root: &std::path::Path) -> bool {
    /// The index mode of a submodule (gitlink) entry.
    const GITLINK_MODE: u32 = 0o160000;
    /// The index mode of a sparse-index directory entry.
    const SPARSE_DIR_MODE: u32 = 0o040000;
    /// `GIT_INDEX_ENTRY_SKIP_WORKTREE`: the path is excluded from the working
    /// tree by a sparse checkout, so it is not evidence of one.
    const SKIP_WORKTREE: u16 = 1 << 14;
    /// How many index entries the check may examine.
    const MAX_ENTRIES: usize = 4096;

    let Ok(index) = repo.index() else {
        return false;
    };
    let mut examined = 0usize;
    for entry in index.iter() {
        if entry.mode == GITLINK_MODE || entry.mode == SPARSE_DIR_MODE {
            continue;
        }
        if entry.flags_extended & SKIP_WORKTREE != 0 {
            continue;
        }
        // No stat data means git did not stage a file for this path.
        if !entry_has_stat_data(&entry) {
            continue;
        }
        examined += 1;
        if examined > MAX_ENTRIES {
            break;
        }
        let name = String::from_utf8_lossy(&entry.path);
        if name.is_empty() {
            continue;
        }
        let path = root.join(name.as_ref());
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() != u64::from(entry.file_size) {
            continue;
        }
        if !mtime_matches(&meta, &entry.mtime) {
            continue;
        }
        return true;
    }
    false
}

/// Whether the index recorded stat data for this entry — the sign that git
/// staged it from a file that existed on disk.
fn entry_has_stat_data(entry: &git2::IndexEntry) -> bool {
    entry.file_size != 0
        || entry.mtime.seconds() != 0
        || entry.ctime.seconds() != 0
        || entry.ino != 0
        || entry.dev != 0
}

/// Whether the file's modification time is the one the index recorded.
///
/// The seconds must match. The nanoseconds are compared only when the index
/// recorded them: a filesystem with a coarse timestamp writes none, and the
/// index then holds none either.
fn mtime_matches(meta: &std::fs::Metadata, recorded: &git2::IndexTime) -> bool {
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    if since_epoch.as_secs() as i64 != i64::from(recorded.seconds()) {
        return false;
    }
    recorded.nanoseconds() == 0 || since_epoch.subsec_nanos() == recorded.nanoseconds()
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

    /// A genuine bare repository can carry a populated index: `git read-tree`
    /// reads tree data into the index without writing a single working-tree
    /// file, and git only says an index is *usually* absent from a bare
    /// repository. The index therefore does not prove a working tree, and
    /// such a repository must not be told to clear a flag that is set
    /// correctly — that would break the one thing it is.
    ///
    /// Both layouts are covered, because they fail for different reasons.
    /// A bare repository whose Git directory is `<name>.git` has no `.git`
    /// entry to follow back to a checkout, so no folder holds its tracked
    /// files. A bare repository *named* `.git` sits inside a folder that
    /// looks like a checkout root, and only the tracked files themselves —
    /// absent from a bare repository — tell the two apart.
    #[test]
    fn a_bare_repository_with_a_populated_index_is_not_told_it_holds_a_working_tree() {
        let td = TestDir::new("project-bare-populated-index");
        init_repo_with_commit(td.path(), &[("file.txt", "content\n")]);
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?} failed");
        };

        for (case, bare) in [
            ("bare.git", td.path().join("bare.git")),
            ("named-git", td.path().join("gitdata").join(".git")),
        ] {
            if case == "named-git" {
                std::fs::create_dir_all(td.path().join("gitdata")).unwrap();
            }
            run(&[
                "clone",
                "--bare",
                td.path().to_str().unwrap(),
                bare.to_str().unwrap(),
            ]);
            run(&["--git-dir", bare.to_str().unwrap(), "read-tree", "HEAD"]);

            // Fixture precondition: the index really is populated — the case
            // the index alone does not distinguish from a checkout.
            let repo = Repository::discover(&bare).unwrap();
            assert!(
                !repo.index().unwrap().is_empty(),
                "fixture precondition ({case}): the bare repository must carry a populated index"
            );

            let reason = resolve(bare.to_str().unwrap()).unwrap_err();
            assert!(
                reason.contains("is a bare repository, not a project working directory"),
                "a genuine bare repository keeps the plain wording ({case}): {reason}"
            );
            assert!(
                !reason.contains("core.bare=true"),
                "a genuine bare repository must not be told to repair a mis-set flag ({case}): {reason}"
            );
        }
    }

    /// Git may write the working tree with CRLF line endings while the blob
    /// keeps LF, so the file on disk differs from the blob in content *and*
    /// in length. The index records the working tree's size, so the checkout
    /// is still recognised.
    #[test]
    fn a_checkout_with_converted_line_endings_is_still_a_working_tree() {
        let td = TestDir::new("project-flagged-crlf");
        init_repo_with_commit(td.path(), &[("file.txt", "content\n")]);
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
            "-C",
            td.path().to_str().unwrap(),
            "config",
            "core.autocrlf",
            "true",
        ]);
        // Dirty the file so git rewrites it through the conversion.
        std::fs::write(td.path().join("file.txt"), "dirty").unwrap();
        run(&[
            "-C",
            td.path().to_str().unwrap(),
            "checkout",
            "-f",
            "--",
            "file.txt",
        ]);
        assert_eq!(
            std::fs::read_to_string(td.path().join("file.txt")).unwrap(),
            "content\r\n",
            "fixture precondition: git must have converted the working tree"
        );

        let repo = Repository::open(td.path()).unwrap();
        repo.config().unwrap().set_bool("core.bare", true).unwrap();
        drop(repo);

        let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a checkout with converted line endings still holds a working tree: {reason}"
        );
    }

    /// A configured checkout filter rewrites the working tree, so the file on
    /// disk differs from the blob in content and in length. Git itself reports
    /// such a file as modified — present, but differing from the blob — and presence is
    /// what the diagnosis asks about.
    #[test]
    fn a_checkout_with_a_filtered_file_is_still_a_working_tree() {
        let td = TestDir::new("project-flagged-filtered");
        init_repo_with_commit(td.path(), &[("file.txt", "content\n")]);
        // The attribute file stays untracked, so the fixture has a single tracked file,
        // tracked file and the evidence can only come from the filtered one.
        std::fs::write(td.path().join(".gitattributes"), "file.txt filter=demo\n").unwrap();
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
            "-C",
            td.path().to_str().unwrap(),
            "config",
            "filter.demo.clean",
            "cat",
        ]);
        run(&[
            "-C",
            td.path().to_str().unwrap(),
            "config",
            "filter.demo.smudge",
            "sed s/content/transformed/",
        ]);
        // Dirty the file so git rewrites it through the filter.
        std::fs::write(td.path().join("file.txt"), "dirty").unwrap();
        run(&[
            "-C",
            td.path().to_str().unwrap(),
            "checkout",
            "-f",
            "--",
            "file.txt",
        ]);
        assert_eq!(
            std::fs::read_to_string(td.path().join("file.txt")).unwrap(),
            "transformed\n",
            "fixture precondition: git must have applied the smudge filter"
        );

        let repo = Repository::open(td.path()).unwrap();
        repo.config().unwrap().set_bool("core.bare", true).unwrap();
        drop(repo);

        let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a checkout whose file was rewritten by a filter still holds a working tree: {reason}"
        );
    }

    /// A sparse working tree leaves tracked paths out of the working tree
    /// while the index keeps them, so the evidence can sit past a long run of
    /// paths that are not there.
    #[test]
    fn a_sparse_working_tree_finds_the_paths_that_are_present() {
        let td = TestDir::new("project-flagged-sparse");
        let absent: Vec<String> = (0..100).map(|i| format!("absent/{i:03}.txt")).collect();
        let mut files: Vec<(&str, &str)> = absent
            .iter()
            .map(|name| (name.as_str(), "content\n"))
            .collect();
        files.push(("present/file.txt", "content\n"));
        init_repo_with_commit(td.path(), &files);
        // The absent paths stay in the index but leave the working tree.
        for name in &absent {
            std::fs::remove_file(td.path().join(name)).unwrap();
        }

        let repo = Repository::open(td.path()).unwrap();
        repo.config().unwrap().set_bool("core.bare", true).unwrap();
        drop(repo);

        let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a sparse working tree still holds the paths it kept: {reason}"
        );
    }

    /// A `.git` *file* may name its Git directory relatively — git writes one
    /// when `worktree.useRelativePaths` is set. Resolving that target against
    /// the process working directory instead of the directory that holds the
    /// link file compares the wrong path, and the checkout is then described
    /// as a bare repository.
    #[test]
    fn a_relative_gitdir_link_is_resolved_from_the_link_file() {
        let td = TestDir::new("project-flagged-relative-gitdir");
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
        // The relative form git writes for a linked worktree, pointing at the
        // Git directory beside the checkout.
        std::fs::write(checkout.join(".git"), "gitdir: ../gitdata\n").unwrap();
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
            "a checkout reached through a relative .git file holds a working tree: {reason}"
        );
    }

    /// A separate Git directory may itself be named `.git`, so the Git
    /// directory's name alone does not stand in for the checkout relationship:
    /// the checkout points at the repository through a `.git` *file*, and that
    /// file is the only way back to it. Reading the name alone would look for
    /// the working tree beside the Git directory and miss the real checkout.
    #[test]
    fn a_separate_git_dir_named_git_is_recognised_as_a_working_tree() {
        let td = TestDir::new("project-flagged-separate-git-dir-named-git");
        let checkout = td.path().join("checkout");
        let git_dir = td.path().join("gitdata").join(".git");
        std::fs::create_dir_all(td.path().join("gitdata")).unwrap();
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
            "a separate Git directory named .git still holds a working tree: {reason}"
        );
        assert!(
            reason.contains(&format!(
                "git -C \"{}\" config core.bare false",
                checkout.display()
            )),
            "the message must carry the repair command: {reason}"
        );
    }

    /// A bare repository may live in a folder called `.git`, and the folder
    /// beside it may hold an unrelated file whose path matches an entry in
    /// the bare repository's index. The path alone is not evidence of a
    /// checkout — the content must match what the index records — so such a
    /// repository keeps the plain wording instead of being told to clear a
    /// flag that is set correctly.
    #[test]
    fn a_bare_repository_beside_a_colliding_file_is_not_told_it_holds_a_working_tree() {
        let td = TestDir::new("project-bare-colliding-file");
        init_repo_with_commit(td.path(), &[("file.txt", "content\n")]);
        let container = td.path().join("container");
        let bare = container.join(".git");
        std::fs::create_dir_all(&container).unwrap();
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
            "clone",
            "--bare",
            td.path().to_str().unwrap(),
            bare.to_str().unwrap(),
        ]);
        run(&["--git-dir", bare.to_str().unwrap(), "read-tree", "HEAD"]);
        // An unrelated file that happens to use a tracked path — and the same
        // size, so the index's recorded size does not rule it out either.
        std::fs::write(container.join("file.txt"), "CONTENT\n").unwrap();

        let reason = resolve(bare.to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("is a bare repository, not a project working directory"),
            "a genuine bare repository keeps the plain wording: {reason}"
        );
        assert!(
            !reason.contains("core.bare=true"),
            "a file that merely shares a path with an index entry is not a working tree: {reason}"
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
