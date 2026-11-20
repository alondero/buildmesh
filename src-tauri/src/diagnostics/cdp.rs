//! The opt-in Chrome DevTools Protocol (CDP) port for the hub window
//! (issue #2836).
//!
//! Why this module exists
//! -----------------------
//! When the hub misbehaves, the decisive data lives **inside** the WebView:
//! the compositor's trace categories, the layer tree, the animation state, the
//! heap. That data is reachable only through the DevTools Protocol, and only
//! when WebView2 was started with a debugging port. Without one, the only
//! remedy — reloading the window — destroys the evidence the investigation
//! needed.
//!
//! So the port is an **opt-in startup switch**: a saved preference that adds
//! `--remote-debugging-port=<port>` to the WebView2 browser arguments, off by
//! default. A debug or `dev-tools` build may also take the port from the
//! `BUILDMESH_DIAG_CDP_PORT` environment variable.
//!
//! Why the environment variable, and not a window builder
//! -------------------------------------------------------
//! WebView2 reads `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` when it creates the
//! browser environment, and Tauri creates the window — and with it that
//! environment — from the event loop's first `Ready` event. Tauri builds the
//! config windows **before** it runs the `setup` hook, so by the time
//! `setup` runs the environment already exists and a `--remote-debugging-port`
//! added there would be too late. The one seam that is early enough is before
//! the app is built at all, which is where [`apply_startup_port`] sits.
//!
//! The repository already uses this lever: `scripts/run-dev.ps1 -CdpPort`
//! sets the same variable, and `scripts/ui-shot.mjs` attaches to the real
//! dev-profile window over it.
//!
//! Loopback only
//! -------------
//! Chromium's DevTools HTTP server binds `127.0.0.1`, and WebView2 exposes
//! the endpoint to the local machine only. The port is therefore reachable
//! from this machine and not from the network — but "any local process can
//! drive this window" is still a real widening of the attack surface, which
//! is why the port is opt-in and why the UI says so while it is on.
//!
//! The environment override is a debug-build lever
//! ------------------------------------------------
//! `BUILDMESH_DIAG_CDP_PORT` is read only in a debug or `dev-tools` build,
//! for the same reason `BUILDMESH_PROFILE_DIR` and `BUILDMESH_PORT_OFFSET`
//! are: a shipped release must not be steerable by a variable it inherited
//! from its parent process. The **preference** is the shipped opt-in, and it
//! is an explicit, persistent, user-visible act.

use serde::Serialize;
use std::sync::OnceLock;
use ts_rs::TS;

/// Environment variable that names the DevTools port for this process.
///
/// Only a debug or `dev-tools` build reads it (see the module docs).
#[cfg(any(test, debug_assertions, feature = "dev-tools"))]
pub(crate) const CDP_PORT_ENV: &str = "BUILDMESH_DIAG_CDP_PORT";

/// The WebView2 environment variable that carries extra browser arguments.
///
/// This one is not build-gated: it is not a lever that changes what the
/// app *does*, it is the variable WebView2 itself reads, so knowing its
/// contents is how the app avoids fighting a launcher that already set a
/// port (and how the UI can report the truth about what is open).
pub(crate) const WEBVIEW2_BROWSER_ARGS_ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

/// The lowest port this module opens.
///
/// Ports below 1024 are privileged on Unix and are rarely a DevTools port in
/// practice, so a value below this is treated as "not a port" rather than
/// opened.
pub(crate) const MIN_PORT: u16 = 1024;

/// Where the effective DevTools port came from.
///
/// Generated to src/types/generated/DevtoolsPortSource.ts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "DevtoolsPortSource.ts")]
pub enum PortSource {
    /// No port: the DevTools endpoint is closed.
    Off,
    /// The saved user preference.
    Preference,
    /// The launch-time environment — the `BUILDMESH_DIAG_CDP_PORT` override,
    /// or a launcher that already put a port in the browser arguments.
    Environment,
}

/// The DevTools port this process opened, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    /// The port, when one is open.
    pub port: Option<u16>,
    /// Where the port came from.
    pub source: PortSource,
    /// A note about an override that could not be used, if there was one.
    ///
    /// The warning is not logged where the value is read — that happens
    /// before the tracing subscriber is installed — so it travels here and is
    /// written out once the log exists.
    pub note: Option<String>,
}

impl Effective {
    /// The state of a process that opened no port.
    fn off() -> Self {
        Self {
            port: None,
            source: PortSource::Off,
            note: None,
        }
    }
}

/// The outcome of resolving the port: what is open, and what WebView2 will be
/// started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// What the process opened.
    pub effective: Effective,
    /// The browser arguments WebView2 will be started with. `None` means the
    /// variable is left as it was — with the preference off, no
    /// debugging port is added at all.
    pub browser_args: Option<String>,
}

/// The resolution this process settled on.
static EFFECTIVE: OnceLock<Effective> = OnceLock::new();

/// The DevTools port this process opened, if any.
///
/// A process that did not run [`apply_startup_port`] — a test binary, a
/// crash-watchdog helper — reports no port.
pub fn effective() -> Effective {
    EFFECTIVE.get().cloned().unwrap_or_else(Effective::off)
}

/// Is `port` a port this module is willing to open?
pub fn is_valid_port(port: u16) -> bool {
    port >= MIN_PORT
}

/// Parse a port from a raw string.
///
/// A blank value, a non-number, and a privileged port are all "not a port":
/// an unusable value must not become one.
pub fn parse_port(raw: &str) -> Option<u16> {
    let port = raw.trim().parse::<u16>().ok()?;
    is_valid_port(port).then_some(port)
}

/// The browser argument that opens the DevTools port.
pub fn remote_debugging_argument(port: u16) -> String {
    format!("--remote-debugging-port={port}")
}

/// The port an existing browser-argument string already opens, if any.
///
/// The port is read as it was written, without this module's own range check:
/// a launcher that asked for port 80 got port 80, and the app has to report
/// what is actually open.
pub fn port_in_browser_args(args: &str) -> Option<u16> {
    args.split_whitespace()
        .find_map(|arg| arg.strip_prefix("--remote-debugging-port="))
        .and_then(|value| value.trim().parse::<u16>().ok())
}

/// Resolve the port and the browser arguments from the three inputs.
///
/// The order is deliberate:
///
/// 1. A launcher that already put a port in the browser arguments wins. It
///    opened the port before this process could, and two ports in one
///    argument string would leave Chromium using the first one — so the
///    effective port has to be the one that is actually there.
/// 2. The environment override wins over the preference: it is the more
///    specific, deliberate act for *this* launch.
/// 3. The preference is the shipped opt-in.
///
/// A port this module would not open is treated as no port at all, so the
/// function is total: an out-of-range input does not become a
/// `--remote-debugging-port` argument.
///
/// The browser arguments are returned rather than applied so the rule is
/// testable without a process environment.
pub fn resolve(
    preference_port: Option<u16>,
    env_port: Option<u16>,
    existing_args: Option<&str>,
) -> Resolution {
    let env_port = env_port.filter(|port| is_valid_port(*port));
    let preference_port = preference_port.filter(|port| is_valid_port(*port));

    let existing = existing_args
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(args) = existing {
        if let Some(port) = port_in_browser_args(args) {
            return Resolution {
                effective: Effective {
                    port: Some(port),
                    source: PortSource::Environment,
                    note: None,
                },
                browser_args: Some(args.to_string()),
            };
        }
    }

    let (port, source) = match (env_port, preference_port) {
        (Some(port), _) => (port, PortSource::Environment),
        (None, Some(port)) => (port, PortSource::Preference),
        (None, None) => {
            return Resolution {
                effective: Effective::off(),
                browser_args: existing.map(|value| value.to_string()),
            }
        }
    };

    let browser_args = match existing {
        Some(value) => format!("{value} {}", remote_debugging_argument(port)),
        None => remote_debugging_argument(port),
    };

    Resolution {
        effective: Effective {
            port: Some(port),
            source,
            note: None,
        },
        browser_args: Some(browser_args),
    }
}

/// The launch-time override, in the builds that honour it.
fn env_port_override() -> (Option<u16>, Option<String>) {
    #[cfg(any(test, debug_assertions, feature = "dev-tools"))]
    {
        let Ok(raw) = std::env::var(CDP_PORT_ENV) else {
            return (None, None);
        };
        match parse_port(&raw) {
            Some(port) => (Some(port), None),
            None => (
                None,
                Some(format!(
                    "{CDP_PORT_ENV}={raw:?} is not a port in {MIN_PORT}..=65535; ignoring it"
                )),
            ),
        }
    }
    #[cfg(not(any(test, debug_assertions, feature = "dev-tools")))]
    (None, None)
}

/// The saved preference's port, read straight off disk.
///
/// The preferences store is not installed until `setup`, and `setup` runs
/// after the window exists, so the startup path does not ask the store. This
/// reads the one field it needs and no more; see
/// [`crate::preferences::storage::read_for_startup`].
fn preference_port(app: &tauri::App) -> Option<u16> {
    use tauri::Manager;

    let profile = crate::startup::resolve_profile_dir(|| {
        app.path().app_data_dir().map_err(|error| {
            crate::startup::StartupFailure::app_data(
                "Buildmesh could not work out where to store its data.",
                error,
                None,
            )
        })
    })
    .ok()?;

    let prefs = crate::preferences::storage::read_for_startup(&profile.join("preferences.json"))?;
    let port = prefs.diag_cdp_port?;
    if is_valid_port(port) {
        Some(port)
    } else {
        None
    }
}

/// Open the DevTools port for this process, if one was asked for.
///
/// Must run **before** the app's window is created: WebView2 reads
/// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` when it creates the browser
/// environment, which happens on the event loop's first `Ready` event —
/// after `setup`, after the preferences store exists, and long after this
/// function returns. A port added later would be a port the UI claims is
/// open while no process is listening on it.
///
/// The preference is the shipped opt-in. The environment override is read in
/// debug and `dev-tools` builds only.
pub fn apply_startup_port(app: &tauri::App) {
    let existing = std::env::var(WEBVIEW2_BROWSER_ARGS_ENV).ok();
    let (env_port, env_note) = env_port_override();
    let preference = preference_port(app);

    let mut resolution = resolve(preference, env_port, existing.as_deref());
    resolution.effective.note = env_note;

    if let Some(args) = resolution.browser_args.as_deref() {
        // `set_var` is safe here in edition 2021: the environment is not read
        // by another thread yet — the app has not been built, let alone run.
        std::env::set_var(WEBVIEW2_BROWSER_ARGS_ENV, args);
    }

    let _ = EFFECTIVE.set(resolution.effective);
}

/// Write the effective port to the log.
///
/// Called once the tracing subscriber exists — the port is resolved before
/// the app is built, which is before any subscriber is installed, so the line
/// is not written where it is decided.
pub fn log_effective_port() {
    let effective = effective();
    match effective.port {
        Some(port) => tracing::info!(
            "DevTools port {port} is open on 127.0.0.1 (source: {:?}); any local process can drive this window",
            effective.source
        ),
        None => tracing::info!("DevTools port is closed (source: off)"),
    }
    if let Some(note) = effective.note.as_deref() {
        tracing::warn!("{note}");
    }
}

#[cfg(test)]
#[path = "cdp_tests.rs"]
mod tests;
