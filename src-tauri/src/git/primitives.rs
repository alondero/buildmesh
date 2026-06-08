//! Low-level git2 helpers shared across commands and services.
//!
//! Each helper takes an already-open `&Repository` (or an OID pair) and returns
//! a `Result`/`Option` — deliberately **not** a bare `bool`. The previous
//! per-module copies disagreed on their error fallback: the sync and close
//! *gates* failed closed (treat an unreadable repo as dirty/risky, don't lose
//! work), while the *display* paths failed open (treat it as clean). Returning
//! the raw computation keeps that choice at the call site — gates use
//! `.unwrap_or(true)`, display uses `.unwrap_or(false)` — instead of baking one
//! direction into a shared helper and silently breaking the other.

use git2::{Oid, Repository, StatusOptions};

use crate::env::to_host_path;

/// Open the repository at a session/internal path, converting it to a
/// host-readable path first (WSL → UNC, etc.). Callers that also need the
/// host-path string for a later subprocess `current_dir` should call
/// [`to_host_path`] themselves and `Repository::open` directly; this helper is
/// for the common open-and-use-the-repo case.
pub fn open_from_host_path(path: &str) -> Result<Repository, git2::Error> {
    Repository::open(to_host_path(path))
}

/// Whether the working tree has any non-ignored change (modified, staged,
/// untracked, deleted, renamed). The canonical dirty check.
///
/// We filter only on `!is_ignored()`. A previous copy also excluded
/// `Status::CURRENT`, but `StatusOptions` here never sets `include_unmodified`,
/// so libgit2 never emits a `CURRENT` entry — the extra clause was a no-op.
pub fn is_dirty(repo: &Repository) -> Result<bool, git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    Ok(repo
        .statuses(Some(&mut opts))?
        .iter()
        .any(|e| !e.status().is_ignored()))
}

/// The abbreviated OID (respects the repo's `core.abbreviate`, default 7), as
/// `git rev-parse --short` would print it. Returns an empty string if the OID
/// can't be resolved (e.g. unborn HEAD passed a sentinel) rather than failing —
/// callers use it for display.
pub fn short_sha(repo: &Repository, oid: Oid) -> String {
    repo.find_object(oid, None)
        .ok()
        .and_then(|obj| obj.short_id().ok())
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
        .unwrap_or_default()
}

/// The branch a repo's HEAD points at, read from HEAD's symbolic target so it
/// survives the branch ref being deleted. Returns `None` for a detached HEAD
/// (no symbolic target).
pub fn head_branch_name(repo: &Repository) -> Option<String> {
    let head = repo.find_reference("HEAD").ok()?;
    let target = head.symbolic_target()?;
    target.strip_prefix("refs/heads/").map(|s| s.to_string())
}

/// The upstream remote-tracking OID configured for a local branch, if there is
/// one and it resolves. `None` covers "no upstream configured" and "upstream
/// ref missing/unreadable" alike — callers that must distinguish those treat
/// `None` as "nothing to compare against".
pub fn upstream_oid_for_branch(repo: &Repository, branch: &str) -> Option<Oid> {
    let refname = format!("refs/heads/{}", branch);
    let upstream_buf = repo.branch_upstream_name(&refname).ok()?;
    let upstream_ref = upstream_buf.as_str()?;
    repo.find_reference(upstream_ref).ok()?.target()
}

/// `(ahead, behind)` of `local` relative to `upstream`, saturating to `u32`.
///
/// Uses git2's `graph_ahead_behind` rather than `git rev-list ...@{u}`: the
/// `@{u}` brace syntax is silently mangled by `Command::args` on Windows.
pub fn ahead_behind(repo: &Repository, local: Oid, upstream: Oid) -> Result<(u32, u32), git2::Error> {
    let (ahead, behind) = repo.graph_ahead_behind(local, upstream)?;
    Ok((
        ahead.try_into().unwrap_or(u32::MAX),
        behind.try_into().unwrap_or(u32::MAX),
    ))
}

/// Normalise a path for equality comparison: convert to host form, trim a
/// trailing separator, and canonicalise (best-effort — on Windows a held-handle
/// error falls back to a slash-normalised string compare). Used to match
/// worktree paths against the live agent-node path list.
pub fn normalize_for_compare(path: &str) -> String {
    let host = to_host_path(path);
    let trimmed = host.trim_end_matches(['/', '\\']);
    std::fs::canonicalize(trimmed)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| trimmed.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_helpers::{commit_file, init_repo_with_commit, TestDir};
    use std::fs;

    #[test]
    fn is_dirty_false_on_clean_repo() {
        let td = TestDir::new("prim_clean");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        assert!(!is_dirty(&repo).unwrap());
    }

    #[test]
    fn is_dirty_true_with_untracked_file() {
        let td = TestDir::new("prim_untracked");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        fs::write(td.path().join("scratch.txt"), "x").unwrap();
        assert!(is_dirty(&repo).unwrap());
    }

    #[test]
    fn is_dirty_true_with_modified_tracked_file() {
        let td = TestDir::new("prim_modified");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        fs::write(td.path().join("f.txt"), "changed\n").unwrap();
        assert!(is_dirty(&repo).unwrap());
    }

    #[test]
    fn short_sha_is_nonempty_prefix_of_full_oid() {
        let td = TestDir::new("prim_shortsha");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        let short = short_sha(&repo, oid);
        assert!(!short.is_empty());
        assert!(oid.to_string().starts_with(&short));
        assert!(short.len() >= 7);
    }

    #[test]
    fn head_branch_name_returns_branch_then_none_when_detached() {
        let td = TestDir::new("prim_headbranch");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        // init_repo_with_commit commits on whatever the default branch is.
        let on_branch = head_branch_name(&repo);
        assert!(on_branch.is_some(), "expected a symbolic HEAD branch");

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.set_head_detached(head.id()).unwrap();
        assert_eq!(head_branch_name(&repo), None, "detached HEAD has no branch");
    }

    #[test]
    fn ahead_behind_counts_both_directions() {
        let td = TestDir::new("prim_aheadbehind");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let base = repo.head().unwrap().peel_to_commit().unwrap().id();
        let next = commit_file(&repo, td.path(), "f.txt", "b\n");

        assert_eq!(ahead_behind(&repo, next, base).unwrap(), (1, 0));
        assert_eq!(ahead_behind(&repo, base, next).unwrap(), (0, 1));
        assert_eq!(ahead_behind(&repo, base, base).unwrap(), (0, 0));
    }

    #[test]
    fn upstream_oid_for_branch_resolves_configured_tracking_branch() {
        let td = TestDir::new("prim_upstream");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let branch = head_branch_name(&repo).unwrap();
        let base = repo.head().unwrap().peel_to_commit().unwrap().id();

        // A configured remote (with its default fetch refspec) is what lets
        // `branch_upstream_name` map the branch's `merge` ref to the
        // `refs/remotes/origin/*` tracking ref.
        repo.remote("origin", td.path().to_str().unwrap()).unwrap();
        // Point a remote-tracking ref at the base commit and wire the branch's
        // upstream config to it, then advance the local branch by one commit.
        repo.reference(
            &format!("refs/remotes/origin/{}", branch),
            base,
            false,
            "test",
        )
        .unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str(&format!("branch.{}.remote", branch), "origin").unwrap();
        cfg.set_str(&format!("branch.{}.merge", branch), &format!("refs/heads/{}", branch))
            .unwrap();
        let local = commit_file(&repo, td.path(), "f.txt", "b\n");

        let upstream = upstream_oid_for_branch(&repo, &branch).expect("upstream resolves");
        assert_eq!(upstream, base);
        assert_eq!(ahead_behind(&repo, local, upstream).unwrap(), (1, 0));
    }

    #[test]
    fn upstream_oid_for_branch_none_without_tracking_config() {
        let td = TestDir::new("prim_no_upstream");
        let repo = init_repo_with_commit(td.path(), &[("f.txt", "a\n")]);
        let branch = head_branch_name(&repo).unwrap();
        assert_eq!(upstream_oid_for_branch(&repo, &branch), None);
    }

    #[test]
    fn normalize_for_compare_is_stable_across_trailing_slash() {
        let td = TestDir::new("prim_normalize");
        let p = td.path().to_string_lossy().to_string();
        let with_slash = format!("{}/", p.trim_end_matches(['/', '\\']));
        assert_eq!(normalize_for_compare(&p), normalize_for_compare(&with_slash));
    }
}
