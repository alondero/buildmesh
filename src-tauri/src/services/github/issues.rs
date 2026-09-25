//! Issue listing, Blocked-by parsing, and trigger-label flags.
//!
//! Mesh-owned feed: each method takes the owner/repo the command layer
//! resolved from the mesh remote. The host token stays in [`super::sync`].

use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};

use super::sync::{rest_failure, GitHubClient, GitHubError};

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
    Regex::new(r"`+[^`\n]*`+").expect("MARKDOWN_CODE_RE is a static literal — must compile")
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

impl GitHubClient {
    /// List open issues (excluding pull requests) for a repository.
    pub fn list_issues_only(&self, owner: &str, repo: &str) -> Result<Vec<Issue>, GitHubError> {
        // Use the search API which lets us filter to only issues (not PRs)
        let url = self.rest_url(&format!(
            "/search/issues?q=repo:{}/{}+is:issue+state:open&per_page=100",
            owner, repo
        ));
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(rest_failure(status, body));
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

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&AddLabels {
                labels: vec![label],
            })
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

        let resp = self
            .client
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
            return Err(rest_failure(status, body));
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

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&Comment { body })
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(rest_failure(status, body));
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

        let resp = self
            .client
            .patch(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&CloseState { state: "closed" })
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(rest_failure(status, body));
        }
        let _ = resp.bytes();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            issue.html_url,
            "https://github.com/alondero/buildmesh/issues/358"
        );
        assert_eq!(issue.state, "open");
        // The collaborator gate (ADR-0012 §5) reads `author` from `user.login`.
        assert_eq!(issue.author, "alondero");
        assert_eq!(
            issue.labels,
            vec!["bug".to_string(), "good first issue".to_string()]
        );
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
        assert!(
            issue.labels.is_empty(),
            "missing labels defaults to empty vec"
        );
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
        assert!(
            result.is_err(),
            "label entry without `name` must fail to parse"
        );
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
        struct SearchResult {
            items: Vec<Issue>,
        }
        let result: SearchResult =
            serde_json::from_str(json).expect("search result with null body must parse");
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
        struct SearchResult {
            items: Vec<Issue>,
        }
        let result: SearchResult = serde_json::from_str(json).expect("search result must parse");
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].labels, vec!["bug".to_string()]);
        assert!(result.items[1].labels.is_empty());
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
        body.push_str(
            "\n**Blocked by**\n----------\n\n* [Issue #481](https://github.com/x/y/issues/481)\n",
        );
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
            other => panic!(
                "LabelNotFound must remain a distinct variant; got {:?}",
                other
            ),
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
            other => panic!(
                "422 with 'Label does not exist' must map to LabelNotFound; got {:?}",
                other
            ),
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
            other => panic!(
                "422 without magic string must map to Api, not LabelNotFound; got {:?}",
                other
            ),
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
            other => panic!(
                "empty-body 422 must collapse to LabelNotFound; got {:?}",
                other
            ),
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
            other => panic!(
                "403 must map to Api(403, ...), not LabelNotFound; got {:?}",
                other
            ),
        }
    }

    #[test]
    fn classify_add_label_response_success_returns_ok() {
        // 200 / 201 with any body (including empty) → Ok(()). We don't
        // parse the success body — just need to confirm the classifier
        // doesn't accidentally treat it as an error.
        for status in [reqwest::StatusCode::OK, reqwest::StatusCode::CREATED] {
            let result = GitHubClient::classify_add_label_response(
                status,
                r#"[{"id":1,"name":"buildmesh:run"}]"#,
                "buildmesh:run",
            );
            assert!(
                result.is_ok(),
                "status {} must succeed; got {:?}",
                status,
                result
            );
        }
        // Empty body on success is also fine.
        let result =
            GitHubClient::classify_add_label_response(reqwest::StatusCode::OK, "", "buildmesh:run");
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
            .remove_issue_label(
                "alondero",
                "buildmesh",
                1,
                "definitely-not-on-this-issue-xyz",
            )
            .expect("removing a missing label must collapse to Ok(())");
    }
}
