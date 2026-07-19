//! Captures GitHub PR URLs from PTY output (issue #37).
//!
//! When an agent opens a PR (typically via `gh pr create`), the URL is
//! echoed on its terminal. This module sniffs PTY chunks for the canonical
//! `github.com/<owner>/<repo>/pull/<n>` shape and returns the first match.
//! The captured URL is persisted to `agent_nodes.pr_url` and serves two
//! purposes:
//!
//! 1. **UI surfacing** — the grid header shows a clickable link to the
//!    agent's PR, distinct from the dynamic `useOpenPr` chip (which
//!    reflects the *current* open PR for the branch).
//! 2. **Resume-by-URL fallback** — when an agent restarts after a crash
//!    and its `cli_session_id` is stale, `auto_resume_agent_nodes` can
//!    fall through to fetching the PR's branch + head SHA and resuming
//!    from there. The PR is the durable artifact even when the session
//!    is not.
//!
//! Mirrors `session_capture.rs` — a tiny pure regex extractor so the
//! parsing seam is unit-testable without a live PTY. GitLab / Bitbucket
//! support is tracked as follow-up work; the issue scopes this to GitHub.

use once_cell::sync::Lazy;
use regex::Regex;

/// Capture group: the full `https://github.com/<owner>/<repo>/pull/<n>`
/// URL, optionally with a trailing slash. The character class allows the
/// GitHub URL forms `gh` actually emits:
///
/// * `https://github.com/<owner>/<repo>/pull/<n>` (canonical)
/// * `https://github.com/<owner>/<repo>/pull/<n>/files` (deep links)
/// * `https://github.com/<owner>/<repo>/pull/<n>#issuecomment-...` (anchors)
///
/// Owner / repo / n are constrained to the GitHub character set:
/// `<owner>` and `<repo>` use `[A-Za-z0-9._-]+` (a hyphen is allowed but
/// not at either end — same rule GitHub's own URL bar enforces);
/// `<n>` is digits. No path/query is captured; the regex stops at the
/// first character that's not part of the PR number, which means the
/// caller can substring it out of larger PTY output without worrying
/// about trailing punctuation swallowing the URL.
static GITHUB_PR_URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"https?://(?:www\.)?github\.com/[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?/[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?/pull/[0-9]+",
    )
    .unwrap()
});

/// Attempt to extract a GitHub PR URL from a PTY output chunk. Returns
/// the URL when the chunk contains one (matching the canonical
/// `github.com/<owner>/<repo>/pull/<n>` shape); `None` otherwise.
///
/// The regex is intentionally conservative — false negatives (a real PR
/// URL we missed) are recoverable by the user (just paste it), false
/// positives (a non-PR URL captured as a PR) are not. The character class
/// on owner/repo matches GitHub's own rules; the pull path segment must
/// be digits (so `/pulls/123` or `/pull-request/123` won't match — only
/// the actual PR view URL).
///
/// Trailing punctuation (e.g. a `.` at the end of a sentence) won't be
/// pulled into the capture because the regex requires digits last. If a
/// URL is followed by `/files`, `#anchor`, or `?query`, the match stops
/// at the digit — the caller gets the bare PR view URL, which is what
/// GitHub's "Open PR" chip and the resume-by-URL fallback both want.
pub fn try_extract_pr_url(data: &str) -> Option<&str> {
    GITHUB_PR_URL_RE
        .find(data)
        .map(|m| m.as_str())
}

/// Return the `<owner>/<repo>#<n>` shorthand for a captured URL, used
/// to drive the resume-by-URL fallback path. Returns `None` if the URL
/// doesn't match the expected shape — `None` here means the regex
/// changed shape and the caller should ignore rather than guess.
pub fn parse_pr_components(url: &str) -> Option<PullRequestRef> {
    // Trim trailing punctuation that some shells append to PTY output
    // (`...pull/123.` at end of line). The detector's regex already
    // excludes trailing punctuation from the URL, but defensive parse
    // here keeps callers honest if they pass a non-detector URL.
    let trimmed = url.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '/');
    // Try the canonical `https://` prefix first, then fall back to `http://`
    // (some on-prem GitHub Enterprise installations default to plain HTTP).
    // `or_else` returns the second branch's `Option` only when the first
    // branch is `None` — important: the original buggy form chained two
    // `strip_prefix?` calls, which short-circuited to `None` on the second
    // prefix whenever the first didn't match.
    let after_domain = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))?;
    let (owner_repo, n_str) = after_domain.split_once("/pull/")?;
    let (owner, repo) = owner_repo.split_once('/')?;
    let n: i64 = n_str.parse().ok()?;
    Some(PullRequestRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        number: n,
    })
}

/// The (owner, repo, number) tuple a captured URL resolves to. Used by
/// the resume-by-URL fallback path (`auto_resume_agent_nodes`) and by
/// any future reader that needs to construct an API call from a stored
/// PR URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestRef {
    pub owner: String,
    pub repo: String,
    pub number: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Happy path: the canonical `gh pr create` echo. The output shape
    /// `https://github.com/<owner>/<repo>/pull/<n>\n` is what `gh` actually
    /// prints (issue #37 — what we expect to capture from a real agent).
    #[test]
    fn captures_canonical_gh_pr_create_output() {
        let data = "Opening pull request...\n\
                    https://github.com/alondero/buildmesh/pull/37\n";
        assert_eq!(
            try_extract_pr_url(data),
            Some("https://github.com/alondero/buildmesh/pull/37"),
        );
    }

    /// URLs with trailing path segments (`/files`, `/commits`) should
    /// still match — the regex stops at the digit and returns the bare
    /// PR view URL, which is what the chip and resume fallback want.
    #[test]
    fn captures_pr_url_followed_by_deep_link() {
        let data = "See https://github.com/foo-bar/baz_qux/pull/42/files for the diff.";
        assert_eq!(
            try_extract_pr_url(data),
            Some("https://github.com/foo-bar/baz_qux/pull/42"),
        );
    }

    /// Mixed output (a commit hash, a stack trace, then the PR URL) —
    /// the detector must find the URL regardless of where it sits in
    /// the chunk, matching the way real PTY output interleaves.
    #[test]
    fn captures_url_from_mixed_pty_chunk() {
        let data = "feat: ship pr-url capture\n\
                    \x1b[32m+\x1b[0m src/db/mod.rs\n\
                    \x1b[?25l\r\
                    remote: Create a pull request for 'feature-x' on GitHub by visiting:\n\
                    \x1b[?25hremote:      https://github.com/Acme-Corp/widgets/pull/1234\n";
        assert_eq!(
            try_extract_pr_url(data),
            Some("https://github.com/Acme-Corp/widgets/pull/1234"),
        );
    }

    /// URL with an anchor (`#issuecomment-...`) — same expectation as
    /// `/files` deep links: regex stops at the digit, returns the bare
    /// view URL. A comment anchor shouldn't make the resume path pick
    /// up the wrong PR.
    #[test]
    fn captures_pr_url_with_anchor() {
        let data = "Comment at https://github.com/me/proj/pull/7#issuecomment-12345.";
        assert_eq!(
            try_extract_pr_url(data),
            Some("https://github.com/me/proj/pull/7"),
        );
    }

    /// Negative: non-GitHub URLs (e.g. GitLab, Bitbucket) must NOT match
    /// — the issue scopes this PR to GitHub only, so false positives on
    /// other forges would silently store an unusable URL.
    #[test]
    fn does_not_match_gitlab_or_bitbucket() {
        assert_eq!(try_extract_pr_url("https://gitlab.com/foo/bar/-/merge_requests/42"), None);
        assert_eq!(try_extract_pr_url("https://bitbucket.org/foo/bar/pull-requests/42"), None);
        assert_eq!(try_extract_pr_url("https://example.com/foo/bar/pull/42"), None);
    }

    /// Negative: GitHub URLs that aren't PR view URLs. `/pulls/` (with
    /// the plural) is the issue list — `/pull/<n>` is the PR view. We
    /// only want the PR view, otherwise the resume fallback would fetch
    /// the wrong page.
    #[test]
    fn does_not_match_pulls_list_or_other_github_paths() {
        assert_eq!(try_extract_pr_url("https://github.com/foo/bar/pulls"), None);
        assert_eq!(try_extract_pr_url("https://github.com/foo/bar/pull"), None);
        assert_eq!(try_extract_pr_url("https://github.com/foo/bar/issues/42"), None);
        assert_eq!(try_extract_pr_url("https://github.com/foo/bar/commit/abcdef"), None);
    }

    /// Negative: invalid owner/repo characters (whitespace, slash) must
    /// not match. The character class `[A-Za-z0-9._-]` already excludes
    /// these, but pinning the behaviour prevents a regex simplification
    /// from widening the class.
    #[test]
    fn does_not_match_invalid_owner_or_repo() {
        assert_eq!(try_extract_pr_url("https://github.com/foo bar/baz/pull/1"), None);
        assert_eq!(try_extract_pr_url("https://github.com/foo/bar baz/pull/1"), None);
        assert_eq!(try_extract_pr_url("https://github.com//bar/pull/1"), None);
    }

    /// Empty / non-URL input — must not match and must not panic. The
    /// detector is called on every PTY chunk including binary garbage;
    /// no input is "too weird" to handle.
    #[test]
    fn empty_or_garbage_input_returns_none() {
        assert_eq!(try_extract_pr_url(""), None);
        assert_eq!(try_extract_pr_url("pull request opened"), None);
        assert_eq!(try_extract_pr_url("404 page not found"), None);
        // A trailing slash without a number must not match — captures a
        // pre-`/pull/` segment that doesn't reach the digit class.
        assert_eq!(try_extract_pr_url("https://github.com/foo/bar/pull/"), None);
    }

    /// `parse_pr_components` — happy path. The resume-by-URL fallback
    /// uses this to fetch the PR's head ref + SHA, so a broken parse
    /// would silently strand the fallback path.
    #[test]
    fn parse_pr_components_extracts_owner_repo_number() {
        let parsed = parse_pr_components("https://github.com/alondero/buildmesh/pull/37").unwrap();
        assert_eq!(parsed.owner, "alondero");
        assert_eq!(parsed.repo, "buildmesh");
        assert_eq!(parsed.number, 37);
    }

    /// `parse_pr_components` — trailing punctuation that the detector
    /// never captures but a caller pasting a raw URL might.
    #[test]
    fn parse_pr_components_trims_trailing_punctuation() {
        let parsed = parse_pr_components("https://github.com/foo/bar/pull/42.").unwrap();
        assert_eq!(parsed.number, 42);
    }

    /// `parse_pr_components` — owner / repo segments that aren't valid
    /// must fail closed (return `None`).
    #[test]
    fn parse_pr_components_rejects_invalid_urls() {
        assert_eq!(parse_pr_components("https://gitlab.com/foo/bar/-/merge_requests/42"), None);
        assert_eq!(parse_pr_components("not a url"), None);
        assert_eq!(parse_pr_components(""), None);
    }
}
