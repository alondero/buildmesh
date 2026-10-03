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
/// The inner `GitHubClient::check_auth()` does a blocking HTTPS GET to
/// `https://api.github.com/user` (bounded by the client's request timeout),
/// so the command runs it on the blocking pool via `spawn_blocking` — a slow
/// or offline network must never park a Tauri async worker (see the
/// overnight-freeze investigation and [`crate::commands::run_blocking`]).
///
/// Moved from `commands::pr` (issue #433): none of the call sites are
/// PR-related — the function is a general auth check used by
/// `commands::git::get_mesh_git_static`, the mobile `GET /api/gh/auth` HTTP
/// route, and `ProjectSettingsTab.tsx`. The function name is the public
/// Tauri-IPC contract; module path is an internal detail.
#[command]
pub async fn check_gh_auth() -> bool {
    // JoinError (the task panicked) is treated as "not authenticated" — the
    // same fail-closed default as a token-resolution error — but logged so a
    // real panic (poisoned mutex, malformed token state) isn't silently
    // invisible behind a plain `false`.
    match tauri::async_runtime::spawn_blocking(check_gh_auth_blocking).await {
        Ok(authed) => authed,
        Err(e) => {
            tracing::warn!(
                "check_gh_auth: auth-check task failed ({e}); reporting not-authenticated"
            );
            false
        }
    }
}

/// Sync core for [`check_gh_auth`]. Kept as a plain fn so the sync
/// `check_gh_auth_cached` (`commands::git`) and the mobile HTTP route
/// (`http::routes::git`) can call it directly off the async runtime.
pub(crate) fn check_gh_auth_blocking() -> bool {
    match GitHubClient::new() {
        Ok(client) => client.check_auth(),
        Err(_) => false,
    }
}

/// Repository labels for the Issues Probe's tag editor.
#[command]
pub async fn get_repo_labels(mesh_id: i64) -> Result<Vec<String>, String> {
    crate::commands::run_blocking("get_repo_labels", move || {
        let mesh = crate::db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
        let (owner, repo) = super::pr::resolve_github_owner_repo(&mesh)?;
        GitHubClient::new()
            .and_then(|client| client.list_repo_labels(&owner, &repo))
            .map_err(|e| e.to_string())
    })
    .await
}

/// Change one label without replacing labels added by collaborators.
#[command]
pub async fn set_issue_label(
    mesh_id: i64,
    issue_number: i64,
    label: String,
    present: bool,
) -> Result<(), String> {
    if issue_number <= 0 || label.trim().is_empty() {
        return Err("A positive issue number and non-empty label are required".to_string());
    }
    crate::commands::run_blocking("set_issue_label", move || {
        let mesh = crate::db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
        let (owner, repo) = super::pr::resolve_github_owner_repo(&mesh)?;
        let client = GitHubClient::new().map_err(|e| e.to_string())?;
        if present {
            client.add_issue_label(&owner, &repo, issue_number, &label)
        } else {
            client.remove_issue_label_checked(&owner, &repo, issue_number, &label)
        }
        .map_err(|e| e.to_string())
    })
    .await
}
