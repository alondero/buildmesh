//! The webview's user-data folder, owned by the app-data profile
//! (issue #2469).
//!
//! Why this module exists
//! ----------------------
//! The webview keeps `localStorage`, IndexedDB and cookies in one folder. On
//! Windows and Linux that folder defaults to the app's *local data* directory
//! — `%LOCALAPPDATA%\<bundle id>` for a desktop build — and WebView2 creates
//! its `EBWebView` profile inside it. Tauri forces that default on both
//! platforms, so the folder is per **identifier** rather than per profile:
//! every `buildmesh-dev` build shared one folder, and a fresh worktree
//! launched with an empty `buildmesh.db` still showed the previous worktree's
//! unread notifications and window layout.
//!
//! `BUILDMESH_PROFILE_DIR` already gives a worktree its own database,
//! preferences and lockfile. This module gives it the matching webview
//! folder, so the whole profile — backend state and frontend state — travels
//! together.
//!
//! Why the folder is moved through the Tauri config
//! -------------------------------------------------
//! `WEBVIEW2_USER_DATA_FOLDER` looks like the obvious switch, and it is the
//! stronger one: measured on this build, it overrides the user-data folder
//! Tauri passes to `CreateCoreWebView2EnvironmentWithOptions`, so a value in
//! the environment wins over the config. That is why this module moves the
//! folder through `appDirectoriesOverride` rather than by setting the
//! variable: the app derives the folder from the profile it already resolved,
//! so it travels with the launch on every platform the app builds for and
//! every launch path that names a profile (the Unix launcher included), and
//! it needs no process-global variable.
//!
//! A repository-wide search for `WEBVIEW2_USER_DATA_FOLDER` finds only this
//! module's documentation, so nothing in the repository sets it. A caller that
//! does set it keeps it — the variable is the stronger lever, and the app
//! cannot take it back.

use std::path::PathBuf;

use tauri::utils::config::{AppDirectoriesOverride, AppDirectoryOverrides};

/// The webview user-data folder for this process, or `None` when the platform
/// default applies.
///
/// The value is the app-data profile itself, so the webview state lands beside
/// the database it belongs to: `<profile>\EBWebView` on Windows, the same
/// shape the shared default has.
///
/// `None` when no profile override is in effect, so a shipped release build
/// keeps the platform default and its webview state stays where it has always
/// been. The dev profile always names a profile, so a dev launch — the main
/// checkout's included — moves its folder.
pub(crate) fn data_dir() -> Option<PathBuf> {
    #[cfg(any(test, debug_assertions, feature = "dev-tools"))]
    {
        // An unusable override resolves to `None` here and fails closed in
        // `startup::bootstrap`, which reports it to the user. Deriving a
        // folder from a value the bootstrap rejects would put webview state
        // somewhere the profile does not own.
        super::profile_dir_override().ok().flatten()
    }
    #[cfg(not(any(test, debug_assertions, feature = "dev-tools")))]
    None
}

/// Point Tauri's webview data directory at the profile's folder.
///
/// A no-op when the process names no profile, so a shipped release build is
/// untouched.
pub(crate) fn apply(config: &mut tauri::utils::config::Config) {
    let Some(dir) = data_dir() else {
        return;
    };
    // Only the local-data directory moves: it is the webview's user-data
    // folder on Windows and Linux, and the app's other directories (config,
    // data, cache, log) keep their platform defaults. The shipped config
    // carries no directory override, so this replaces nothing today.
    config.app.app_directories_override =
        Some(AppDirectoriesOverride::Directories(AppDirectoryOverrides {
            local_data: Some(dir),
            ..AppDirectoryOverrides::default()
        }));
}

#[cfg(test)]
#[path = "webview_data_tests.rs"]
mod tests;
