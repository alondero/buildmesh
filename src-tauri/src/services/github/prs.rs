//! Pull-request listing, merge strategy, and contributor data.
//!
//! Mesh-owned feed: each method takes the owner/repo the command layer
//! resolved from the mesh remote. The host token stays in [`super::sync`].

use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};

use super::sync::{
    graphql_repository_or_error, rest_failure, GitHubClient, GitHubError, HTTP_WRITE_REQUEST_TIMEOUT,
};

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

/// Parameters for [`GitHubClient::create_pull_request_idempotent`] (and the
/// low-level [`GitHubClient::create_pull_request_details`]). A typed struct
/// rather than positional `&str`s so the six string fields can't be silently
/// transposed. `'a` lifetime so callers pass borrowed `&str`s without an
/// allocation.
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
        let resp = self
            .client
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
            return Err(rest_failure(status, body));
        }

        #[derive(Deserialize)]
        struct PermissionResponse {
            #[serde(default)]
            permission: String,
        }
        let parsed: PermissionResponse = resp.json()?;
        Ok(CollaboratorPermission::from_api_str(&parsed.permission))
    }

    /// Low-level primitive — direct `POST /pulls` with no idempotency
    /// recovery. Callers that need to handle the "retry after slow POST
    /// timed out" duplicate-PR case should use
    /// [`create_pull_request_idempotent`](Self::create_pull_request_idempotent)
    /// instead. Kept `pub` (not `pub(crate)`) because future callers may
    /// legitimately want the non-idempotent version (e.g. dry-run tooling
    /// that wants to surface GitHub's raw 422 verbatim).
    pub fn create_pull_request_details(
        &self,
        req: CreatePrRequest<'_>,
    ) -> Result<PullRequest, GitHubError> {
        let CreatePrRequest {
            owner,
            repo,
            title,
            body,
            head,
            base,
        } = req;
        let url = self.rest_url(&format!("/repos/{}/{}/pulls", owner, repo));

        #[derive(Serialize)]
        struct CreatePrBody<'a> {
            title: &'a str,
            body: &'a str,
            head: &'a str,
            base: &'a str,
        }

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&CreatePrBody {
                title,
                body,
                head,
                base,
            })
            .timeout(HTTP_WRITE_REQUEST_TIMEOUT)
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(rest_failure(status, body));
        }

        resp.json().map_err(GitHubError::from)
    }

    /// Create a pull request, recovering from GitHub's
    /// "a pull request already exists" 422 on retry (issue #771).
    ///
    /// On `POST /pulls` returning 422 with a structured `errors[].message`
    /// of "A pull request already exists …", call `find_open_pr_for_branch`
    /// to look up the existing PR and return it. Other 422 shapes
    /// (missing branch, protected-branch rejection, etc.) propagate
    /// unchanged so the caller can distinguish duplicate from validation.
    /// The recovery discards the user-supplied `title` and `body` — that
    /// is the point of the recovery — and logs a `tracing::warn!` so
    /// "why didn't my title apply" is auditable.
    pub fn create_pull_request_idempotent(
        &self,
        req: CreatePrRequest<'_>,
    ) -> Result<PullRequest, GitHubError> {
        match self.create_pull_request_details(req) {
            Ok(pr) => Ok(pr),
            Err(GitHubError::Api(422, ref_body)) if is_duplicate_pr_error(&ref_body) => {
                let CreatePrRequest {
                    owner, repo, head, ..
                } = req;
                tracing::warn!(
                    "POST /pulls returned 422 'already exists' for {owner}/{repo} head={head} — recovering via find_open_pr_for_branch; user-supplied title/body discarded"
                );
                match self.find_open_pr_for_branch(owner, repo, head)? {
                    Some(existing) => Ok(existing),
                    // 422 said "exists" but the recovery GET returned nothing.
                    // Almost certainly a permission scope mismatch (the POST
                    // scope sees the PR; the GET scope doesn't). Surface the
                    // original 422 so the caller sees the real diagnostic.
                    None => Err(GitHubError::Api(422, ref_body.clone())),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Find the first open pull request whose `head` matches.
    /// Returns `Ok(None)` when the repository or branch is unknown to GitHub
    /// (treated as "no PR" — common for never-pushed branches). Other
    /// non-success statuses propagate as `GitHubError::Api`.
    ///
    /// `head` is the value GitHub's `head=OWNER:BRANCH` filter expects. If
    /// it already contains a `:` (i.e. the caller has pre-qualified it, e.g.
    /// `fork_user:branch` for a cross-repo PR from a fork), it is used
    /// verbatim — otherwise `owner:` is prepended. Branch names without a
    /// `:` are never pre-qualified by the GitHub API.
    pub fn find_open_pr_for_branch(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
    ) -> Result<Option<PullRequest>, GitHubError> {
        // GitHub's `head=OWNER:BRANCH` filter matches the head ref of a PR.
        // The `state=open` filter is the only thing we care about; `per_page=1`
        // is the invariant: one branch → at most one open PR.
        //
        // The `head` value is passed through `RequestBuilder::query`, which
        // percent-encodes `:` / `/` / `&` / `?` / `#` via `serde_urlencoded`
        // (the same encoding the prior hand-rolled `percent_encode_path_component`
        // produced for these characters). That keeps the wire shape
        // byte-identical for the common case while removing the dual-purpose
        // path-component encoder that masked the fact that the param lives
        // in a query string.
        let head_value = if head.contains(':') {
            head.to_string()
        } else {
            format!("{owner}:{head}")
        };
        let url = self.rest_url(&format!("/repos/{owner}/{repo}/pulls"));
        let resp = self
            .client
            .get(&url)
            .query(&[
                ("head", head_value.as_str()),
                ("state", "open"),
                ("per_page", "1"),
            ])
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
            return Err(rest_failure(status, body));
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
        let resp = self
            .client
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
        Err(rest_failure(status, body))
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
        let url = self.rest_url(&format!("/repos/{}/{}/pulls/{}", owner, repo, pr_number));
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

        // The endpoint returns a bare array, NOT a `{files: [...]}` wrapper.
        let files: Vec<PrFile> = resp.json()?;
        Ok(files)
    }

    /// Merge a pull request with the caller-chosen merge method
    /// (`merge`, `squash`, or `rebase` — GitHub's REST vocabulary) and
    /// delete the branch. The method used is echoed back in the success
    /// message so the UI never has to guess which strategy landed.
    ///
    /// The `merge_method` argument is validated at the seam in
    /// `commands::pr::merge_pr_blocking` — by the time it reaches this
    /// fn it is one of the three literals above, so the request body
    /// can carry it verbatim.
    pub fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: i64,
        merge_method: &str,
    ) -> Result<String, GitHubError> {
        let url = self.rest_url(&format!(
            "/repos/{}/{}/pulls/{}/merge",
            owner, repo, pr_number
        ));

        #[derive(Serialize)]
        struct MergePr<'a> {
            merge_method: &'a str,
        }

        let resp = self
            .client
            .put(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .json(&MergePr { merge_method })
            .timeout(HTTP_WRITE_REQUEST_TIMEOUT)
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(rest_failure(status, body));
        }

        #[derive(Deserialize)]
        struct MergeResult {
            #[serde(default)]
            message: String,
            sha: String,
        }

        let result: MergeResult = resp.json()?;

        // Now delete the branch. First, get the PR to find the head ref.
        let pr_url = self.rest_url(&format!("/repos/{}/{}/pulls/{}", owner, repo, pr_number));
        // Post-merge read: the merge already succeeded, so this GET is
        // best-effort. The 30s default would otherwise abort the function
        // with `Err` even though GitHub confirms the merge — use the write
        // timeout so a slow followup can't undo a successful merge in the
        // caller's view (issue #762 review).
        let pr_resp = self
            .client
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
                let _ = self
                    .client
                    .delete(&delete_url)
                    .header(AUTHORIZATION, format!("Bearer {}", self.token))
                    .header(USER_AGENT, "buildmesh")
                    .header(ACCEPT, "application/vnd.github+json")
                    .send();
            }
        }

        Ok(format!(
            "Merged ({}) via {} — {}",
            merge_method, result.sha, result.message
        ))
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
            return Err(rest_failure(status, text));
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
        // `repository: null` WITH errors is the error itself (rate limit,
        // SAML enforcement, missing permission) — never a 404. Only an
        // error-free null repository means "no such repo".
        let (repo_data, errors) = graphql_repository_or_error(
            data.repository,
            joined_errors,
            status.as_u16(),
            owner,
            repo,
        )?;
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

    /// Cohesive PR-summary query (issue #1529).
    ///
    /// Cost is proportional to pages, not PR count: one GraphQL connection
    /// request per page of up to 100 PRs. The UI calls this through
    /// `get_repo_pulls` and never issues per-PR detail requests.
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
    /// GitHub login of the PR's author — the contributor pill on the PRs
    /// probe row links to `https://github.com/<login>`. Empty when the
    /// GraphQL node omits the author (the pill then doesn't render).
    pub author: String,
    /// `Some(true)` mergeable, `Some(false)` conflicting, `None` while
    /// GitHub is still computing (`UNKNOWN`) — mirrors the REST detail's
    /// `mergeable: null` contract so the panel's "checking" state is
    /// preserved without a second request.
    pub mergeable: Option<bool>,
    /// Lowercase REST vocabulary (`clean`, `dirty`, `blocked`, `behind`,
    /// `unstable`, `unknown`, …) mapped from GraphQL `mergeStateStatus`.
    pub mergeable_state: String,
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
        author { login }
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
    /// The PR's author — the contributor pill on the PRs probe row links
    /// to `https://github.com/<login>`.
    #[serde(default)]
    author: Option<GraphQLAuthor>,
    #[serde(default, rename = "headRepository")]
    head_repository: Option<GraphQLHeadRepo>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default, rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
}

/// `author { login }` on the summaries node. GraphQL's `author` is an
/// `Actor` (users AND bots), so the field is `Option` throughout: a bot
/// or deleted account still yields a login, but a missing object degrades
/// to an empty string on the summary rather than failing the page.
#[derive(Debug, Deserialize)]
struct GraphQLAuthor {
    #[serde(default)]
    login: Option<String>,
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
                let owner = repo.owner.and_then(|o| o.login).unwrap_or_default();
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
            state: node
                .state
                .map(|s| map_graphql_state(&s))
                .unwrap_or_else(|| "open".to_string()),
            draft: node.is_draft.unwrap_or(false),
            head_ref: node.head_ref_name.unwrap_or_default(),
            head_repo_owner: head_owner,
            head_repo_clone_url: head_clone_url,
            head_sha: node.head_ref_oid.unwrap_or_default(),
            author: node.author.and_then(|a| a.login).unwrap_or_default(),
            mergeable: node
                .mergeable
                .as_deref()
                .map(map_graphql_mergeable)
                .unwrap_or(None),
            mergeable_state: node
                .merge_state_status
                .as_deref()
                .map(map_graphql_merge_state)
                .unwrap_or_else(|| "unknown".to_string()),
        }
    }
}

/// True iff the body of a GitHub 422 response signals that the requested
/// pull request already exists. Matches GitHub's structured
/// `errors[].message` of the form "A pull request already exists for …"
/// (case-insensitive). The structured match avoids false positives from
/// unrelated validation failures whose bodies happen to echo user-supplied
/// fields — e.g. a title that includes the literal phrase "already exists"
/// would otherwise incorrectly trigger the recovery path on a
/// protected-branch rejection.
fn is_duplicate_pr_error(body: &str) -> bool {
    #[derive(Deserialize)]
    struct ApiError {
        #[serde(default)]
        message: Option<String>,
    }
    #[derive(Deserialize)]
    struct ApiErrorEnvelope {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        errors: Vec<ApiError>,
    }
    let parsed: ApiErrorEnvelope = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let needle = "already exists";
    if parsed.errors.iter().any(|e| {
        e.message
            .as_deref()
            .is_some_and(|m| m.to_ascii_lowercase().contains(needle))
    }) {
        return true;
    }
    parsed
        .message
        .as_deref()
        .is_some_and(|m| m.to_ascii_lowercase().contains(needle))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Collaborator permission — the wire→enum mapping the Autopilot gate keys
    // off (ADR-0012 §5). GitHub's legacy `permission` field is one of
    // admin/write/read/none; `has_push_access` is the trust boundary.
    // -----------------------------------------------------------------------

    #[test]
    fn collaborator_permission_maps_legacy_values() {
        assert_eq!(
            CollaboratorPermission::from_api_str("admin"),
            CollaboratorPermission::Admin
        );
        assert_eq!(
            CollaboratorPermission::from_api_str("write"),
            CollaboratorPermission::Write
        );
        assert_eq!(
            CollaboratorPermission::from_api_str("read"),
            CollaboratorPermission::Read
        );
        assert_eq!(
            CollaboratorPermission::from_api_str("none"),
            CollaboratorPermission::None
        );
    }

    #[test]
    fn collaborator_permission_maps_granular_roles_and_is_case_insensitive() {
        // `maintain` can push → Write; `triage` cannot → Read. Mixed case and
        // stray whitespace (defensive) still parse.
        assert_eq!(
            CollaboratorPermission::from_api_str("  Maintain "),
            CollaboratorPermission::Write
        );
        assert_eq!(
            CollaboratorPermission::from_api_str("TRIAGE"),
            CollaboratorPermission::Read
        );
    }

    #[test]
    fn collaborator_permission_unknown_value_is_conservative_none() {
        // An unrecognised level must never grant push — it falls to None.
        assert_eq!(
            CollaboratorPermission::from_api_str("superadmin"),
            CollaboratorPermission::None
        );
        assert_eq!(
            CollaboratorPermission::from_api_str(""),
            CollaboratorPermission::None
        );
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
        assert_eq!(
            pr.html_url,
            "https://github.com/alondero/buildmesh/pull/412"
        );
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

        let clean: Detail =
            serde_json::from_str(r#"{"mergeable": true, "mergeable_state": "clean"}"#).unwrap();
        assert_eq!(clean.mergeable, Some(true));
        assert_eq!(clean.mergeable_state, "clean");

        let dirty: Detail =
            serde_json::from_str(r#"{"mergeable": false, "mergeable_state": "dirty"}"#).unwrap();
        assert_eq!(dirty.mergeable, Some(false));
        assert_eq!(dirty.mergeable_state, "dirty");

        // `null` (still computing) must stay `None`, not become `false`.
        let computing: Detail =
            serde_json::from_str(r#"{"mergeable": null, "mergeable_state": "unknown"}"#).unwrap();
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
        assert!(
            file.patch.starts_with("@@"),
            "patch should round-trip verbatim"
        );
        assert!(
            file.previous_filename.is_none(),
            "no rename → no previous_filename"
        );
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
            "author": {"login": "contributor-jane"},
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
        // The contributor pill on the PRs probe row reads `author` from
        // the `author { login }` selection — pin the projection so the
        // field can't silently drop off the query.
        assert_eq!(s.author, "contributor-jane");
        assert_eq!(s.mergeable, Some(true));
        assert_eq!(s.mergeable_state, "clean");
    }

    #[test]
    fn pr_summary_from_graphql_node_missing_author_defaults_empty() {
        // A node without `author` (deleted account edge cases, older
        // cached payloads) must degrade to `\"\"` — the pill simply doesn't
        // render — never fail the page.
        let node: GraphQLPrNode = serde_json::from_value(serde_json::json!({
            "number": 10,
            "title": "Authorless PR",
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN"
        }))
        .expect("node parses");
        let s = PullRequestSummary::from_graphql_node(node);
        assert_eq!(s.author, "", "missing author must default to empty");
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
        assert_eq!(
            s.head_ref, "",
            "missing head degrades to empty, not failure"
        );
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
        /// PUT `/repos/{o}/{r}/pulls/{n}/merge` — 200 OK with a merged
        /// result body. The fake asserts the request line is the merge
        /// endpoint AND that the request body's `merge_method` equals
        /// `expected_method` — that's the assertion that pins the
        /// merge-strategy dropdown (squash / merge / rebase) reaching
        /// GitHub verbatim. Follow with `PullHead` for the post-merge
        /// branch-cleanup read.
        MergePullRequest {
            expected_method: String,
        },
        /// GET `/repos/{o}/{r}/pulls/{n}` — 200 OK with a head-ref body.
        /// Serves the post-merge read that finds the branch to delete.
        PullHead,
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
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        fake_server(
            pages
                .into_iter()
                .map(|(n, h, c)| Scripted::Page(n, h, c))
                .collect(),
        )
    }

    /// Same fake with an explicit script (pages + REST details in order).
    pub(crate) fn fake_server(
        script: Vec<Scripted>,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
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
                reader
                    .read_line(&mut request_line)
                    .expect("read request line");
                let mut body_text = String::new();
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
                    body_text = String::from_utf8(body).expect("utf8 request body");
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
                    Scripted::ListPulls {
                        body,
                        expected_head,
                    } => {
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
                            request_line.starts_with("POST ") && request_line.contains("/pulls"),
                            "scripted a CreatePrConflict but client sent: {}",
                            request_line.trim()
                        );
                        let bytes = body.into_bytes();
                        ("HTTP/1.1 422 Unprocessable Entity\r\n".to_string(), bytes)
                    }
                    Scripted::CreatePullRequest(body) => {
                        assert!(
                            request_line.starts_with("POST ") && request_line.contains("/pulls"),
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
                            request_line.starts_with("POST ") && request_line.contains("/pulls"),
                            "scripted a CreatePrError but client sent: {}",
                            request_line.trim()
                        );
                        let bytes = body.into_bytes();
                        let reason = reqwest::StatusCode::from_u16(status)
                            .ok()
                            .and_then(|s| s.canonical_reason().map(str::to_string))
                            .unwrap_or_else(|| "Error".to_string());
                        (format!("HTTP/1.1 {status} {reason}\r\n"), bytes)
                    }
                    Scripted::MergePullRequest { expected_method } => {
                        assert!(
                            request_line.starts_with("PUT ") && request_line.contains("/merge"),
                            "scripted a MergePullRequest but client sent: {}",
                            request_line.trim()
                        );
                        // The whole point of the merge-strategy dropdown:
                        // the method the user picked must reach GitHub
                        // verbatim in the request body.
                        assert!(
                            body_text.contains(&format!("\"merge_method\":\"{expected_method}\"")),
                            "merge body must carry merge_method {expected_method:?}, got: {body_text}"
                        );
                        let body = serde_json::json!({
                            "sha": "abc123merged",
                            "merged": true,
                            "message": "Pull Request successfully merged"
                        });
                        let bytes = serde_json::to_vec(&body).expect("serialise");
                        ("HTTP/1.1 200 OK\r\n".to_string(), bytes)
                    }
                    Scripted::PullHead => {
                        assert!(
                            request_line.starts_with("GET /repos/")
                                && request_line.contains("/pulls/"),
                            "scripted a PullHead but client sent: {}",
                            request_line.trim()
                        );
                        let body = serde_json::json!({
                            "head": { "ref": "feat/merged-branch" }
                        });
                        let bytes = serde_json::to_vec(&body).expect("serialise");
                        ("HTTP/1.1 200 OK\r\n".to_string(), bytes)
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
        let nodes = serde_json::Value::Array((1..=20).map(fake_node).collect::<Vec<_>>());
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
        let nodes = serde_json::Value::Array((1..=100).map(fake_node).collect::<Vec<_>>());
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

    // -----------------------------------------------------------------------
    // Merge method plumbing (merge-strategy dropdown). The PUT body must
    // carry the caller's method verbatim — the fake server asserts the
    // request body's `merge_method`, so a regression that hard-codes
    // `"squash"` (or drops the field) fails the test.
    // -----------------------------------------------------------------------

    #[test]
    fn merge_pull_request_sends_the_chosen_method_and_echoes_it() {
        let (base, count, handle) = fake_server(vec![
            Scripted::MergePullRequest {
                expected_method: "rebase".to_string(),
            },
            Scripted::PullHead,
        ]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let msg = client
            .merge_pull_request("acme", "demo", 7, "rebase")
            .expect("merge must succeed");

        assert!(
            msg.contains("Merged (rebase)"),
            "success message must name the method used, got: {msg}"
        );
        handle.join().expect("server");
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "exactly the merge PUT + the post-merge head read"
        );
    }

    #[test]
    fn merge_pull_request_defaults_path_sends_squash() {
        // The historical behaviour: squash + delete branch. Pin it so the
        // new parameter is visibly opt-in per call site.
        let (base, _count, handle) = fake_server(vec![
            Scripted::MergePullRequest {
                expected_method: "squash".to_string(),
            },
            Scripted::PullHead,
        ]);
        let client = GitHubClient::for_test(&base, "fake-token").expect("client");

        let msg = client
            .merge_pull_request("acme", "demo", 7, "squash")
            .expect("merge must succeed");

        assert!(msg.contains("Merged (squash)"), "got: {msg}");
        handle.join().expect("server");
    }

    #[test]
    fn list_pr_summaries_reports_missing_repo_as_404_only_without_errors() {
        // The 404 is reserved for a genuinely absent repository: null with
        // an EMPTY errors array. (No errors key at all parses the same way
        // via #[serde(default)].)
        let (base, handle) =
            fake_graphql_raw_server("200 OK", r#"{"data": {"repository": null}, "errors": []}"#);
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
