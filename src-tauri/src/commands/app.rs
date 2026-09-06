//! Generic app-level metadata commands. Lives in its own module so future
//! trivial readouts (`get_app_version`, `get_app_name`, …) can join here
//! without polluting a domain module. Issue #826 — the frontend guards the
//! in-app updater against the *dev* profile (`com.alond.buildmesh.dev`) by
//! reading the runtime identifier, since `tauri:build:dev` is also a
//! production-mode Vite build and a simple `import.meta.env.PROD` check would
//! let the dev app offer to upgrade itself to the stable release.

use tauri::command;

/// Returns the running app's bundle identifier (`com.alond.buildmesh` for
/// the stable hub, `com.alond.buildmesh.dev` for the dev profile). Cheap —
/// Tauri's `AppHandle` already holds the parsed config; no I/O.
#[command]
pub fn get_app_identifier(app: tauri::AppHandle) -> String {
    app.config().identifier.clone()
}

/// Retract a user close request the frontend vetoed (issue #1501).
///
/// The backend `CloseRequested` handler eagerly sets `USER_CLOSE_REQUESTED`
/// and writes the watchdog's expected-exit marker before the frontend
/// exit-confirmation modal has run. When the user cancels ("Keep Working"),
/// the frontend calls this so a later real crash is still classified as a
/// crash (auto-relaunch preserved) instead of an expected exit. Pure sync —
/// an atomic store plus a best-effort marker-file removal; runs on Tauri's
/// IPC worker, NOT the bounded tokio pool (issue #1380 review point 4).
#[command]
pub fn cancel_window_close() -> Result<(), String> {
    crate::cancel_close_request();
    Ok(())
}

/// Confirmed exit (issue #1501).
///
/// The exit-confirmation modal's "Exit Buildmesh" must not depend on the
/// webview-side `destroy` window IPC: window commands are ACL-gated and the
/// ACL is compiled into the binary, so that call can be rejected. This
/// custom command is not ACL-gated and hands shutdown to the lifecycle
/// owner instead of destroying a raw window:
///
/// - `USER_CLOSE_REQUESTED` is set first so the `Destroyed` handler
///   classifies the teardown as user-initiated rather than the
///   webview/GPU-death crash signature (which auto-relaunches).
/// - `AppHandle::exit` emits `RunEvent::ExitRequested { code: Some(0) }`,
///   where `lib.rs` writes the watchdog expected-exit marker, runs the
///   suspend sweep, and kills agent processes.
///
/// Fire-and-forget: `exit` enqueues the request on the event loop and has
/// no failure mode to report.
#[command]
pub fn exit_application(app: tauri::AppHandle) {
    tracing::info!("exit_application: initiating application shutdown");
    crate::mark_user_close_requested();
    app.exit(0);
}
