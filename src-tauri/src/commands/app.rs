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

/// Hand the maximise button's measured box to the native Snap Layouts overlay
/// (ADR-0035).
///
/// The frontend measures the button from the DOM instead of the backend
/// assuming its size, because the overlay has to land exactly on the real
/// button: a constant that drifts from a Tailwind class stops the Windows 11
/// flyout appearing and breaks nothing else, which is a failure nobody
/// notices. Logical (CSS) pixels in — `windowing` applies the window DPI scale,
/// since the frontend cannot see it.
///
/// A no-op off Windows; `HTMAXBUTTON` and Snap Layouts are Windows shell
/// features. Called on mount and again on resize, so the implementation is
/// idempotent rather than install-once.
#[command]
pub fn set_titlebar_maximize_metrics(
    window: tauri::WebviewWindow,
    right_inset: f64,
    top: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    crate::windowing::set_maximize_metrics(
        &window,
        crate::windowing::MaximizeMetrics {
            right_inset,
            top,
            width,
            height,
        },
    )
}
