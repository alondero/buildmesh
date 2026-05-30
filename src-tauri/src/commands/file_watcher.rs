//! File system watcher using notify crate

use crate::db;
use crate::env;
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
    let resolved = env::resolve_agent_path(&node.path, node.worktree_name.as_deref());
    let watch_path = resolved.host_path;

    let watch_path_for_callback = watch_path.clone();
    let last_emit = Arc::new(Mutex::new(Instant::now() - std::time::Duration::from_secs(1)));

    // internal_path must match what getNodeGitPath() computes on the frontend:
    // node.path for non-worktree nodes, node.path/.claude/worktrees/name for worktree nodes.
    let internal_path = match node.worktree_name.as_deref() {
        Some(wt) if !wt.is_empty() => format!("{}/.claude/worktrees/{}", node.path, wt),
        _ => node.path.clone(),
    };
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

/// Stop watching a session
#[command]
pub fn unwatch_session(session_id: i64) -> Result<(), String> {
    let mut watchers = WATCHERS.lock().unwrap();
    watchers.remove(&session_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::env::resolve_agent_path;

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
}
