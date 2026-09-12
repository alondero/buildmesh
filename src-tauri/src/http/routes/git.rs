//! Per-agent git inspection routes.
//!
//! These power the mobile "Changes" view: a tree of edited files, a +/~/-
//! summary in the header, and an inline diff for each tapped file. All
//! routes resolve against the agent node's worktree (its `path` column),
//! so they work whether the agent runs in-place or in a `git worktree`.
//!
//! **Threading model (issue #762):** every backing Tauri command was moved
//! onto the blocking pool via `crate::commands::run_blocking`. The routes
//! here `.await` the async wrapper so the actual libgit2 work runs on
//! `tauri::async_runtime::spawn_blocking`'s pool rather than the bounded
//! tokio worker pool — mirroring the desktop fix and keeping mobile
//! sessions from parking their accept-loop worker on a stalled
//! `Repository::open` (e.g. WSL UNC path with paused VM).

use crate::db;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;

/// `GET /api/agents/{id}/git/status` — the file-by-file tree of everything the
/// node changed since it branched (ADR 0005), so it agrees with the per-file
/// `/diff` endpoint below. Empty list for a node with no changes since base.
pub async fn status(req: &ParsedRequest) -> Response {
    let agent_id = req.id0();
    if db::get_agent_node_by_id(agent_id).is_err() {
        return Response::json_error("404 Not Found", "Agent not found");
    }
    match crate::commands::diff::node_changed_files(agent_id).await {
        Ok(status) => {
            let body = serde_json::to_string(&status).unwrap_or_else(|_| "[]".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

/// `GET /api/agents/{id}/git/summary` — `{total, added, modified, deleted}`
/// counts, folded from the same since-branch list as `/git/status` so the
/// header and the tree agree.
pub async fn summary(req: &ParsedRequest) -> Response {
    let agent_id = req.id0();
    if db::get_agent_node_by_id(agent_id).is_err() {
        return Response::json_error("404 Not Found", "Agent not found");
    }
    match crate::commands::diff::node_changed_summary(agent_id).await {
        Ok(summary) => {
            let body = serde_json::to_string(&summary).unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

/// `GET /api/agents/{id}/git/branch` — current branch name.
pub async fn branch(req: &ParsedRequest) -> Response {
    let agent_id = req.id0();
    match crate::commands::pr::get_current_branch(agent_id).await {
        Ok(name) => {
            let body = serde_json::to_string(&serde_json::json!({ "branch": name }))
                .unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("404 Not Found", &e),
    }
}

/// `GET /api/agents/{id}/diff?path=<relative-file-path>` — file diff against
/// the agent's merge-base with its mesh `base_ref` (ADR 0005), so committed and
/// uncommitted agent work both show. Rendered server-side via
/// `commands::diff::diff_node_file_against_base` so the mobile UI gets
/// pre-highlighted hunks ready to display.
pub async fn diff(req: &ParsedRequest) -> Response {
    let agent_id = req.id0();
    let file_path = req.query_param("path").unwrap_or_default();
    if file_path.is_empty() {
        return Response::json_error("400 Bad Request", "Missing ?path=");
    }
    // Defense against a request like `?path=../../etc/passwd` — the diff
    // command lives inside the agent's worktree, but any caller using a
    // relative-up path could break out. Reject path traversal.
    if file_path
        .split(['/', '\\'])
        .any(|seg| seg == ".." || seg.is_empty())
    {
        return Response::json_error("400 Bad Request", "Invalid path");
    }
    if db::get_agent_node_by_id(agent_id).is_err() {
        return Response::json_error("404 Not Found", "Agent not found");
    }
    match crate::commands::diff::diff_node_file_against_base(agent_id, file_path).await {
        Ok(diff) => {
            let body = serde_json::to_string(&diff).unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

/// `GET /api/gh/auth` — whether `gh auth status` is happy on this host.
/// Used to gate the mobile "Create PR" button.
pub async fn gh_auth(_req: &ParsedRequest) -> Response {
    // Await the async wrapper so the blocking auth HTTPS GET runs on the
    // blocking pool, not this route's Tauri async-runtime worker.
    let ok = crate::commands::github::check_gh_auth().await;
    let body = serde_json::to_string(&serde_json::json!({ "ok": ok }))
        .unwrap_or_else(|_| "{}".to_string());
    Response::json("200 OK", body)
}
