//! The DevTools (CDP) port status for the running hub (issue #2836).
//!
//! The port itself is a **startup** switch: WebView2 reads its browser
//! arguments when the window is created, which happens before the settings
//! UI can be used. So the *intent* (the saved preference) and the *realized*
//! state (is a port open in this process right now) are two different facts,
//! and this command reports the realized one.
//!
//! The distinction matters in both directions:
//!
//! * the preference says 9222 but this process started before it was
//!   changed — the port is not open, and the UI must not claim it is;
//! * a launcher passed `--remote-debugging-port=9223` in the browser
//!   arguments — a port **is** open, and the UI has to say so, because any
//!   local process can drive the window through it.
//!
//! The status is a pure in-memory read of what the startup path decided
//! ([`crate::diagnostics::cdp`]), so this command stays a fast sync command
//! with no I/O.

use serde::Serialize;
use tauri::command;
use ts_rs::TS;

/// The DevTools port this process opened, and where it came from.
///
/// Generated to src/types/generated/DevtoolsStatus.ts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "DevtoolsStatus.ts")]
pub struct DevtoolsStatus {
    /// The port that is open, or `None` when no DevTools port is open.
    pub port: Option<u16>,
    /// Where the port came from.
    ///
    /// `preference` is the saved user setting; `environment` is a
    /// launch-time override; `off` means no port is open.
    pub source: crate::diagnostics::cdp::PortSource,
}

/// The DevTools port this process opened, if any.
///
/// The frontend uses this to tell the user, while the port is on, that any
/// process on this machine can drive the window through it.
#[command]
pub fn get_devtools_status() -> DevtoolsStatus {
    let effective = crate::diagnostics::cdp::effective();
    DevtoolsStatus {
        port: effective.port,
        source: effective.source,
    }
}

#[cfg(test)]
#[path = "devtools_tests.rs"]
mod tests;
