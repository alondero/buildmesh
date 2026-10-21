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
    let folder = std::fs::canonicalize(crate::env::to_host_path(selected)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "Project folder unavailable: {selected}. Restore access to the folder \
                 (check its drive or network mapping), then retry. Details: {e}"
            )
        } else {
            format!("Cannot access project folder {selected}: {e}")
        }
    })?;
    if !folder.is_dir() {
        return Err(format!("Project folder {selected} is not a directory"));
    }
    let repo = open(selected).map_err(|e| {
        if e.code() == git2::ErrorCode::Owner {
            format!(
                "Git ownership check rejected project folder {selected}. Review its ownership \
                 and trust its repository root in your global Git safe.directory configuration, \
                 then retry. Details: {e}"
            )
        } else {
            format!("No working Git repository for {selected}: {e}")
        }
    })?;
    let root = repo
        .workdir()
        .ok_or_else(|| no_working_directory_reason(selected, &repo))?;
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
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
/// tree: an entry that carries the identity of a file — its inode and the
/// device it lives on, or on Windows its creation time — was staged from a
/// real file, and that file is still there. The identity is used rather than
/// the cached size and timestamps, which go stale as soon as a file is
/// edited. A file that *replaced* the tracked one has a new identity, so the
/// repository's own record of having had a work tree settles that case; a
/// bare repository has no such record, so it does not pass whatever its index
/// says and whatever file happens to sit beside it. The flag is read from the
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
/// points at it, and it is then separated from a checkout by the identity
/// check in [`tracked_files_present`].
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
/// The index is git's record of the working tree, but what the index records
/// is not proof of one on its own. Git keeps the size, the timestamps and the
/// mode there as a *cache* for change detection, and that cache goes stale the
/// moment a file is edited, so comparing it against the file on disk reports
/// a dirty checkout as missing. Two kinds of evidence settle it instead.
///
/// **The file's identity.** What names the file itself is its identity: the
/// inode and the device it lives on (Unix), or the creation time (Windows,
/// where git caches the creation time because the platform has no stable
/// inode to record). An identity survives an edit, a checkout filter and a
/// line-ending conversion, and an unrelated file that happens to share a
/// tracked path, a size and a modification time does not share it.
///
/// **The repository's own record of a work tree.** An identity does not
/// survive a file that was *replaced* rather than edited: an editor that
/// saves by writing a temporary file and renaming it over the tracked path
/// leaves the path holding a valid, dirty working-tree file under a new
/// identity. Git records whether a repository was set up with a work tree —
/// it writes `core.logallrefupdates` for one, and such a repository keeps a
/// reflog of HEAD's moves — while a repository created as bare writes no
/// reflog configuration and keeps no reflog. So the evidence stays with the
/// checkout rather than with a file that merely reuses a tracked path.
///
/// The scan is bounded: a diagnostic must not walk a whole repository, and it
/// reads no file content at all, so a large tracked file is not loaded into memory.
fn tracked_files_present(repo: &Repository, root: &std::path::Path) -> bool {
    /// How many index entries the check may examine.
    const MAX_ENTRIES: usize = 4096;

    let Ok(index) = repo.index() else {
        return false;
    };
    let symlinks_emulated = symlinks_are_emulated(repo);
    let used_with_work_tree = used_with_a_work_tree(repo);
    let mut examined = 0usize;
    for entry in index.iter() {
        if entry.mode == GITLINK_MODE || entry.mode == SPARSE_DIR_MODE {
            continue;
        }
        if entry.flags_extended & SKIP_WORKTREE != 0 {
            continue;
        }
        examined += 1;
        if examined > MAX_ENTRIES {
            break;
        }
        let path = tracked_path(root, &entry.path);
        if same_file_as_indexed(&entry, &path, symlinks_emulated) {
            return true;
        }
        // A file that replaced the tracked one has a new identity, so the
        // repository's own record of a work tree is what settles it.
        if used_with_work_tree && tracked_path_occupied(&path) {
            return true;
        }
    }
    false
}

/// The index mode of a tracked symbolic link.
const SYMLINK_MODE: u32 = 0o120000;
/// The index mode of a regular file.
const REGULAR_MODE: u32 = 0o100644;
/// The index mode of an executable regular file.
const EXECUTABLE_MODE: u32 = 0o100755;
/// The index mode of a submodule (gitlink) entry.
const GITLINK_MODE: u32 = 0o160000;
/// The index mode of a sparse-index directory entry.
const SPARSE_DIR_MODE: u32 = 0o040000;
/// `GIT_INDEX_ENTRY_SKIP_WORKTREE`: the path is excluded from the working
/// tree by a sparse checkout, so it is not evidence of one.
const SKIP_WORKTREE: u16 = 1 << 14;

/// The path of an indexed entry under the checkout root.
///
/// Git pathnames are byte sequences on Unix and are not required to be valid
/// UTF-8, so the raw bytes are used there: decoding them lossily replaces
/// the invalid bytes and looks up a different path, which misses the tracked
/// file. Windows paths are Unicode, so the bytes are decoded there.
fn tracked_path(root: &std::path::Path, name: &[u8]) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(name))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(name).as_ref())
    }
}

/// Whether a tracked path is occupied by a file on disk.
fn tracked_path_occupied(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        let file_type = meta.file_type();
        file_type.is_file() || file_type.is_symlink()
    })
}

/// Whether the repository records that it was set up with a work tree.
///
/// Git writes `core.logallrefupdates` for a repository it created with a work
/// tree, and such a repository keeps a reflog of HEAD's moves. A repository
/// created as bare writes no reflog configuration and keeps no reflog:
/// `git init --bare` and `git clone --bare` produce a repository without
/// either.
///
/// The repository's *own* configuration is read, not the merged one: a global
/// `core.logallrefupdates` would make the bare repositories on the machine
/// look like working ones.
fn used_with_a_work_tree(repo: &Repository) -> bool {
    let configured = repo
        .config()
        .ok()
        .and_then(|config| config.open_level(git2::ConfigLevel::Local).ok())
        .and_then(|config| config.get_bool("core.logallrefupdates").ok())
        .unwrap_or(false);
    configured || repo.path().join("logs").join("HEAD").is_file()
}

/// Whether git checks tracked symlinks out as plain files here.
///
/// Git writes a tracked symlink as a plain file holding the link text when
/// `core.symlinks` is false, which is the default on Windows; elsewhere the
/// default is a real symlink.
fn symlinks_are_emulated(repo: &Repository) -> bool {
    let configured = repo
        .config()
        .ok()
        .and_then(|config| config.open_level(git2::ConfigLevel::Local).ok())
        .and_then(|config| config.get_bool("core.symlinks").ok());
    match configured {
        Some(value) => !value,
        None => cfg!(windows),
    }
}

/// Whether the index recorded enough identity data to name a file.
///
/// Git records the file's inode and the device it lives on; on Windows, where
/// there is no stable inode, it records the file's creation time instead. An
/// index written by `git read-tree` carries no identity, so it names no file.
fn entry_has_identity(entry: &git2::IndexEntry) -> bool {
    if cfg!(unix) {
        entry.ino != 0 && entry.dev != 0
    } else {
        // Windows has no stable inode to record, so git caches the file's
        // creation time instead.
        entry.ctime.seconds() != 0
    }
}

/// What the file on disk is, as the index's mode is compared against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnDisk {
    /// A regular file.
    File,
    /// A symbolic link, read without following it.
    Symlink,
    /// Anything else — a directory, a socket, a device.
    Other,
}

/// Whether the file on disk is the kind of file the index recorded.
///
/// A tracked symlink must be a symlink on disk, and a tracked regular file
/// must be a regular file on disk: a symlink standing where the index
/// records a file is a different file, not a working tree. The exception is
/// git's symlink emulation, where a tracked symlink is checked out as a
/// plain file holding the link text.
fn matches_entry_kind(mode: u32, on_disk: OnDisk, symlinks_emulated: bool) -> bool {
    match (mode, on_disk) {
        (SYMLINK_MODE, OnDisk::Symlink) => true,
        (SYMLINK_MODE, OnDisk::File) => symlinks_emulated,
        (REGULAR_MODE | EXECUTABLE_MODE, OnDisk::File) => true,
        _ => false,
    }
}

/// Whether the file on disk is the same file the index staged.
///
/// Identity, not size, modification time or content: those describe what the
/// file holds, and they move when the file is edited, when git rewrites it
/// through a filter, or when it converts the file's line endings. Identity
/// names the file itself, and only the file the index staged has it.
fn same_file_as_indexed(
    entry: &git2::IndexEntry,
    path: &std::path::Path,
    symlinks_emulated: bool,
) -> bool {
    if !entry_has_identity(entry) {
        return false;
    }
    // `symlink_metadata` does not follow the link: git stages a symlink from
    // the link itself, so the link's own metadata is the evidence — and a
    // broken symlink still has metadata to compare.
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let file_type = meta.file_type();
    let on_disk = if file_type.is_symlink() {
        OnDisk::Symlink
    } else if file_type.is_file() {
        OnDisk::File
    } else {
        OnDisk::Other
    };
    if !matches_entry_kind(entry.mode, on_disk, symlinks_emulated) {
        return false;
    }
    identity_matches(entry, &meta)
}

/// Whether the file's identity is the one the index recorded.
///
/// The inode and the device name the file itself, and git stages a symlink
/// from the link, so the link's own inode is what the index records. The
/// comparison truncates to 32 bits, the width the index format stores, and
/// compares the way git's own `ce_match_stat` does.
#[cfg(unix)]
fn identity_matches(entry: &git2::IndexEntry, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.ino() as u32 == entry.ino && meta.dev() as u32 == entry.dev
}

/// Whether the file's identity is the one the index recorded.
///
/// Windows has no stable inode to cache, so git caches the file's creation
/// time instead — the C runtime reports the creation time as the change time,
/// and it does not move when the file is edited.
#[cfg(not(unix))]
fn identity_matches(entry: &git2::IndexEntry, meta: &std::fs::Metadata) -> bool {
    creation_time_matches(meta, &entry.ctime)
}

/// Whether the file's creation time is the one the index recorded.
///
/// The seconds must match. The nanoseconds are compared only when the index
/// recorded them, and within one 100-nanosecond tick: Windows records time in
/// those ticks, so a difference of one is a rounding difference and not a
/// different file.
#[cfg(not(unix))]
fn creation_time_matches(meta: &std::fs::Metadata, recorded: &git2::IndexTime) -> bool {
    let Ok(created) = meta.created() else {
        return false;
    };
    let Ok(since_epoch) = created.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    // Git stores the seconds as an unsigned 32-bit value, so compare in that
    // width and let a date past 2038 wrap the same way on both sides.
    if since_epoch.as_secs() as u32 != recorded.seconds() as u32 {
        return false;
    }
    recorded.nanoseconds() == 0
        || since_epoch.subsec_nanos().abs_diff(recorded.nanoseconds()) <= 100
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

    /// The output of a git command, with its trailing newline removed.
    fn git_output(args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .expect("git");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

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

    /// A checkout whose tracked file was edited after the index was written is
    /// still a working tree. Git's cached size and modification time are a
    /// change-detection cache, and they go stale on the first edit, so
    /// comparing them against the file on disk reports a dirty checkout as
    /// missing. The file is the same file, and its identity is the evidence.
    #[test]
    fn a_dirty_checkout_is_still_a_working_tree() {
        let td = TestDir::new("project-flagged-dirty");
        init_repo_with_commit(td.path(), &[("file.txt", "content\n")]);
        // Edit the file in place: the same file, with a different size and a
        // different modification time. A file that is *replaced* rather than
        // edited is a different file, and that is a different question.
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(td.path().join("file.txt"))
                .unwrap();
            file.set_len(0).unwrap();
            file.write_all(b"edited, and longer than it was\n").unwrap();
        }

        let repo = Repository::open(td.path()).unwrap();
        repo.config().unwrap().set_bool("core.bare", true).unwrap();
        drop(repo);

        let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a checkout whose file was edited still holds a working tree: {reason}"
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

    /// An index copied out of a checkout carries the identity of *that*
    /// checkout's files, so a bare repository that keeps one can stand
    /// beside an unrelated file that uses a tracked path. The file shares
    /// everything the cached stat data describes — the path, the size, the
    /// modification time — and not the file's identity, so the
    /// repository is not told that it holds a working tree: that advice
    /// would break the one thing it is.
    #[test]
    fn a_bare_repository_with_a_copied_index_is_not_told_it_holds_a_working_tree() {
        let td = TestDir::new("project-bare-copied-index");
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
        // The bare repository now carries the checkout's index, stat data and
        // all: an entry that names a real file, elsewhere.
        std::fs::copy(td.path().join(".git").join("index"), bare.join("index")).unwrap();

        let entry = Repository::open(td.path())
            .unwrap()
            .index()
            .unwrap()
            .get_path(std::path::Path::new("file.txt"), 0)
            .unwrap();
        // An unrelated file that shares the tracked path, the tracked size and
        // the tracked modification time.
        let unrelated = container.join("file.txt");
        std::fs::write(&unrelated, "CONTENT\n").unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&unrelated)
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(
            std::time::UNIX_EPOCH
                + std::time::Duration::new(
                    u64::from(entry.mtime.seconds() as u32),
                    entry.mtime.nanoseconds(),
                ),
        ))
        .unwrap();
        drop(file);

        // Fixture preconditions: the file matches the cached stat data the
        // index carries, and it does not share the file's identity. That is
        // the case a cached-stat comparison does not see.
        let meta = std::fs::metadata(&unrelated).unwrap();
        assert_eq!(
            meta.len(),
            u64::from(entry.file_size),
            "fixture precondition: the unrelated file must share the tracked size"
        );
        let modified = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        assert_eq!(
            (modified.as_secs() as u32, modified.subsec_nanos()),
            (entry.mtime.seconds() as u32, entry.mtime.nanoseconds()),
            "fixture precondition: the unrelated file must share the tracked modification time"
        );
        assert!(
            !identity_matches(&entry, &meta),
            "fixture precondition: the unrelated file must not share the tracked identity"
        );
        let clone = Repository::discover(&bare).unwrap();
        assert!(
            clone
                .config()
                .unwrap()
                .get_bool("core.bare")
                .unwrap_or(false),
            "fixture precondition: the bare clone must carry the flagged config"
        );
        drop(clone);

        let reason = resolve(bare.to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("is a bare repository, not a project working directory"),
            "a genuine bare repository keeps the plain wording: {reason}"
        );
        assert!(
            !reason.contains("core.bare=true"),
            "an unrelated file beside a bare repository is not a working tree: {reason}"
        );
    }

    /// A tracked symlink is staged from the link itself, so a broken link is
    /// still evidence of a working tree. Reading the link's *target* — the
    /// way a plain metadata lookup does — fails outright on a broken link and
    /// compares the wrong file's size and timestamps on a live one.
    #[cfg(unix)]
    #[test]
    fn a_broken_tracked_symlink_is_still_a_working_tree() {
        let td = TestDir::new("project-flagged-broken-symlink");
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", td.path().to_str().unwrap()]);
        std::os::unix::fs::symlink("missing-target", td.path().join("link")).unwrap();
        run(&["-C", td.path().to_str().unwrap(), "add", "link"]);
        run(&[
            "-C",
            td.path().to_str().unwrap(),
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "initial",
        ]);
        run(&[
            "-C",
            td.path().to_str().unwrap(),
            "config",
            "core.bare",
            "true",
        ]);

        let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a checkout whose only tracked file is a broken symlink still holds a working tree: {reason}"
        );
    }

    /// An editor that saves by writing a temporary file and renaming it over
    /// the tracked path leaves the path holding a valid, dirty working-tree
    /// file — under a new identity, because the file was replaced rather than
    /// edited. The repository's own record of having had a work tree is what
    /// settles it; the file's identity does not survive the replacement.
    #[test]
    fn an_atomically_replaced_file_is_still_a_working_tree() {
        let td = TestDir::new("project-flagged-replaced");
        init_repo_with_commit(td.path(), &[("file.txt", "content\n")]);
        // Save the way an editor does: write a temporary file and rename it
        // over the tracked path.
        let temp = td.path().join("file.txt.tmp");
        std::fs::write(&temp, "edited content\n").unwrap();
        std::fs::rename(&temp, td.path().join("file.txt")).unwrap();

        let repo = Repository::open(td.path()).unwrap();
        repo.config().unwrap().set_bool("core.bare", true).unwrap();
        drop(repo);

        let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a checkout whose file was replaced still holds a working tree: {reason}"
        );
    }

    /// With `core.symlinks=false` — git's default on Windows — a tracked
    /// symlink is checked out as a plain file holding the link text, and the
    /// index still records the entry as a symlink. The check has to accept
    /// that representation, or a checkout whose only tracked evidence is an
    /// emulated symlink is reported as bare.
    #[test]
    fn an_emulated_symlink_is_still_a_working_tree() {
        let td = TestDir::new("project-flagged-emulated-symlink");
        let dir = td.path().to_str().unwrap().to_string();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", &dir]);
        run(&["-C", &dir, "config", "core.symlinks", "false"]);
        // Stage the symlink without needing the privilege to create one: git
        // keeps the link target as a blob and the entry as a symlink.
        std::fs::write(td.path().join("link-target.txt"), "target/elsewhere.txt").unwrap();
        let blob = git_output(&["-C", &dir, "hash-object", "-w", "link-target.txt"]);
        let cacheinfo = format!("120000,{blob},link");
        run(&[
            "-C",
            &dir,
            "update-index",
            "--add",
            "--cacheinfo",
            &cacheinfo,
        ]);
        run(&[
            "-C",
            &dir,
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "initial",
        ]);
        // Git checks the entry out as the plain file the emulation produces.
        run(&["-C", &dir, "checkout", "--", "link"]);
        // The fixture drops the repository's record of having had a work tree,
        // so the checked-out file itself is the only evidence left.
        run(&["-C", &dir, "config", "core.logallrefupdates", "false"]);
        std::fs::remove_dir_all(td.path().join(".git").join("logs")).unwrap();
        run(&["-C", &dir, "config", "core.bare", "true"]);

        let reason = resolve(&dir).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a checkout whose only tracked file is an emulated symlink still holds a working tree: {reason}"
        );
    }

    /// Git pathnames are byte sequences on Unix and need not be valid UTF-8.
    /// Decoding them lossily replaces the invalid bytes and looks up a
    /// different path, so a checkout whose only tracked evidence has such a
    /// name was reported as bare.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_tracked_name_is_still_a_working_tree() {
        use std::os::unix::ffi::OsStrExt;
        let td = TestDir::new("project-flagged-non-utf8");
        let dir = td.path().to_str().unwrap().to_string();
        let name = std::ffi::OsStr::from_bytes(b"weird\xffname.txt");
        std::fs::write(td.path().join(name), b"content\n").unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", &dir]);
        // The name is not valid UTF-8, so it is handed to git as raw bytes.
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .arg("add")
            .arg(name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git add");
        assert!(status.success(), "git add failed");
        run(&[
            "-C",
            &dir,
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "initial",
        ]);
        run(&["-C", &dir, "config", "core.bare", "true"]);

        let reason = resolve(&dir).unwrap_err();
        assert!(
            reason.contains("core.bare=true"),
            "a checkout whose tracked file has a non-UTF-8 name still holds a working tree: {reason}"
        );
    }

    /// The index's mode and the file on disk must agree on what kind of file
    /// it is: a symlink standing where the index records a regular file is a
    /// different file, and a regular file standing where the index records a
    /// symlink is a different file too — unless git emulates the symlink as a
    /// plain file.
    #[test]
    fn entry_kind_matching_separates_symlinks_from_regular_files() {
        assert!(matches_entry_kind(SYMLINK_MODE, OnDisk::Symlink, false));
        assert!(!matches_entry_kind(SYMLINK_MODE, OnDisk::File, false));
        assert!(matches_entry_kind(SYMLINK_MODE, OnDisk::File, true));
        assert!(!matches_entry_kind(SYMLINK_MODE, OnDisk::Other, true));
        assert!(matches_entry_kind(REGULAR_MODE, OnDisk::File, false));
        assert!(matches_entry_kind(EXECUTABLE_MODE, OnDisk::File, false));
        assert!(!matches_entry_kind(REGULAR_MODE, OnDisk::Symlink, false));
        assert!(!matches_entry_kind(REGULAR_MODE, OnDisk::Other, false));
        assert!(!matches_entry_kind(GITLINK_MODE, OnDisk::File, false));
        assert!(!matches_entry_kind(SPARSE_DIR_MODE, OnDisk::Other, false));
    }

    /// The path of an indexed entry keeps the bytes git recorded, so a
    /// tracked name that is not valid UTF-8 still names the file on disk.
    #[cfg(unix)]
    #[test]
    fn tracked_paths_keep_the_index_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let path = tracked_path(std::path::Path::new("/root"), b"weird\xffname.txt");
        assert_eq!(path.as_os_str().as_bytes(), b"/root/weird\xffname.txt");
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
