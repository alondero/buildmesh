//! Tauri commands for Harness Logins (additional Codex sign-ins).
//!
//! Each command validates its input, calls one function in
//! [`crate::services::harness_logins`] on the blocking pool, and announces a
//! change to the login list through `provider-list-changed` so the spawn menu
//! and the Usage tab re-read it.

use tauri::{command, Emitter};

use crate::preferences::harness_logins::HarnessLogin;
use crate::services::harness_logins::{self as service, HarnessLoginStatus, SignInState};

fn announce(app: &tauri::AppHandle) {
    let _ = app.emit("provider-list-changed", ());
}

fn required(value: &str, what: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{what} is required"));
    }
    Ok(value.to_string())
}

/// The default Codex login plus every additional one, with sign-in state.
#[command]
pub async fn list_harness_logins() -> Result<Vec<HarnessLoginStatus>, String> {
    crate::commands::run_blocking("list_harness_logins", service::list_blocking).await
}

/// Add a Codex login and create its home folder.
#[command]
pub async fn add_harness_login(
    app: tauri::AppHandle,
    label: String,
) -> Result<HarnessLogin, String> {
    let login =
        crate::commands::run_blocking("add_harness_login", move || service::add_blocking(&label))
            .await?;
    announce(&app);
    Ok(login)
}

/// Rename a Codex login.
#[command]
pub async fn rename_harness_login(
    app: tauri::AppHandle,
    id: String,
    label: String,
) -> Result<HarnessLogin, String> {
    let id = required(&id, "Login id")?;
    let login = crate::commands::run_blocking("rename_harness_login", move || {
        service::rename_blocking(&id, &label)
    })
    .await?;
    announce(&app);
    Ok(login)
}

/// Remove a Codex login from the list (its folder stays on disk).
#[command]
pub async fn remove_harness_login(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let id = required(&id, "Login id")?;
    crate::commands::run_blocking("remove_harness_login", move || {
        service::remove_blocking(&id)
    })
    .await?;
    announce(&app);
    Ok(())
}

/// Run `codex login` against a login's home. Poll `list_harness_logins` for
/// progress; the login reads as signed in once Codex writes its `auth.json`.
#[command]
pub async fn start_harness_login_sign_in(id: String) -> Result<SignInState, String> {
    let id = required(&id, "Login id")?;
    crate::commands::run_blocking("start_harness_login_sign_in", move || {
        service::start_sign_in_blocking(&id)
    })
    .await
}

/// Stop a running sign-in.
#[command]
pub async fn cancel_harness_login_sign_in(id: String) -> Result<(), String> {
    let id = required(&id, "Login id")?;
    crate::commands::run_blocking("cancel_harness_login_sign_in", move || {
        service::cancel_sign_in_blocking(&id)
    })
    .await
}
