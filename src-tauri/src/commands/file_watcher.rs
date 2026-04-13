//! File system watcher using notify crate

use crate::models::FileChange;
use notify::{Config, RecommendedWatcher, Watcher, Event};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{command, Emitter};

static WATCHERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, RecommendedWatcher>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Start watching a workspace for file changes
#[command]
pub fn watch_workspace(
    workspace_id: i64,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let app_handle_clone = app_handle.clone();
    let workspace_id_clone = workspace_id;

    let watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| {
            if let Ok(event) = result {
                let changes: Vec<FileChange> = event.paths.iter().map(|p| {
                    let kind = match event.kind {
                        notify::EventKind::Create(_) => "created",
                        notify::EventKind::Modify(_) => "modified",
                        notify::EventKind::Remove(_) => "deleted",
                        _ => "modified",
                    };
                    FileChange {
                        path: p.to_string_lossy().to_string(),
                        kind: kind.to_string(),
                    }
                }).collect();

                for change in changes {
                    let _ = app_handle_clone.emit("file-change", serde_json::json!({
                        "workspace_id": workspace_id_clone,
                        "change": change
                    }));
                }
            }
        },
        Config::default(),
    ).map_err(|e| e.to_string())?;

    let mut watchers = WATCHERS.lock().unwrap();
    watchers.insert(workspace_id, watcher);

    Ok(())
}

/// Stop watching a workspace
#[command]
pub fn unwatch_workspace(workspace_id: i64) -> Result<(), String> {
    let mut watchers = WATCHERS.lock().unwrap();
    watchers.remove(&workspace_id);
    Ok(())
}
