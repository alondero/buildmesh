//! Shared GitHub fetch policy for the Issues and Pull Requests probes.
//!
//! Host-owned: the token and HTTP client live here. Mesh-owned owner/repo
//! pairs are arguments on the resource methods, not state in this module.
//!
//! [`refresh_decision`] and [`combine_live_and_cache`] are the shared TTL,
//! in-flight coalescing, and live-over-cache rules both probes must use if
//! a snapshot store is added. Live list methods fetch every time today.

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

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
/// ([`GitHubClient::create_pull_request_idempotent`], [`GitHubClient::merge_pull_request`]).
///
/// Read-side calls use `HTTP_REQUEST_TIMEOUT` (30s, bounded by GitHub's
/// pagination). Write-side calls can run 60–90s on a congested network
/// (CI hook initialisation, status checks); a 30s cap would abort
/// slow-but-progressing writes and force a retry, risking a duplicate PR
/// (issue #762). 180s is generous headroom while the 10s connect-timeout
/// keeps a hard network failure prompt.
///
/// **Idempotency (issue #771).** Slow-but-progressing writes that time out
/// client-side may have already succeeded server-side. The
/// `create_pull_request_idempotent` helper recovers a duplicate-create
/// 422 by re-querying `find_open_pr_for_branch`.
pub(super) const HTTP_WRITE_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

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
    pub(super) client: Client,
    pub(super) token: String,
    /// API base without trailing slash (e.g. `https://api.github.com` in
    /// prod, `http://127.0.0.1:<port>` in fake-server tests). All REST paths
    /// are joined onto this; GraphQL appends `/graphql`. Stored (rather
    /// than read from env per-call) so tests can point one client at a fake
    /// server without process-global env races.
    base_url: String,
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
    pub(super) fn rest_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// GraphQL endpoint for the configured base (`<base>/graphql`).
    pub(super) fn graphql_url(&self) -> String {
        format!("{}/graphql", self.base_url)
    }

    /// Verify the token is valid by calling GET /user.
    pub fn check_auth(&self) -> bool {
        let url = self.rest_url("/user");
        let resp = self
            .client
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

    /// Run one `/search/issues` query and parse the `{items: [...]}`
    /// envelope. Shared by the labelled issue/PR ingest queries (issue
    /// #482 / #1208) so the hand-rolled URL encoding lives in exactly
    /// one place: spaces become `+` (GitHub's search-query form),
    /// `"`, `#`, and `&` are percent-encoded.
    pub(super) fn search_issues<T: serde::de::DeserializeOwned>(
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
        let url = self.rest_url(&format!("/search/issues?q={}&per_page=100", encoded));
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
        struct SearchResult<T> {
            items: Vec<T>,
        }

        let result: SearchResult<T> = resp.json()?;
        Ok(result.items)
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
    let output =
        crate::process_util::run_command_with_timeout(cmd, "gh auth token", GH_AUTH_TOKEN_TIMEOUT)
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
        paths.push(
            PathBuf::from(&home)
                .join(".config")
                .join("gh")
                .join("hosts.yml"),
        );
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
        if in_github_section
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && !trimmed.is_empty()
        {
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
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))?;

    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        let repo = parts[1].trim_end_matches(".git");
        Some((parts[0].to_string(), repo.to_string()))
    } else {
        None
    }
}

/// How long a successful Issues or Pull Requests probe read stays fresh
/// enough to skip another background fetch. Both probes share one constant
/// so they cannot drift into two cadences. Recency is in-memory only: a
/// process start is always stale, the same rule as the spawn-time fetch
/// TTL (a restart must not trust a snapshot from a previous run).
///
/// Live list methods do not consult this yet; they fetch every time.
#[cfg_attr(not(test), allow(dead_code))]
pub const PROBE_FETCH_TTL: Duration = Duration::from_secs(60);

/// Whether a probe refresh should hit the network.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshDecision {
    /// Last success is still inside [`PROBE_FETCH_TTL`].
    ServeCached,
    /// No fresh success, and no fetch is running. Start one live read.
    FetchNow,
    /// A live read is already running. Do not start a second one.
    JoinInFlight,
}

/// `age` is the time since the last *successful* live read. `None` means
/// this process has not seen a success. A fresh snapshot is served even
/// when some other refresh is in flight — an in-flight call must not hide
/// a read that already succeeded. Once the snapshot is stale, an in-flight
/// refresh coalesces further callers onto that one request.
#[cfg_attr(not(test), allow(dead_code))]
pub fn refresh_decision(age: Option<Duration>, refresh_in_flight: bool) -> RefreshDecision {
    if matches!(age, Some(age) if age < PROBE_FETCH_TTL) {
        return RefreshDecision::ServeCached;
    }
    if refresh_in_flight {
        return RefreshDecision::JoinInFlight;
    }
    RefreshDecision::FetchNow
}

/// What a probe should show after a live attempt and an optional snapshot.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeRead<T, E> {
    /// The live fetch succeeded. Show this, not the cache.
    Live(T),
    /// The live fetch failed and a prior snapshot exists (offline).
    Cached(T),
    /// The live fetch failed and there is no snapshot.
    Unavailable(E),
}

/// Merge a live attempt with an optional cached snapshot.
///
/// A successful live value always wins, including when `cache` is missing
/// or older than [`PROBE_FETCH_TTL`]. The cache is consulted only after
/// the live fetch fails, and a missing cache then surfaces that failure
/// rather than an empty success. That is the #1073 rule: a fallback must
/// not discard a live result that already succeeded.
#[cfg_attr(not(test), allow(dead_code))]
pub fn combine_live_and_cache<T, E>(live: Result<T, E>, cache: Option<T>) -> ProbeRead<T, E> {
    match live {
        Ok(value) => ProbeRead::Live(value),
        Err(err) => match cache {
            Some(cached) => ProbeRead::Cached(cached),
            None => ProbeRead::Unavailable(err),
        },
    }
}

/// True when a GitHub error body is a primary or secondary rate limit.
/// Callers keep the upstream status and body; this only decides whether
/// to log the failure as a rate limit rather than a missing repository.
pub(super) fn is_rate_limit_body(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("rate limit") || lower.contains("secondary rate")
}

/// Non-success REST responses stay [`GitHubError::Api`]. A 429, or a body
/// that says the call was rate limited, is logged and still returned as
/// that API error — it is never rewritten into "not found".
pub(super) fn rest_failure(status: reqwest::StatusCode, body: String) -> GitHubError {
    if status.as_u16() == 429 || is_rate_limit_body(&body) {
        tracing::warn!(
            "GitHub rate limit ({}) — not a missing repository: {}",
            status.as_u16(),
            body
        );
    }
    GitHubError::Api(status.as_u16(), body)
}

/// `repository: null` on a GraphQL payload. An `errors` entry is the
/// failure itself (rate limit, SAML, permissions) and keeps the HTTP
/// status. Only an error-free null repository is "no such repo" (404).
pub(super) fn graphql_repository_or_error<T>(
    repository: Option<T>,
    errors: Option<String>,
    http_status: u16,
    owner: &str,
    repo: &str,
) -> Result<(T, Option<String>), GitHubError> {
    match repository {
        Some(value) => Ok((value, errors)),
        None => Err(missing_graphql_repository(errors, http_status, owner, repo)),
    }
}

fn missing_graphql_repository(
    errors: Option<String>,
    http_status: u16,
    owner: &str,
    repo: &str,
) -> GitHubError {
    match errors {
        Some(msg) => {
            if is_rate_limit_body(&msg) {
                tracing::warn!(
                    "GitHub rate limit while reading {}/{} pull requests: {}",
                    owner,
                    repo,
                    msg
                );
            }
            GitHubError::Api(http_status, msg)
        }
        None => GitHubError::Api(404, format!("repository {owner}/{repo} not found")),
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
        assert_eq!(
            result,
            Some(("alondero".to_string(), "buildmesh".to_string()))
        );
    }

    #[test]
    fn test_parse_owner_repo_ssh() {
        let result = parse_owner_repo("git@github.com:alondero/buildmesh.git");
        assert_eq!(
            result,
            Some(("alondero".to_string(), "buildmesh".to_string()))
        );
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
        assert_eq!(
            parse_gh_hosts_yaml(content),
            Some("gho_abc123def456".to_string())
        );
    }

    #[test]
    fn test_parse_gh_hosts_yaml_quoted() {
        let content = r#""github.com":
    oauth_token: "gho_quoted_token"
"#;
        assert_eq!(
            parse_gh_hosts_yaml(content),
            Some("gho_quoted_token".to_string())
        );
    }

    #[test]
    fn test_parse_gh_hosts_yaml_missing() {
        let content = r#"gitlab.com:
    oauth_token: gho_wrong
"#;
        assert_eq!(parse_gh_hosts_yaml(content), None);
    }

    #[test]
    fn fresh_snapshot_skips_a_background_fetch() {
        assert_eq!(
            refresh_decision(Some(Duration::from_secs(1)), false),
            RefreshDecision::ServeCached
        );
    }

    #[test]
    fn ttl_expiry_starts_a_fetch() {
        assert_eq!(
            refresh_decision(Some(PROBE_FETCH_TTL), false),
            RefreshDecision::FetchNow,
            "age equal to the TTL is expired"
        );
        assert_eq!(
            refresh_decision(Some(PROBE_FETCH_TTL - Duration::from_nanos(1)), false),
            RefreshDecision::ServeCached
        );
    }

    #[test]
    fn cold_start_starts_a_fetch() {
        assert_eq!(refresh_decision(None, false), RefreshDecision::FetchNow);
    }

    #[test]
    fn in_flight_refresh_coalesces_when_the_snapshot_is_stale() {
        assert_eq!(
            refresh_decision(Some(PROBE_FETCH_TTL), true),
            RefreshDecision::JoinInFlight
        );
        assert_eq!(
            refresh_decision(None, true),
            RefreshDecision::JoinInFlight,
            "a second caller must not start its own fetch"
        );
    }

    #[test]
    fn fresh_snapshot_is_served_even_if_another_refresh_is_in_flight() {
        assert_eq!(
            refresh_decision(Some(Duration::from_secs(5)), true),
            RefreshDecision::ServeCached
        );
    }

    #[test]
    fn successful_live_fetch_wins_over_a_missing_cache() {
        let read = combine_live_and_cache::<&str, &str>(Ok("live"), None);
        assert_eq!(read, ProbeRead::Live("live"));
    }

    #[test]
    fn successful_live_fetch_wins_over_a_stale_cache() {
        let read: ProbeRead<&str, &str> = combine_live_and_cache(Ok("live"), Some("stale"));
        assert_eq!(
            read,
            ProbeRead::Live("live"),
            "a stale or present cache must not replace a live success"
        );
    }

    #[test]
    fn failed_live_fetch_falls_back_to_cache_when_offline() {
        let read = combine_live_and_cache(Err("offline"), Some("cached"));
        assert_eq!(read, ProbeRead::Cached("cached"));
    }

    #[test]
    fn failed_live_fetch_without_cache_stays_unavailable() {
        let read = combine_live_and_cache::<&str, &str>(Err("offline"), None);
        assert_eq!(read, ProbeRead::Unavailable("offline"));
    }

    #[test]
    fn rate_limit_graphql_error_is_not_rewritten_as_not_found() {
        let err = graphql_repository_or_error(
            None::<()>,
            Some("API rate limit exceeded for user ID 123.".to_string()),
            200,
            "acme",
            "demo",
        )
        .expect_err("rate limit must fail the read");
        match err {
            GitHubError::Api(status, msg) => {
                assert_ne!(status, 404, "rate-limit error must not become 404");
                assert!(msg.contains("rate limit"), "got: {msg}");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn missing_repository_without_graphql_errors_is_404() {
        let err = graphql_repository_or_error(None::<()>, None, 200, "acme", "demo")
            .expect_err("missing repo must fail");
        match err {
            GitHubError::Api(404, msg) => assert!(msg.contains("acme/demo"), "got: {msg}"),
            other => panic!("expected Api(404), got {other:?}"),
        }
    }

    #[test]
    fn rest_rate_limit_preserves_status_and_body() {
        let err = rest_failure(
            reqwest::StatusCode::FORBIDDEN,
            "API rate limit exceeded".to_string(),
        );
        match err {
            GitHubError::Api(403, msg) => assert!(msg.contains("rate limit")),
            other => panic!("expected Api(403), got {other:?}"),
        }
    }
}
