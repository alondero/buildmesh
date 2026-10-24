//! The one SHA resolver (issue #2533).
//!
//! Before this module the crate had three separate "resolve a SHA" helpers
//! that disagreed about their fallback policy:
//!
//!   * `git::worktree::resolve_base_commit` / `resolve_base_ref_sha` —
//!     libgit2, with a HEAD fallback when the ref does not resolve,
//!   * `git::worktree::provision::read_origin_ref_sha` — a `git rev-parse`
//!     shell-out, **no** fallback (a missing remote ref is a real condition
//!     the caller must be able to see), and
//!   * the warm pool's `read_warm_head_sha` — a `git rev-parse HEAD`
//!     shell-out against the *worktree* directory, no fallback.
//!
//! Three implementations of one question is three places to get it wrong, so
//! this module answers it once: [`resolve_sha`] takes an explicit
//! [`ShaSource`] and applies the fallback policy that source implies.
//!
//! The source is explicit on purpose. "Which helper was called" was the only
//! thing that distinguished the three policies, and a caller could not read
//! the policy off the call site. Now the policy is a value:
//!
//!   * [`ShaSource::OriginRef`] — resolve the named ref. A missing
//!     remote-tracking ref is reported as an error rather than silently
//!     degraded to HEAD: the
//!     spawn path treats the absence as "skip the drift check", and a silent
//!     HEAD fallback would have turned that into a false "no drift".
//!   * [`ShaSource::LocalHead`] — resolve the repository's own HEAD; an empty
//!     repository is reported as an error.
//!   * [`ShaSource::BaseRef`] — resolve the mesh's base ref, falling back to
//!     HEAD when the ref is unresolvable (offline, unfetched). ADR 0001's
//!     offline-resilience rule: a spawn proceeds from local HEAD rather
//!     than hard-failing.
//!
//! Each source resolves through libgit2 (ADR 0007's rule that Git access
//! lives in `git/` and goes through one library). The shell-outs these
//! helpers replaced were pure ref lookups — no checkout, no worktree
//! mutation — so they did not need the CLI's write path.

use git2::{Oid, Repository};

use crate::env::to_host_path;

/// Where a SHA comes from, and therefore how a failure to resolve it is
/// handled. See the module docs for the per-source policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShaSource {
    /// A remote-tracking ref — e.g. `origin/feat-x` for a same-repo PR, or
    /// `fork-alice/feat-x` for a fork PR. Resolved without a fallback: a
    /// missing ref is an error rather than a HEAD fallback.
    OriginRef(String),
    /// The repository's own HEAD. Resolved without a fallback.
    LocalHead,
    /// The mesh's base ref — e.g. `origin/main`, or the literal `HEAD` for
    /// the explicit "Head" option. Falls back to HEAD when the ref is
    /// unresolvable (ADR 0001's offline fallback).
    BaseRef(String),
}

impl ShaSource {
    /// The ref this source names, for log messages.
    fn describe(&self) -> String {
        match self {
            ShaSource::OriginRef(name) => format!("remote ref '{}'", name),
            ShaSource::LocalHead => "HEAD".to_string(),
            ShaSource::BaseRef(name) => format!("base ref '{}'", name),
        }
    }
}

/// Why a SHA could not be resolved.
#[derive(Debug)]
pub enum ShaResolveError {
    /// The repository could not be opened at the given path.
    Repository { path: String, error: git2::Error },
    /// The source named a ref that does not exist, or does not name a commit.
    Unresolved {
        source: ShaSource,
        error: git2::Error,
    },
    /// The repository has no HEAD commit (an empty repository).
    NoHead { error: git2::Error },
}

impl std::fmt::Display for ShaResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShaResolveError::Repository { path, error } => {
                write!(f, "Failed to open repository at {}: {}", path, error)
            }
            ShaResolveError::Unresolved { source, error } => {
                write!(f, "Could not resolve {}: {}", source.describe(), error)
            }
            ShaResolveError::NoHead { error } => {
                write!(f, "Failed to resolve HEAD commit: {}", error)
            }
        }
    }
}

impl std::error::Error for ShaResolveError {}

/// Resolve `source` against an already-open repository.
///
/// This is the single implementation behind the crate's SHA lookups;
/// [`resolve_sha_at`] is the path-opening wrapper around it.
pub(crate) fn resolve_sha(repo: &Repository, source: &ShaSource) -> Result<Oid, ShaResolveError> {
    match source {
        ShaSource::OriginRef(name) => {
            // No fallback here: a missing remote-tracking ref means
            // the local cache is stale — the caller decides what to do about
            // that, and must not be handed HEAD by accident.
            repo.revparse_single(name)
                .and_then(|obj| obj.peel_to_commit())
                .map(|commit| commit.id())
                .map_err(|error| ShaResolveError::Unresolved {
                    source: source.clone(),
                    error,
                })
        }
        ShaSource::LocalHead => head_commit(repo).map(|commit| commit.id()),
        ShaSource::BaseRef(name) => {
            if name.is_empty() || name == "HEAD" {
                return head_commit(repo).map(|commit| commit.id());
            }
            match repo
                .revparse_single(name)
                .and_then(|obj| obj.peel_to_commit())
            {
                Ok(commit) => Ok(commit.id()),
                Err(error) => {
                    // ADR 0001's offline-resilience rule: a spawn proceeds
                    // from local HEAD rather than hard-failing when the base
                    // ref is unresolvable (offline, not fetched).
                    tracing::warn!(
                        "resolve_sha: could not resolve {} ({}); falling back to HEAD",
                        source.describe(),
                        error
                    );
                    head_commit(repo).map(|commit| commit.id())
                }
            }
        }
    }
}

/// Resolve `source` against the repository at `path`.
///
/// `path` may be a WSL path or a host path; it is converted through
/// [`to_host_path`] first, so callers may pass the DB-stored form.
pub fn resolve_sha_at(path: &str, source: &ShaSource) -> Result<Oid, ShaResolveError> {
    let host_path = to_host_path(path);
    let repo =
        super::primitives::open_at(&host_path).map_err(|error| ShaResolveError::Repository {
            path: host_path.clone(),
            error,
        })?;
    resolve_sha(&repo, source)
}

/// The repository's HEAD commit, or an error when it has none.
fn head_commit(repo: &Repository) -> Result<git2::Commit<'_>, ShaResolveError> {
    repo.head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| ShaResolveError::NoHead { error })
}

#[cfg(test)]
#[path = "sha_tests.rs"]
mod tests;
