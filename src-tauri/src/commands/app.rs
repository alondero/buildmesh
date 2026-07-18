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
