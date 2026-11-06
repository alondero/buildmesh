//! Tests for the webview user-data folder derivation (issue #2469).
//!
//! The behaviour under test is the *identity* of the folder: a dev launch
//! that names its own app-data profile must not share the webview's
//! `localStorage` with another worktree, and a launch that names no profile
//! must keep the platform default.

use std::path::PathBuf;

use tauri::utils::config::{AppDirectoriesOverride, AppDirectoryOverrides};

use super::{apply, data_dir};

/// The `BUILDMESH_PROFILE_DIR` a launcher pins for a linked worktree.
const PROFILE_DIR_ENV: &str = crate::startup::PROFILE_DIR_ENV;

/// An absolute path the code under test does not open: it only derives the
/// folder name.
fn scratch_profile(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("bm-webview-{name}-{}-{stamp}", std::process::id()))
}

/// A profile override moves the webview folder with it. Two worktrees pin two
/// profiles, so two webview folders, and neither holds the other's
/// `localStorage`.
#[test]
fn a_profile_override_points_the_webview_folder_at_the_profile() {
    let _lock = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let first = scratch_profile("first");
    let second = scratch_profile("second");

    let resolved =
        crate::env::with_env_vars(&[(PROFILE_DIR_ENV, Some(first.as_os_str()))], data_dir);
    assert_eq!(
        resolved,
        Some(first.clone()),
        "the webview folder must be the profile the launcher pinned"
    );

    let other = crate::env::with_env_vars(&[(PROFILE_DIR_ENV, Some(second.as_os_str()))], data_dir);
    assert_eq!(
        other,
        Some(second.clone()),
        "a second worktree must get its own webview folder"
    );
    assert_ne!(
        resolved, other,
        "two worktrees must not share one webview folder"
    );
}

/// No override means the platform default, and the config is left alone: a
/// build that names no profile — a shipped release build — keeps the folder it
/// always had.
#[test]
fn no_profile_override_keeps_the_platform_default() {
    let _lock = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::env::with_env_vars(&[(PROFILE_DIR_ENV, None)], || {
        assert_eq!(data_dir(), None, "an unset override names no folder");

        let mut config = tauri::utils::config::Config::default();
        let before = config.clone();
        apply(&mut config);
        assert_eq!(
            config.app.app_directories_override, before.app.app_directories_override,
            "an unset override must leave the app directories where they were"
        );
    });
}

/// An override the bootstrap rejects — relative, empty — names no folder. The
/// bootstrap fails closed on it and reports it; deriving a webview folder from
/// it would put frontend state somewhere the profile does not own.
#[test]
fn an_unusable_profile_override_names_no_webview_folder() {
    let _lock = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for value in ["relative\\profile", " ", ""] {
        let resolved = crate::env::with_env_vars(
            &[(PROFILE_DIR_ENV, Some(std::ffi::OsStr::new(value)))],
            data_dir,
        );
        assert_eq!(resolved, None, "{value:?} must not name a folder");
    }
}

/// The override moves the local-data directory — the webview's user-data
/// folder on Windows and Linux — and nothing else. The app's config, data,
/// cache and log directories keep their platform defaults.
#[test]
fn the_override_moves_only_the_local_data_directory() {
    let _lock = crate::env::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let profile = scratch_profile("apply");
    crate::env::with_env_vars(&[(PROFILE_DIR_ENV, Some(profile.as_os_str()))], || {
        let mut config = tauri::utils::config::Config {
            identifier: "com.alond.buildmesh.dev".into(),
            app: tauri::utils::config::AppConfig {
                windows: vec![tauri::utils::config::WindowConfig {
                    label: "main".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        apply(&mut config);

        assert_eq!(
            config.app.app_directories_override,
            Some(AppDirectoriesOverride::Directories(AppDirectoryOverrides {
                local_data: Some(profile.clone()),
                ..AppDirectoryOverrides::default()
            })),
            "the webview folder must be the profile, and the other app directories must not move"
        );
        assert_eq!(
            config.identifier, "com.alond.buildmesh.dev",
            "the override must not touch the bundle identifier"
        );
        assert_eq!(
            config.app.windows.len(),
            1,
            "the override must not touch the window configuration"
        );
    });
}
