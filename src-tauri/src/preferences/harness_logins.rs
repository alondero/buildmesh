//! Harness Logins: more than one harness-native sign-in for the same harness.
//!
//! Codex keeps its ChatGPT login in `<CODEX_HOME>/auth.json` and has no way to
//! switch between accounts inside one home. Swapping that file is unsafe: Codex
//! writes refreshed tokens back to it, so two agents running on different
//! accounts would overwrite each other's login. Each additional login therefore
//! owns a whole Codex home of its own, and a launch selects it by setting
//! `CODEX_HOME` for the child process.
//!
//! The **default login** is implicit: it is the home the runtime would use
//! without Buildmesh (`$CODEX_HOME`, else `~/.codex`) and is never stored.
//! Stored rows are the additional logins only. A launch records the login it
//! used on its frozen [`super::launch_configurations::ResolvedLaunchPlan`], so
//! resuming a node returns to the home that holds its session even after the
//! login is renamed or removed from the list.
//!
//! Only native launches can select an additional login. WSL guests resolve
//! their own Codex home, and a Windows path is not a guest home.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ts_rs::TS;

use super::AppPreferences;

/// Id of the implicit login that uses the runtime's ordinary Codex home.
pub const DEFAULT_LOGIN_ID: &str = "default";

/// Harnesses whose sign-in can live in a selectable home.
pub const LOGIN_HARNESSES: &[&str] = &["codex"];

/// Folder under the user's home that holds Buildmesh-created login homes.
pub const MANAGED_HOMES_DIR: &str = ".codex-accounts";

/// Files copied from the default home when Buildmesh creates a login home, so
/// the new login starts with the user's settings, MCP servers and global
/// instructions. Copied once; afterwards each home's files are its own.
pub const SEEDED_FILES: &[&str] = &["config.toml", "AGENTS.md"];

const MAX_LABEL_CHARS: usize = 40;

/// One additional harness login stored in preferences.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "HarnessLogin.ts")]
pub struct HarnessLogin {
    /// Stable slug, unique across logins. Never [`DEFAULT_LOGIN_ID`].
    pub id: String,
    /// Harness profile id the login belongs to (`codex`).
    pub harness_id: String,
    /// User-chosen display name.
    pub label: String,
    /// Absolute host path of the harness home (`CODEX_HOME`).
    pub home: String,
}

/// The login a launch used, frozen on its Resolved Launch Plan. Absent on the
/// plan means the default login.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "HarnessLoginRef.ts")]
pub struct HarnessLoginRef {
    pub id: String,
    pub label: String,
    pub home: String,
}

impl From<&HarnessLogin> for HarnessLoginRef {
    fn from(login: &HarnessLogin) -> Self {
        Self {
            id: login.id.clone(),
            label: login.label.clone(),
            home: login.home.clone(),
        }
    }
}

/// Whether `harness_id` (a harness profile id or adapter id) supports logins.
pub fn supports_logins(harness_id: &str) -> bool {
    LOGIN_HARNESSES.contains(&harness_id)
}

/// Normalise a login id: blank and [`DEFAULT_LOGIN_ID`] both mean the default.
pub fn normalize_login_id(id: Option<&str>) -> Option<&str> {
    id.map(str::trim)
        .filter(|id| !id.is_empty() && *id != DEFAULT_LOGIN_ID)
}

/// Trim and check a display name.
pub fn validate_label(label: &str) -> Result<String, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("Login name is required".into());
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "Login name must be {MAX_LABEL_CHARS} characters or fewer"
        ));
    }
    if label.eq_ignore_ascii_case("default") {
        return Err("\"Default\" is the name of your existing Codex login".into());
    }
    Ok(label.to_string())
}

/// A filesystem- and id-safe slug for `label` that no stored login uses.
pub fn unique_id_for(label: &str, existing: &[HarnessLogin]) -> String {
    let mut slug = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    let base = if slug.is_empty() || slug == DEFAULT_LOGIN_ID {
        "login".to_string()
    } else {
        slug
    };
    let taken = |id: &str| existing.iter().any(|login| login.id == id);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .expect("an unbounded counter always finds a free id")
}

/// The stored logins for `harness_id`, in the order the user added them.
pub fn logins_for<'a>(prefs: &'a AppPreferences, harness_id: &str) -> Vec<&'a HarnessLogin> {
    prefs
        .harness_logins
        .iter()
        .filter(|login| login.harness_id == harness_id)
        .collect()
}

/// Resolve a requested login for a launch of `harness_id`.
///
/// `None` (or the default id) is the default login. A login on a harness that
/// does not support logins, or one that no longer exists, is an error rather
/// than a silent fallback: falling back would bill the wrong account.
pub fn resolve_login(
    prefs: &AppPreferences,
    harness_id: &str,
    requested: Option<&str>,
) -> Result<Option<HarnessLoginRef>, String> {
    let Some(id) = normalize_login_id(requested) else {
        return Ok(None);
    };
    if !supports_logins(harness_id) {
        return Err(format!(
            "Only Codex launches can choose a login; this launch uses '{harness_id}'"
        ));
    }
    prefs
        .harness_logins
        .iter()
        .find(|login| login.id == id && login.harness_id == harness_id)
        .map(|login| Some(HarnessLoginRef::from(login)))
        .ok_or_else(|| {
            format!("Codex login '{id}' no longer exists; choose another login in the spawn menu")
        })
}

/// Why a launch on `env_type` cannot use an additional login, if it cannot.
pub fn runtime_refusal(
    login: &HarnessLoginRef,
    env_type: crate::models::EnvType,
) -> Option<String> {
    match env_type {
        crate::models::EnvType::Windows => None,
        crate::models::EnvType::Wsl | crate::models::EnvType::WindowsInterop => Some(format!(
            "The Codex login '{}' is only available for native Windows launches. \
             Open this Mesh from a Windows path, or launch with the Default login.",
            login.label
        )),
    }
}

/// The home Buildmesh creates for a new login.
pub fn managed_home_for(user_home: &Path, id: &str) -> PathBuf {
    user_home.join(MANAGED_HOMES_DIR).join(id)
}

/// Create `home` and copy the [`SEEDED_FILES`] from `default_home` into it.
///
/// Existing files in `home` are never overwritten, so pointing a login at a
/// folder that already has its own settings keeps them.
pub fn seed_home(home: &Path, default_home: Option<&Path>) -> Result<(), String> {
    std::fs::create_dir_all(home)
        .map_err(|e| format!("Could not create the login folder {}: {e}", home.display()))?;
    let Some(default_home) = default_home else {
        return Ok(());
    };
    if default_home == home {
        return Ok(());
    }
    for name in SEEDED_FILES {
        let source = default_home.join(name);
        let target = home.join(name);
        if source.is_file() && !target.exists() {
            std::fs::copy(&source, &target).map_err(|e| {
                format!(
                    "Could not copy {} into the login folder: {e}",
                    source.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Every additional login home for `harness_id`, for readers that must find a
/// session wherever it was written.
pub fn additional_homes(prefs: &AppPreferences, harness_id: &str) -> Vec<PathBuf> {
    logins_for(prefs, harness_id)
        .into_iter()
        .map(|login| PathBuf::from(&login.home))
        .collect()
}

/// [`additional_homes`] from the current preferences. A preferences read
/// failure yields no extra homes: the default home is still searched.
pub fn current_additional_homes(harness_id: &str) -> Vec<PathBuf> {
    match super::load() {
        Ok(prefs) => additional_homes(&prefs, harness_id),
        Err(error) => {
            tracing::warn!(
                "harness logins: preferences unavailable, searching default home only: {error}"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
#[path = "harness_logins_tests.rs"]
mod tests;
