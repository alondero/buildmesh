//! File system watcher using notify crate

use crate::db;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher, Event};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{command, Emitter};

static WATCHERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, RecommendedWatcher>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

fn resolve_watch_path(path: &str, worktree_name: Option<&str>) -> String {
    match worktree_name {
        Some(wt_name) if !wt_name.is_empty() => format!("{}/.claude/worktrees/{}", path, wt_name),
        _ => path.to_string(),
    }
}

/// Start watching a session's worktree for file changes
#[command]
pub fn watch_session(
    session_id: i64,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let node = db::get_agent_node_by_id(session_id).map_err(|e| e.to_string())?;
    let watch_path = resolve_watch_path(&node.path, node.worktree_name.as_deref());

    let watch_path_for_callback = watch_path.clone();
    let last_emit = Arc::new(Mutex::new(Instant::now() - std::time::Duration::from_secs(1)));

    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| {
            if let Ok(_event) = result {
                let mut last = last_emit.lock().unwrap();
                let now = Instant::now();
                if now.duration_since(*last).as_millis() >= 500 {
                    *last = now;
                    let _ = app_handle.emit("git-changed", serde_json::json!({
                        "path": &watch_path_for_callback
                    }));
                }
            }
        },
        Config::default(),
    ).map_err(|e| e.to_string())?;

    let path = std::path::Path::new(&watch_path);
    if path.exists() {
        watcher.watch(path, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
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
    use super::resolve_watch_path;

    #[test]
    fn test_watch_path_with_worktree_name() {
        assert_eq!(
            resolve_watch_path("/Users/adam/myproject", Some("gentle-fox")),
            "/Users/adam/myproject/.claude/worktrees/gentle-fox"
        );
    }

    #[test]
    fn test_watch_path_without_worktree_name() {
        assert_eq!(
            resolve_watch_path("/Users/adam/myproject", None),
            "/Users/adam/myproject"
        );
    }
}
