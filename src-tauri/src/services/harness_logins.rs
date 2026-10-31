//! Harness Login management: add, rename and remove additional Codex logins,
//! report each login's sign-in state, and drive `codex login` for one.
//!
//! The stored model and its pure rules live in
//! [`crate::preferences::harness_logins`]; this module owns the filesystem and
//! process side effects.
//!
//! Signing in runs the user's own `codex login` with `CODEX_HOME` pointing at
//! the login's home, so Codex writes the new `auth.json` exactly where a launch
//! will read it. Buildmesh never reads or writes tokens itself. `codex login`
//! serves its OAuth callback on a fixed local port, so only one sign-in runs at
//! a time; starting another cancels the first.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use ts_rs::TS;

use crate::preferences::harness_logins::{self as model, HarnessLogin, DEFAULT_LOGIN_ID};
use crate::services::usage::adapters::codex::{read_login_identity, CodexLoginIdentity};

/// How long a sign-in may wait for the browser before it is abandoned.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The harness this module manages logins for.
const HARNESS: &str = "codex";

/// One login as the settings pane shows it.
#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "HarnessLoginStatus.ts")]
pub struct HarnessLoginStatus {
    pub id: String,
    pub harness_id: String,
    pub label: String,
    pub home: String,
    /// The implicit login that uses the runtime's ordinary home.
    pub is_default: bool,
    pub signed_in: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
    /// Label of another login that is signed in to the same ChatGPT account.
    pub same_account_as: Option<String>,
    /// The sign-in in progress (or just finished) for this login.
    pub sign_in: Option<SignInState>,
}

/// Progress of a `codex login` run.
#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "HarnessLoginSignIn.ts")]
pub struct SignInState {
    /// `codex login` is still waiting for the browser.
    pub running: bool,
    /// The authorisation URL `codex login` printed, for opening it in a
    /// different browser or profile.
    pub url: Option<String>,
    /// Why the sign-in stopped without signing in.
    pub error: Option<String>,
}

struct SignInSession {
    login_id: String,
    child: Child,
    #[cfg(target_os = "windows")]
    job: Option<crate::process_util::JobHandle>,
    output: Arc<Mutex<SignInOutput>>,
    started: Instant,
    finished: Option<SignInState>,
}

#[derive(Default)]
struct SignInOutput {
    url: Option<String>,
    last_line: Option<String>,
}

static SIGN_IN: Mutex<Option<SignInSession>> = Mutex::new(None);

/// The default login plus every stored Codex login, with sign-in state.
pub fn list_blocking() -> Result<Vec<HarnessLoginStatus>, String> {
    let prefs = crate::preferences::load()?;
    Ok(list_with(
        &prefs,
        &crate::env::codex_dir(),
        poll_sign_in,
        read_login_identity,
    ))
}

/// [`list_blocking`] with the sign-in poll and the `auth.json` reader passed
/// in. The poll runs first: `codex login` writes `auth.json` before it exits,
/// so a sign-in reported finished has already left its `auth.json` for the
/// reads that follow.
fn list_with(
    prefs: &crate::preferences::AppPreferences,
    default_home: &Path,
    poll: impl FnOnce() -> Option<(String, SignInState)>,
    read: impl Fn(&Path) -> CodexLoginIdentity,
) -> Vec<HarnessLoginStatus> {
    let session = poll();
    let mut entries: Vec<(HarnessLoginStatus, Option<String>)> = Vec::new();
    entries.push(status_entry(
        DEFAULT_LOGIN_ID,
        "Default",
        default_home,
        true,
        read(default_home),
    ));
    for login in model::logins_for(prefs, HARNESS) {
        let home = Path::new(&login.home);
        entries.push(status_entry(
            &login.id,
            &login.label,
            home,
            false,
            read(home),
        ));
    }
    let labels: Vec<(Option<String>, String)> = entries
        .iter()
        .map(|(status, user)| (user.clone(), status.label.clone()))
        .collect();
    entries
        .into_iter()
        .enumerate()
        .map(|(index, (mut status, user))| {
            status.same_account_as = duplicate_of(index, user.as_deref(), &labels);
            if let Some((login_id, state)) = &session {
                if login_id == &status.id {
                    status.sign_in = Some(state.clone());
                }
            }
            status
        })
        .collect()
}

fn status_entry(
    id: &str,
    label: &str,
    home: &Path,
    is_default: bool,
    identity: CodexLoginIdentity,
) -> (HarnessLoginStatus, Option<String>) {
    let user = identity
        .signed_in
        .then_some(identity.subscription)
        .flatten();
    (
        HarnessLoginStatus {
            id: id.to_string(),
            harness_id: HARNESS.to_string(),
            label: label.to_string(),
            home: home.to_string_lossy().into_owned(),
            is_default,
            signed_in: identity.signed_in,
            email: identity.email,
            plan: identity.plan,
            same_account_as: None,
            sign_in: None,
        },
        user,
    )
}

/// The label of the first *other* login signed in as the same person.
pub(crate) fn duplicate_of(
    index: usize,
    user: Option<&str>,
    labels: &[(Option<String>, String)],
) -> Option<String> {
    let user = user?;
    labels
        .iter()
        .enumerate()
        .find(|(other, (other_user, _))| *other != index && other_user.as_deref() == Some(user))
        .map(|(_, (_, label))| label.clone())
}

/// Add a login named `label`, creating its home under the user's home folder
/// and seeding it from the default home.
pub fn add_blocking(label: &str) -> Result<HarnessLogin, String> {
    add_in(
        &crate::services::usage::types::home_dir(),
        &crate::env::codex_dir(),
        label,
    )
}

/// [`add_blocking`] with the user's home and the default Codex home supplied.
pub(crate) fn add_in(
    user_home: &Path,
    default_home: &Path,
    label: &str,
) -> Result<HarnessLogin, String> {
    let label = model::validate_label(label)?;
    let prefs = crate::preferences::load()?;
    ensure_label_free(&prefs.harness_logins, None, &label)?;
    let id = model::unique_id_for(&label, &prefs.harness_logins);
    let login = HarnessLogin {
        harness_id: HARNESS.to_string(),
        label: label.clone(),
        home: model::managed_home_for(user_home, &id)
            .to_string_lossy()
            .into_owned(),
        id,
    };
    // Create the folder before the preference write, outside the preferences
    // lock: a login is only listed once its home exists.
    model::seed_home(Path::new(&login.home), Some(default_home))?;
    crate::preferences::storage::try_update(|prefs| {
        ensure_label_free(&prefs.harness_logins, None, &label)?;
        if prefs
            .harness_logins
            .iter()
            .any(|other| other.id == login.id)
        {
            return Err("Another login was added at the same time; try again".into());
        }
        prefs.harness_logins.push(login.clone());
        Ok(())
    })?;
    Ok(login)
}

fn ensure_label_free(
    logins: &[HarnessLogin],
    except_id: Option<&str>,
    label: &str,
) -> Result<(), String> {
    if logins.iter().any(|login| {
        login.harness_id == HARNESS
            && Some(login.id.as_str()) != except_id
            && login.label.eq_ignore_ascii_case(label)
    }) {
        return Err(format!("A Codex login named \"{label}\" already exists"));
    }
    Ok(())
}

/// Rename a stored login. Its id and home are unchanged, so nodes that froze
/// it keep resuming in the same home.
pub fn rename_blocking(id: &str, label: &str) -> Result<HarnessLogin, String> {
    let label = model::validate_label(label)?;
    let mut renamed: Option<HarnessLogin> = None;
    crate::preferences::storage::try_update(|prefs| {
        ensure_label_free(&prefs.harness_logins, Some(id), &label)?;
        let login = prefs
            .harness_logins
            .iter_mut()
            .find(|login| login.id == id)
            .ok_or_else(|| format!("Codex login '{id}' no longer exists"))?;
        login.label = label.clone();
        renamed = Some(login.clone());
        Ok(())
    })?;
    renamed.ok_or_else(|| "The login was not saved".to_string())
}

/// Remove a stored login from the list. Its home folder (and the sessions in
/// it) is left on disk; Launch Configurations that pinned it are unpinned, so
/// they launch with the login picked in the spawn menu, and existing nodes
/// keep their frozen home.
pub fn remove_blocking(id: &str) -> Result<(), String> {
    cancel_sign_in_for(id);
    crate::preferences::storage::try_update(|prefs| {
        let before = prefs.harness_logins.len();
        prefs.harness_logins.retain(|login| login.id != id);
        if prefs.harness_logins.len() == before {
            return Err(format!("Codex login '{id}' no longer exists"));
        }
        for configuration in &mut prefs.spawn_configurations {
            if configuration.login_id.as_deref() == Some(id) {
                configuration.login_id = None;
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Start `codex login` for `id`, replacing any sign-in already running.
pub fn start_sign_in_blocking(id: &str) -> Result<SignInState, String> {
    let home = home_for(id)?;
    std::fs::create_dir_all(&home)
        .map_err(|e| format!("Could not create the login folder {}: {e}", home.display()))?;
    let executable = crate::agent::detection::resolve_spawn_binary("codex")
        .ok_or("Codex is not installed or not on PATH; install it and try again")?;
    // Held until the new session is stored, so two starts cannot both spawn
    // and leave one `codex login` running untracked.
    let mut slot = lock();
    if let Some(mut previous) = slot.take() {
        terminate(&mut previous);
    }

    let mut command = crate::process_util::command_no_window(&executable);
    command
        .arg("login")
        .env("CODEX_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start `codex login`: {e}"))?;
    #[cfg(target_os = "windows")]
    let job = crate::process_util::JobHandle::contain(child.id());
    let output = Arc::new(Mutex::new(SignInOutput::default()));
    if let Some(stdout) = child.stdout.take() {
        watch_output(stdout, Arc::clone(&output));
    }
    if let Some(stderr) = child.stderr.take() {
        watch_output(stderr, Arc::clone(&output));
    }
    *slot = Some(SignInSession {
        login_id: id.to_string(),
        child,
        #[cfg(target_os = "windows")]
        job,
        output,
        started: Instant::now(),
        finished: None,
    });
    Ok(SignInState {
        running: true,
        url: None,
        error: None,
    })
}

/// Stop the sign-in for `id` if one is running.
pub fn cancel_sign_in_blocking(id: &str) -> Result<(), String> {
    cancel_sign_in_for(id);
    Ok(())
}

fn home_for(id: &str) -> Result<PathBuf, String> {
    if id == DEFAULT_LOGIN_ID {
        return Ok(crate::env::codex_dir());
    }
    let prefs = crate::preferences::load()?;
    model::logins_for(&prefs, HARNESS)
        .into_iter()
        .find(|login| login.id == id)
        .map(|login| PathBuf::from(&login.home))
        .ok_or_else(|| format!("Codex login '{id}' no longer exists"))
}

fn lock() -> std::sync::MutexGuard<'static, Option<SignInSession>> {
    SIGN_IN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn watch_output(stream: impl std::io::Read + Send + 'static, output: Arc<Mutex<SignInOutput>>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let mut output = output.lock().unwrap_or_else(|p| p.into_inner());
            if output.url.is_none() {
                output.url = auth_url_in(&line);
            }
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                output.last_line = Some(trimmed.to_string());
            }
        }
    });
}

/// The first OpenAI authorisation URL in a line of `codex login` output.
pub(crate) fn auth_url_in(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    url.contains("openai.com").then_some(url)
}

/// Advance the running session (exit, timeout) and report it.
fn poll_sign_in() -> Option<(String, SignInState)> {
    let mut guard = lock();
    let session = guard.as_mut()?;
    if session.finished.is_none() {
        let output = session.output.lock().unwrap_or_else(|p| p.into_inner());
        let url = output.url.clone();
        let last_line = output.last_line.clone();
        drop(output);
        let finished = match session.child.try_wait() {
            Ok(Some(status)) if status.success() => Some(SignInState {
                running: false,
                url,
                error: None,
            }),
            Ok(Some(_)) => Some(SignInState {
                running: false,
                url,
                error: Some(
                    last_line.unwrap_or_else(|| "`codex login` exited without signing in".into()),
                ),
            }),
            Ok(None) if session.started.elapsed() > SIGN_IN_TIMEOUT => {
                terminate(session);
                Some(SignInState {
                    running: false,
                    url,
                    error: Some("Sign-in timed out; start it again when you are ready".into()),
                })
            }
            Ok(None) => {
                return Some((
                    session.login_id.clone(),
                    SignInState {
                        running: true,
                        url,
                        error: None,
                    },
                ))
            }
            Err(error) => Some(SignInState {
                running: false,
                url,
                error: Some(format!("Could not check `codex login`: {error}")),
            }),
        };
        session.finished = finished;
    }
    session
        .finished
        .clone()
        .map(|state| (session.login_id.clone(), state))
}

fn cancel_sign_in_for(id: &str) {
    let mut guard = lock();
    if guard.as_ref().is_some_and(|session| session.login_id == id) {
        if let Some(mut session) = guard.take() {
            terminate(&mut session);
        }
    }
}

/// Kill `codex login` and everything it started. On Windows the npm shim runs
/// Codex as a grandchild that would otherwise keep the callback port open.
fn terminate(session: &mut SignInSession) {
    #[cfg(target_os = "windows")]
    if let Some(job) = session.job.take() {
        job.terminate();
    }
    let _ = session.child.kill();
    let _ = session.child.wait();
}

#[cfg(test)]
#[path = "harness_logins_tests.rs"]
mod tests;
