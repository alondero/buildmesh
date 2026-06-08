//! Mesh auto-sync: fetch + fast-forward the parent Mesh before a worktree spawn.
//!
//! `fetch_origin` is the best-effort, never-blocking auto-sync that runs before
//! a new Worktree Node is cut (ADR 0001). It used to live in `env`; it belongs
//! with the rest of Buildmesh's git access (ADR 0007). `commits_behind_upstream`
//! is shared with the manual `git_sync` command so both compute "behind" the
//! same way.

use crate::env::to_host_path;
use crate::git::primitives;
use crate::process_util::command_no_window;

/// Outcome of a single `fetch_origin` invocation. The variants let the
/// caller (spawn.rs) decide whether to surface a warning toast without
/// having to reparse strings — every non-`Skipped*` non-`UpToDate` outcome
/// is something the user might want to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Parent repo has uncommitted changes; the pull is skipped per
    /// issue #213's "skip if dirty" criterion. This is silent from the
    /// user's perspective — the user clearly knows the working tree is
    /// dirty, and we don't want to nag them on every spawn.
    SkippedDirty,
    /// No `origin` remote is configured. A purely local Mesh is a
    /// valid setup; we don't surface a toast for that.
    SkippedNoRemote,
    /// Fetched and there were no new commits to pull — already up to
    /// date.
    UpToDate,
    /// Fetched and fast-forwarded `new_commits` commits onto the
    /// current branch. The Agent Node will start from the latest
    /// upstream HEAD.
    Synced { new_commits: u32 },
    /// Fetched `new_commits` commits but the fast-forward was rejected
    /// (local history has diverged from the remote). The Agent Node
    /// still spawns — from the local HEAD — but the user should be
    /// told the auto-sync was partial, so this is the case that
    /// surfaces a warning toast.
    FetchedButDiverged { new_commits: u32, reason: String },
}

/// Failure modes for `fetch_origin` that the caller should treat as
/// "auto-sync unavailable, proceed with local HEAD anyway". The
/// variants carry enough context to compose a user-readable toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The path exists but isn't a git repository (or can't be
    /// opened as one). Unusual — a Mesh without a repo is a
    /// misconfiguration — but we still proceed to spawn since the
    /// agent's behaviour with a non-repo path is a separate concern.
    RepoUnusable(String),
    /// `git fetch` itself failed (most commonly: no network, DNS
    /// failure, or `origin` was removed between the no-remote check
    /// and the fetch). Carries stderr for the toast.
    FetchFailed(String),
}

/// Derive the remote name from a configured `base_ref` string, if the
/// string form names one. Pure string parsing — no repo, no I/O — so
/// it can be unit-tested without a checkout. See issue #276 for the
/// full rationale.
///
/// Returns `Some(remote)` for:
///   - `origin/main`                 → `Some("origin")`
///   - `upstream/feature/auth`       → `Some("upstream")`
///   - `refs/remotes/origin/main`    → `Some("origin")`
///   - `refs/remotes/upstream/feat/x`→ `Some("upstream")`
///
/// Returns `None` for:
///   - `HEAD` / `FETCH_HEAD` / empty / whitespace-only
///     (caller falls back to `origin`)
///   - `refs/heads/main`             (caller looks up `branch.<name>.remote`)
///   - bare `main` / `develop`       (caller looks up `branch.<name>.remote`)
///
/// **Limitation:** a plain nested branch name like `feature/auth`
/// **can't be disambiguated from a remote-qualified form** without a
/// repo, so the parser mirrors [`parse_local_branch`](
/// ../commands/git.rs )'s heuristic and treats the first segment
/// as the remote — `parse_remote_for_base_ref("feature/auth")`
/// returns `Some("feature")`. In practice this is harmless: a
/// `feature` remote almost never exists, so `find_remote("feature")`
/// fails and `fetch_origin` falls through to `SkippedNoRemote`. The
/// buildmesh DB default (`origin/main`) and the common short forms
/// (`main`, `develop`, `origin/develop`) all resolve correctly.
/// Callers that need to disambiguate should use the full
/// `refs/heads/feature/auth` form for the local case or
/// `refs/remotes/<remote>/feature/auth` for the remote case.
fn parse_remote_for_base_ref(base_ref: &str) -> Option<String> {
    let trimmed = base_ref.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("HEAD")
        || trimmed.eq_ignore_ascii_case("FETCH_HEAD")
    {
        return None;
    }
    // refs/remotes/<remote>/<branch...> — remote is the first segment.
    if let Some(after) = trimmed.strip_prefix("refs/remotes/") {
        return after
            .split_once('/')
            .map(|(remote, _)| remote.to_string())
            .filter(|r| is_valid_remote_segment(r));
    }
    // refs/heads/<branch...> never names a remote in the string.
    if trimmed.starts_with("refs/heads/") {
        return None;
    }
    // Short form: `<remote>/<branch>` if it has a slash and the
    // leading segment looks like a remote. The first-segment heuristic
    // matches `parse_local_branch`'s inverse so the two stay
    // symmetric.
    trimmed
        .split_once('/')
        .filter(|(remote, rest)| !rest.is_empty() && is_valid_remote_segment(remote))
        .map(|(remote, _)| remote.to_string())
}

fn is_valid_remote_segment(s: &str) -> bool {
    !s.is_empty()
        && !s.eq_ignore_ascii_case("HEAD")
        && !s.eq_ignore_ascii_case("FETCH_HEAD")
}

/// Look up the upstream remote configured for the current branch.
/// Used as a fallback when `base_ref` doesn't name a remote in its
/// string form (e.g. `refs/heads/main` or a bare `main`).
///
/// Mirrors the `branch.<name>.remote` lookup git itself uses for
/// `branch.<name>.merge`; we derive the remote from the upstream
/// refname (which is `refs/remotes/<remote>/<branch>`) rather than
/// shelling out, to keep the path git2-only and avoid a Windows
/// CreateProcess round-trip.
fn current_branch_upstream_remote(repo: &git2::Repository) -> Option<String> {
    let head = repo.head().ok()?;
    // Detached HEAD has no branch to look up upstream for. `is_branch`
    // returns false on detached HEAD.
    if !head.is_branch() {
        return None;
    }
    let branch_name = head.shorthand()?;
    let local_refname = format!("refs/heads/{}", branch_name);
    let upstream_refname = repo.branch_upstream_name(&local_refname).ok()?;
    let upstream_str = upstream_refname.as_str()?;
    // Format: refs/remotes/<remote>/<branch...>
    upstream_str
        .strip_prefix("refs/remotes/")
        .and_then(|s| s.split_once('/').map(|(r, _)| r.to_string()))
}

/// Fetch from the configured upstream and fast-forward the parent
/// **Mesh**'s current branch onto the remote tip.
///
/// The remote is derived from `base_ref` (issue #276), so a Mesh
/// configured with `base_ref = "upstream/main"` syncs against
/// `upstream` rather than the hardcoded `origin` that issue #213
/// used. The fall-back chain is:
/// 1. The string form of `base_ref` (e.g. `upstream/main` → `upstream`,
///    `refs/remotes/origin/main` → `origin`).
/// 2. The current branch's configured upstream
///    (`git config branch.<name>.remote`) — only reached when
///    `base_ref` is `refs/heads/<branch>` or a bare branch name.
/// 3. The literal string `"origin"` (preserves the issue #213
///    behaviour for `HEAD` / empty / detached cases).
///
/// Behaviour (issue #213, refined by #276):
/// 1. **Dirty parent → skip silently.** A repo with uncommitted
///    changes returns `Ok(SkippedDirty)`. The user is in the middle
///    of something; we don't want to surface a warning on every spawn.
/// 2. **Derived remote missing → skip silently.** A purely local
///    Mesh is valid; this returns `Ok(SkippedNoRemote)`.
/// 3. **Clean repo with remote → fetch, then `git pull --ff-only`.**
///    `Ok(UpToDate)` if nothing to pull, `Ok(Synced)` if commits were
///    pulled, `Ok(FetchedButDiverged)` if the history has diverged
///    (caller surfaces a warning toast), `Err(FetchFailed)` if the
///    fetch itself failed (caller surfaces a warning toast).
///
/// **This function never blocks the spawn.** The caller (`spawn.rs`)
/// is expected to call it as a best-effort step and continue with
/// local-HEAD fallback on any non-`Ok(Synced|_UpToDate|_Skipped*)`
/// outcome.
pub fn fetch_origin(project_root: &str, base_ref: &str) -> Result<FetchOutcome, FetchError> {
    let host_root = to_host_path(project_root);

    // Step 1: open the repo. If the path isn't a git repo, we can't
    // decide dirty / has-remote / pull — bail with a typed error.
    let repo = match git2::Repository::open(&host_root) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "fetch_origin: path {} is not a usable git repo ({}); skipping auto-sync",
                host_root,
                e
            );
            return Err(FetchError::RepoUnusable(e.to_string()));
        }
    };

    // Step 2: skip if the working tree is dirty. Untracked files and
    // staged-but-uncommitted changes both count. We deliberately do
    // this BEFORE shelling out — no point spending a network round
    // trip on a sync that we'd refuse to apply. The
    // `unwrap_or(true)` is the safer fallback: if the status call
    // itself fails (corrupt index, permission denied on .git), we
    // treat the repo as dirty and skip rather than try to pull into
    // a state we couldn't even read.
    // Fail closed: if we can't even read status (corrupt index, permission
    // denied on .git), treat the repo as dirty and skip rather than pull into
    // a state we couldn't read.
    let is_dirty = primitives::is_dirty(&repo).unwrap_or(true);
    if is_dirty {
        tracing::info!(
            "fetch_origin: {} has uncommitted changes; skipping auto-sync",
            host_root
        );
        return Ok(FetchOutcome::SkippedDirty);
    }

    // Step 3: derive the remote from `base_ref` (issue #276). The
    // three-tier fallback is documented in the function-level comment;
    // here we just apply it.
    let remote_name = parse_remote_for_base_ref(base_ref)
        .or_else(|| current_branch_upstream_remote(&repo))
        .unwrap_or_else(|| "origin".to_string());

    // Step 4: skip if the derived remote is missing. A Mesh may be a
    // purely local repo (not yet pushed) and that's a valid state —
    // and a `base_ref = "upstream/main"` against a repo with no
    // `upstream` configured is also valid; we just don't sync.
    if repo.find_remote(&remote_name).is_err() {
        tracing::info!(
            "fetch_origin: {} has no '{}' remote (base_ref={:?}); skipping auto-sync",
            host_root,
            remote_name,
            base_ref
        );
        return Ok(FetchOutcome::SkippedNoRemote);
    }

    // Step 5: `git fetch <remote>`. If this fails (network down,
    // auth, remote deleted between the has-remote check and the
    // fetch) we return FetchFailed and the caller surfaces a toast.
    tracing::info!(
        "fetch_origin: running git fetch {} in {}",
        remote_name,
        host_root
    );
    let fetch_output = match command_no_window("git")
        .args(["fetch", &remote_name])
        .current_dir(&host_root)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("fetch_origin: failed to spawn git fetch: {}", e);
            return Err(FetchError::FetchFailed(e.to_string()));
        }
    };
    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        let trimmed = stderr.trim();
        tracing::warn!(
            "fetch_origin: git fetch {} failed: {}",
            remote_name,
            trimmed
        );
        return Err(FetchError::FetchFailed(trimmed.to_string()));
    }

    // Step 6: count how many commits the local branch is behind the
    // upstream. If 0, we're already in sync. We use git2's
    // `graph_ahead_behind` (not `git rev-list HEAD..@{u}`) to avoid
    // the Windows `@{u}` brace-stripping bug — see the comment in
    // commands/git.rs for the same pattern.
    let new_commits = match commits_behind_upstream(&repo) {
        Ok(n) => n,
        Err(e) => {
            // No upstream configured is the most common cause here
            // (e.g. a fresh local-only branch that just gained a
            // remote via `git remote add` but never pushed). Treat
            // it as "nothing to pull" rather than a hard error.
            tracing::info!(
                "fetch_origin: no upstream to compare against ({}); treating as up-to-date",
                e
            );
            return Ok(FetchOutcome::UpToDate);
        }
    };
    if new_commits == 0 {
        return Ok(FetchOutcome::UpToDate);
    }

    // Step 7: `git pull --ff-only --no-rebase`. We pass --no-rebase
    // explicitly because a user's global `pull.rebase=true` would
    // otherwise turn this into a rebase — and rebase on a diverged
    // history produces a merge conflict (write conflict markers to
    // the working tree) instead of the clean "not fast-forwardable"
    // rejection we want. The auto-sync is read-only by policy
    // (issue #213: spawn never blocks, never mutates the local
    // branch on failure), so we must never silently rebase.
    tracing::info!(
        "fetch_origin: running git pull --ff-only --no-rebase ({} new commit{} behind)",
        new_commits,
        if new_commits == 1 { "" } else { "s" }
    );
    let pull_output = match command_no_window("git")
        .args(["pull", "--ff-only", "--no-rebase"])
        .current_dir(&host_root)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Ok(FetchOutcome::FetchedButDiverged {
                new_commits,
                reason: format!("git pull --ff-only failed to start: {}", e),
            });
        }
    };
    if !pull_output.status.success() {
        let stderr = String::from_utf8_lossy(&pull_output.stderr);
        let trimmed = stderr.trim();
        tracing::warn!(
            "fetch_origin: git pull --ff-only rejected: {}",
            trimmed
        );
        return Ok(FetchOutcome::FetchedButDiverged {
            new_commits,
            reason: if trimmed.is_empty() {
                "fast-forward rejected (likely local history diverged)".to_string()
            } else {
                trimmed.to_string()
            },
        });
    }

    Ok(FetchOutcome::Synced { new_commits })
}

/// How many commits the current branch is behind its upstream. Shared by
/// `fetch_origin` (auto-sync) and the manual `git_sync` command to decide
/// whether a `git pull --ff-only` would do anything. Returns `Err` when there's
/// no upstream to compare against — both callers treat that as "nothing to pull".
pub fn commits_behind_upstream(repo: &git2::Repository) -> Result<u32, String> {
    let head_oid = repo
        .head()
        .map_err(|e| format!("Failed to read HEAD: {}", e))?
        .peel_to_commit()
        .map_err(|e| format!("HEAD is not a commit: {}", e))?
        .id();

    let branch_name = repo
        .head()
        .map_err(|e| format!("Failed to read HEAD: {}", e))?
        .shorthand()
        .ok_or_else(|| "HEAD is not on a branch".to_string())?
        .to_string();
    let upstream_oid = primitives::upstream_oid_for_branch(repo, &branch_name)
        .ok_or_else(|| format!("no upstream configured for refs/heads/{}", branch_name))?;

    let (_ahead, behind) = primitives::ahead_behind(repo, head_oid, upstream_oid)
        .map_err(|e| format!("graph_ahead_behind failed: {}", e))?;
    Ok(behind)
}

#[cfg(test)]
#[path = "fetch_origin_tests.rs"]
mod fetch_origin_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // Issue #276 — parse_remote_for_base_ref (pure string parser).
    // The integration tests in fetch_origin_tests.rs exercise the
    // end-to-end fetch path; these cover the parser edge cases
    // (HEAD / empty / refs/heads / nested branch / whitespace)
    // without needing a git repo.
    // -----------------------------------------------------------------

    #[test]
    fn parse_remote_accepts_origin_slash_main() {
        // The buildmesh DB default.
        assert_eq!(
            parse_remote_for_base_ref("origin/main"),
            Some("origin".to_string())
        );
    }

    #[test]
    fn parse_remote_accepts_upstream_slash_branch() {
        // The headline case from issue #276: a Mesh that points at
        // a project-of-record upstream, not the user's personal fork.
        assert_eq!(
            parse_remote_for_base_ref("upstream/main"),
            Some("upstream".to_string())
        );
        assert_eq!(
            parse_remote_for_base_ref("upstream/develop"),
            Some("upstream".to_string())
        );
    }

    #[test]
    fn parse_remote_keeps_remote_for_nested_branch() {
        // "upstream/feature/auth" — the remote is `upstream`; the
        // branch (with its slashes) doesn't change the remote.
        assert_eq!(
            parse_remote_for_base_ref("upstream/feature/auth"),
            Some("upstream".to_string())
        );
        assert_eq!(
            parse_remote_for_base_ref("origin/release/v1.0"),
            Some("origin".to_string())
        );
    }

    #[test]
    fn parse_remote_accepts_refs_remotes_form() {
        assert_eq!(
            parse_remote_for_base_ref("refs/remotes/origin/main"),
            Some("origin".to_string())
        );
        assert_eq!(
            parse_remote_for_base_ref("refs/remotes/upstream/feature/auth"),
            Some("upstream".to_string())
        );
    }

    #[test]
    fn parse_remote_returns_none_for_refs_heads_form() {
        // `refs/heads/<branch>` never names a remote in the string;
        // the caller must look up `branch.<name>.remote`.
        assert_eq!(parse_remote_for_base_ref("refs/heads/main"), None);
        assert_eq!(
            parse_remote_for_base_ref("refs/heads/feature/auth"),
            None
        );
    }

    #[test]
    fn parse_remote_returns_none_for_bare_branch_name() {
        // A bare `main` / `develop` could be either a remote-qualified
        // ref (without the prefix) or a local branch. Without a repo
        // we can't disambiguate, so we return None and the caller
        // falls back to `branch.<current>.remote` or `origin`.
        assert_eq!(parse_remote_for_base_ref("main"), None);
        assert_eq!(parse_remote_for_base_ref("develop"), None);
    }

    #[test]
    fn parse_remote_treats_nested_branch_first_segment_as_remote() {
        // `feature/auth` is ambiguous: a remote `feature` with branch
        // `auth`, or a local branch `feature/auth`. We mirror
        // `parse_local_branch`'s heuristic and assume the first
        // segment is the remote. In practice this is harmless — a
        // `feature` remote almost never exists, so the caller falls
        // through to `SkippedNoRemote` and the spawn proceeds from
        // local HEAD.
        assert_eq!(
            parse_remote_for_base_ref("feature/auth"),
            Some("feature".to_string())
        );
    }

    #[test]
    fn parse_remote_rejects_head_and_empty() {
        assert_eq!(parse_remote_for_base_ref("HEAD"), None);
        assert_eq!(parse_remote_for_base_ref("head"), None);
        assert_eq!(parse_remote_for_base_ref("FETCH_HEAD"), None);
        assert_eq!(parse_remote_for_base_ref(""), None);
        assert_eq!(parse_remote_for_base_ref("   "), None);
    }

    #[test]
    fn parse_remote_trims_whitespace() {
        assert_eq!(
            parse_remote_for_base_ref("  upstream/main  "),
            Some("upstream".to_string())
        );
    }
}
