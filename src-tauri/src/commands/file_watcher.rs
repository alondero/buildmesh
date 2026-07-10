//! File system watcher using notify crate

use crate::db;
use crate::env;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher, Event};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{command, Emitter};

static WATCHERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, RecommendedWatcher>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Minimum spacing between `git-changed` emits while filesystem events keep
/// arriving, and the quiet gap that ends a burst. 500ms preserves the
/// pre-existing "at most ~2 refreshes per second while an agent works"
/// cadence that every `GIT_CHANGED` consumer was tuned against.
const EMIT_INTERVAL: Duration = Duration::from_millis(500);

/// Coalescing emit loop, run on a dedicated thread per watched node.
///
/// The previous implementation was a leading-edge-only throttle inside the
/// notify callback: an event was emitted if ≥500ms had passed since the last
/// emit, otherwise silently dropped. That had two problems:
///
/// 1. **Staleness**: the *final* event of a burst is exactly the one that
///    describes the settled on-disk state — dropping it left the diff panel
///    and git chips permanently stale until some unrelated change fired.
/// 2. **Storm amplification**: every emitted event fans out to every
///    subscribed hook (diff, status, summary, branch, health, open-PR — some
///    of which are network calls), so emitting on the leading edge of every
///    500ms window while an agent streams edits kept all of them refetching
///    at the maximum rate with no coalescing of the tail.
///
/// This loop keeps the responsive leading edge (first event of a burst emits
/// immediately), emits at most once per `EMIT_INTERVAL` while events keep
/// arriving (so long bursts still refresh live), and **guarantees a trailing
/// emit** once the burst goes quiet — the settled state always lands.
///
/// Extracted from the watcher wiring so tests can drive it with a plain
/// channel and count emits (`quiet_gap` is injectable for fast tests).
pub(crate) fn run_coalescer(rx: Receiver<()>, quiet_gap: Duration, mut emit: impl FnMut()) {
    // Outer loop: wait (unbounded) for the first event of a burst.
    while rx.recv().is_ok() {
        emit(); // leading edge — first change renders promptly
        let mut last_emit = Instant::now();
        let mut pending = false;
        // Inner loop: absorb the burst. Every `quiet_gap` of silence ends it.
        loop {
            match rx.recv_timeout(quiet_gap) {
                Ok(()) => {
                    pending = true;
                    if last_emit.elapsed() >= EMIT_INTERVAL {
                        emit();
                        last_emit = Instant::now();
                        pending = false;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    // Watcher dropped (unwatch/shutdown): flush the settled
                    // state so the last edits aren't lost, then exit.
                    if pending {
                        emit();
                    }
                    return;
                }
            }
        }
        if pending {
            emit(); // trailing edge — the settled state after the burst
        }
    }
}

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

    // Clone before the closures consume app_handle — needed for the immediate emit below.
    let app_handle_outer = app_handle.clone();

    // Coalescer thread: the notify callback only pokes this channel (cheap,
    // non-blocking); the thread owns the throttle/trailing-emit logic and the
    // actual Tauri emit. See `run_coalescer` for why this replaced the
    // leading-edge-only throttle that used to live in the callback.
    let (tx, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();
    {
        let app_handle = app_handle.clone();
        let watch_path = watch_path.clone();
        let internal_path = internal_path.clone();
        std::thread::Builder::new()
            .name(format!("git-changed-coalescer-{}", node_id))
            .spawn(move || {
                run_coalescer(rx, EMIT_INTERVAL, || {
                    let _ = app_handle.emit("git-changed", serde_json::json!({
                        "path": &watch_path,
                        "internal_path": &internal_path
                    }));
                });
            })
            .map_err(|e| format!("failed to spawn coalescer thread: {}", e))?;
    }

    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| {
            match result {
                // Unbounded std channel: send never blocks the notify thread.
                // When the watcher is dropped (unwatch), this closure — and
                // with it the Sender — is dropped, which ends the coalescer.
                Ok(_event) => {
                    let _ = tx.send(());
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

    // ── run_coalescer ───────────────────────────────────────────────────────
    //
    // These drive the extracted loop with a plain channel and a short quiet
    // gap. Sleeps use generous margins relative to the gap — Windows timer
    // resolution makes tight margins flaky (see pool_worker's 20ms lesson).

    use super::run_coalescer;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_QUIET_GAP: Duration = Duration::from_millis(50);

    fn spawn_coalescer(
    ) -> (std::sync::mpsc::Sender<()>, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        let (tx, rx) = channel();
        let emits = Arc::new(AtomicUsize::new(0));
        let emits_clone = emits.clone();
        let handle = std::thread::spawn(move || {
            run_coalescer(rx, TEST_QUIET_GAP, || {
                emits_clone.fetch_add(1, Ordering::SeqCst);
            });
        });
        (tx, emits, handle)
    }

    /// A single event emits exactly once (the leading edge) — no phantom
    /// trailing emit for a burst of one.
    #[test]
    fn coalescer_single_event_emits_once() {
        let (tx, emits, handle) = spawn_coalescer();
        tx.send(()).unwrap();
        // Wait well past the quiet gap so any (incorrect) trailing emit lands.
        std::thread::sleep(TEST_QUIET_GAP * 4);
        assert_eq!(emits.load(Ordering::SeqCst), 1);
        drop(tx);
        handle.join().unwrap();
        assert_eq!(emits.load(Ordering::SeqCst), 1, "drop must not re-emit with nothing pending");
    }

    /// A rapid burst emits the leading edge immediately AND a trailing edge
    /// after the burst goes quiet — the settled state always lands. (The old
    /// leading-edge-only throttle dropped the trailing events: the panel
    /// showed the state as of the FIRST write of a burst, forever.)
    #[test]
    fn coalescer_burst_emits_leading_and_trailing() {
        let (tx, emits, handle) = spawn_coalescer();
        // Burst: several events well inside one quiet gap.
        for _ in 0..5 {
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        // Let the quiet gap elapse → trailing emit.
        std::thread::sleep(TEST_QUIET_GAP * 4);
        let count = emits.load(Ordering::SeqCst);
        assert_eq!(count, 2, "leading + trailing, got {count}");
        drop(tx);
        handle.join().unwrap();
    }

    /// Dropping the sender mid-burst (unwatch while edits are in flight)
    /// still flushes the pending trailing emit before the thread exits.
    #[test]
    fn coalescer_flushes_pending_on_disconnect() {
        let (tx, emits, handle) = spawn_coalescer();
        tx.send(()).unwrap(); // leading emit
        std::thread::sleep(Duration::from_millis(10));
        tx.send(()).unwrap(); // pending, inside the quiet gap
        drop(tx); // disconnect before the gap elapses
        handle.join().unwrap();
        assert_eq!(emits.load(Ordering::SeqCst), 2, "pending trailing emit must flush on drop");
    }
}
