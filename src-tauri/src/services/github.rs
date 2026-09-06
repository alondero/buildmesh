//! GitHub REST API service — replaces `gh` CLI calls with direct HTTP requests.

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::process_util::command_no_window;

/// Error type for GitHub API operations
#[derive(Debug)]
pub enum GitHubError {
    NoToken,
    Http(reqwest::Error),
    Api(u16, String),
    /// `POST /repos/{o}/{r}/issues/{n}/labels` rejected the label because
    /// it doesn't exist on the repo (GitHub returns 422 with
    /// `{"message":"Label does not exist"}` in that case). The string is
    /// the label name as the caller passed it so the UI can render a
    /// precise remediation toast ("Label `buildmesh:run` doesn't exist
    /// on the repo — create it on GitHub first."). The endpoint is
    /// POST-only — DELETE on `/labels/{name}` returns 404 for a missing
    /// label, which collapses to a no-op success there.
    LabelNotFound(String),
}

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitHubError::NoToken => write!(f, "No GitHub token found. Set GITHUB_TOKEN env var or authenticate with `gh auth login`."),
            GitHubError::Http(e) => write!(f, "HTTP error: {}", e),
            GitHubError::Api(status, msg) => write!(f, "GitHub API error ({}): {}", status, msg),
            GitHubError::LabelNotFound(label) => write!(f, "Label `{}` doesn't exist on the repo — create it on GitHub first", label),
        }
    }
}

impl std::error::Error for GitHubError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GitHubError::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for GitHubError {
    fn from(e: reqwest::Error) -> Self {
        GitHubError::Http(e)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub number: i64,
    pub title: String,
    /// Issue body (`body` in the GitHub API). GitHub returns `null` for
    /// issues created without a description (e.g. alondero/buildmesh #1210
    /// has `body: null`). `#[serde(default)]` only rescues a *missing* key,
    /// so the field also layers `deserialize_with = "deserialize_opt_string"`
    /// to collapse the `null` value to `""`. Without this, a single
    /// bodyless issue poisons the entire `SearchResult { items }` response
    /// and `get_repo_issues` rejects with
    /// `HTTP error: error decoding response body`. Pinned by
    /// `issue_deserialises_with_null_body_defaults_to_empty` and the
    /// end-to-end `issue_search_result_with_mixed_null_body_items_parses_end_to_end`.
    #[serde(default, deserialize_with = "deserialize_opt_string")]
    pub body: String,
    /// Absolute GitHub URL for the issue (`html_url` in the API response).
    /// The mobile "View ↗" link opens this directly; the desktop modal
    /// currently ignores it. `#[serde(default)]` so a partial response
    /// (older / cached) still parses — the value is then `""`.
    #[serde(default)]
    pub html_url: String,
    /// Issue state — `"open"` or `"closed"`. The list_issues_only endpoint
    /// filters to open today, but we keep the field so the modal can render
    /// a closed chip if a future endpoint widens to both. `#[serde(default)]`
    /// is the safety net for partial responses.
    #[serde(default)]
    pub state: String,
    /// Label names. GitHub's wire format is `[{id, name, color, ...}]` — we
    /// flatten to `Vec<String>` so the wire shape matches the TS type
    /// (`string[]`) one-to-one, eliminating the need for defensive `?? []`
    /// defaults in the mobile screen. Empty when the issue has no labels.
    #[serde(default, deserialize_with = "deserialize_label_names")]
    pub labels: Vec<String>,
    /// GitHub login of the issue's author (`user.login` in the API response).
    /// Captured so Autopilot's collaborator gate (ADR-0012 §5) can check the
    /// author's push access before auto-running a trigger. `#[serde(default)]`
    /// plus the `user.login` projection keeps a partial response parsing — the
    /// value is then `""`, which the gate treats as "unknown → require approval".
    ///
    /// `alias = "user"` is load-bearing: `deserialize_with` keys off the *field*
    /// name, but GitHub sends the author under the `user` key — the alias routes
    /// `user`'s value into this field while keeping the field's own name
    /// `author` for serialisation.
    #[serde(default, alias = "user", deserialize_with = "deserialize_user_login")]
    pub author: String,
}

/// Tolerates both an *absent* key (rescued by `#[serde(default)]` on the
/// field) and a present-but-`null` value. The latter is the gotcha: serde
/// does NOT invoke `Default` when the key is present with a JSON `null`,
/// so a per-field `deserialize_with` is required for any field that may
/// arrive as `null`. `body` is the only such field today — GitHub emits
/// `body: null` on issues opened without a description (issue #1210 in
/// alondero/buildmesh).
fn deserialize_opt_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// Private GitHub wire shape for the `user` object on an issue/PR. We only need
/// the `login`. Either GitHub's `user: { login, … }` object or a bare login
/// string: the object form is what GitHub sends; the bare-string form makes
/// [`deserialize_user_login`] tolerant of `Issue`'s *own* serialised output
/// (where `author` is a plain string), so a serialize→deserialize round-trip of
/// an `Issue` doesn't error on the author field.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawUser {
    Object {
        #[serde(default)]
        login: String,
    },
    Bare(String),
}

/// Project `user: { login, … }` (or a bare login string) → the `login` string at
/// deserialise time, so `Issue.author` is the natural `String` the collaborator
/// gate expects. `#[serde(default)]` on the field means this is only called when
/// the `user` key is present; an absent `user` leaves `author` at its default `""`.
fn deserialize_user_login<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match RawUser::deserialize(deserializer)? {
        RawUser::Object { login } => login,
        RawUser::Bare(login) => login,
    })
}

/// Private GitHub wire shape for a single label entry. The public API only
/// needs the `name`; we discard `id`, `color`, `default`, `description` etc.
#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

/// Flatten `Vec<{id, name, color, ...}>` → `Vec<String>` at deserialise time
/// so the `Issue` struct's `labels` field is the natural `Vec<String>` the
/// rest of the codebase already expects. `#[serde(default)]` on the field
/// means this fn is only called when the key is present; an absent `labels`
/// key leaves the field at its default `vec![]`.
fn deserialize_label_names<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<RawLabel>::deserialize(deserializer).map(|v| v.into_iter().map(|l| l.name).collect())
}

// ---------------------------------------------------------------------------
// Blocked-by body parser
// ---------------------------------------------------------------------------

/// Cap for body-length scanning. GitHub allows up to ~65 KiB per issue
/// body, and real "Blocked by" sections sit near the top — so a 64 KiB
/// cap is comfortably above the noise floor while bounding the regex
/// scanner's internal buffer. Bodies beyond the cap are scanned only up
/// to this point, so a Blocked-by section at the very end of a 65-KiB
/// body would be missed. That's an acceptable trade-off — GitHub's UI
/// renders the section near the top in practice, and the comment in the
/// test module documents the assumption.
const BLOCKED_BY_BODY_CAP: usize = 64 * 1024;

/// Section header — matches either:
///
/// - **Setext-style:** `**Blocked by**` (asterisks optional, case-insensitive)
///   followed by an underline of `-` or `=` characters on the next line.
///   This is the shape GitHub's issue editor emits for `**Blocked by**`.
/// - **ATX-style:** `# Blocked by` (1–6 `#` characters, case-insensitive)
///   followed by one or more newlines. Less common but worth covering.
///
/// Both alternatives share a single lazy capture group `(.*?)` that
/// terminates at the first blank line or end of input. The `(?mis)`
/// flag set enables multi-line matching (`.` matches newlines),
/// case-insensitivity, and `^`/`$` line boundaries.
static BLOCKED_BY_SECTION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?mis)(?:^\*{0,2}\s*Blocked\s+by\s*\*{0,2}\s*\n[-=]{2,}\s*\n|^\#{1,6}\s*Blocked\s+by[^\n]*\n+)(.*?)(?:\n\s*\n|\z)",
    )
    .expect("BLOCKED_BY_SECTION_RE is a static literal — must compile")
});

/// Issue reference — matches either:
///
/// - `/issues/{N}` URLs anywhere in the section (the format the manual
///   issue editor emits),
/// - bare `#NNN` text references at the start of a line, after a
///   bullet marker (`-`, `*`, or `+`) — the format GitHub issue forms
///   / templates auto-render the "Blocked by" field as (real shape of
///   issue #503 in alondero/buildmesh).
///
/// The bare-ref alternative is line-anchored to a bullet marker for two
/// reasons:
///
/// 1. **Avoids narrative false-positives.** A `#NNN` mentioned in
///    prose inside the section ("unblocks once #500 ships") is not a
///    blocker; only bullet items are.
/// 2. **Avoids URL-fragment false-positives.** A `#NNN` mid-URL (e.g.
///    a `/issues/481#issuecomment-12345` permalink) never appears
///    right after a bullet marker.
///
/// Note: the URL alternative does NOT require `(?m)^` line-anchoring —
/// GitHub users sometimes write a bare URL inside a bullet's prose
/// ("See /issues/481 for context") and that should still be picked up.
///
/// `(?m)` enables `^`/`$` line-boundary matching for the bare-ref
/// alternative; the URL alternative works position-by-position so the
/// multi-line flag is harmless to it. The two share a single
/// non-capturing group so `captures_iter` returns matches in source
/// order, and the first alternative only matches `/issues/` (so
/// `/pull/481` is naturally excluded).
static BLOCKED_BY_REF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)(?:/issues/(\d+)\b|^\s*[-*+]\s+#(\d+)\b)")
        .expect("BLOCKED_BY_REF_RE is a static literal — must compile")
});

/// Markdown link — captures the text and URL of a `[text](url)` link.
/// Used to strip link text from the section BEFORE bare-ref matching,
/// so a `#NNN` inside `[title #NNN](url)` doesn't false-positive as a
/// blocker. The URL form of the same link is preserved (replaced with
/// just the URL), so the issue-URL regex still extracts the number.
static MARKDOWN_LINK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\[([^\]]*)\]\(([^)]*)\)")
        .expect("MARKDOWN_LINK_RE is a static literal — must compile")
});

/// Markdown code span — matches a backtick-fenced inline code segment
/// `` `#like this` ``. Stripped before bare-ref matching so a `#NNN`
/// used as a literal identifier, command, or filename inside a code
/// span (very common in issue bodies) is not extracted as a blocker.
/// The strip is greedy on the backticks, so `` ``#500`` `` (two
/// backticks) is also consumed. Newlines inside the span aren't
/// supported (real inline code is single-line).
static MARKDOWN_CODE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"`+[^`\n]*`+")
        .expect("MARKDOWN_CODE_RE is a static literal — must compile")
});

/// Extract the list of GitHub issue numbers referenced under the issue
/// body's `**Blocked by**` section. Returns an empty `Vec` when:
///
/// - the body is empty,
/// - the body has no `**Blocked by**` section,
/// - the section is the literal "None" / "None - ..." (the common
///   no-blockers idiom),
/// - the body is larger than [`BLOCKED_BY_BODY_CAP`] bytes (the section
///   is unreachable in that case — see the cap's doc comment).
///
/// Source order is preserved; duplicates are removed via Vec membership
/// check. The function is purely string-in / vec-out so it's trivially
/// unit-testable without an Issue struct or a fixture.
///
/// Both reference forms are extracted from within the section:
///
/// - `/issues/N` URLs anywhere (the manual issue editor),
/// - bare `#NNN` text references at the start of a bullet line
///   (GitHub issue forms / templates — the real shape of issue
///   #503 in alondero/buildmesh). The bullet-anchor is required
///   to avoid false positives on narrative mentions and on URL
///   fragments like `#issuecomment-NNN`.
///
/// Two preprocessor passes run before ref extraction, in order:
///
/// 1. **Link-strip.** `[text](url)` → `url`. Removes the link's
///    title so a `#NNN` inside `[title #NNN](url)` doesn't
///    false-positive. The URL is preserved (it's what the
///    issue-URL regex matches), so an issue listed via the
///    manual editor's link form is still picked up. PR mentions
///    like `[Related PR #480](.../pull/480)` correctly contribute
///    nothing because the URL form lacks `/issues/`.
/// 2. **Code-span-strip.** `` `#like this` `` → `` ``. Removes
///    backtick-fenced inline code so a `#NNN` used as an
///    identifier, command, or filename inside a code span
///    doesn't false-positive.
///
/// Both passes return `Cow<str>` and borrow from `section` when
/// there's no match, so the common case (no links, no code spans)
/// is allocation-free apart from the `cleaned` join in the
/// short-circuit above.
pub fn parse_blocked_by(body: &str) -> Vec<i64> {
    if body.is_empty() {
        return Vec::new();
    }

    // Bound the scan. Real GitHub bodies can include emoji and CJK
    // characters (each codepoint up to 4 bytes), so the raw byte cap can
    // land mid-codepoint and `&body[..scan_end]` would panic with
    // "byte index is not a char boundary". `floor_char_boundary` is
    // stable on &str since 1.79 and snaps the index down to the nearest
    // valid char boundary, matching the comment's intent.
    let scan_end = body.floor_char_boundary(body.len().min(BLOCKED_BY_BODY_CAP));
    let scan = &body[..scan_end];

    let section = match BLOCKED_BY_SECTION_RE.captures(scan) {
        Some(c) => match c.get(1) {
            Some(m) => m.as_str(),
            None => return Vec::new(),
        },
        None => return Vec::new(),
    };

    // Short-circuit on the "None" idiom. We strip leading bullet markers
    // (`*`, `-`, `+`) and leading/trailing whitespace, then lower-case
    // the result. "None.", "none", "None - can start immediately" all
    // collapse to the same empty/blocker-free signal. Runs BEFORE the
    // link-strip pass so a `[None](url)` link's text is still detected.
    let cleaned: String = section
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.trim_start_matches(['*', '-', '+']).trim())
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.is_empty() || cleaned.to_lowercase().starts_with("none") {
        return Vec::new();
    }

    // Strip markdown link text — `[text](url)` → `url` — so a bare
    // `#NNN` inside a link's title doesn't false-positive as a blocker.
    // The URL form of the same link is preserved (it's what the
    // issue-URL regex matches), so an issue listed via the manual
    // editor's link form is still picked up. replace_all returns a
    // `Cow<str>` that borrows from `section` when there's no match —
    // no allocation in the common case.
    let after_links = MARKDOWN_LINK_RE.replace_all(section, "$2");

    // Strip backtick-fenced code spans — `` `#like this` `` → `` `` —
    // so a `#NNN` used as an identifier/command/filename inside an
    // inline code segment (very common in issue bodies) doesn't
    // false-positive. Chained off `after_links` so the link-strip
    // pass and the code-strip pass are independent of each other's
    // match positions; both return `Cow<str>` that borrows from
    // their input when no match is found, so the no-link-no-code
    // case stays allocation-free apart from the `cleaned` join.
    let stripped = MARKDOWN_CODE_RE.replace_all(after_links.as_ref(), "");

    // Walk the section (with link text and code spans removed) and
    // extract both `/issues/N` URLs and bullet-anchored bare `#NNN`
    // references in source order. `captures_iter` returns matches by
    // position, so a line that contains a link's URL (`/issues/N`) and
    // another line that contains a bare ref (`- #N`) are interleaved
    // in document order rather than batched by form. The dedupe
    // covers both the same-number-twice case (editor copy/paste) and
    // the link-URL-and-link-text-same-number case.
    let mut result: Vec<i64> = Vec::new();
    for cap in BLOCKED_BY_REF_RE.captures_iter(&stripped) {
        // The regex has two alternatives; whichever group matched
        // carries the number we want.
        let m = cap.get(1).or_else(|| cap.get(2));
        if let Some(m) = m {
            if let Ok(n) = m.as_str().parse::<i64>() {
                if !result.contains(&n) {
                    result.push(n);
                }
            }
        }
    }

    result
}

#[derive(Debug, Clone, Serialize)]
pub struct PullRequest {
    pub number: i64,
    pub html_url: String,
    /// Human-readable PR title — surfaced in the chip tooltip.
    #[serde(default)]
    pub title: String,
    /// `true` for draft PRs. GitHub always returns this field on `/pulls` responses.
    #[serde(default)]
    pub draft: bool,
    /// PR description body. `#[serde(default)]` so a partial response (or a PR
    /// opened with no body) parses to `""` rather than failing.
    #[serde(default)]
    pub body: String,
    /// `"open"` or `"closed"`. The list endpoint echoes the `state` filter, but
    /// we keep the field so the PR panel can render a closed chip without a
    /// second lookup. `#[serde(default)]` covers partial responses.
    #[serde(default)]
    pub state: String,
    /// PR's source-branch ref name (e.g. `"feature/some-thing"`). Captured from
    /// the GitHub API's `head.ref` field on both the list and detail endpoints
    /// via the custom `Deserialize` impl below. Empty when the PR is from a
    /// fork and the detail endpoint was the only source of truth — the
    /// fork-spawn path (issue #443) reads `head_repo_owner` + `head_repo_clone_url`
    /// to register the fork as a remote and fetch the head ref from there.
    #[serde(default)]
    pub head_ref: String,
    /// Owner login of the PR's head repo (e.g. `"alice"` for a fork PR opened
    /// from `alice/buildmesh`). Captured from `head.repo.owner.login`. For
    /// same-repo PRs the head's repo is the destination repo, so the value
    /// matches the destination owner. Empty when the field is missing from the
    /// API response. Issue #443 uses this to derive the `fork-<login>` remote
    /// alias when the head repo's owner differs from the destination.
    #[serde(default)]
    pub head_repo_owner: String,
    /// HTTPS clone URL of the PR's head repo (e.g.
    /// `"https://github.com/alice/buildmesh.git"`). Captured from
    /// `head.repo.clone_url`. Issue #443 uses this to register the fork as a
    /// remote when spawning an agent on a fork PR (worktree adoption, #36).
    /// Empty when the field is missing.
    #[serde(default)]
    pub head_repo_clone_url: String,
    /// PR's head commit SHA (e.g. `"0123456789abcdef..."`). Captured from
    /// the GitHub API's `head.sha` field via the custom `Deserialize` impl
    /// below. Used by issue #444's exact-pinning: the spawn path stores this
    /// on the new agent node and verifies the local `origin/<head_ref>` SHA
    /// matches it after `git fetch`. Empty on partial responses and some
    /// fork-PR payloads — same `#[serde(default)]` rationale as `head_ref`.
    #[serde(default)]
    pub head_sha: String,
    /// GitHub login of the PR's author (`user.login`). Captured for Autopilot's
    /// collaborator gate (ADR-0012 §5) — the author of an external PR is the
    /// identity whose push access the gate checks before auto-running. Distinct
    /// from `head_repo_owner`: for a fork PR the author and the fork owner are
    /// usually the same person, but the gate is about *who opened the PR*, which
    /// `user.login` answers directly. Empty when the API omits `user`.
    #[serde(default)]
    pub author: String,
}

// ---------------------------------------------------------------------------
// Custom `Deserialize` for `PullRequest`.
//
// GitHub's `/pulls` response nests the head commit under a `head` object:
//   { "head": { "ref": "feat/420-pr-spawn", ... } }
//
// The struct above exposes the `ref` field as flat top-level `head_ref` (so
// call sites stay branch-free and the Tauri-side wire shape stays flat). The
// derive macro can't do that flattening in one pass because `ref` is a Rust
// keyword and we want `head.ref` → `head_ref`, not nested under a `head` key.
// The custom impl below reads `head` once and projects `head.ref` onto
// `head_ref`, leaving the rest of the struct's `#[serde(default)]` rules in
// place.
// ---------------------------------------------------------------------------
impl<'de> serde::Deserialize<'de> for PullRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct RepoHelper {
            #[serde(default, rename = "clone_url")]
            clone_url: String,
            #[serde(default)]
            owner: Option<OwnerHelper>,
        }
        #[derive(serde::Deserialize)]
        struct OwnerHelper {
            #[serde(default, rename = "login")]
            login: String,
        }
        #[derive(serde::Deserialize)]
        struct HeadHelper {
            #[serde(default, rename = "ref")]
            ref_: String,
            #[serde(default)]
            repo: Option<RepoHelper>,
            // The SHA lives next to `ref` on the same `head` object; we lift
            // it to a top-level `head_sha` for the same reason as `head_ref` —
            // so the spawn path doesn't have to walk a nested struct just to
            // read a string. `#[serde(default)]` keeps partial responses
            // (some fork payloads, older list endpoints) parseable.
            #[serde(default)]
            sha: String,
        }
        #[derive(serde::Deserialize)]
        struct Raw {
            pub number: i64,
            pub html_url: String,
            #[serde(default)]
            pub title: String,
            #[serde(default)]
            pub draft: bool,
            #[serde(default)]
            pub body: String,
            #[serde(default)]
            pub state: String,
            #[serde(default)]
            pub head: Option<HeadHelper>,
            // The PR author lives under a top-level `user` object, the same
            // `{login}` shape as `head.repo.owner`. Lifted to `author` below.
            #[serde(default)]
            pub user: Option<OwnerHelper>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let author = raw.user.map(|u| u.login).unwrap_or_default();
        // Project `head.repo.owner.login` → `head_repo_owner` and
        // `head.repo.clone_url` → `head_repo_clone_url` at deserialise time so
        // the public struct stays flat (the same reason the `head.ref` →
        // `head_ref` projection exists at the top of this file). Issue #443
        // reads both fields on fork PRs to register `fork-<owner>` as a
        // remote and fetch the head ref from there. Both default to "" when
        // the API omits the nested object — the call site treats "" as
        // "same-repo PR" (the #420 origin/<head_ref> branch).
        let head = raw.head;
        let head_ref = head.as_ref().map(|h| h.ref_.clone()).unwrap_or_default();
        // Read `head_sha` from `head.as_ref()` before `head` is moved into the
        // `and_then` below. The struct-init shorthand at the bottom just hands
        // the value through unchanged.
        let head_sha = head.as_ref().map(|h| h.sha.clone()).unwrap_or_default();
        let (head_repo_owner, head_repo_clone_url) = match head.and_then(|h| h.repo) {
            Some(repo) => (
                repo.owner.map(|o| o.login).unwrap_or_default(),
                repo.clone_url,
            ),
            None => (String::new(), String::new()),
        };
        Ok(PullRequest {
            number: raw.number,
            html_url: raw.html_url,
            title: raw.title,
            draft: raw.draft,
            body: raw.body,
            state: raw.state,
            head_ref,
            head_repo_owner,
            head_repo_clone_url,
            head_sha,
            author,
        })
    }
}

/// A single file changed in a pull request — the wire shape of
/// `GET /repos/{o}/{r}/pulls/{n}/files`. The `patch` field is the unified
/// diff text GitHub renders; the frontend parses it line-by-line to colour
/// +/−/context rows (rather than trying to reconstruct our own `DiffHunk`
/// structure, which would be brittle given GitHub's non-standard hunks —
/// missing context lines, inline `rename from`/`rename to`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrFile {
    /// Path of the file at the head of the PR (after any rename).
    pub filename: String,
    /// `"added" | "modified" | "deleted" | "renamed" | "copied" | "changed" | "unchanged"`.
    /// Mirrors the `FileDiffStatus` vocabulary; "renamed" is the only rename
    /// state we care about, "copied" / "changed" / "unchanged" surface as
    /// "modified" on the frontend (the panel isn't a GitHub API mirror).
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub additions: i64,
    #[serde(default)]
    pub deletions: i64,
    /// Unified diff text — empty for binary files (which omit `patch`).
    #[serde(default)]
    pub patch: String,
    /// For renames, the path the file had on the base branch. `None` for
    /// everything else.
    #[serde(default)]
    pub previous_filename: Option<String>,
}

/// A user's push-access level on a repository, as reported by
/// `GET /repos/{owner}/{repo}/collaborators/{username}/permission`. GitHub's
/// `permission` field collapses its granular roles to four legacy values:
/// `maintain` reports as `write` and `triage` as `read`. So `Admin`/`Write`
/// exactly mean "has push access" and `Read`/`None` mean "does not" — which is
/// the trust boundary Autopilot's collaborator gate keys off (ADR-0012 §5).
/// An unrecognised value parses to `None` (conservative: an unknown level is
/// never granted auto-run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollaboratorPermission {
    Admin,
    Write,
    Read,
    None,
}

impl CollaboratorPermission {
    /// Map GitHub's `permission` string to the enum. The legacy field only ever
    /// emits `admin`/`write`/`read`/`none`, but the granular role names
    /// (`maintain`, `triage`) are mapped too so reading `role_name` later needs
    /// no change here. Anything unknown falls to `None`, so a future GitHub
    /// change can only ever *withhold* auto-run, never grant it by accident.
    pub fn from_api_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "admin" => CollaboratorPermission::Admin,
            "write" | "maintain" => CollaboratorPermission::Write,
            "read" | "triage" => CollaboratorPermission::Read,
            _ => CollaboratorPermission::None,
        }
    }

    /// `true` when this level can push to the repo (`Admin` or `Write`).
    pub fn has_push_access(self) -> bool {
        matches!(
            self,
            CollaboratorPermission::Admin | CollaboratorPermission::Write
        )
    }
}

/// Max time to establish a TCP+TLS connection to the GitHub API.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Max time for a whole request (connect + send + receive body). Without a
/// finite bound here, a half-open connection (laptop sleep/resume, dropped
/// Wi-Fi) parks the calling thread *forever* — the probe UI spins endlessly
/// and the thread never frees. The command layer offloads these calls onto
/// the blocking pool (`crate::commands::run_blocking`), so the bound protects
/// a blocking-pool thread rather than a tokio worker; either way an unbounded
/// call is a resource leak — see the overnight-freeze investigation.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-request timeout for **mutating** GitHub calls
/// ([`GitHubClient::create_pull_request`], [`GitHubClient::merge_pull_request`]).
///
/// Read-side calls (issues/PRs listings, repo queries) use the client-level
/// `HTTP_REQUEST_TIMEOUT` (30s) because they're cheap and bounded by GitHub's
/// pagination guarantees. Write-side calls can take *much* longer on a
/// congested network — a `create_pr` that triggers GitHub's CI hook
/// initialisation can take 60–90s on the project's main repo, and `merge_pr`
/// has to wait for any required status checks to clear. A 30s cap there
/// aborts slow-but-progressing writes and forces the user to retry, risking
/// a **duplicate PR** (issue #762). 180s is generous headroom while still
/// bounding "stuck forever" — the underlying TLS read/write half still has
/// the connect-timeout backstop at 10s, so a hard network failure aborts
/// promptly and only legitimate progress extends the window.
///
/// **Idempotency.** Slow-but-progressing writes that time out client-side
/// may have already succeeded server-side. `create_pull_request_idempotent`
/// (issue #771) handles the duplicate-create case by recognising GitHub's
/// 422 ("a pull request already exists") and recovering via
/// `find_open_pr_for_branch` so a retry returns the existing PR's URL
/// rather than a confusing error.
const HTTP_WRITE_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Build the blocking HTTP client with bounded timeouts. Extracted as a seam
/// so the timeout wiring is regression-tested against a never-responding
/// server (`github_client_request_times_out_when_server_never_responds`).
fn build_http_client(
    connect_timeout: Duration,
    request_timeout: Duration,
) -> Result<Client, reqwest::Error> {
    Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .build()
}

/// Default GitHub REST/GraphQL API base. Overridable in tests via
/// `GitHubClient::for_test` (fake-server request-counter tests for issue
/// #1529) — production always uses this value through `GitHubClient::new()`.
const DEFAULT_API_BASE: &str = "https://api.github.com";

/// A lightweight GitHub API client.
pub struct GitHubClient {
    client: Client,
    token: String,
    /// API base without trailing slash (e.g. `https://api.github.com` in
    /// prod, `http://127.0.0.1:<port>` in fake-server tests). All REST paths
    /// are joined onto this; GraphQL appends `/graphql`. Stored (rather
    /// than read from env per-call) so tests can point one client at a fake
    /// server without process-global env races.
    base_url: String,
}

/// Parameters for [`GitHubClient::create_pull_request_idempotent`]. A typed
/// struct rather than positional `&str`s so the six string fields can't be
/// silently transposed (a real bug class for `&str`-heavy APIs).
///
/// `'a` lifetime so callers can pass borrowed `&str`s without an allocation.
#[derive(Debug, Clone, Copy)]
pub struct CreatePrRequest<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub head: &'a str,
    pub base: &'a str,
}

impl GitHubClient {
    /// Create a new client, resolving the token from environment or gh config.
    pub fn new() -> Result<Self, GitHubError> {
        let token = resolve_token()?;
        Self::with_token_and_base(token, DEFAULT_API_BASE)
    }

    /// Build a client from an explicit token + API base. The base is
    /// normalised (trailing `/` trimmed) so `rest_url("/repos/...")` joins
    /// correctly whether the caller passes `...com` or `...com/`.
    pub fn with_token_and_base(token: String, base_url: &str) -> Result<Self, GitHubError> {
        let client = build_http_client(HTTP_CONNECT_TIMEOUT, HTTP_REQUEST_TIMEOUT)
            .map_err(GitHubError::Http)?;
        Ok(Self {
            client,
            token,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Test-only constructor: explicit base URL + token, no env/gh-config
    /// resolution. Used by the issue-#1529 fake-server tests that assert
    /// O(pages) request counts — each test spins its own loopback server
    /// and points one client at it.
    #[cfg(test)]
    pub fn for_test(base_url: &str, token: &str) -> Result<Self, GitHubError> {
        Self::with_token_and_base(token.to_string(), base_url)
    }

    /// Join a REST path (leading `/`) onto the configured base.
    fn rest_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// GraphQL endpoint for the configured base (`<base>/graphql`).
    fn graphql_url(&self) -> String {
        format!("{}/graphql", self.base_url)
    }

    /// Verify the token is valid by calling GET /user.
    pub fn check_auth(&self) -> bool {
        let url = self.rest_url("/user");
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send();

        match resp {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    /// Fetch a user's push-access level on a repo via
    /// `GET /repos/{owner}/{repo}/collaborators/{username}/permission`. A `404`
    /// means the caller can't see the collaborator (e.g. a private repo it lacks
    /// access to) or the user has no association with the repo — both map to
    /// `None` (no push access), so a non-collaborator trigger is *gated* rather
    /// than erroring. Other non-success statuses propagate as `GitHubError::Api`,
    /// mirroring `find_open_pr_for_branch`'s 404-is-a-value handling.
    ///
    /// Seam: the only caller is `autopilot::gate_trigger`, part of the
    /// not-yet-built Autopilot trigger pipeline (issue #499 ships the gate
    /// helpers; the pipeline that drives them is a later slice). `allow(dead_code)`
    /// until that lands — the logic it feeds is covered by the gate's tests.
    #[allow(dead_code)]
    pub fn collaborator_permission(
        &self,
        owner: &str,
        repo: &str,
        username: &str,
    ) -> Result<CollaboratorPermission, GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{owner}/{repo}/collaborators/{username}/permission"
        ));
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(CollaboratorPermission::None);
        }
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        #[derive(Deserialize)]
        struct PermissionResponse {
            #[serde(default)]
            permission: String,
        }
        let parsed: PermissionResponse = resp.json()?;
        Ok(CollaboratorPermission::from_api_str(&parsed.permission))
    }

    /// List open issues (excluding pull requests) for a repository.
    pub fn list_issues_only(&self, owner: &str, repo: &str) -> Result<Vec<Issue>, GitHubError> {
        // Use the search API which lets us filter to only issues (not PRs)
        let url = self.rest_url(&format!(
            "/search/issues?q=repo:{}/{}+is:issue+state:open&per_page=100",
            owner, repo
        ));
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        #[derive(Deserialize)]
        struct SearchResult {
            items: Vec<Issue>,
        }

        let result: SearchResult = resp.json()?;
        Ok(result.items)
    }

    /// List open issues (excluding pull requests) carrying `label`. The
    /// Autopilot poller's ingest query (issue #482): because it always asks
    /// GitHub for the *current* open+labelled set, issues closed or untagged
    /// while the app was offline simply never appear — state reconciliation
    /// falls out of the query shape rather than needing a diff pass.
    ///
    /// The label is quoted in the search qualifier (labels may contain
    /// spaces) and percent-encoded for the URL; embedded `"` are stripped
    /// (GitHub label names can't contain them, and passing one through
    /// would break the qualifier quoting).
    pub fn list_open_issues_with_label(
        &self,
        owner: &str,
        repo: &str,
        label: &str,
    ) -> Result<Vec<Issue>, GitHubError> {
        let query = format!(
            "repo:{}/{} is:issue state:open label:\"{}\"",
            owner,
            repo,
            label.replace('"', "")
        );
        self.search_issues(&query)
    }

    /// Run one `/search/issues` query and parse the `{items: [...]}`
    /// envelope. Shared by the labelled issue/PR ingest queries (issue
    /// #482 / #1208) so the hand-rolled URL encoding lives in exactly
    /// one place: spaces become `+` (GitHub's search-query form),
    /// `"`, `#`, and `&` are percent-encoded.
    fn search_issues<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
    ) -> Result<Vec<T>, GitHubError> {
        let encoded: String = query
            .chars()
            .map(|c| match c {
                ' ' => "+".to_string(),
                '"' => "%22".to_string(),
                '#' => "%23".to_string(),
                '&' => "%26".to_string(),
                other => other.to_string(),
            })
            .collect();
        let url = self.rest_url(&format!(
            "/search/issues?q={}&per_page=100",
            encoded
        ));
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        #[derive(Deserialize)]
        struct SearchResult<T> {
            items: Vec<T>,
        }

        let result: SearchResult<T> = resp.json()?;
        Ok(result.items)
    }

    /// Create a pull request and return GitHub's typed response.
    ///
    /// Low-level primitive — direct `POST /pulls` with no idempotency
    /// recovery. Callers that need to handle the "retry after slow POST
    /// timed out" duplicate-PR case should use
    /// [`create_pull_request_idempotent`](Self::create_pull_request_idempotent)
    /// instead. Kept `pub` (not `pub(crate)`) because future callers may
    /// legitimately want the non-idempotent version (e.g. dry-run tooling
    /// that wants to surface GitHub's raw 422 verbatim).
    pub fn create_pull_request_details(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<PullRequest, GitHubError> {
        let url = self.rest_url(&format!("/repos/{}/{}/pulls", owner, repo));

        #[derive(Serialize)]
        struct CreatePr<'a> {
            title: &'a str,
            body: &'a str,
            head: &'a str,
            base: &'a str,
        }

        let resp = self.client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&CreatePr { title, body, head, base })
            .timeout(HTTP_WRITE_REQUEST_TIMEOUT)
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        resp.json().map_err(GitHubError::from)
    }

    /// Create a pull request, recovering from GitHub's
    /// "a pull request already exists" response on retry (issue #771).
    ///
    /// **Why optimistic, not pessimistic.** A pessimistic "GET first, then
    /// POST" pre-check would add a 200-800ms round trip to *every* create
    /// just to protect against a rare retry-after-timeout race. The
    /// optimistic path keeps the happy case at one POST; only the actual
    /// duplicate-conflict case pays for the recovery GET. This is also
    /// simpler: no "fall-through-on-pre-check-error" branch to design and
    /// test — a pre-check 5xx followed by a POST 422 would surface as a
    /// confusing 422 instead of the real 5xx. With optimistic, the 422
    /// is the single signal we need.
    ///
    /// **What the recovery does.** On `POST /pulls` returning 422 with a
    /// body containing "already exists", call `find_open_pr_for_branch`
    /// to look up the existing PR and return it. Other 422 shapes (e.g.
    /// "head branch does not exist") propagate as the typed
    /// `GitHubError::Api` so the caller can distinguish the duplicate case
    /// from missing-branch / permission errors.
    ///
    /// **Caller side.** The command layer converts the returned `PullRequest`
    /// to its `html_url` for the frontend. When the recovery path fires
    /// (i.e. we return the existing PR rather than a freshly-created one)
    /// the user-supplied `title` and `body` are discarded — that's the
    /// point of the recovery, but the command layer logs a `tracing::warn!`
    /// so an audit trail exists for "why didn't my title apply".
    pub fn create_pull_request_idempotent(
        &self,
        req: CreatePrRequest<'_>,
    ) -> Result<PullRequest, GitHubError> {
        let CreatePrRequest { owner, repo, title, body, head, base } = req;
        match self.create_pull_request_details(owner, repo, title, body, head, base) {
            Ok(pr) => Ok(pr),
            // GitHub's "duplicate PR" 422 has the form
            //   { "message": "Validation Failed",
            //     "errors": [{"message": "A pull request already exists for <owner>:<head>."}] }
            // The body string is the cheapest reliable detector — no schema
            // version drift to worry about, and the recovery path stays a
            // single match arm.
            Err(GitHubError::Api(422, ref_body))
                if ref_body.contains("already exists") =>
            {
                tracing::warn!(
                    "POST /pulls returned 422 'already exists' for {owner}/{repo} head={head} — recovering via find_open_pr_for_branch; user-supplied title/body discarded"
                );
                match self.find_open_pr_for_branch(owner, repo, head)? {
                    Some(existing) => Ok(existing),
                    // Pathological: 422 said "exists" but the follow-up
                    // GET returns nothing. Almost certainly a permission
                    // scope mismatch (the POST scope sees the PR; the GET
                    // scope doesn't). Surface as 422 with the original body
                    // so the frontend toasts the real diagnostic.
                    None => Err(GitHubError::Api(422, ref_body.clone())),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Find the first open pull request whose `head.ref` matches `branch`.
    /// Returns `Ok(None)` when the repository or branch is unknown to GitHub
    /// (treated as "no PR" — common for never-pushed branches). Other
    /// non-success statuses propagate as `GitHubError::Api`.
    pub fn find_open_pr_for_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Option<PullRequest>, GitHubError> {
        // GitHub's `head=OWNER:BRANCH` filter matches the head ref of a PR.
        // The `state=open` filter is the only thing we care about; `per_page=1`
        // is the invariant: one branch → at most one open PR.
        //
        // The `head` value goes in the query string, so `owner:branch` MUST
        // be percent-encoded — `:` and `/` (in branch names like `feat/foo`)
        // would otherwise corrupt the URL, and `#` / `?` / `&` (rare but
        // legal in ref names) would silently break the query parsing. The
        // encoder is the same `percent_encode_path_component` used for label
        // paths; per RFC 3986 the unreserved set is identical for both path
        // components and query values, and "encode everything else" is
        // correct for both.
        let head_param = GitHubClient::percent_encode_path_component(&format!("{owner}:{branch}"));
        let url = self.rest_url(&format!(
            "/repos/{owner}/{repo}/pulls?head={head_param}&state=open&per_page=1"
        ));
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        // 404 is "no such repo OR no such branch on this repo" — both mean "no PR".
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        let prs: Vec<PullRequest> = resp.json()?;
        Ok(prs.into_iter().next())
    }

    /// Has this pull request been merged? Uses `GET /pulls/{n}/merge`, which
    /// answers with a bare status: `204` = merged, `404` = not merged (or
    /// closed without merging). Cheaper and less ambiguous than fetching the
    /// full PR detail and combining `state` + `merged_at`.
    pub fn pull_request_merged(
        &self,
        owner: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<bool, GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{}/{}/pulls/{}/merge",
            owner, repo, pr_number
        ));
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(true);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let body = resp.text().unwrap_or_default();
        Err(GitHubError::Api(status.as_u16(), body))
    }

    /// Fetch a single PR's mergeability via the detail endpoint. The list
    /// endpoint omits `mergeable`/`mergeable_state`; only `GET /pulls/{n}`
    /// carries them. `mergeable` is `null` while GitHub is still computing
    /// the merge — we surface that as `None` rather than coercing to `false`,
    /// so the UI can show a "checking" state and the user isn't told a
    /// freshly-opened PR has conflicts when it doesn't.
    pub fn pull_request_mergeability(
        &self,
        owner: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<(Option<bool>, String), GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{}/{}/pulls/{}",
            owner, repo, pr_number
        ));
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        #[derive(Deserialize)]
        struct Detail {
            #[serde(default)]
            mergeable: Option<bool>,
            #[serde(default)]
            mergeable_state: String,
        }

        let detail: Detail = resp.json()?;
        Ok((detail.mergeable, detail.mergeable_state))
    }

    /// List the files changed in a single pull request.
    /// (`GET /repos/{o}/{r}/pulls/{n}/files`.) Backed by the per-PR files
    /// endpoint rather than `/compare/{base}...{head}` so we don't have to
    /// know the head ref or fall back to a `git fetch` if the branch isn't
    /// local — the PR number is the only key the panel needs.
    pub fn list_pr_files(
        &self,
        owner: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<Vec<PrFile>, GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{}/{}/pulls/{}/files?per_page=100",
            owner, repo, pr_number
        ));
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        // The endpoint returns a bare array, NOT a `{files: [...]}` wrapper.
        let files: Vec<PrFile> = resp.json()?;
        Ok(files)
    }

    /// Merge a pull request via squash and delete the branch.
    pub fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<String, GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{}/{}/pulls/{}/merge",
            owner, repo, pr_number
        ));

        #[derive(Serialize)]
        struct MergePr {
            merge_method: &'static str,
        }

        let resp = self.client
            .put(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&MergePr { merge_method: "squash" })
            .timeout(HTTP_WRITE_REQUEST_TIMEOUT)
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        #[derive(Deserialize)]
        struct MergeResult {
            #[serde(default)]
            message: String,
            sha: String,
        }

        let result: MergeResult = resp.json()?;

        // Now delete the branch. First, get the PR to find the head ref.
        let pr_url = self.rest_url(&format!(
            "/repos/{}/{}/pulls/{}",
            owner, repo, pr_number
        ));
        // Post-merge read: the merge already succeeded, so this GET is
        // best-effort. The 30s default would otherwise abort the function
        // with `Err` even though GitHub confirms the merge — use the write
        // timeout so a slow followup can't undo a successful merge in the
        // caller's view (issue #762 review).
        let pr_resp = self.client
            .get(&pr_url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .timeout(HTTP_WRITE_REQUEST_TIMEOUT)
            .send()?;

        if pr_resp.status().is_success() {
            #[derive(Deserialize)]
            struct PrHead {
                #[serde(rename = "ref")]
                ref_name: String,
            }
            #[derive(Deserialize)]
            struct PrDetail {
                head: PrHead,
            }
            if let Ok(detail) = pr_resp.json::<PrDetail>() {
                let delete_url = self.rest_url(&format!(
                    "/repos/{}/{}/git/refs/heads/{}",
                    owner, repo, detail.head.ref_name
                ));
                // Best-effort branch deletion; ignore errors.
                let _ = self.client
                    .delete(&delete_url)
                    .header(AUTHORIZATION, format!("Bearer {}", self.token))
                    .header(USER_AGENT, "buildmesh")
                    .header(ACCEPT, "application/vnd.github+json")
                    .send();
            }
        }

        Ok(format!("Merged (squash) via {} — {}", result.sha, result.message))
    }

    /// Percent-encode a label name for safe inclusion in a URL path component.
    /// GitHub label names can contain `:`, `/`, spaces (rare), and other
    /// characters that are not safe in a path segment. Per RFC 3986, the
    /// unreserved set is `A-Z a-z 0-9 - _ . ~`; everything else must be
    /// percent-encoded. We keep the encoder inline (no `urlencoding` crate
    /// dependency) so the file's "no extra deps for trivial work" ethos
    /// holds — the call sites are one DELETE path and one POST body.
    fn percent_encode_path_component(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            // RFC 3986 unreserved: ALPHA / DIGIT / "-" / "_" / "." / "~"
            let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
            if unreserved {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{:02X}", b));
            }
        }
        out
    }

    /// Add a label to an issue. Idempotent: GitHub returns 200 with the
    /// updated label list when the label is already present, and 422 when
    /// the label doesn't exist on the repo (mapped to
    /// [`GitHubError::LabelNotFound`] so the UI can toast "create the
    /// label on GitHub first"). Backs the Issues Probe's trigger-label
    /// toggle (issue #979). Uses the default read-side timeout (30s) —
    /// label writes are fast and a 422 mapping is more useful than a
    /// long-tail retry window.
    ///
    /// Wire shape: `POST /repos/{o}/{r}/issues/{n}/labels` with a
    /// `{"labels":[name]}` body. The endpoint accepts multiple labels in
    /// one call but we send a single-element array to keep the contract
    /// 1:1 with the toggle UI.
    pub fn add_issue_label(
        &self,
        owner: &str,
        repo: &str,
        issue_number: i64,
        label: &str,
    ) -> Result<(), GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{}/{}/issues/{}/labels",
            owner, repo, issue_number
        ));

        #[derive(Serialize)]
        struct AddLabels<'a> {
            labels: Vec<&'a str>,
        }

        let resp = self.client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&AddLabels { labels: vec![label] })
            .send()?;

        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        Self::classify_add_label_response(status, &body, label)
    }

    /// Classify a `POST /repos/{o}/{r}/issues/{n}/labels` response into
    /// either `Ok(())`, [`GitHubError::LabelNotFound`], or a generic
    /// [`GitHubError::Api`]. Extracted so the 422 → `LabelNotFound`
    /// mapping is unit-testable without standing up an HTTP server — the
    /// mapping is the load-bearing piece of the Issues Probe's error
    /// UX (issue #979 decision #4 / ticket #980 acceptance "422 from
    /// GitHub (label doesn't exist on repo) → toast: 'Label `X` doesn't
    /// exist on the repo — create it on GitHub first.'").
    ///
    /// Rules:
    /// - 422 with a body containing `"Label does not exist"` →
    ///   `LabelNotFound` (the documented GitHub error shape for this case).
    /// - 422 with an empty body → `LabelNotFound` (defensive: a partial /
    ///   truncated response that GitHub nonetheless classifies as 422 most
    ///   plausibly came from the same code path, and treating it as
    ///   `Api(422, "")` would surface a useless empty error message in
    ///   the toast).
    /// - 422 with a different body → `Api(422, body)` (preserves the raw
    ///   text for diagnostics on the rare other-422 path).
    /// - Any other non-success → `Api(status, body)`.
    /// - Success → `Ok(())`.
    fn classify_add_label_response(
        status: reqwest::StatusCode,
        body: &str,
        label: &str,
    ) -> Result<(), GitHubError> {
        if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            if body.contains("Label does not exist") || body.is_empty() {
                return Err(GitHubError::LabelNotFound(label.to_string()));
            }
            return Err(GitHubError::Api(status.as_u16(), body.to_string()));
        }
        if !status.is_success() {
            return Err(GitHubError::Api(status.as_u16(), body.to_string()));
        }
        Ok(())
    }

    /// Remove a label from an issue. Idempotent on a missing label: GitHub
    /// returns 404 for "label not on this issue", which we collapse to
    /// `Ok(())` so the toggle can be retried freely without surfacing a
    /// stale "label wasn't there" error. The endpoint is
    /// `DELETE /repos/{o}/{r}/issues/{n}/labels/{name}` and the label
    /// name goes in the URL path, so we percent-encode it for safety
    /// (labels commonly contain `:`, `/`, etc.).
    ///
    /// Backs the Issues Probe's trigger-label toggle (issue #979).
    pub fn remove_issue_label(
        &self,
        owner: &str,
        repo: &str,
        issue_number: i64,
        label: &str,
    ) -> Result<(), GitHubError> {
        let encoded = Self::percent_encode_path_component(label);
        let url = self.rest_url(&format!(
            "/repos/{}/{}/issues/{}/labels/{}",
            owner, repo, issue_number, encoded
        ));

        let resp = self.client
            .delete(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        // 404 covers two cases — label not on this issue, OR label
        // doesn't exist on the repo at all. Both are "label isn't
        // present, which is the state the caller wanted" → idempotent
        // success.
        if status == reqwest::StatusCode::NOT_FOUND {
            let _ = resp.bytes();
            return Ok(());
        }
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        // 204 No Content is the documented success body. Drain either way
        // so the connection can be reused.
        let _ = resp.bytes();
        Ok(())
    }

    /// Post a comment on an issue or PR (`POST /repos/{o}/{r}/issues/{n}/comments`
    /// — the issues endpoint covers both, which is why the circuit engine's
    /// PostComment action needs no PR-specific call). Backs the circuit
    /// GithubAction vocabulary (issue #1208). Uses the read-side timeout:
    /// comment writes are fast and idempotent to retry by re-triggering.
    pub fn add_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: i64,
        body: &str,
    ) -> Result<(), GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{}/{}/issues/{}/comments",
            owner, repo, issue_number
        ));

        #[derive(Serialize)]
        struct Comment<'a> {
            body: &'a str,
        }

        let resp = self.client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&Comment { body })
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }
        let _ = resp.bytes();
        Ok(())
    }

    /// Close an issue (`PATCH /repos/{o}/{r}/issues/{n}` with
    /// `{"state": "closed"}`). Idempotent on an already-closed issue —
    /// GitHub answers 200 either way. Backs the circuit GithubAction
    /// vocabulary (issue #1208).
    pub fn close_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: i64,
    ) -> Result<(), GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{}/{}/issues/{}",
            owner, repo, issue_number
        ));

        #[derive(Serialize)]
        struct CloseState {
            state: &'static str,
        }

        let resp = self.client
            .patch(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&CloseState { state: "closed" })
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }
        let _ = resp.bytes();
        Ok(())
    }

    /// List open pull requests carrying `label` via the search API
    /// (`is:pr` instead of `is:issue`). The circuit GitHub-poll pass's
    /// PR-trigger ingest query (issue #1208): same reconciliation shape
    /// as [`Self::list_open_issues_with_label`] — the query returns the
    /// current open+labelled set, so a PR closed or untagged while the
    /// app was offline never appears.
    pub fn list_open_pull_requests_with_label(
        &self,
        owner: &str,
        repo: &str,
        label: &str,
    ) -> Result<Vec<PullRequest>, GitHubError> {
        let query = format!(
            "repo:{}/{} is:pr state:open label:\"{}\"",
            owner,
            repo,
            label.replace('"', "")
        );
        // Search results carry the issue-shaped wire form for PRs too;
        // PullRequest's custom Deserialize already tolerates it (the
        // head object is optional with serde defaults).
        self.search_issues(&query)
    }

    /// Fetch one page of PR summaries via GitHub's GraphQL connection.
    ///
    /// A single page carries up to `first` PRs with their mergeability
    /// (`mergeable` + `mergeStateStatus`) inline — the REST list endpoint
    /// omits both, which is what forced the old N+1 detail loop. The
    /// caller ([`Self::list_pr_summaries`]) pages `after` until
    /// `hasNextPage` is false or the list cap is reached, so the total
    /// HTTP cost is O(pages), not O(PRs).
    fn fetch_pr_summaries_page(
        &self,
        owner: &str,
        repo: &str,
        states: &[&str],
        first: i64,
        after: Option<&str>,
    ) -> Result<(Vec<PullRequestSummary>, PageInfo), GitHubError> {
        #[derive(Serialize)]
        struct Variables<'a> {
            owner: &'a str,
            name: &'a str,
            states: Vec<&'a str>,
            first: i64,
            after: Option<&'a str>,
        }
        #[derive(Serialize)]
        struct GraphQLRequest<'a> {
            query: &'a str,
            variables: Variables<'a>,
        }
        let body = GraphQLRequest {
            query: PR_SUMMARIES_QUERY,
            variables: Variables {
                owner,
                name: repo,
                states: states.to_vec(),
                first,
                after,
            },
        };
        let url = self.graphql_url();
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&body)
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), text));
        }

        let parsed: GraphQLResponse = resp.json().map_err(GitHubError::Http)?;
        // Join a non-empty `errors` array once — it is the error below
        // whenever there is no usable repository to read.
        let joined_errors: Option<String> = parsed.errors.as_ref().and_then(|errors| {
            if errors.is_empty() {
                None
            } else {
                Some(
                    errors
                        .iter()
                        .map(|e| e.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            }
        });
        let data = parsed.data.ok_or_else(|| {
            GitHubError::Api(
                status.as_u16(),
                joined_errors
                    .clone()
                    .unwrap_or_else(|| "GitHub GraphQL returned no data".to_string()),
            )
        })?;
        match (data.repository, joined_errors) {
            (Some(repo_data), errors) => {
                // Partial data with errors: keep the usable rows and log the
                // rest. A null node inside `nodes` is skipped per-row below,
                // so one bad PR never fails its page.
                if let Some(msg) = errors {
                    tracing::warn!("GitHub GraphQL partial errors: {}", msg);
                }
                let conn = repo_data.pull_requests;
                let page_info = conn.page_info;
                let mut out = Vec::with_capacity(conn.nodes.len());
                for node in conn.nodes.into_iter().flatten() {
                    out.push(PullRequestSummary::from_graphql_node(node));
                }
                Ok((out, page_info))
            }
            // `repository: null` WITH errors is the error itself (rate limit,
            // SAML enforcement, missing permission) — never a 404. Only an
            // error-free null repository means "no such repo".
            (None, Some(msg)) => Err(GitHubError::Api(status.as_u16(), msg)),
            (None, None) => Err(GitHubError::Api(
                404,
                format!("repository {}/{} not found", owner, repo),
            )),
        }
    }
}

/// One page-cursor for the PR-summaries connection.
#[derive(Debug, Clone, Default, Deserialize)]
struct PageInfo {
    #[serde(default)]
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(default)]
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

/// Cohesive PR summary: the REST `/pulls` list fields PLUS mergeability.
///
/// Returned by [`GitHubClient::list_pr_summaries`] in O(pages) GraphQL
/// requests. The UI consumes this single shape and never orchestrates
/// per-row enrichment calls.
#[derive(Debug, Clone)]
pub struct PullRequestSummary {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub html_url: String,
    /// `"open"` or `"closed"` (GraphQL `MERGED` maps to `"closed"` to match
    /// the REST list's vocabulary and the frontend's `StateFilter`).
    pub state: String,
    pub draft: bool,
    pub head_ref: String,
    pub head_repo_owner: String,
    pub head_repo_clone_url: String,
    pub head_sha: String,
    /// `Some(true)` mergeable, `Some(false)` conflicting, `None` while
    /// GitHub is still computing (`UNKNOWN`) — mirrors the REST detail's
    /// `mergeable: null` contract so the panel's "checking" state is
    /// preserved without a second request.
    pub mergeable: Option<bool>,
    /// Lowercase REST vocabulary (`clean`, `dirty`, `blocked`, `behind`,
    /// `unstable`, `unknown`, …) mapped from GraphQL `mergeStateStatus`.
    pub mergeable_state: String,
}

/// Cohesive PR-summary query (issue #1529).
///
/// Cost is proportional to pages, not PR count: one GraphQL connection
/// request per page of up to 100 PRs. The UI calls this through
/// `get_repo_pulls` and never issues per-PR detail requests.
impl GitHubClient {
    pub fn list_pr_summaries(
        &self,
        owner: &str,
        repo: &str,
        state: &str,
    ) -> Result<Vec<PullRequestSummary>, GitHubError> {
        let states = graphql_states_for_filter(state);
        let mut all: Vec<PullRequestSummary> = Vec::new();
        let mut after: Option<String> = None;
        // Bound the page walk: a well-behaved server ends it via
        // `has_next_page == false` (or the cap, first page today), but a
        // buggy cursor that repeats forever must not park this blocking-pool
        // thread — same "bound everything" ethos as the HTTP timeouts above.
        let mut pages_fetched: usize = 0;
        loop {
            pages_fetched += 1;
            if pages_fetched > PR_SUMMARY_MAX_PAGES {
                tracing::warn!(
                    "list_pr_summaries: stopped after {} pages for {}/{} — cursor did not terminate",
                    PR_SUMMARY_MAX_PAGES,
                    owner,
                    repo
                );
                break;
            }
            let (mut page, info) = self.fetch_pr_summaries_page(
                owner,
                repo,
                &states,
                PR_SUMMARY_PAGE_SIZE,
                after.as_deref(),
            )?;
            all.append(&mut page);
            // Preserve the REST list's 100-row cap: one page already covers
            // it, so a second request only fires if the cap is raised later.
            if all.len() >= PR_SUMMARY_CAP || !info.has_next_page {
                break;
            }
            after = info.end_cursor;
            if after.is_none() {
                break;
            }
        }
        all.truncate(PR_SUMMARY_CAP);
        Ok(all)
    }
}

/// Page size for the GraphQL PR-summaries connection. Matches the REST
/// list's `per_page=100` so the current 100-row behaviour costs exactly
/// one HTTP request.
const PR_SUMMARY_PAGE_SIZE: i64 = 100;
/// List cap preserved from the REST `per_page=100` behaviour.
const PR_SUMMARY_CAP: usize = 100;
/// Hard ceiling on pages per `list_pr_summaries` call. With the 100-row cap
/// the walk ends on page 1 today; the ceiling only binds a misbehaving
/// cursor (e.g. a repeated `endCursor` with `hasNextPage: true`) so one
/// refresh can never issue more than this many requests.
const PR_SUMMARY_MAX_PAGES: usize = 10;

/// Map the panel's `state` filter to GraphQL `PullRequestState` values.
/// REST `state=closed` includes merged PRs, so the GraphQL side must ask
/// for both `CLOSED` and `MERGED`; anything that isn't an explicit
/// `"closed"` falls back to `OPEN` (mirrors `get_repo_pulls_blocking`'s
/// normalisation so an arbitrary string can't reach the API).
fn graphql_states_for_filter(state: &str) -> Vec<&'static str> {
    if state == "closed" {
        vec!["CLOSED", "MERGED"]
    } else {
        vec!["OPEN"]
    }
}

/// Map GraphQL `mergeable` (`MERGEABLE` / `CONFLICTING` / `UNKNOWN`) to the
/// REST detail's `Option<bool>` contract. `UNKNOWN` means GitHub is still
/// computing — surfaced as `None` so the UI shows its distinct
/// checking/unknown state rather than falsely claiming conflicts. Any
/// unrecognised future value is conservative `None` (unknown), never a
/// false `Some(false)`.
fn map_graphql_mergeable(s: &str) -> Option<bool> {
    match s {
        "MERGEABLE" => Some(true),
        "CONFLICTING" => Some(false),
        _ => None,
    }
}

/// Map GraphQL `mergeStateStatus` (`CLEAN`, `DIRTY`, `BLOCKED`, `BEHIND`,
/// `UNSTABLE`, `UNKNOWN`, `DRAFT`, `HAS_HOOKS`) to the lowercase REST
/// `mergeable_state` vocabulary the panel already renders. Lowercasing is
/// the whole mapping (`HAS_HOOKS` → `has_hooks`); unknown future values
/// lowercase through unchanged so they render via the panel's fallback
/// wording instead of failing the page.
fn map_graphql_merge_state(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Map GraphQL PR `state` (`OPEN` / `CLOSED` / `MERGED`) to the REST list's
/// `"open"` / `"closed"` vocabulary. `MERGED` maps to `"closed"` because
/// the REST list reports merged PRs as closed and the frontend's
/// `StateFilter` only knows those two values.
fn map_graphql_state(s: &str) -> String {
    match s {
        "OPEN" => "open".to_string(),
        "CLOSED" => "closed".to_string(),
        "MERGED" => "closed".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

/// GraphQL connection query for one page of PR summaries. Fetches the list
/// fields the panel renders plus `mergeable` / `mergeStateStatus` inline —
/// the two fields that previously required one REST detail request per PR.
/// `orderBy: CREATED_AT DESC` mirrors the REST list's newest-first order.
const PR_SUMMARIES_QUERY: &str = r#"
query PrSummaries($owner: String!, $name: String!, $states: [PullRequestState!], $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: $first, after: $after, states: $states, orderBy: {field: CREATED_AT, direction: DESC}) {
      nodes {
        number
        title
        body
        url
        state
        isDraft
        headRefName
        headRefOid
        headRepository {
          owner { login }
          url
        }
        mergeable
        mergeStateStatus
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

#[derive(Debug, Deserialize)]
struct GraphQLResponse {
    #[serde(default)]
    data: Option<GraphQLData>,
    #[serde(default)]
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct GraphQLData {
    #[serde(default)]
    repository: Option<GraphQLRepository>,
}

#[derive(Debug, Deserialize)]
struct GraphQLRepository {
    #[serde(default, rename = "pullRequests")]
    pull_requests: GraphQLConnection,
}

#[derive(Debug, Default, Deserialize)]
struct GraphQLConnection {
    #[serde(default)]
    nodes: Vec<Option<GraphQLPrNode>>,
    #[serde(default, rename = "pageInfo")]
    page_info: PageInfo,
}

/// One PR node from the summaries connection. Every field GitHub may omit
/// (deleted fork, missing body, still-computing merge) is `Option` or
/// `#[serde(default)]` so a partial node degrades to empty/unknown rather
/// than failing the whole page (issue #1529 partial-data requirement).
#[derive(Debug, Deserialize)]
struct GraphQLPrNode {
    number: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default, rename = "isDraft")]
    is_draft: Option<bool>,
    #[serde(default, rename = "headRefName")]
    head_ref_name: Option<String>,
    #[serde(default, rename = "headRefOid")]
    head_ref_oid: Option<String>,
    #[serde(default, rename = "headRepository")]
    head_repository: Option<GraphQLHeadRepo>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default, rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphQLHeadRepo {
    #[serde(default)]
    owner: Option<GraphQLOwner>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphQLOwner {
    #[serde(default)]
    login: Option<String>,
}

impl PullRequestSummary {
    /// Map one GraphQL node onto the cohesive summary. Null/missing fields
    /// degrade to the same empty/unknown sentinels the REST mapper uses so
    /// a partial node never fails its page.
    fn from_graphql_node(node: GraphQLPrNode) -> Self {
        let (head_owner, head_clone_url) = match node.head_repository {
            Some(repo) => {
                let owner = repo
                    .owner
                    .and_then(|o| o.login)
                    .unwrap_or_default();
                let clone_url = repo.url.map(|u| format!("{}.git", u)).unwrap_or_default();
                (owner, clone_url)
            }
            None => (String::new(), String::new()),
        };
        Self {
            number: node.number,
            title: node.title.unwrap_or_default(),
            body: node.body.unwrap_or_default(),
            html_url: node.url.unwrap_or_default(),
            state: node.state.map(|s| map_graphql_state(&s)).unwrap_or_else(|| "open".to_string()),
            draft: node.is_draft.unwrap_or(false),
            head_ref: node.head_ref_name.unwrap_or_default(),
            head_repo_owner: head_owner,
            head_repo_clone_url: head_clone_url,
            head_sha: node.head_ref_oid.unwrap_or_default(),
            mergeable: node.mergeable.as_deref().map(map_graphql_mergeable).unwrap_or(None),
            mergeable_state: node
                .merge_state_status
                .as_deref()
                .map(map_graphql_merge_state)
                .unwrap_or_else(|| "unknown".to_string()),
        }
    }
}

/// Resolve a GitHub token from environment or gh CLI config.
fn resolve_token() -> Result<String, GitHubError> {
    // 1. Try GITHUB_TOKEN env var
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // 2. Try GH_TOKEN env var (gh CLI also respects this)
    if let Ok(token) = std::env::var("GH_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // 3. Try to read from gh CLI config file
    if let Some(token) = read_gh_config_token() {
        return Ok(token);
    }

    // 4. Fall back to `gh auth token` which retrieves from secure storage (keyring/credential manager)
    if let Some(token) = run_gh_auth_token() {
        return Ok(token);
    }

    Err(GitHubError::NoToken)
}

/// Wall-clock timeout for the `gh auth token` shell-out. The CLI typically
/// returns in <100ms (it reads from keyring/credential manager on disk);
/// 5s is generous headroom for a slow disk while still bounding
/// "filesystem hung" → resource leak on the blocking pool (issue #762).
const GH_AUTH_TOKEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Retrieve token via `gh auth token` (works when token is in secure storage).
///
/// **Timeout (issue #762):** the previous implementation called
/// `Command::output()` with no bound. If the `gh` subprocess hung (waiting
/// on a stuck keyring prompt, paused WSL interop, etc.) the calling
/// blocking-pool thread leaked indefinitely. The `GH_AUTH_TOKEN_TIMEOUT`
/// bound kills the child and returns `None` so the caller falls through
/// to its `Err(GitHubError::NoToken)` error path — same observable
/// behaviour as a missing-token user, which the UI already handles.
fn run_gh_auth_token() -> Option<String> {
    let mut cmd = command_no_window("gh");
    cmd.args(["auth", "token"]);
    let output = crate::process_util::run_command_with_timeout(
        cmd,
        "gh auth token",
        GH_AUTH_TOKEN_TIMEOUT,
    )
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let token = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Read the oauth_token from gh CLI's hosts.yml config file.
fn read_gh_config_token() -> Option<String> {
    let config_paths = gh_config_paths();

    for path in config_paths {
        if let Ok(content) = std::fs::read_to_string(&path) {
            // Parse the YAML manually (avoid adding a full YAML crate dependency).
            // The format is:
            // github.com:
            //     oauth_token: gho_XXXX
            //     ...
            // or the newer format:
            // github.com:
            //     user: ...
            //     oauth_token: gho_XXXX
            if let Some(token) = parse_gh_hosts_yaml(&content) {
                return Some(token);
            }
        }
    }
    None
}

/// Get candidate paths for gh CLI hosts.yml.
fn gh_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // XDG / standard config dir
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg).join("gh").join("hosts.yml"));
    }

    // HOME-based (macOS / Linux)
    if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(&home).join(".config").join("gh").join("hosts.yml"));
    }

    // Windows: %APPDATA%
    if let Ok(appdata) = std::env::var("APPDATA") {
        paths.push(PathBuf::from(appdata).join("GitHub CLI").join("hosts.yml"));
    }

    paths
}

/// Parse the oauth_token for github.com from gh's hosts.yml content.
/// Handles both old format (oauth_token as direct field) and the simple YAML structure.
fn parse_gh_hosts_yaml(content: &str) -> Option<String> {
    let mut in_github_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Check for the github.com section header
        if trimmed == "github.com:" || trimmed == "\"github.com\":" {
            in_github_section = true;
            continue;
        }

        // If we hit another top-level key (not indented), exit the section
        if in_github_section && !line.starts_with(' ') && !line.starts_with('\t') && !trimmed.is_empty() {
            break;
        }

        if in_github_section {
            // Look for oauth_token field
            if let Some(rest) = trimmed.strip_prefix("oauth_token:") {
                let token = rest.trim().trim_matches('"').trim_matches('\'');
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// Parse owner/repo from a GitHub remote URL.
/// Handles both HTTPS (https://github.com/owner/repo) and SSH (git@github.com:owner/repo) formats.
pub fn parse_owner_repo(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))?;

    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        let repo = parts[1].trim_end_matches(".git");
        Some((parts[0].to_string(), repo.to_string()))
    } else {
        None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Regression for the overnight-freeze bug: a GitHub probe against a
    /// server that accepts the TCP connection but never sends a response (a
    /// half-open connection after laptop sleep / dropped Wi-Fi) must *error
    /// out*, not hang forever. A hung blocking request parks a Tauri tokio
    /// worker permanently; enough of them starve the pool and every async
    /// command (agent keystrokes, other probes) stops responding while the
    /// UI stays alive.
    ///
    /// The guard thread + `recv_timeout` turns a *hang* into a test
    /// *failure*: without the client's `.timeout(...)` the `send()` never
    /// returns, `recv_timeout` elapses, and we panic with a clear message
    /// instead of wedging CI.
    #[test]
    fn github_client_request_times_out_when_server_never_responds() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");

        // Acceptor: accept the connection and hold it open without ever
        // writing a response, so only the request timeout can end the call.
        thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                thread::sleep(Duration::from_secs(30));
            }
        });

        // Short timeouts keep the test fast; this exercises the same builder
        // wiring `GitHubClient::new` uses.
        let client = build_http_client(Duration::from_secs(5), Duration::from_secs(1))
            .expect("build client");

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = client.get(format!("http://{addr}/")).send();
            let _ = tx.send(result.is_err());
        });

        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(true) => { /* request timed out and returned Err — correct */ }
            Ok(false) => panic!("request unexpectedly succeeded against a silent server"),
            Err(_) => panic!(
                "client.send() did not return within 10s against a never-responding \
                 server — the HTTP client has no request timeout, so a stalled probe \
                 would park a tokio worker forever (worker-starvation freeze)"
            ),
        }
    }

    #[test]
    fn test_parse_owner_repo_https() {
        let result = parse_owner_repo("https://github.com/alondero/buildmesh.git");
        assert_eq!(result, Some(("alondero".to_string(), "buildmesh".to_string())));
    }

    #[test]
    fn test_parse_owner_repo_ssh() {
        let result = parse_owner_repo("git@github.com:alondero/buildmesh.git");
        assert_eq!(result, Some(("alondero".to_string(), "buildmesh".to_string())));
    }

    #[test]
    fn test_parse_owner_repo_no_git_suffix() {
        let result = parse_owner_repo("https://github.com/foo/bar");
        assert_eq!(result, Some(("foo".to_string(), "bar".to_string())));
    }

    #[test]
    fn test_parse_owner_repo_invalid() {
        assert_eq!(parse_owner_repo("https://gitlab.com/foo/bar"), None);
    }

    #[test]
    fn test_parse_gh_hosts_yaml() {
        let content = r#"github.com:
    user: testuser
    oauth_token: gho_abc123def456
    git_protocol: ssh
"#;
        assert_eq!(parse_gh_hosts_yaml(content), Some("gho_abc123def456".to_string()));
    }

    #[test]
    fn test_parse_gh_hosts_yaml_quoted() {
        let content = r#""github.com":
    oauth_token: "gho_quoted_token"
"#;
        assert_eq!(parse_gh_hosts_yaml(content), Some("gho_quoted_token".to_string()));
    }

    #[test]
    fn test_parse_gh_hosts_yaml_missing() {
        let content = r#"gitlab.com:
    oauth_token: gho_wrong
"#;
        assert_eq!(parse_gh_hosts_yaml(content), None);
    }

    // -----------------------------------------------------------------------
    // Issue deserialisation — covers the wider wire shape (html_url, state,
    // labels[].name) the mobile "View ↗" link and label chips depend on.
    // Issue #358: previously the struct only kept number/title/body, so the
    // mobile screen had to defensively default `issue.labels` and hide the
    // link when `url` was missing. These tests pin the new wire contract.
    // -----------------------------------------------------------------------

    #[test]
    fn issue_deserialises_full_github_search_shape() {
        // Realistic GitHub `/search/issues` item — keys GitHub always sends.
        // `labels` arrives as `[{id, name, color, ...}]`; we flatten to names.
        let json = r#"{
            "number": 358,
            "title": "Widen Rust GitHubIssue",
            "body": "Expose url/labels/state on the wire",
            "html_url": "https://github.com/alondero/buildmesh/issues/358",
            "state": "open",
            "user": {"login": "alondero", "id": 42, "type": "User"},
            "labels": [
                {"id": 1, "name": "bug", "color": "d73a4a", "default": true},
                {"id": 2, "name": "good first issue", "color": "7057ff", "default": false}
            ]
        }"#;
        let issue: Issue = serde_json::from_str(json).expect("full shape must parse");
        assert_eq!(issue.number, 358);
        assert_eq!(issue.title, "Widen Rust GitHubIssue");
        assert_eq!(issue.body, "Expose url/labels/state on the wire");
        assert_eq!(issue.html_url, "https://github.com/alondero/buildmesh/issues/358");
        assert_eq!(issue.state, "open");
        // The collaborator gate (ADR-0012 §5) reads `author` from `user.login`.
        assert_eq!(issue.author, "alondero");
        assert_eq!(issue.labels, vec!["bug".to_string(), "good first issue".to_string()]);
    }

    #[test]
    fn issue_author_defaults_empty_when_user_absent() {
        // A partial response without `user` must leave `author` at "" rather
        // than failing — the gate treats "" as "unknown → require approval".
        let json = r#"{ "number": 7, "title": "Legacy issue" }"#;
        let issue: Issue = serde_json::from_str(json).expect("partial shape must parse");
        assert_eq!(issue.author, "", "missing user defaults author to empty");
    }

    #[test]
    fn issue_survives_serialize_deserialize_round_trip() {
        // `Issue` derives both Serialize and Deserialize; serialising emits
        // `author` as a bare string. Re-deserialising must not error on that
        // string (the `author` field's deserializer expects `user.login`) — the
        // untagged `RawUser::Bare` arm covers it.
        let json = r#"{ "number": 5, "title": "t", "user": {"login": "octocat"} }"#;
        let issue: Issue = serde_json::from_str(json).expect("parses from GitHub shape");
        let serialised = serde_json::to_string(&issue).expect("serialises");
        let round: Issue = serde_json::from_str(&serialised).expect("round-trips");
        assert_eq!(round.author, "octocat");
        assert_eq!(round.number, 5);
    }

    // -----------------------------------------------------------------------
    // Collaborator permission — the wire→enum mapping the Autopilot gate keys
    // off (ADR-0012 §5). GitHub's legacy `permission` field is one of
    // admin/write/read/none; `has_push_access` is the trust boundary.
    // -----------------------------------------------------------------------

    #[test]
    fn collaborator_permission_maps_legacy_values() {
        assert_eq!(CollaboratorPermission::from_api_str("admin"), CollaboratorPermission::Admin);
        assert_eq!(CollaboratorPermission::from_api_str("write"), CollaboratorPermission::Write);
        assert_eq!(CollaboratorPermission::from_api_str("read"), CollaboratorPermission::Read);
        assert_eq!(CollaboratorPermission::from_api_str("none"), CollaboratorPermission::None);
    }

    #[test]
    fn collaborator_permission_maps_granular_roles_and_is_case_insensitive() {
        // `maintain` can push → Write; `triage` cannot → Read. Mixed case and
        // stray whitespace (defensive) still parse.
        assert_eq!(
            CollaboratorPermission::from_api_str("  Maintain "),
            CollaboratorPermission::Write
        );
        assert_eq!(CollaboratorPermission::from_api_str("TRIAGE"), CollaboratorPermission::Read);
    }

    #[test]
    fn collaborator_permission_unknown_value_is_conservative_none() {
        // An unrecognised level must never grant push — it falls to None.
        assert_eq!(
            CollaboratorPermission::from_api_str("superadmin"),
            CollaboratorPermission::None
        );
        assert_eq!(CollaboratorPermission::from_api_str(""), CollaboratorPermission::None);
    }

    #[test]
    fn has_push_access_only_for_admin_and_write() {
        assert!(CollaboratorPermission::Admin.has_push_access());
        assert!(CollaboratorPermission::Write.has_push_access());
        assert!(!CollaboratorPermission::Read.has_push_access());
        assert!(!CollaboratorPermission::None.has_push_access());
    }

    #[test]
    fn collaborator_permission_parses_from_api_response_shape() {
        // Pin the `{permission, role_name, user}` shape `collaborator_permission`
        // parses, so a GitHub change surfaces here rather than at runtime.
        #[derive(Deserialize)]
        struct PermissionResponse {
            #[serde(default)]
            permission: String,
        }
        let json = r#"{"permission": "write", "role_name": "write", "user": {"login": "jane"}}"#;
        let parsed: PermissionResponse =
            serde_json::from_str(json).expect("permission shape parses");
        assert_eq!(
            CollaboratorPermission::from_api_str(&parsed.permission),
            CollaboratorPermission::Write
        );
    }

    #[test]
    fn issue_deserialises_with_missing_url_state_and_labels() {
        // Partial / older response: the mobile screen must not see `undefined`
        // or an unwrapping panic. `#[serde(default)]` on each new field is the
        // safety net — html_url/state become "" and labels becomes vec![].
        let json = r#"{
            "number": 7,
            "title": "Legacy issue"
        }"#;
        let issue: Issue = serde_json::from_str(json).expect("partial shape must parse");
        assert_eq!(issue.number, 7);
        assert_eq!(issue.title, "Legacy issue");
        assert_eq!(issue.body, "", "body is #[serde(default)]");
        assert_eq!(issue.html_url, "", "missing html_url defaults to empty");
        assert_eq!(issue.state, "", "missing state defaults to empty");
        assert!(issue.labels.is_empty(), "missing labels defaults to empty vec");
    }

    #[test]
    fn issue_deserialises_with_empty_labels_array() {
        // An issue with no labels still sends `"labels": []` — make sure that
        // path is exercised separately from the "key absent" path.
        let json = r#"{
            "number": 9,
            "title": "No labels",
            "html_url": "https://github.com/x/y/issues/9",
            "state": "closed",
            "labels": []
        }"#;
        let issue: Issue = serde_json::from_str(json).expect("empty labels must parse");
        assert!(issue.labels.is_empty());
        assert_eq!(issue.state, "closed");
        assert_eq!(issue.html_url, "https://github.com/x/y/issues/9");
    }

    #[test]
    fn issue_deserialises_label_entry_missing_name_field() {
        // A label entry missing `name` is malformed (GitHub always sends it),
        // but the deserialiser should still fail loud with a useful error
        // — not panic or silently drop the whole issue. This pins the
        // `Vec<RawLabel>` shape so a future refactor that switches to
        // `#[serde(flatten)]` doesn't accidentally swallow this error.
        let json = r#"{
            "number": 11,
            "title": "x",
            "labels": [{"id": 1, "color": "d73a4a"}]
        }"#;
        let result: Result<Issue, _> = serde_json::from_str(json);
        assert!(result.is_err(), "label entry without `name` must fail to parse");
    }

    #[test]
    fn issue_deserialises_with_null_body_defaults_to_empty() {
        // Pin for the alondero/buildmesh regression: issue #1210 was opened
        // without a description, so GitHub returns `"body": null`. The probe
        // panel must show the issue as a titled, body-less row rather than
        // failing the entire `get_repo_issues` IPC with
        // `HTTP error: error decoding response body`. `#[serde(default)]`
        // alone is insufficient because it only rescues a *missing* key,
        // not a present-but-null value; the field needs an explicit
        // `Option<String>`-round-trip deserializer to tolerate `null`.
        let json = r#"{
            "number": 1210,
            "title": "Ship parity — presets, master controls & legacy cutover",
            "body": null,
            "html_url": "https://github.com/alondero/buildmesh/issues/1210",
            "state": "open",
            "user": {"login": "alondero", "id": 1269060, "type": "User"},
            "labels": [{"id": 1, "name": "needs-triage", "color": "0052CC", "default": false}]
        }"#;
        let issue: Issue = serde_json::from_str(json).expect("null body must default to empty");
        assert_eq!(issue.number, 1210);
        assert_eq!(issue.body, "", "body: null must default to empty string");
        assert_eq!(issue.state, "open");
        assert_eq!(issue.author, "alondero");
    }

    #[test]
    fn issue_search_result_with_mixed_null_body_items_parses_end_to_end() {
        // End-to-end through `SearchResult { items: Vec<Issue> }` — the
        // exact deserialise path `list_issues_only` uses. Two items: the
        // first is fully populated, the second mirrors alondero/buildmesh
        // issue #1210 (body: null). Without the null-tolerant field
        // deserialiser on `body`, the second item poisons the whole batch
        // and the surface to the UI reads as
        // `Failed to load issues — HTTP error: error decoding response body`.
        let json = r#"{
            "total_count": 2,
            "incomplete_results": false,
            "items": [
                {
                    "number": 1212,
                    "title": "Circuits walking skeleton",
                    "body": "Follow-ups from #1206",
                    "html_url": "https://github.com/alondero/buildmesh/issues/1212",
                    "state": "open",
                    "user": {"login": "alondero", "id": 1269060, "type": "User"},
                    "labels": [{"id": 1, "name": "needs-triage", "color": "0052CC", "default": false}]
                },
                {
                    "number": 1210,
                    "title": "Ship parity — presets, master controls & legacy cutover",
                    "body": null,
                    "html_url": "https://github.com/alondero/buildmesh/issues/1210",
                    "state": "open",
                    "user": {"login": "alondero", "id": 1269060, "type": "User"},
                    "labels": []
                }
            ]
        }"#;
        #[derive(Deserialize)]
        struct SearchResult { items: Vec<Issue> }
        let result: SearchResult = serde_json::from_str(json).expect("search result with null body must parse");
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].body, "Follow-ups from #1206");
        assert_eq!(result.items[1].body, "");
        assert_eq!(result.items[1].number, 1210);
    }

    #[test]
    fn issue_deserialisation_via_search_result_wraps_items_array() {
        // End-to-end-ish: the search response wraps `items: [Issue, ...]`.
        // Mirrors the shape `list_issues_only` parses from GitHub.
        let json = r#"{
            "total_count": 2,
            "incomplete_results": false,
            "items": [
                {
                    "number": 1,
                    "title": "First",
                    "html_url": "https://github.com/x/y/issues/1",
                    "state": "open",
                    "labels": [{"name": "bug"}]
                },
                {
                    "number": 2,
                    "title": "Second",
                    "html_url": "https://github.com/x/y/issues/2",
                    "state": "open",
                    "labels": []
                }
            ]
        }"#;
        #[derive(Deserialize)]
        struct SearchResult { items: Vec<Issue> }
        let result: SearchResult = serde_json::from_str(json).expect("search result must parse");
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].labels, vec!["bug".to_string()]);
        assert!(result.items[1].labels.is_empty());
    }

    // -----------------------------------------------------------------------
    // Pull request deserialisation — the `/pulls` list shape (number, title,
    // body, draft, state) and the `/pulls/{n}` detail shape (mergeable,
    // mergeable_state). The list endpoint omits mergeability; the detail
    // endpoint can return `mergeable: null` while GitHub computes the merge.
    // -----------------------------------------------------------------------

    #[test]
    fn pull_request_deserialises_full_list_shape() {
        // Realistic `GET /repos/{o}/{r}/pulls` item — the keys the panel reads.
        // The `head` block is what the spawn path consumes to fetch the head ref
        // (issue #420); pin the parsing so a GitHub API change surfaces as a
        // test failure rather than a silent empty ref at runtime.
        let json = r#"{
            "number": 412,
            "html_url": "https://github.com/alondero/buildmesh/pull/412",
            "title": "Add PR probe panel",
            "body": "Lists open/closed PRs and merges mergeable ones",
            "draft": false,
            "state": "open",
            "user": {"login": "contributor-jane", "id": 99, "type": "User"},
            "head": {
                "ref": "feat/420-pr-spawn",
                "sha": "0123456789abcdef0123456789abcdef01234567"
            }
        }"#;
        let pr: PullRequest = serde_json::from_str(json).expect("full PR shape must parse");
        assert_eq!(pr.number, 412);
        assert_eq!(pr.html_url, "https://github.com/alondero/buildmesh/pull/412");
        assert_eq!(pr.title, "Add PR probe panel");
        assert_eq!(pr.body, "Lists open/closed PRs and merges mergeable ones");
        assert!(!pr.draft);
        assert_eq!(pr.state, "open");
        assert_eq!(pr.head_ref, "feat/420-pr-spawn");
        // The collaborator gate keys off *who opened the PR* — `user.login`,
        // projected to `author` through the custom Deserialize.
        assert_eq!(pr.author, "contributor-jane");
        // Issue #444 — `head_sha` is the exact-pinning handle used by the
        // PR-spawn drift check. It MUST survive the projection through the
        // custom Deserialize so `create_pr_node` can persist it for stage-2.
        assert_eq!(
            pr.head_sha, "0123456789abcdef0123456789abcdef01234567",
            "head_sha must be projected from head.sha so the spawn path can pin the worktree"
        );
    }

    /// When GitHub omits `head.sha` (some fork responses, stale list
    /// endpoints), the deserialiser must default `head_sha` to "" rather
    /// than failing the whole list. Matches the existing default-on-missing
    /// rule for `head_ref`.
    #[test]
    fn pull_request_deserialises_with_missing_head_sha() {
        let json = r#"{
            "number": 8,
            "html_url": "https://github.com/x/y/pull/8",
            "title": "PR with no head sha",
            "head": { "ref": "f8" }
        }"#;
        let pr: PullRequest = serde_json::from_str(json).expect("head without sha must parse");
        assert_eq!(pr.head_ref, "f8");
        assert_eq!(pr.head_sha, "", "missing head.sha must default to empty");
    }

    #[test]
    fn pull_request_deserialises_with_missing_body_state_and_draft() {
        // Partial response: body/state/draft default rather than failing.
        let json = r#"{
            "number": 7,
            "html_url": "https://github.com/x/y/pull/7",
            "title": "Legacy PR"
        }"#;
        let pr: PullRequest = serde_json::from_str(json).expect("partial PR shape must parse");
        assert_eq!(pr.number, 7);
        assert_eq!(pr.body, "", "missing body defaults to empty");
        assert_eq!(pr.state, "", "missing state defaults to empty");
        assert!(!pr.draft, "missing draft defaults to false");
        assert_eq!(pr.head_ref, "", "missing head.ref defaults to empty");
        assert_eq!(
            pr.head_repo_owner, "",
            "missing head.repo.owner.login defaults to empty"
        );
        assert_eq!(
            pr.head_repo_clone_url, "",
            "missing head.repo.clone_url defaults to empty"
        );
        assert_eq!(pr.head_sha, "", "missing head.sha defaults to empty");
    }

    /// Issue #443: a fork PR (head's `repo.owner.login` differs from the
    /// destination) carries the fork's owner login + clone URL on
    /// `head_repo_owner` + `head_repo_clone_url`. The list endpoint
    /// includes the head's full `head.repo` object, so a single call
    /// surfaces everything the spawn path needs to register the fork as a
    /// remote and fetch the head ref. Pin the fork shape so a future
    /// refactor that drops `head.repo` from the projection surfaces as a
    /// test failure rather than a silent spawn on the wrong commits.
    #[test]
    fn pull_request_deserialises_fork_pr_with_head_repo_metadata() {
        let json = r#"{
            "number": 443,
            "html_url": "https://github.com/alondero/buildmesh/pull/443",
            "title": "fork PR worktree adoption",
            "body": "spawn on a fork's head ref",
            "draft": false,
            "state": "open",
            "head": {
                "ref": "feat/443-fork",
                "sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "repo": {
                    "clone_url": "https://github.com/alice/buildmesh.git",
                    "owner": {"login": "alice"}
                }
            }
        }"#;
        let pr: PullRequest = serde_json::from_str(json).expect("fork PR shape must parse");
        assert_eq!(pr.head_ref, "feat/443-fork");
        assert_eq!(
            pr.head_repo_owner, "alice",
            "head.repo.owner.login is the fork's owner"
        );
        assert_eq!(
            pr.head_repo_clone_url, "https://github.com/alice/buildmesh.git",
            "head.repo.clone_url is the fork's clone URL"
        );
    }

    /// A same-repo PR's `head.repo` IS the destination repo — the values
    /// are still populated (and equal the destination owner / URL). Stage-2
    /// spawn (`spawn_agent_inner`, issue #443) keys the fork-vs-same-repo
    /// decision on whether these fields are `Some` (fork → register a
    /// `fork-<login>` remote) or empty (same-repo → `git fetch origin
    /// <head_ref>`). Pin the populated values so a future refactor that
    /// special-cases the same-repo case to drop the projection is caught
    /// (we still want the fields populated so the comparison has inputs).
    #[test]
    fn pull_request_deserialises_same_repo_pr_with_destination_repo_metadata() {
        let json = r#"{
            "number": 439,
            "html_url": "https://github.com/alondero/buildmesh/pull/439",
            "title": "same-repo PR",
            "draft": false,
            "state": "open",
            "head": {
                "ref": "feat/420-pr-spawn",
                "sha": "abc123abc123abc123abc123abc123abc123abcd",
                "repo": {
                    "clone_url": "https://github.com/alondero/buildmesh.git",
                    "owner": {"login": "alondero"}
                }
            }
        }"#;
        let pr: PullRequest = serde_json::from_str(json).expect("same-repo PR shape must parse");
        assert_eq!(pr.head_repo_owner, "alondero");
        assert_eq!(
            pr.head_repo_clone_url,
            "https://github.com/alondero/buildmesh.git"
        );
    }

    #[test]
    fn pull_request_list_deserialises_as_bare_array() {
        // The `/pulls` endpoint returns a bare array, NOT a `{items: [...]}`
        // wrapper like the search API — pin that so a future refactor doesn't
        // accidentally reuse the SearchResult wrapper here.
        let json = r#"[
            {"number": 1, "html_url": "https://github.com/x/y/pull/1", "title": "First", "state": "open", "draft": false, "head": {"ref": "f1", "sha": "aaa"}},
            {"number": 2, "html_url": "https://github.com/x/y/pull/2", "title": "Second", "state": "open", "draft": true, "head": {"ref": "f2", "sha": "bbb"}}
        ]"#;
        let prs: Vec<PullRequest> = serde_json::from_str(json).expect("PR list must parse");
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 1);
        assert!(prs[1].draft);
    }

    #[test]
    fn pr_detail_mergeability_parses_true_false_and_null() {
        // The detail endpoint carries `mergeable` (bool | null) and
        // `mergeable_state`. We deserialise the same private `Detail` shape
        // `pull_request_mergeability` uses.
        #[derive(Deserialize)]
        struct Detail {
            #[serde(default)]
            mergeable: Option<bool>,
            #[serde(default)]
            mergeable_state: String,
        }

        let clean: Detail = serde_json::from_str(
            r#"{"mergeable": true, "mergeable_state": "clean"}"#,
        ).unwrap();
        assert_eq!(clean.mergeable, Some(true));
        assert_eq!(clean.mergeable_state, "clean");

        let dirty: Detail = serde_json::from_str(
            r#"{"mergeable": false, "mergeable_state": "dirty"}"#,
        ).unwrap();
        assert_eq!(dirty.mergeable, Some(false));
        assert_eq!(dirty.mergeable_state, "dirty");

        // `null` (still computing) must stay `None`, not become `false`.
        let computing: Detail = serde_json::from_str(
            r#"{"mergeable": null, "mergeable_state": "unknown"}"#,
        ).unwrap();
        assert_eq!(computing.mergeable, None);
        assert_eq!(computing.mergeable_state, "unknown");

        // Keys absent entirely (defensive) — both fall to their defaults.
        let absent: Detail = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(absent.mergeable, None);
        assert_eq!(absent.mergeable_state, "");
    }

    // -----------------------------------------------------------------------
    // PR files deserialisation — issue #421, the wire shape of
    // `GET /repos/{o}/{r}/pulls/{n}/files`. The endpoint returns a bare
    // array; each item carries `filename`, `status`, `additions`,
    // `deletions`, and a unified `patch`. The frontend renders the patch
    // line-by-line, so we don't need to reconstruct hunk structure here.
    // `previous_filename` is only set for renames (the corresponding entry
    // in GitHub's response has it; everything else omits it).
    // -----------------------------------------------------------------------

    #[test]
    fn pr_file_deserialises_full_shape_with_patch() {
        // Realistic `/pulls/{n}/files` item — a modified file with a patch.
        // The patch text spans lines and includes hunk markers; we don't try
        // to parse it (the frontend does that), just confirm it round-trips.
        let json = r#"{
            "filename": "src/app.ts",
            "status": "modified",
            "additions": 3,
            "deletions": 1,
            "changes": 4,
            "blob_url": "https://github.com/alondero/buildmesh/blob/.../src/app.ts",
            "raw_url": "https://raw.githubusercontent.com/.../src/app.ts",
            "contents_url": "https://api.github.com/repos/.../contents/src/app.ts",
            "sha": "abc123",
            "patch": "@@ -1,5 +1,7 @@\n line1\n-line2\n+line2-tweaked\n+line2b\n line3\n"
        }"#;
        let file: PrFile = serde_json::from_str(json).expect("full PR file shape must parse");
        assert_eq!(file.filename, "src/app.ts");
        assert_eq!(file.status, "modified");
        assert_eq!(file.additions, 3);
        assert_eq!(file.deletions, 1);
        assert!(file.patch.starts_with("@@"), "patch should round-trip verbatim");
        assert!(file.previous_filename.is_none(), "no rename → no previous_filename");
    }

    #[test]
    fn pr_file_deserialises_rename_with_previous_filename() {
        // A renamed file: status = "renamed", previous_filename = the old path.
        let json = r#"{
            "filename": "src/new-name.ts",
            "previous_filename": "src/old-name.ts",
            "status": "renamed",
            "additions": 0,
            "deletions": 0,
            "patch": ""
        }"#;
        let file: PrFile = serde_json::from_str(json).expect("rename shape must parse");
        assert_eq!(file.filename, "src/new-name.ts");
        assert_eq!(file.previous_filename.as_deref(), Some("src/old-name.ts"));
        assert_eq!(file.status, "renamed");
    }

    #[test]
    fn pr_file_deserialises_binary_file_with_empty_patch() {
        // GitHub omits `patch` for binary files; our `#[serde(default)]` makes
        // it parse to "" rather than fail. Status is typically "modified" for
        // binary blobs.
        let json = r#"{
            "filename": "assets/logo.png",
            "status": "modified",
            "additions": 0,
            "deletions": 0
        }"#;
        let file: PrFile = serde_json::from_str(json).expect("binary file shape must parse");
        assert_eq!(file.filename, "assets/logo.png");
        assert_eq!(file.patch, "", "missing patch defaults to empty string");
        assert!(file.previous_filename.is_none());
    }

    #[test]
    fn pr_file_list_deserialises_as_bare_array() {
        // The `/pulls/{n}/files` endpoint returns a bare array, just like
        // `/pulls`. Pin that so a future refactor doesn't accidentally wrap
        // it in an object.
        let json = r#"[
            {
                "filename": "a.txt",
                "status": "added",
                "additions": 1,
                "deletions": 0,
                "patch": "@@ -0,0 +1 @@\n+new line\n"
            },
            {
                "filename": "b.txt",
                "status": "deleted",
                "additions": 0,
                "deletions": 1,
                "patch": "@@ -1 +0,0 @@\n-gone\n"
            }
        ]"#;
        let files: Vec<PrFile> = serde_json::from_str(json).expect("PR file list must parse");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].filename, "a.txt");
        assert_eq!(files[0].status, "added");
        assert_eq!(files[1].filename, "b.txt");
        assert_eq!(files[1].status, "deleted");
    }

    // -----------------------------------------------------------------------
    // Blocked-by body parser — extracts the list of GitHub issue numbers
    // referenced under a `**Blocked by**` markdown section in an issue body.
    // The Issues Probe (issue #481) renders a red flag when an issue's
    // blockers are still in the loaded open-issues list. The parser
    // matches BOTH reference forms:
    //
    //   - `/issues/N` URLs anywhere in the section (the format the
    //     manual issue editor emits),
    //   - bare `#NNN` text references at the start of a bullet line
    //     (the format GitHub issue forms / templates auto-render the
    //     "Blocked by" field as — real shape of issue #503 in
    //     alondero/buildmesh).
    //
    // Two preprocessor passes strip false-positive sources before the
    // ref extraction:
    //
    //   1. Markdown-link text — `[title #481](url)` → `url` so a
    //      `#NNN` inside a link's title isn't picked up bare-style.
    //   2. Backtick-fenced code spans — `` `#481` `` → `` `` so a
    //      `#NNN` used as an identifier / command / filename isn't
    //      picked up bare-style.
    //
    // The bare-ref form is line-anchored to a bullet marker so a `#NNN`
    // in narrative prose ("unblocks once #500 ships") is excluded.
    // PR mentions (`/pull/N`) are naturally excluded because the
    // issue-URL regex only matches `/issues/N`.
    //
    // Bodies are assumed to be ≤64 KiB; the helper caps the scan to bound
    // regex memory. Real issue bodies from `list_issues_only` are <16 KiB
    // in practice (GitHub's body length cap is 65,536 chars).
    // -----------------------------------------------------------------------

    #[test]
    fn parse_blocked_by_empty_body() {
        assert!(parse_blocked_by("").is_empty());
        assert!(parse_blocked_by("   \n  ").is_empty());
    }

    #[test]
    fn parse_blocked_by_no_section() {
        // Body has the word "blocked" but not the `**Blocked by**` header.
        let body = "This issue was blocked by a flaky test last week.\nNo formal relationship.";
        assert!(parse_blocked_by(body).is_empty());
    }

    #[test]
    fn parse_blocked_by_setext_underline_single_blocker() {
        // Real shape from issue #482 in alondero/buildmesh.
        let body = "\
Some intro paragraph.

**Blocked by**
----------

*   [Autopilot 1: Mesh Schema & Config UI #481](https://github.com/alondero/buildmesh/issues/481)

Some closing paragraph.";
        assert_eq!(parse_blocked_by(body), vec![481]);
    }

    #[test]
    fn parse_blocked_by_setext_underline_multiple_blockers_source_order() {
        let body = "\
**Blocked by**
----------

*   [Issue A #481](https://github.com/x/y/issues/481)
*   [Issue B #482](https://github.com/x/y/issues/482)
*   [Issue C #483](https://github.com/x/y/issues/483)
";
        assert_eq!(parse_blocked_by(body), vec![481, 482, 483]);
    }

    #[test]
    fn parse_blocked_by_none_short_circuit() {
        // Real shape from issue #481 in alondero/buildmesh — "no blockers"
        // is the common idiom.
        let body = "\
**Blocked by**
----------

None - can start immediately.

Some narrative below.";
        assert!(parse_blocked_by(body).is_empty());
    }

    #[test]
    fn parse_blocked_by_none_alone_short_circuits() {
        let body = "**Blocked by**\n----------\n\nNone\n";
        assert!(parse_blocked_by(body).is_empty());
    }

    #[test]
    fn parse_blocked_by_atx_heading_variant() {
        // Some bodies use `# Blocked by` instead of setext `---` underline.
        let body = "\
# Blocked by

*   [Issue A #481](https://github.com/x/y/issues/481)
";
        assert_eq!(parse_blocked_by(body), vec![481]);
    }

    #[test]
    fn parse_blocked_by_atx_heading_with_bare_reference() {
        // Regression for issue #503 in alondero/buildmesh: the issue body
        // was generated from a GitHub issue form (template) which renders
        // the "Blocked by" field as a bare reference list (`- #NNN`),
        // not the `[Title #N](issues/N)` link form that the manual editor
        // emits. Before this fix, the URL-only parser returned `vec![]`
        // for this body, so the Issues Probe never rendered the blocked-
        // by flag — silently dropping the warning for every form-created
        // issue in the repo.
        let body = "\
## Parent

#494

## What to build

Add the global \"Drive Kill-Switch\" toggle and the `/admin/kill-switch` API.

## Acceptance criteria

- [ ] Setting saved in the database, defaulting to enabled.
- [ ] Toggle is visible and editable in the desktop UI settings.
- [ ] API routes reject write operations immediately with `403 Forbidden`.
- [ ] Integration tests verify prompt rejection when the kill switch is off.

## Blocked by

- #500
";
        assert_eq!(parse_blocked_by(body), vec![500]);
    }

    #[test]
    fn parse_blocked_by_atx_heading_with_bare_references_multiple() {
        // An issue form can produce multiple bare references when the
        // user picks 2+ blockers in the template's multi-select field.
        // Source order is preserved.
        let body = "\
## Blocked by

- #481
- #482
- #483
";
        assert_eq!(parse_blocked_by(body), vec![481, 482, 483]);
    }

    #[test]
    fn parse_blocked_by_mixed_url_and_bare_references_source_order() {
        // A section can mix URL form (manual editor) and bare form
        // (issue form) — e.g. a user types one manually and the form
        // auto-populates the other. Source order is preserved across
        // the two alternatives, and a number that appears in both
        // forms (URL + bare in link text, or repeated across lines)
        // is deduped.
        let body = "\
## Blocked by

- [First #481](https://github.com/x/y/issues/481)
- #482
- [Third #483](https://github.com/x/y/issues/483)
";
        assert_eq!(parse_blocked_by(body), vec![481, 482, 483]);
    }

    #[test]
    fn parse_blocked_by_url_fragment_in_link_does_not_false_positive() {
        // Regression F1: a GitHub permalink with a comment anchor
        // (`#issuecomment-NNN`) must not contribute the comment
        // number to the blocked-by list. The URL form picks up the
        // issue; the bare-ref form, being line-anchored to a bullet
        // marker, never reaches the fragment because it lives in the
        // middle of the link's URL.
        let body = "\
**Blocked by**
----------

*   [Issue 481](https://github.com/x/y/issues/481#issuecomment-12345)
";
        assert_eq!(parse_blocked_by(body), vec![481]);
    }

    #[test]
    fn parse_blocked_by_bare_ref_inside_code_span_excluded() {
        // Regression F2: `#NNN` inside a backtick-fenced code span
        // is literal content (an identifier, command, filename —
        // common in issue bodies), not a blocker reference. The
        // code-span strip pass removes the span before bare-ref
        // matching, so `#500` inside backticks is not extracted.
        let body = "\
**Blocked by**
----------

*   Use the `kill_switch` helper from `#500` to disable writes
";
        assert!(parse_blocked_by(body).is_empty());
    }

    #[test]
    fn parse_blocked_by_narrative_mention_inside_section_excluded() {
        // Regression F3: a `#NNN` mentioned in narrative prose
        // inside the section (not as a bullet item) is not a
        // blocker. The bare-ref form requires a bullet marker, so
        // a mention like "this unblocks once #500 ships" is left
        // alone. Only the bullet items are extracted.
        let body = "\
**Blocked by**
----------

*   [Issue 481](https://github.com/x/y/issues/481)
This unblocks once #500 ships — see the linked discussion.
*   [Issue 600](https://github.com/x/y/issues/600)
";
        assert_eq!(parse_blocked_by(body), vec![481, 600]);
    }

    #[test]
    fn parse_blocked_by_dedupes_repeated_link() {
        // Same issue listed twice (editor copy/paste) → one entry.
        let body = "\
**Blocked by**
----------

*   [Issue A #481](https://github.com/x/y/issues/481)
*   [Issue A again #481](https://github.com/x/y/issues/481)
";
        assert_eq!(parse_blocked_by(body), vec![481]);
    }

    #[test]
    fn parse_blocked_by_excludes_pull_request_urls() {
        // A PR mention in the section must NOT be treated as an issue
        // blocker. Real bodies often reference context-PRs under the same
        // header.
        let body = "\
**Blocked by**
----------

*   [Issue #481](https://github.com/x/y/issues/481)
*   [Related PR #480](https://github.com/x/y/pull/480)
";
        assert_eq!(parse_blocked_by(body), vec![481]);
    }

    #[test]
    fn parse_blocked_by_stray_mention_outside_section_excluded() {
        // `#481` mentioned in narrative text outside the Blocked-by section
        // must not be picked up — the header is the signal.
        let body = "\
This issue is related to #481 in a narrative sense.

**Blocked by**
----------

*   [Real blocker #999](https://github.com/x/y/issues/999)
";
        assert_eq!(parse_blocked_by(body), vec![999]);
    }

    #[test]
    fn parse_blocked_by_cross_repo_url_extracted() {
        // We only need the issue number; cross-repo blockers are still
        // listed. The frontend's `stillBlockedBy` cross-reference against
        // the loaded open-issues set will simply not match them, which is
        // the documented limitation in the plan.
        let body = "\
**Blocked by**
----------

*   [Other repo #123](https://github.com/other-org/other-repo/issues/123)
";
        assert_eq!(parse_blocked_by(body), vec![123]);
    }

    #[test]
    fn parse_blocked_by_handles_64kib_capped_body() {
        // Defensive: bodies can theoretically be up to ~65 KiB. The helper
        // caps the scan at 64 KiB; a Blocked-by section at the very end
        // (just inside the cap) must still be found.
        let padding = "x".repeat(60 * 1024);
        let body = format!(
            "{padding}\n\n**Blocked by**\n----------\n\n*   [Issue #481](https://github.com/x/y/issues/481)\n"
        );
        assert_eq!(parse_blocked_by(&body), vec![481]);
    }

    #[test]
    fn parse_blocked_by_section_past_cap_excluded_gracefully() {
        // Blocked-by section beyond the 64 KiB cap is unreachable — the
        // helper returns []. This documents the assumption that real
        // blockers live within the cap (true for GitHub's 65,536 char
        // body limit since the section sits at the top of the body).
        let padding = "x".repeat(70 * 1024);
        let body = format!(
            "{padding}\n\n**Blocked by**\n----------\n\n*   [Issue #481](https://github.com/x/y/issues/481)\n"
        );
        assert!(parse_blocked_by(&body).is_empty());
    }

    #[test]
    fn parse_blocked_by_does_not_panic_on_multibyte_char_at_cap_boundary() {
        // Regression: real GitHub bodies contain emoji (4-byte UTF-8) and
        // CJK characters (3-byte). Without `floor_char_boundary`, the
        // `&body[..scan_end]` slice would panic with
        // "byte index N is not a char boundary" whenever a multi-byte
        // codepoint straddled the 64 KiB cap. This test pins that the
        // helper stays panic-free across the boundary; the assertion is
        // intentionally loose (no specific number expected) because the
        // important property is "doesn't panic".
        let mut body = "x".repeat(65_534);
        body.push('🐛'); // 4 bytes — straddles byte 65,534 / 65,535 / 65,536 / 65,537
        body.push_str("\n**Blocked by**\n----------\n\n* [Issue #481](https://github.com/x/y/issues/481)\n");
        // Just verify it doesn't panic. The exact return value depends on
        // where the floor_char_boundary snaps the index, but for a body
        // this size the Blocked-by section sits inside the floored region.
        let _ = parse_blocked_by(&body);
    }

    // -----------------------------------------------------------------------
    // Label add/remove + 422 → LabelNotFound mapping (issue #979)
    //
    // The Issues Probe's trigger-label toggle drives two new methods:
    // `add_issue_label` (POST) and `remove_issue_label` (DELETE). The load-
    // bearing pieces that need pinning are:
    //
    //   1. The `percent_encode_path_component` helper — labels commonly
    //      contain `:`, `/`, and spaces (`buildmesh:run`, `area/auth`),
    //      and the DELETE endpoint embeds the label in the URL path.
    //      A regression here surfaces as a 404 from GitHub even when
    //      the label IS on the issue.
    //   2. The 422 → `LabelNotFound` mapping on POST — that's the only
    //      422 path the endpoint documents, and the UI toast depends on
    //      the typed error to render a precise remediation message.
    //   3. The 404 → `Ok(())` collapse on DELETE — makes the toggle
    //      idempotent on a missing label, so a retry doesn't surface
    //      a stale "label wasn't there" error.
    //
    // We test (1) + (2) + (3) inline. The full live HTTP round-trip is
    // covered by the `#[ignore]`-gated `integration_*_label_live` tests
    // at the bottom — mirrors the file's existing
    // `integration_find_open_pr_for_branch_live` opt-in pattern.
    // -----------------------------------------------------------------------

    #[test]
    fn percent_encode_path_component_passes_through_unreserved_chars() {
        // The RFC 3986 unreserved set (`A-Z a-z 0-9 - _ . ~`) is passed
        // through verbatim — no `%XX` escapes. Pins the "don't over-encode"
        // half of the helper so a future refactor that switches to
        // blanket encoding doesn't generate noise like `b%75g` for `bug`.
        assert_eq!(
            GitHubClient::percent_encode_path_component("bug"),
            "bug",
            "ASCII letters must pass through verbatim"
        );
        assert_eq!(
            GitHubClient::percent_encode_path_component("buildmesh.run"),
            "buildmesh.run",
            "`.` is unreserved"
        );
        assert_eq!(
            GitHubClient::percent_encode_path_component("a-b_c~d"),
            "a-b_c~d",
            "all unreserved punctuation passes through"
        );
        assert_eq!(
            GitHubClient::percent_encode_path_component("v1.2.3-rc4"),
            "v1.2.3-rc4",
            "version-shaped labels pass through verbatim"
        );
    }

    #[test]
    fn percent_encode_path_component_encodes_unsafe_chars() {
        // Labels commonly contain characters that are unsafe in a URL
        // path segment — `:` (the `namespace:name` shape), `/` (path-like
        // labels), spaces (rare but allowed), and `?`/`#`/`&` (which
        // would change the URL's query/fragment/separator meaning).
        assert_eq!(
            GitHubClient::percent_encode_path_component("buildmesh:run"),
            "buildmesh%3Arun",
            "`:` in a label name must percent-encode to %3A"
        );
        assert_eq!(
            GitHubClient::percent_encode_path_component("area/auth"),
            "area%2Fauth",
            "`/` in a label name must percent-encode to %2F"
        );
        assert_eq!(
            GitHubClient::percent_encode_path_component("needs review"),
            "needs%20review",
            "spaces must encode to %20 (NOT `+`, which is form-encoded)"
        );
        assert_eq!(
            GitHubClient::percent_encode_path_component("a&b"),
            "a%26b",
            "`&` must encode — it would otherwise be parsed as a query separator"
        );
        assert_eq!(
            GitHubClient::percent_encode_path_component("a?b#c"),
            "a%3Fb%23c",
            "`?` and `#` must encode — they would otherwise change the URL shape"
        );
    }

    #[test]
    fn percent_encode_path_component_handles_empty_and_unicode() {
        // Empty input round-trips to empty (a label name can't actually be
        // empty per GitHub, but the helper stays total). Multi-byte UTF-8
        // is encoded byte-by-byte — each byte of the emoji's UTF-8
        // representation gets its own `%XX`. The helper is bytes-in/bytes-
        // out and intentionally doesn't try to be Unicode-aware.
        assert_eq!(GitHubClient::percent_encode_path_component(""), "");
        assert_eq!(
            GitHubClient::percent_encode_path_component("🐛"),
            "%F0%9F%90%9B",
            "emoji encodes byte-by-byte per UTF-8"
        );
    }

    #[test]
    fn git_hub_error_label_not_found_display_includes_label_name() {
        // The Display impl is what the frontend toast surfaces when
        // POST returns 422 for a label that doesn't exist on the repo.
        // The label name must appear verbatim so the user can fix it
        // by creating the label on GitHub.
        let err = GitHubError::LabelNotFound("buildmesh:run".to_string());
        let msg = err.to_string();
        assert!(
            msg.contains("buildmesh:run"),
            "Display must surface the requested label name; got: {}",
            msg
        );
        assert!(
            msg.contains("Label") && msg.contains("repo"),
            "Display must include the remediation hint; got: {}",
            msg
        );
    }

    #[test]
    fn git_hub_error_label_not_found_is_a_distinct_variant() {
        // Pins that `LabelNotFound` is its own variant and not collapsed
        // into `Api(422, ...)` — the frontend distinguishes them so it
        // can show "create the label on GitHub first" vs the generic
        // 422 wall. Match on the variant directly to guard against a
        // future refactor that re-merges them.
        let err = GitHubError::LabelNotFound("foo".to_string());
        match err {
            GitHubError::LabelNotFound(name) => assert_eq!(name, "foo"),
            other => panic!("LabelNotFound must remain a distinct variant; got {:?}", other),
        }
    }

    // ----- classify_add_label_response (issue #979) -----------------------
    //
    // The 422 → `LabelNotFound` mapping is the load-bearing piece of the
    // Issues Probe's error UX: a precise "Label `X` doesn't exist on the
    // repo — create it on GitHub first" toast versus the generic 422 wall.
    // Extracted from `add_issue_label` so the mapping is unit-testable
    // without standing up an HTTP server. Each branch is exercised here
    // so a future refactor that drops the magic-string check (or flips
    // the empty-body fallback) surfaces as a test failure rather than a
    // confusing user-facing toast.

    #[test]
    fn classify_add_label_response_422_with_label_does_not_exist_maps_to_label_not_found() {
        // The canonical GitHub response for this case carries
        // `"Label does not exist"` as the top-level `message` field. The
        // classifier must surface it as `LabelNotFound`, NOT as a generic
        // `Api(422, ...)`.
        let body = r#"{"message":"Label does not exist","errors":[{"resource":"Label","code":"not_found","field":"name"}],"documentation_url":"https://docs.github.com/rest/issues/labels#add-labels-to-an-issue"}"#;
        // Sanity: the canonical body actually contains the magic string.
        assert!(body.contains("Label does not exist"));

        let result = GitHubClient::classify_add_label_response(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            body,
            "buildmesh:run",
        );
        match result {
            Err(GitHubError::LabelNotFound(name)) => assert_eq!(name, "buildmesh:run"),
            other => panic!("422 with 'Label does not exist' must map to LabelNotFound; got {:?}", other),
        }
    }

    #[test]
    fn classify_add_label_response_422_with_different_body_collapses_to_api() {
        // The documented 422 is the "Label does not exist" case. A 422
        // with a different body is some other validation failure —
        // preserve the raw text via `Api(422, body)` so diagnostics
        // aren't lost. Don't over-classify to LabelNotFound.
        let body = r#"{"message":"Validation Failed","errors":[{"resource":"Issue","code":"missing","field":"title"}]}"#;
        // Sanity: this body MUST NOT contain the magic string — that's
        // the contract the classifier depends on.
        assert!(!body.contains("Label does not exist"));

        let result = GitHubClient::classify_add_label_response(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            body,
            "buildmesh:run",
        );
        match result {
            Err(GitHubError::Api(status, msg)) => {
                assert_eq!(status, 422);
                assert_eq!(msg, body, "raw body must be preserved for non-magic 422s");
            }
            other => panic!("422 without magic string must map to Api, not LabelNotFound; got {:?}", other),
        }
    }

    #[test]
    fn classify_add_label_response_422_with_empty_body_maps_to_label_not_found() {
        // Defensive: a partial / truncated response that GitHub
        // nonetheless classifies as 422 is most plausibly the same code
        // path. Surface it as LabelNotFound so the toast stays precise
        // (an empty `Api(422, "")` message would be useless to the user).
        let result = GitHubClient::classify_add_label_response(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "",
            "buildmesh:run",
        );
        match result {
            Err(GitHubError::LabelNotFound(name)) => assert_eq!(name, "buildmesh:run"),
            other => panic!("empty-body 422 must collapse to LabelNotFound; got {:?}", other),
        }
    }

    #[test]
    fn classify_add_label_response_403_collapses_to_api() {
        // Permission errors (Write or Triage required) come back as 403,
        // not 422. They must NOT map to LabelNotFound — the toast text
        // for LabelNotFound is wrong ("create the label first" isn't
        // actionable when the actual issue is missing triage access).
        let result = GitHubClient::classify_add_label_response(
            reqwest::StatusCode::FORBIDDEN,
            "Resource not accessible by integration",
            "buildmesh:run",
        );
        match result {
            Err(GitHubError::Api(403, msg)) => assert!(msg.contains("Resource not accessible")),
            other => panic!("403 must map to Api(403, ...), not LabelNotFound; got {:?}", other),
        }
    }

    #[test]
    fn classify_add_label_response_success_returns_ok() {
        // 200 / 201 with any body (including empty) → Ok(()). We don't
        // parse the success body — just need to confirm the classifier
        // doesn't accidentally treat it as an error.
        for status in [
            reqwest::StatusCode::OK,
            reqwest::StatusCode::CREATED,
        ] {
            let result = GitHubClient::classify_add_label_response(
                status,
                r#"[{"id":1,"name":"buildmesh:run"}]"#,
                "buildmesh:run",
            );
            assert!(result.is_ok(), "status {} must succeed; got {:?}", status, result);
        }
        // Empty body on success is also fine.
        let result = GitHubClient::classify_add_label_response(
            reqwest::StatusCode::OK,
            "",
            "buildmesh:run",
        );
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // Live API round-trips — opt-in only, gated behind `--ignored`. Mirrors
    // the existing `integration_find_open_pr_for_branch_live` pattern: a
    // real `GITHUB_TOKEN` / `gh auth login` is required, and the test
    // only runs against a fixture repo where the caller has triage access.
    // Run with: `cargo test -- --ignored add_issue_label_live`.
    //
    // We don't pull in `wiremock` or a similar dependency just for these
    // two calls; the URL-encoding logic + the 422/404 mapping are pinned
    // by the unit tests above, and the live tests cover the wire shape.
    // -----------------------------------------------------------------------

    #[test]
    #[ignore]
    fn integration_add_issue_label_live() {
        let client = GitHubClient::new().expect("GITHUB_TOKEN must be set");
        // Apply + immediately remove so the fixture issue ends in its
        // starting state — no permanent side effect from a re-run.
        client
            .add_issue_label("alondero", "buildmesh", 1, "buildmesh:run")
            .expect("add must succeed for an existing label");
        client
            .remove_issue_label("alondero", "buildmesh", 1, "buildmesh:run")
            .expect("remove must succeed for an applied label");
    }

    #[test]
    #[ignore]
    fn integration_remove_issue_label_idempotent_on_missing_label() {
        // Confirms the 404 → Ok(()) collapse on a label that was never
        // applied. Requires a real fixture issue + token.
        let client = GitHubClient::new().expect("GITHUB_TOKEN must be set");
        client
            .remove_issue_label("alondero", "buildmesh", 1, "definitely-not-on-this-issue-xyz")
            .expect("removing a missing label must collapse to Ok(())");
    }

    // -----------------------------------------------------------------------
    // PR summaries GraphQL seam (issue #1529) — mapping + O(pages) cost.
    //
    // The old panel cost N+1 HTTP requests per refresh (1 REST list + N REST
    // details). The cohesive summary query must cost O(pages): 1 GraphQL
    // request for 1/20/100 PRs (all fit the 100-row first page), 2 for a
    // two-page connection, regardless of PR count. A fake loopback server
    // with a request counter pins the cost; pure mapping tests pin the
    // null/unknown semantics explicitly (UNKNOWN ≠ conflict ≠ transport
    // failure).
    // -----------------------------------------------------------------------

    #[test]
    fn graphql_states_for_filter_maps_open_and_closed() {
        assert_eq!(graphql_states_for_filter("open"), vec!["OPEN"]);
        assert_eq!(
            graphql_states_for_filter("closed"),
            vec!["CLOSED", "MERGED"],
            "REST closed includes merged, so GraphQL must ask for both"
        );
        assert_eq!(
            graphql_states_for_filter("bogus"),
            vec!["OPEN"],
            "arbitrary strings fall back to open rather than reaching the API"
        );
    }

    #[test]
    fn map_graphql_mergeable_preserves_null_unknown_semantics() {
        assert_eq!(map_graphql_mergeable("MERGEABLE"), Some(true));
        assert_eq!(map_graphql_mergeable("CONFLICTING"), Some(false));
        assert_eq!(
            map_graphql_mergeable("UNKNOWN"),
            None,
            "UNKNOWN (still computing) must stay None, not coerce to Some(false)"
        );
        assert_eq!(
            map_graphql_mergeable("FUTURE_VALUE"),
            None,
            "unrecognised values are conservative unknown, never false conflicts"
        );
    }

    #[test]
    fn map_graphql_merge_state_lowercases_rest_vocabulary() {
        assert_eq!(map_graphql_merge_state("CLEAN"), "clean");
        assert_eq!(map_graphql_merge_state("DIRTY"), "dirty");
        assert_eq!(map_graphql_merge_state("BLOCKED"), "blocked");
        assert_eq!(map_graphql_merge_state("BEHIND"), "behind");
        assert_eq!(map_graphql_merge_state("UNSTABLE"), "unstable");
        assert_eq!(map_graphql_merge_state("UNKNOWN"), "unknown");
        assert_eq!(map_graphql_merge_state("DRAFT"), "draft");
        assert_eq!(map_graphql_merge_state("HAS_HOOKS"), "has_hooks");
    }

    #[test]
    fn map_graphql_state_merges_merged_into_closed() {
        assert_eq!(map_graphql_state("OPEN"), "open");
        assert_eq!(map_graphql_state("CLOSED"), "closed");
        assert_eq!(
            map_graphql_state("MERGED"),
            "closed",
            "REST reports merged as closed and the frontend filter only knows open/closed"
        );
    }

    #[test]
    fn pr_summary_from_graphql_node_maps_all_fields() {
        let node: GraphQLPrNode = serde_json::from_value(serde_json::json!({
            "number": 7,
            "title": "Add widget",
            "body": "Adds the widget",
            "url": "https://github.com/acme/demo/pull/7",
            "state": "OPEN",
            "isDraft": false,
            "headRefName": "feat/7-widget",
            "headRefOid": "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1",
            "headRepository": {
                "owner": {"login": "acme"},
                "url": "https://github.com/acme/demo"
            },
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN"
        }))
        .expect("node parses");
        let s = PullRequestSummary::from_graphql_node(node);
        assert_eq!(s.number, 7);
        assert_eq!(s.title, "Add widget");
        assert_eq!(s.html_url, "https://github.com/acme/demo/pull/7");
        assert_eq!(s.state, "open");
        assert!(!s.draft);
        assert_eq!(s.head_ref, "feat/7-widget");
        assert_eq!(s.head_repo_owner, "acme");
        assert_eq!(s.head_repo_clone_url, "https://github.com/acme/demo.git");
        assert_eq!(s.mergeable, Some(true));
        assert_eq!(s.mergeable_state, "clean");
    }

    #[test]
    fn pr_summary_from_graphql_node_unknown_stays_none_not_false() {
        // GitHub still computing: UNKNOWN/UNKNOWN must surface as
        // (None, "unknown") — visually distinct from (Some(false), "dirty").
        let node: GraphQLPrNode = serde_json::from_value(serde_json::json!({
            "number": 8,
            "title": "Fresh PR",
            "state": "OPEN",
            "isDraft": false,
            "mergeable": "UNKNOWN",
            "mergeStateStatus": "UNKNOWN"
        }))
        .expect("node parses");
        let s = PullRequestSummary::from_graphql_node(node);
        assert_eq!(s.mergeable, None);
        assert_eq!(s.mergeable_state, "unknown");
        assert_eq!(s.head_ref, "", "missing head degrades to empty, not failure");
        assert_eq!(s.head_repo_owner, "");
    }

    #[test]
    fn pr_summary_from_graphql_node_tolerates_deleted_fork() {
        // Deleted fork: headRepository null. The row keeps its list fields
        // with empty fork metadata (the spawn path treats empty as
        // same-repo/fail-open) rather than failing the page.
        let node: GraphQLPrNode = serde_json::from_value(serde_json::json!({
            "number": 9,
            "title": "Fork PR",
            "state": "OPEN",
            "isDraft": false,
            "headRefName": "feat/fork",
            "headRefOid": "bbb",
            "headRepository": null,
            "mergeable": "CONFLICTING",
            "mergeStateStatus": "DIRTY"
        }))
        .expect("node parses");
        let s = PullRequestSummary::from_graphql_node(node);
        assert_eq!(s.head_repo_owner, "");
        assert_eq!(s.head_repo_clone_url, "");
        assert_eq!(s.mergeable, Some(false));
        assert_eq!(s.mergeable_state, "dirty");
    }

    /// One scripted interaction for the fake server, in the exact order the
    /// client is expected to issue it. `Page` answers `POST /graphql` with
    /// one connection page; `Detail` answers `GET /repos/.../pulls/{n}`
    /// with a clean/mergeable detail (the per-PR fallback path);
    /// `ListPulls` / `CreatePrConflict` / `CreatePullRequest` /
    /// `CreatePrError` drive the create-PR path tests (issue #771 optimistic
    /// recovery) — same fake, no second server.
    ///
    /// `pub(crate)` so `commands::pr` tests can script the same fake for
    /// the summaries-then-detail fallback without a second server.
    pub(crate) enum Scripted {
        Page(serde_json::Value, bool, Option<String>),
        Detail,
        /// GET `/repos/{o}/{r}/pulls?head=<encoded>&state=open&per_page=1`
        /// — 200 OK with the given JSON array body. `expected_head` is the
        /// EXACT percent-encoded `head` value the request must carry (e.g.
        /// `acme%3Afeat%2F771`); the fake asserts the request line contains
        /// it as a substring so a future regression that drops URL encoding
        /// fails the test rather than corrupting the URL silently.
        ListPulls {
            body: serde_json::Value,
            expected_head: String,
        },
        /// POST `/repos/{o}/{r}/pulls` — 422 Unprocessable Entity with the
        /// given body. Mirrors GitHub's "a pull request already exists"
        /// response. The body must contain the substring "already exists"
        /// for `create_pull_request_idempotent`'s recovery arm to match.
        CreatePrConflict(String),
        /// POST `/repos/{o}/{r}/pulls` — 201 Created with the given PR JSON
        /// body (mirrors GitHub's successful `create_pull_request` response
        /// shape).
        CreatePullRequest(serde_json::Value),
        /// POST `/repos/{o}/{r}/pulls` — non-422 error (e.g. 403, 404, 500)
        /// with the given status + body. Used to verify that the optimistic
        /// recovery path doesn't swallow non-duplicate errors.
        CreatePrError(u16, String),
    }

    /// Spin a fake GitHub server that counts requests and serves `script` in
    /// order. Returns `(base_url, request_count, server_guard)`. Socket
    /// lifecycle, stated precisely so future editors don't misread it:
    /// - The guard serves exactly `script.len()` connections, then exits and
    ///   drops the listener. An over-eager client (N+1 regression) gets
    ///   connection-refused on the extra request, so its call returns `Err`
    ///   and the test fails fast — the counter is the assertion, not the
    ///   join.
    /// - An under-eager client leaves the guard parked in `accept()`; that
    ///   is harmless because every test asserts on payload length / counts
    ///   BEFORE joining, so a short client fails on assertions first and
    ///   never reaches `join`. The parked thread dies with the test process.
    /// - A request of an unexpected kind (POST where Detail was scripted or
    ///   vice versa) panics the guard with the request line, failing loudly.
    fn fake_graphql_server(
        pages: Vec<(serde_json::Value, bool, Option<String>)>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>, std::thread::JoinHandle<()>) {
        fake_server(pages.into_iter().map(|(n, h, c)| Scripted::Page(n, h, c)).collect())
    }

    /// Same fake with an explicit script (pages + REST details in order).
    pub(crate) fn fake_server(
        script: Vec<Scripted>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&count);
        let handle = std::thread::spawn(move || {
            for step in script {
                let (mut sock, _) = listener.accept().expect("accept");
                count_clone.fetch_add(1, Ordering::SeqCst);
                // Read request line + headers, then body per Content-Length.
                let mut reader = BufReader::new(sock.try_clone().expect("clone"));
                let mut request_line = String::new();
                reader.read_line(&mut request_line).expect("read request line");
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("read header");
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some(v) = trimmed.strip_prefix("Content-Length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    } else if let Some(v) = trimmed.strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                if content_length > 0 {
                    let mut body = vec![0u8; content_length];
                    reader.read_exact(&mut body).expect("read body");
                }
                let (status_line, body_bytes): (String, Vec<u8>) = match step {
                    Scripted::Page(nodes, has_next, cursor) => {
                        assert!(
                            request_line.starts_with("POST "),
                            "scripted a GraphQL page but client sent: {}",
                            request_line.trim()
                        );
                        let body = serde_json::json!({
                            "data": {
                                "repository": {
                                    "pullRequests": {
                                        "nodes": nodes,
                                        "pageInfo": {
                                            "hasNextPage": has_next,
                                            "endCursor": cursor,
                                        }
                                    }
                                }
                            }
                        });
                        let bytes = serde_json::to_vec(&body).expect("serialise");
                        ("HTTP/1.1 200 OK\r\n".to_string(), bytes)
                    }
                    Scripted::Detail => {
                        assert!(
                            request_line.starts_with("GET /repos/"),
                            "scripted a REST detail but client sent: {}",
                            request_line.trim()
                        );
                        let body = serde_json::json!({
                            "mergeable": true,
                            "mergeable_state": "clean"
                        });
                        let bytes = serde_json::to_vec(&body).expect("serialise");
                        ("HTTP/1.1 200 OK\r\n".to_string(), bytes)
                    }
                    Scripted::ListPulls { body, expected_head } => {
                        // Optimistic-recovery follow-up: GET /repos/{o}/{r}/pulls?head=<encoded>&state=open
                        // — strict URL assertion via `expected_head` (the percent-encoded
                        // form the client must produce). A regression that drops URL
                        // encoding would corrupt the query string and fail this assertion.
                        assert!(
                            request_line.starts_with("GET ")
                                && request_line.contains("/pulls?head=")
                                && request_line.contains(&format!("head={expected_head}")),
                            "scripted ListPulls expected `head={expected_head}` but client sent: {}",
                            request_line.trim()
                        );
                        let bytes = serde_json::to_vec(&body).expect("serialise");
                        ("HTTP/1.1 200 OK\r\n".to_string(), bytes)
                    }
                    Scripted::CreatePrConflict(body) => {
                        // Optimistic-recovery trigger: POST /repos/{o}/{r}/pulls
                        // → 422 with the duplicate-create body. `create_pull_request_idempotent`
                        // pattern-matches on this status + "already exists" substring.
                        assert!(
                            request_line.starts_with("POST ")
                                && request_line.contains("/pulls"),
                            "scripted a CreatePrConflict but client sent: {}",
                            request_line.trim()
                        );
                        let bytes = body.into_bytes();
                        ("HTTP/1.1 422 Unprocessable Entity\r\n".to_string(), bytes)
                    }
                    Scripted::CreatePullRequest(body) => {
                        assert!(
                            request_line.starts_with("POST ")
                                && request_line.contains("/pulls"),
                            "scripted a CreatePullRequest but client sent: {}",
                            request_line.trim()
                        );
                        let bytes = serde_json::to_vec(&body).expect("serialise");
                        ("HTTP/1.1 201 Created\r\n".to_string(), bytes)
                    }
                    Scripted::CreatePrError(status, body) => {
                        // Non-422 error path — verifies the optimistic helper
                        // doesn't fall through to a recovery GET when the
                        // failure isn't a duplicate-create.
                        assert!(
                            request_line.starts_with("POST ")
                                && request_line.contains("/pulls"),
                            "scripted a CreatePrError but client sent: {}",
                            request_line.trim()
                        );
                        let bytes = body.into_bytes();
                        let reason = reqwest::StatusCode::from_u16(status)
                            .ok()
                            .and_then(|s| s.canonical_reason().map(str::to_string))
                            .unwrap_or_else(|| "Error".to_string());
                        (
                            format!("HTTP/1.1 {status} {reason}\r\n"),
                            bytes,
                        )
                    }
                };
                let http = if body_bytes.is_empty() {
                    format!(
                        "{}Content-Length: 0\r\nConnection: close\r\n\r\n",
                        status_line
                    )
                } else {
                    let body_str = std::str::from_utf8(&body_bytes).expect("utf8 body");
                    format!(
                        "{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status_line,
                        body_str.len(),
                        body_str
                    )
                };
                sock.write_all(http.as_bytes()).expect("write");
            }
        });
        (format!("http://{}", addr), count, handle)
    }

    /// Build one GraphQL node JSON value with a numeric suffix so N PRs are
    /// distinguishable by number/title.
    pub(crate) fn fake_node(n: i64) -> serde_json::Value {
        serde_json::json!({
            "number": n,
            "title": format!("PR {}", n),
            "body": format!("Body {}", n),
            "url": format!("https://github.com/acme/demo/pull/{}", n),
            "state": "OPEN",
            "isDraft": false,
            "headRefName": format!("feat/{}-x", n),
            "headRefOid": "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1",
            "headRepository": {
                "owner": {"login": "acme"},
                "url": "https://github.com/acme/demo"
            },
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN"
        })
    }

    #[test]
    fn list_pr_summaries_costs_one_request_for_one_pr() {
        use std::sync::atomic::Ordering;
        let nodes = serde_json::Value::Array(vec![fake_node(1)]);
        let (base, count, handle) = fake_graphql_server(vec![(nodes, false, None)]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let out = client
            .list_pr_summaries("acme", "demo", "open")
            .expect("summaries");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].number, 1);
        assert_eq!(out[0].mergeable, Some(true));
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "1 PR must cost 1 request, not N+1"
        );
    }

    #[test]
    fn list_pr_summaries_costs_one_request_for_twenty_prs() {
        use std::sync::atomic::Ordering;
        let nodes =
            serde_json::Value::Array((1..=20).map(fake_node).collect::<Vec<_>>());
        let (base, count, handle) = fake_graphql_server(vec![(nodes, false, None)]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let out = client
            .list_pr_summaries("acme", "demo", "open")
            .expect("summaries");
        assert_eq!(out.len(), 20);
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "20 PRs must cost 1 request, not 21"
        );
    }

    #[test]
    fn list_pr_summaries_costs_one_request_for_one_hundred_prs() {
        use std::sync::atomic::Ordering;
        let nodes =
            serde_json::Value::Array((1..=100).map(fake_node).collect::<Vec<_>>());
        let (base, count, handle) = fake_graphql_server(vec![(nodes, false, None)]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let out = client
            .list_pr_summaries("acme", "demo", "open")
            .expect("summaries");
        assert_eq!(out.len(), 100);
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "100 PRs must cost 1 request, not 101"
        );
    }

    #[test]
    fn list_pr_summaries_paginates_by_cursor_not_by_pr() {
        use std::sync::atomic::Ordering;
        // Two pages: 60 + 40. Cost must be 2 (pages), not 100 (PRs).
        let p1 = serde_json::Value::Array((1..=60).map(fake_node).collect::<Vec<_>>());
        let p2 = serde_json::Value::Array((61..=100).map(fake_node).collect::<Vec<_>>());
        let (base, count, handle) = fake_graphql_server(vec![
            (p1, true, Some("cursor1".to_string())),
            (p2, false, None),
        ]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let out = client
            .list_pr_summaries("acme", "demo", "open")
            .expect("summaries");
        assert_eq!(out.len(), 100);
        handle.join().expect("server");
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "two pages must cost 2 requests"
        );
    }

    #[test]
    fn list_pr_summaries_skips_null_nodes_partial_data() {
        use std::sync::atomic::Ordering;
        // One null node (deleted/partial) + one valid node: the page keeps
        // the valid row instead of failing.
        let nodes = serde_json::Value::Array(vec![serde_json::Value::Null, fake_node(2)]);
        let (base, count, handle) = fake_graphql_server(vec![(nodes, false, None)]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let out = client
            .list_pr_summaries("acme", "demo", "open")
            .expect("summaries");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].number, 2);
        handle.join().expect("server");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn list_pr_summaries_propagates_http_errors_for_retry() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        // Minimal 403 server (rate limit): the caller must see Err(Api) so
        // the panel can render a retryable error, not silent unknown rows.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut content_length: usize = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header");
                if line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.trim().strip_prefix("Content-Length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            if content_length > 0 {
                let mut buf = vec![0u8; content_length];
                use std::io::Read;
                reader.read_exact(&mut buf).expect("body");
            }
            let body = r#"{"message":"API rate limit exceeded"}"#;
            let http = format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(http.as_bytes()).expect("write");
        });
        let client =
            GitHubClient::for_test(&format!("http://{}", addr), "fake-token").expect("client");
        let err = client
            .list_pr_summaries("acme", "demo", "open")
            .expect_err("rate limit must propagate");
        match err {
            GitHubError::Api(403, msg) => assert!(msg.contains("rate limit")),
            other => panic!("expected Api(403), got {:?}", other),
        }
        handle.join().expect("server");
    }

    /// Serve one connection with a literal HTTP status + JSON body, for
    /// GraphQL error-envelope shapes the scripted fake cannot express.
    fn fake_graphql_raw_server(
        status_line: &str,
        raw_json: &str,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let status_line = status_line.to_string();
        let raw_json = raw_json.to_string();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut content_length: usize = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header");
                if line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.trim().strip_prefix("Content-Length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            if content_length > 0 {
                let mut buf = vec![0u8; content_length];
                reader.read_exact(&mut buf).expect("body");
            }
            let http = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status_line,
                raw_json.len(),
                raw_json
            );
            sock.write_all(http.as_bytes()).expect("write");
        });
        (format!("http://{}", addr), handle)
    }

    #[test]
    fn list_pr_summaries_reports_graphql_field_errors_not_404() {
        // Regression: GraphQL field errors (rate limit, SAML, permissions)
        // arrive as HTTP 200 with `{"data": {"repository": null}, "errors":
        // [...]}`. A null repository WITH errors is the error itself — it
        // must propagate verbatim, never collapse to a fake 404 "not found".
        let (base, handle) = fake_graphql_raw_server(
            "200 OK",
            r#"{"data": {"repository": null}, "errors": [{"message": "API rate limit exceeded for user ID 123."}]}"#,
        );
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let err = client
            .list_pr_summaries("acme", "demo", "open")
            .expect_err("field error must propagate");
        match err {
            GitHubError::Api(status, msg) => {
                assert_ne!(status, 404, "rate-limit error must not become 404");
                assert!(msg.contains("rate limit"), "got: {}", msg);
            }
            other => panic!("expected Api error, got {:?}", other),
        }
        handle.join().expect("server");
    }

    #[test]
    fn list_pr_summaries_reports_missing_repo_as_404_only_without_errors() {
        // The 404 is reserved for a genuinely absent repository: null with
        // an EMPTY errors array. (No errors key at all parses the same way
        // via #[serde(default)].)
        let (base, handle) = fake_graphql_raw_server(
            "200 OK",
            r#"{"data": {"repository": null}, "errors": []}"#,
        );
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");
        let err = client
            .list_pr_summaries("acme", "demo", "open")
            .expect_err("missing repo must error");
        match err {
            GitHubError::Api(404, msg) => assert!(msg.contains("acme/demo"), "got: {}", msg),
            other => panic!("expected Api(404), got {:?}", other),
        }
        handle.join().expect("server");
    }
}
