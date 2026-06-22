//! Per-agent git inspection routes.
//!
//! These power the mobile "Changes" view: a tree of edited files, a +/~/-
//! summary in the header, and an inline diff for each tapped file. All
//! routes resolve against the agent node's worktree (its `path` column),
//! so they work whether the agent runs in-place or in a `git worktree`.

use crate::db;
use crate::http::request;
use crate::http::MaybeTls;

/// `GET /api/agents/{id}/git/status` — full file-by-file status. Empty list
/// for a clean tree.
pub async fn status(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    agent_id: i64,
) {
    let Ok(node) = db::get_agent_node_by_id(agent_id) else {
        request::send_json_error(lines, "404 Not Found", "Agent not found").await;
        return;
    };
    match crate::commands::git::get_git_status(node.path) {
        Ok(status) => {
            let body = serde_json::to_string(&status).unwrap_or_else(|_| "[]".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}

/// `GET /api/agents/{id}/git/summary` — `{added, modified, deleted}` counts.
pub async fn summary(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    agent_id: i64,
) {
    let Ok(node) = db::get_agent_node_by_id(agent_id) else {
        request::send_json_error(lines, "404 Not Found", "Agent not found").await;
        return;
    };
    match crate::commands::git::get_git_summary(node.path) {
        Ok(summary) => {
            let body = serde_json::to_string(&summary).unwrap_or_else(|_| "{}".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}

/// `GET /api/agents/{id}/git/branch` — current branch name.
pub async fn branch(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    agent_id: i64,
) {
    match crate::commands::pr::get_current_branch(agent_id) {
        Ok(name) => {
            let body = serde_json::to_string(&serde_json::json!({ "branch": name }))
                .unwrap_or_else(|_| "{}".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "404 Not Found", &e).await;
        }
    }
}

/// `GET /api/agents/{id}/diff?path=<relative-file-path>` — file diff against
/// the agent's merge-base with its mesh `base_ref` (ADR 0005), so committed and
/// uncommitted agent work both show. Rendered server-side via
/// `commands::diff::diff_node_file_against_base` so the mobile UI gets
/// pre-highlighted hunks ready to display.
pub async fn diff(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    agent_id: i64,
    file_path: &str,
) {
    if file_path.is_empty() {
        request::send_json_error(lines, "400 Bad Request", "Missing ?path=").await;
        return;
    }
    // Defense against a request like `?path=../../etc/passwd` — the diff
    // command lives inside the agent's worktree, but any caller using a
    // relative-up path could break out. Reject path traversal.
    if file_path
        .split(['/', '\\'])
        .any(|seg| seg == ".." || seg.is_empty())
    {
        request::send_json_error(lines, "400 Bad Request", "Invalid path").await;
        return;
    }
    if db::get_agent_node_by_id(agent_id).is_err() {
        request::send_json_error(lines, "404 Not Found", "Agent not found").await;
        return;
    }
    match crate::commands::diff::diff_node_file_against_base(agent_id, file_path.to_string()).await {
        Ok(diff) => {
            let body = serde_json::to_string(&diff).unwrap_or_else(|_| "{}".to_string());
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(e) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}

/// `GET /api/gh/auth` — whether `gh auth status` is happy on this host.
/// Used to gate the mobile "Create PR" button.
pub async fn gh_auth(lines: &mut tokio::io::BufStream<MaybeTls>) {
    let ok = crate::commands::pr::check_gh_auth();
    let body = serde_json::to_string(&serde_json::json!({ "ok": ok }))
        .unwrap_or_else(|_| "{}".to_string());
    let _ = request::write_json(lines, "200 OK", &body).await;
}
