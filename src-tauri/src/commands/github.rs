//! General GitHub auth Tauri commands.
//!
//! Lives here (not under `commands::pr`) because these commands are about
//! GitHub auth state itself, not pull-request operations. The PR commands in
//! `commands/pr.rs` all need a GitHub token to talk to the REST API, but the
//! auth check is also surfaced independently — see the callers listed in
//! [`check_gh_auth`]'s doc comment — so it earns its own module rather than
//! being grouped with one of its many consumers.

use crate::services::github::GitHubClient;
use tauri::command;

/// Check whether the user has a valid GitHub token (env var or gh config).
///
/// `#[command(async)]` moves this to a worker thread: the inner
/// `GitHubClient::check_auth()` does a blocking HTTPS GET to
/// `https://api.github.com/user` (~100–500 ms), and a slow or offline network
/// must never freeze the main thread (which would stall the whole UI and all
/// other IPC). The same rationale applies to every other command in
/// `commands::pr.rs`.
///
/// Moved from `commands::pr` (issue #433): none of the call sites are
/// PR-related — the function is a general auth check used by
/// `commands::git::get_mesh_git_static`, the mobile `GET /api/gh/auth` HTTP
/// route, and `MeshPropertiesTab.tsx`. The function name is the public
/// Tauri-IPC contract; module path is an internal detail.
#[command(async)]
pub fn check_gh_auth() -> bool {
    match GitHubClient::new() {
        Ok(client) => client.check_auth(),
        Err(_) => false,
    }
}