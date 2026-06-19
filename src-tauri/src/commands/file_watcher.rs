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

/// Start watching an agent node's worktree for file changes
#[command]
pub fn watch_agent_node(
    node_id: i64,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    // The directory actually watched: the canonical Node Working Directory
    // (host form), which gates on `use_worktree` and trims the name. Using the
    // canonical resolver means a Root Node with a stale `worktree_name` watches
    // the Mesh root, not a non-existent worktree subdir.
    let resolved = env::node_working_path(&node);
    let watch_path = resolved.host_path;
    let internal_path = resolved.raw_path;

    let watch_path_for_callback = watch_path.clone();
    let last_emit = Arc::new(Mutex::new(Instant::now() - std::time::Duration::from_secs(1)));
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
                    tracing::warn!("File watcher error for node {}: {:?}", node_id, e);
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
    watchers.insert(node_id, watcher);

    Ok(())
}

/// Stop watching an agent node
#[command]
pub fn unwatch_agent_node(node_id: i64) -> Result<(), String> {
    let mut watchers = WATCHERS.lock().unwrap();
    watchers.remove(&node_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::env::{node_working_path, resolve_agent_path};
    use crate::models::AgentNode;

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
    /// drive `node_working_path`. Mirrors the `env` module's fixture.
    /// `..Default::default()` covers the rest so future optional columns
    /// don't reopen this fixture (issue #457).
    fn node(use_worktree: bool, worktree_name: Option<&str>) -> AgentNode {
        AgentNode {
            path: "/home/user/my-repo".to_string(),
            worktree_name: worktree_name.map(str::to_string),
            use_worktree,
            ..Default::default()
        }
    }

    /// A Worktree Node's GIT_CHANGED `internal_path` is its worktree subdir —
    /// matching `getNodeGitPath()`. The path is now sourced from the canonical
    /// `env::node_working_path(...).raw_path` (issue #409).
    #[test]
    fn internal_path_for_worktree_node_is_worktree_subdir() {
        assert_eq!(
            node_working_path(&node(true, Some("gentle-fox"))).raw_path,
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
            node_working_path(&node(false, Some("gentle-fox"))).raw_path,
            "/home/user/my-repo"
        );
    }

    /// No worktree name → Mesh root.
    #[test]
    fn internal_path_without_worktree_name_is_mesh_root() {
        assert_eq!(
            node_working_path(&node(true, None)).raw_path,
            "/home/user/my-repo"
        );
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
            node_working_path(&node(true, Some("  gentle-fox  "))).raw_path,
            "/home/user/my-repo/.claude/worktrees/gentle-fox"
        );
    }

    /// A whitespace-only worktree name trims to empty → Mesh root (parity with
    /// the canonical `env::worktree_segment` rule).
    #[test]
    fn internal_path_for_whitespace_only_worktree_name_is_mesh_root() {
        assert_eq!(
            node_working_path(&node(true, Some("   "))).raw_path,
            "/home/user/my-repo"
        );
    }
}
