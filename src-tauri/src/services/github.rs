//! GitHub REST API service — replaces `gh` CLI calls with direct HTTP requests.

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: i64,
    pub html_url: String,
    /// Human-readable PR title — surfaced in the chip tooltip.
    #[serde(default)]
    pub title: String,
    /// `true` for draft PRs. GitHub always returns this field on `/pulls` responses.
    #[serde(default)]
    pub draft: bool,
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
        let client = Client::builder()
            .build()
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
}
