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
}

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitHubError::NoToken => write!(f, "No GitHub token found. Set GITHUB_TOKEN env var or authenticate with `gh auth login`."),
            GitHubError::Http(e) => write!(f, "HTTP error: {}", e),
            GitHubError::Api(status, msg) => write!(f, "GitHub API error ({}): {}", status, msg),
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
    #[serde(default)]
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

/// A lightweight GitHub API client.
pub struct GitHubClient {
    client: Client,
    token: String,
}

impl GitHubClient {
    /// Create a new client, resolving the token from environment or gh config.
    pub fn new() -> Result<Self, GitHubError> {
        let token = resolve_token()?;
        let client = build_http_client(HTTP_CONNECT_TIMEOUT, HTTP_REQUEST_TIMEOUT)
            .map_err(GitHubError::Http)?;
        Ok(Self { client, token })
    }

    /// Verify the token is valid by calling GET /user.
    pub fn check_auth(&self) -> bool {
        let resp = self.client
            .get("https://api.github.com/user")
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
        let url = format!(
            "https://api.github.com/repos/{owner}/{repo}/collaborators/{username}/permission"
        );
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
        let url = format!(
            "https://api.github.com/search/issues?q=repo:{}/{}+is:issue+state:open&per_page=100",
            owner, repo
        );
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

    /// Create a pull request. Returns the PR URL.
    pub fn create_pull_request(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<String, GitHubError> {
        let url = format!("https://api.github.com/repos/{}/{}/pulls", owner, repo);

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
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GitHubError::Api(status.as_u16(), body));
        }

        let pr: PullRequest = resp.json()?;
        Ok(pr.html_url)
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
        let url = format!(
            "https://api.github.com/repos/{owner}/{repo}/pulls?head={owner}:{branch}&state=open&per_page=1"
        );
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

    /// List pull requests for a repository, filtered by `state`
    /// (`"open"`, `"closed"`, or `"all"`). Unlike `list_issues_only` (which
    /// uses the search API and wraps results in `items`), the `/pulls`
    /// endpoint returns the array directly.
    pub fn list_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        state: &str,
    ) -> Result<Vec<PullRequest>, GitHubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls?state={}&per_page=100",
            owner, repo, state
        );
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

        let prs: Vec<PullRequest> = resp.json()?;
        Ok(prs)
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
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            owner, repo, pr_number
        );
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
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/files?per_page=100",
            owner, repo, pr_number
        );
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
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/merge",
            owner, repo, pr_number
        );

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
        let pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            owner, repo, pr_number
        );
        let pr_resp = self.client
            .get(&pr_url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
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
                let delete_url = format!(
                    "https://api.github.com/repos/{}/{}/git/refs/heads/{}",
                    owner, repo, detail.head.ref_name
                );
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

/// Retrieve token via `gh auth token` (works when token is in secure storage).
fn run_gh_auth_token() -> Option<String> {
    let output = command_no_window("gh")
        .args(["auth", "token"])
        .output()
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
mod tests {
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
}
