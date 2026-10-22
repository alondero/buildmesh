//! Derived trust for Windows access to WSL linked worktrees. The parent must
//! already open normally. Grants live in a profile-owned, globally included Git
//! config, so libgit2 and Windows Git agree without changing process settings.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use git2::{Config, Error, ErrorClass, ErrorCode, Repository};

const TRUST_FILE: &str = "worktree-trust.gitconfig";
const WRITE_WAIT: Duration = Duration::from_secs(3);

fn owner_error(message: impl AsRef<str>) -> Error {
    Error::new(ErrorCode::Owner, ErrorClass::Config, message.as_ref())
}

fn store_path() -> Result<PathBuf, Error> {
    crate::preferences::app_data_dir()
        .map(|dir| dir.join(TRUST_FILE))
        .ok_or_else(|| {
            owner_error(
                "Cannot record WSL worktree trust: Buildmesh profile directory is unavailable",
            )
        })
}

fn path_identity(path: &Path) -> Result<String, Error> {
    let path = std::fs::canonicalize(path).map_err(|e| Error::from_str(&e.to_string()))?;
    Ok(crate::env::wsl_host_path_aliases(&path)
        .into_iter()
        .next()
        .unwrap_or_else(|| crate::env::canonical_host_path_string(&path)))
}

fn linked_path(base: &Path, text: &str) -> PathBuf {
    let host = PathBuf::from(crate::env::to_host_path(text.trim()));
    if host.is_absolute() {
        host
    } else {
        base.join(host)
    }
}

/// Establish registration and both directions of the link without opening the
/// untrusted candidate as a repository. Administrative names need not match
/// checkout directory names (warm-worktree moves retain their original name).
fn verify_linkage(parent: &Repository, candidate: &Path) -> Result<(), Error> {
    let candidate_id = path_identity(candidate)?;
    let common_id = path_identity(parent.commondir())?;
    let link = std::fs::read_to_string(candidate.join(".git"))
        .map_err(|e| owner_error(format!("Cannot verify WSL worktree gitlink: {e}")))?;
    let target = link
        .trim()
        .strip_prefix("gitdir: ")
        .ok_or_else(|| owner_error("Cannot trust WSL worktree: invalid .git link"))?;
    let admin = std::fs::canonicalize(linked_path(candidate, target))
        .map_err(|e| owner_error(format!("Cannot verify WSL worktree admin directory: {e}")))?;
    let admin_parent = admin
        .parent()
        .ok_or_else(|| owner_error("Invalid worktree admin directory"))?;
    if path_identity(admin_parent)? != path_identity(&parent.commondir().join("worktrees"))? {
        return Err(owner_error("Cannot trust WSL worktree: gitlink is outside the trusted repository's worktree registry"));
    }
    let name = admin
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| owner_error("Invalid worktree admin name"))?;
    let worktree = parent.find_worktree(name)?;
    let back = std::fs::read_to_string(admin.join("gitdir"))
        .map_err(|e| owner_error(format!("Cannot verify WSL worktree backpointer: {e}")))?;
    let common = std::fs::read_to_string(admin.join("commondir"))
        .map_err(|e| owner_error(format!("Cannot verify WSL worktree common directory: {e}")))?;
    if path_identity(worktree.path())? == candidate_id
        && path_identity(&linked_path(&admin, &common))? == common_id
        && path_identity(&linked_path(&admin, &back))? == path_identity(&candidate.join(".git"))?
    {
        Ok(())
    } else {
        Err(owner_error(
            "Cannot trust WSL worktree: registration and Git links disagree",
        ))
    }
}

// libgit2 locks the target config before reading its current contents. Matching
// the escaped exact value makes concurrent inserts idempotent, preserves empty
// safe.directory reset entries and never replaces another path or include.
fn set_exact(config: &mut Config, key: &str, value: &str) -> Result<(), Error> {
    config.set_multivar(key, &format!("^{}$", regex::escape(value)), value)
}

fn write_retry(mut operation: impl FnMut() -> Result<(), Error>) -> Result<(), Error> {
    let start = Instant::now();
    loop {
        match operation() {
            Err(error) if error.code() == ErrorCode::Locked && start.elapsed() < WRITE_WAIT => {
                std::thread::sleep(Duration::from_millis(25));
            }
            result => return result,
        }
    }
}

fn record_grants(store: &Path, paths: &[String]) -> Result<(), Error> {
    if let Some(dir) = store.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::from_str(&e.to_string()))?;
    }
    for path in paths {
        write_retry(|| set_exact(&mut Config::open(store)?, "safe.directory", path))?;
    }
    let include = crate::env::canonical_host_path_string(store);
    write_retry(|| {
        let mut defaults = Config::open_default()?;
        set_exact(&mut defaults.open_global()?, "include.path", &include)
    })
}

/// Called only by Buildmesh worktree lifecycle operations, with their resolved
/// candidate path. A user-trusted root never grants arbitrary neighboring repos.
pub(super) fn ensure_linked_worktree(parent: &Repository, candidate: &Path) -> Result<(), Error> {
    if !cfg!(windows) || crate::env::wsl_host_path_aliases(candidate).is_empty() {
        return Ok(());
    }
    match Repository::open(candidate) {
        Ok(_) => return Ok(()),
        Err(error) if error.code() == ErrorCode::Owner => {}
        Err(error) => return Err(error),
    }
    // Reopen, rather than treating an init handle or a stale caller's handle as
    // proof that the parent still passes ownership validation.
    // A Mesh may itself be a trusted linked checkout. Its main working tree
    // need not have an independent safe.directory entry.
    let parent = Repository::open(parent.workdir().unwrap_or_else(|| parent.path()))?;
    verify_linkage(&parent, candidate)?;
    let paths = crate::env::wsl_host_path_aliases(
        &std::fs::canonicalize(candidate).map_err(|e| Error::from_str(&e.to_string()))?,
    );
    let store = store_path()?;
    record_grants(&store, &paths).map_err(|error| Error::new(error.code(), error.class(), format!(
        "Cannot record WSL worktree trust in {}: {error}. Check write access to the Buildmesh profile and Windows Git's global configuration, then retry", store.display()
    )))?;
    Repository::open(candidate).map(|_| ()).map_err(|error| {
        Error::new(error.code(), error.class(), format!("WSL worktree trust was recorded, but Git still refuses this path: {error}. Check global Git include.path and safe.directory reset entries"))
    })
}

/// Revoke only Buildmesh-owned entries after successful move/removal. User
/// safe.directory entries and grants in other profiles remain untouched.
pub(super) fn forget_worktree(candidate: &Path) -> Result<(), Error> {
    if !cfg!(windows) {
        return Ok(());
    }
    // A removal retry may only have the lexical spelling of an absent path.
    // Resolve its surviving parent so aliases through a symlinked container
    // still revoke the canonical grant recorded before deletion.
    let candidate = match std::fs::canonicalize(candidate) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match (candidate.parent(), candidate.file_name()) {
                (Some(parent), Some(name)) if parent.exists() => parent
                    .canonicalize()
                    .map_err(|e| Error::from_str(&e.to_string()))?
                    .join(name),
                _ => candidate.to_path_buf(),
            }
        }
        Err(error) => return Err(Error::from_str(&error.to_string())),
    };
    let paths = crate::env::wsl_host_path_aliases(&candidate);
    if paths.is_empty() {
        return Ok(());
    }
    let store = store_path()?;
    if !store.exists() {
        return Ok(());
    }
    for path in paths {
        write_retry(|| {
            let mut config = Config::open(&store)?;
            match config.remove_multivar("safe.directory", &format!("^{}$", regex::escape(&path))) {
                Err(error) if error.code() == ErrorCode::NotFound => Ok(()),
                result => result,
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "trust_tests.rs"]
mod tests;
