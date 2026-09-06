//! Generic app-level metadata commands. Lives in its own module so future
//! trivial readouts (`get_app_version`, `get_app_name`, …) can join here
//! without polluting a domain module. Issue #826 — the frontend guards the
//! in-app updater against the *dev* profile (`com.alond.buildmesh.dev`) by
//! reading the runtime identifier, since `tauri:build:dev` is also a
//! production-mode Vite build and a simple `import.meta.env.PROD` check would
//! let the dev app offer to upgrade itself to the stable release.

use tauri::{command, Manager};

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

/// Confirmed exit (issue #1501 regression, 2026-09-06).
///
/// `WindowCloseGuard`'s "Exit Buildmesh" used to call the webview-side
/// `getCurrentWindow().destroy()` IPC. That command is gated by Tauri's ACL,
/// which is compiled into the binary at build time: when the running binary
/// predates the `core:window:allow-destroy` capability, the call is rejected
/// ("Command plugin:window|destroy not allowed by ACL" — captured in
/// `buildmesh.log` at 15:34/15:43/16:15 on 2026-09-06) and the app silently
/// stayed open. The user-visible symptom was the button flickering to
/// "Exiting…" and doing nothing.
///
/// This command is the ACL-proof path: custom app commands only need to be
/// registered in `generate_handler!`, not granted through a capability, so
/// the exit works regardless of which capability set the binary was built
/// with. Force-destroying the window skips `CloseRequested` (the frontend
/// already vetoed it and is showing the modal) and lands on the same
/// `RunEvent::ExitRequested` sweep as a normal close — sessions marked
/// suspended, processes killed, watchdog expected-exit marker written.
#[command]
pub fn exit_application(app: tauri::AppHandle) -> Result<(), String> {
    tracing::info!("exit_application: force-destroying main window (ACL-proof exit path)");
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found (already destroyed?)".to_string())?;
    window
        .destroy()
        .map_err(|e| format!("window destroy failed: {e}"))
}
