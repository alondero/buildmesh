//! Remote and repository-identity reads (issue #2526).
//!
//! The PR and portability flows need to know which repository they are in:
//! its current branch, its `origin` URL, and the names of its remotes. That
//! is a read of the repository's identity, so it lives here rather than in
//! the command layer holding a `Repository` handle.

use git2::Repository;

/// The repository's identity: which branch it is on, where `origin` points,
/// and what other remotes it has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoIdentity {
    /// The current branch, or the short commit SHA when HEAD is detached.
    /// Empty when HEAD cannot be resolved.
    pub branch: String,
    /// The URL of the `origin` remote, when it is configured.
    pub remote_url: Option<String>,
    /// The names of the configured remotes.
    pub remotes: Vec<String>,
}

/// The identity of the repository at `repo_path`.
pub fn repo_identity(repo_path: &str) -> Result<RepoIdentity, String> {
    let repo = open_trusted(repo_path)?;
    let branch = current_branch_of(&repo);
    let remote_url = configured_remote_url(&repo, "origin");
    let remotes = remote_names(&repo);
    Ok(RepoIdentity {
        branch,
        remote_url,
        remotes,
    })
}

/// The URL of a configured remote, if it has one.
///
/// A remote that is not configured is `None`; so is one whose URL cannot be
/// read. Both mean "no URL to show", which is what every caller does with
/// the answer.
pub fn remote_url(repo_path: &str, name: &str) -> Result<Option<String>, String> {
    let repo = open_trusted(repo_path)?;
    Ok(configured_remote_url(&repo, name))
}

/// The current branch, read from an open repository.
///
/// The short commit SHA when HEAD is detached; empty when HEAD cannot be
/// resolved (an unborn HEAD).
fn current_branch_of(repo: &Repository) -> String {
    match repo.head() {
        Ok(head) => {
            if head.is_branch() {
                match head.shorthand() {
                    Ok(name) => name.to_string(),
                    Err(_) => {
                        tracing::warn!(
                            "Could not read Git branch name; treating it as unavailable"
                        );
                        String::new()
                    }
                }
            } else {
                head.target()
                    .map(|oid| oid.to_string()[..8].to_string())
                    .unwrap_or_default()
            }
        }
        Err(_) => String::new(),
    }
}

/// Open the repository, mapping libgit2's ownership error to the
/// safe-directory hint the user needs to act on it.
fn open_trusted(repo_path: &str) -> Result<Repository, String> {
    super::primitives::open_at(repo_path).map_err(|error| {
        if error.code() == git2::ErrorCode::Owner {
            let command = safe_directory_command(
                &crate::env::to_host_path(repo_path),
                cfg!(windows),
            );
            let runtime = if cfg!(windows) { "Windows" } else { "host" };
            format!("git error: {error}. If you trust this repository, add its exact path to {runtime} Git configuration: {command}")
        } else {
            format!("git error: {error}")
        }
    })
}

/// The URL of a configured remote, read from an open repository.
fn configured_remote_url(repo: &Repository, name: &str) -> Option<String> {
    let remote = match repo.find_remote(name) {
        Ok(remote) => remote,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return None,
        Err(_) => {
            tracing::warn!(
                remote = name,
                "Could not read Git remote; treating it as unavailable"
            );
            return None;
        }
    };

    match remote.url() {
        Ok(url) => Some(url.to_string()),
        Err(_) => {
            tracing::warn!(
                remote = name,
                "Could not read Git remote URL; treating it as unavailable"
            );
            None
        }
    }
}

/// The names of the configured remotes, read from an open repository.
fn remote_names(repo: &Repository) -> Vec<String> {
    match repo.remotes() {
        Ok(names) => (0..names.len())
            .filter_map(|index| match names.get(index) {
                Ok(Some(name)) => Some(name.to_string()),
                Ok(None) => None,
                Err(_) => {
                    tracing::warn!(index, "Could not read a Git remote name; omitting it");
                    None
                }
            })
            .collect(),
        Err(_) => {
            tracing::warn!("Could not list Git remotes; returning an empty remote list");
            Vec::new()
        }
    }
}

/// The `git config --global --add safe.directory` command that trusts a
/// repository the user has vouched for.
pub(crate) fn safe_directory_command(host_path: &str, windows: bool) -> String {
    let quoted = if windows {
        // PowerShell accepts smart apostrophes as string delimiters too, so the
        // shared literal is the only escaping that survives such a path.
        crate::env::powershell_literal(&host_path.replace('\\', "/"))
    } else {
        format!("'{}'", host_path.replace('\'', "'\\''"))
    };
    format!("git config --global --add safe.directory {quoted}")
}
