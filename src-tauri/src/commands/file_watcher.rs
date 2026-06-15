//! File system watcher using notify crate

use crate::db;
use crate::env;
use crate::models::AgentNode;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher, Event};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{command, Emitter};

static WATCHERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, RecommendedWatcher>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Start watching a session's worktree for file changes
#[command]
pub fn watch_session(
    session_id: i64,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let node = db::get_agent_node_by_id(session_id).map_err(|e| e.to_string())?;
    // The directory actually watched: the canonical Node Working Directory
    // (host form), which gates on `use_worktree` and trims the name. Using the
    // canonical resolver means a Root Node with a stale `worktree_name` watches
    // the Mesh root, not a non-existent worktree subdir.
    let watch_path = env::node_working_path(&node).host_path;

    let watch_path_for_callback = watch_path.clone();
    let last_emit = Arc::new(Mutex::new(Instant::now() - std::time::Duration::from_secs(1)));

    let internal_path = node_internal_path(&node);
    let internal_path_for_callback = internal_path.clone();

    // Clone before the closure consumes app_handle — needed for the immediate emit below.
    let app_handle_outer = app_handle.clone();

    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| {
            match result {
                Ok(_event) => {
                    let mut last = last_emit.lock().unwrap();
                    let now = Instant::now();
                    if now.duration_since(*last).as_millis() >= 500 {
                        *last = now;
                        let _ = app_handle.emit("git-changed", serde_json::json!({
                            "path": &watch_path_for_callback,
                            "internal_path": &internal_path_for_callback
                        }));
                    }
                }
                Err(e) => {
                    tracing::warn!("File watcher error for session {}: {:?}", session_id, e);
                }
            }
        },
        Config::default().with_poll_interval(std::time::Duration::from_secs(2)),
    ).map_err(|e| e.to_string())?;

    let path = std::path::Path::new(&watch_path);
    if path.exists() {
        watcher.watch(path, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
        // Emit an immediate GIT_CHANGED so any pre-existing uncommitted changes
        // are reflected without waiting for the next file write.
        let _ = app_handle_outer.emit("git-changed", serde_json::json!({
            "path": &watch_path,
            "internal_path": &internal_path
        }));
    }

    let mut watchers = WATCHERS.lock().unwrap();
    watchers.insert(session_id, watcher);

    Ok(())
}

/// The `internal_path` carried in the GIT_CHANGED payload. This is the RAW
/// effective path (`node.path`, or `node.path/.claude/worktrees/<name>`), NOT
/// the host form — it must match what the frontend's `getNodeGitPath()` computes
/// (`src/lib/paths.ts`), because that's the string components subscribe with.
///
/// The rule (gated on `use_worktree`, trimmed, non-empty) is the same one
/// `env::worktree_segment` uses; both this helper and `getNodeGitPath()` are
/// paired copies of that rule, kept in lockstep via tests on each side (issue
/// #387). Without the `use_worktree` gate, a Root Node (`use_worktree = false`)
/// carrying a stale `worktree_name` emitted a worktree subdir the frontend
/// never subscribes to, so its GIT_CHANGED never matched and the node's
/// changed-files went stale until remount. Without the trim, a padded
/// `worktree_name` produced an `internal_path` that the frontend (now also
/// trimming) wouldn't subscribe to — same staleness, narrower trigger.
fn node_internal_path(node: &AgentNode) -> String {
    let trimmed = node.worktree_name.as_deref().map(str::trim);
    match trimmed {
        Some(wt) if node.use_worktree && !wt.is_empty() => {
            format!("{}/.claude/worktrees/{}", node.path, wt)
        }
        _ => node.path.clone(),
    }
}

/// Stop watching a session
#[command]
pub fn unwatch_session(session_id: i64) -> Result<(), String> {
    let mut watchers = WATCHERS.lock().unwrap();
    watchers.remove(&session_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::node_internal_path;
    use crate::env::resolve_agent_path;
    use crate::models::{AgentNode, EnvType, Provider, SessionStatus};
    use chrono::Utc;

    #[test]
    fn test_watch_path_with_worktree_name() {
        let resolved = resolve_agent_path("/Users/adam/myproject", Some("gentle-fox"));
        // On Windows, Unix-style paths like /Users/... are stored by the DB as-is
        // and host_path returns them unchanged (no WSL conversion)
        assert!(resolved.host_path.contains(".claude/worktrees/gentle-fox"));
    }

    #[test]
    fn test_watch_path_without_worktree_name() {
        let resolved = resolve_agent_path("/Users/adam/myproject", None);
        assert!(resolved.host_path.contains("/Users/adam/myproject"));
    }

    /// Minimal Agent Node fixture; only `use_worktree`/`worktree_name`/`path`
    /// drive `node_internal_path`. Mirrors the `env` module's fixture.
    fn node(use_worktree: bool, worktree_name: Option<&str>) -> AgentNode {
        AgentNode {
            id: 1,
            mesh_id: 1,
            name: "n".to_string(),
            path: "/home/user/my-repo".to_string(),
            branch: "main".to_string(),
            env: EnvType::Wsl,
            provider: Provider::Anthropic,
            status: SessionStatus::Running,
            cli_session_id: None,
            worktree_name: worktree_name.map(str::to_string),
            use_worktree,
            source_issue: None,
            position: 0,
            created_at: Utc::now(),
        }
    }

    /// A Worktree Node's GIT_CHANGED `internal_path` is its worktree subdir —
    /// matching `getNodeGitPath()`.
    #[test]
    fn internal_path_for_worktree_node_is_worktree_subdir() {
        assert_eq!(
            node_internal_path(&node(true, Some("gentle-fox"))),
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// Regression: a Root Node (`use_worktree = false`) with a STALE
    /// `worktree_name` must emit the Mesh root, not the worktree subdir. Before
    /// the `use_worktree` gate, it emitted the subdir — a path the frontend's
    /// `getNodeGitPath()` (which gates on `use_worktree`) never subscribes to, so
    /// the node's GIT_CHANGED never matched and its changed-files went stale.
    #[test]
    fn internal_path_for_root_node_ignores_stale_worktree_name() {
        assert_eq!(
            node_internal_path(&node(false, Some("gentle-fox"))),
            "/home/user/my-repo"
        );
    }

    /// No worktree name → Mesh root.
    #[test]
    fn internal_path_without_worktree_name_is_mesh_root() {
        assert_eq!(node_internal_path(&node(true, None)), "/home/user/my-repo");
    }

    /// A padded `worktree_name` is trimmed to match the canonical
    /// `env::worktree_segment` rule (and the frontend `getNodeGitPath()` in
    /// `src/lib/paths.ts`). The GIT_CHANGED `internal_path` is the string the
    /// frontend subscribes with — if it diverges from `getNodeGitPath()` by
    /// even whitespace, the event never matches and changed-files go stale.
    /// See issue #387. Paired-constant pattern, not a single source of truth.
    #[test]
    fn internal_path_for_padded_worktree_name_is_trimmed() {
        assert_eq!(
            node_internal_path(&node(true, Some("  gentle-fox  "))),
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// A whitespace-only worktree name trims to empty → Mesh root (parity with
    /// the canonical `env::worktree_segment` rule).
    #[test]
    fn internal_path_for_whitespace_only_worktree_name_is_mesh_root() {
        assert_eq!(
            node_internal_path(&node(true, Some("   "))),
            "/home/user/my-repo"
        );
    }
}
