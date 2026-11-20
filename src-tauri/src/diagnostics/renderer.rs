//! The renderer diagnostics sink (issue #2833).
//!
//! ## Why a second file
//!
//! The native process already has an always-on timeline
//! (`logs/diagnostics.log`, written by the sampler in the parent module). The
//! **WebView** — where the UI actually runs — had none. Issue #2830 is the
//! proof: typing lagged for days and nothing recorded it.
//!
//! The renderer now writes its own line every 10 seconds. It goes into its
//! own file, `logs/renderer-diagnostics.log`, rather than into the native
//! `diagnostics.log`:
//!
//! * The two timelines are written by different processes at different
//!   cadences, and interleaving them would make both harder to read.
//! * The native file is the *native* story; the renderer file is the *webview*
//!   story. A user handing back "the renderer log" wants one file.
//! * `diagnostics.log` is written by a dedicated sampler thread with its own
//!   writer. Sharing that writer across threads would need a lock on the hot
//!   path of a sampler that must never stall.
//!
//! ## The bounds
//!
//! The same [`RotatingWriter`] the native log uses: a byte cap and a bounded
//! number of generations. A diagnostics file that can fill the disk cannot
//! diagnose a disk fill.
//!
//! The renderer line is written once every 10 seconds — six lines a minute —
//! so the file is fsynced on **every** line rather than on a cadence. The
//! native writer syncs every sixth line because it is written by a thread
//! that must never stall on a thrashing disk; here the write happens on a
//! Tauri command and one fsync every 10 seconds is nothing, while the
//! evidence survives a hard reboot.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use super::writer::RotatingWriter;

/// The file the renderer health lines are written to.
pub const RENDERER_LOG_FILE: &str = "renderer-diagnostics.log";

/// The prefix that marks a frontend message as a renderer health line.
///
/// It must match `RENDER_HEALTH_PREFIX` in
/// `src/lib/diagnostics/rendererHealth.ts`. The prefix is the whole
/// contract: the frontend sends one structured line through the ordinary
/// `log_frontend` bridge, and the native side routes it by this marker.
pub const RENDER_HEALTH_PREFIX: &str = "RENDER_DIAG";

/// Byte cap for the renderer log. At roughly 700 bytes a line and one line
/// per 10 seconds this is about a month of continuous running.
const RENDERER_LOG_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Rotated generations kept beside the live file.
const RENDERER_LOG_KEEP: usize = 1;

/// What happened to a renderer health line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererLineOutcome {
    /// The line was written to the renderer sink.
    Written,
    /// Diagnostics are switched off (`BUILDMESH_DIAG=0`), so the line was
    /// dropped rather than pushed into the main log. The kill switch means
    /// "no diagnostics", and a line every 10 seconds in `buildmesh.log` would
    /// be exactly the noise the user asked to stop.
    Disabled,
    /// The sink could not be opened. The caller falls back to the main log so
    /// the information is not lost.
    Unavailable,
}

/// Whether a frontend message is a renderer health line.
pub fn is_renderer_health_line(message: &str) -> bool {
    message.starts_with(RENDER_HEALTH_PREFIX)
}

static RENDERER_LOG: OnceLock<Mutex<Option<RotatingWriter>>> = OnceLock::new();

/// Open the renderer sink in `log_dir`.
///
/// Called once during startup, before the app is usable, so a path that
/// cannot be written is reported while there is still a log to report it in.
/// The first installation wins: a second call is ignored rather than
/// replacing a live writer.
pub fn install_log_dir(log_dir: PathBuf) {
    let path = log_dir.join(RENDERER_LOG_FILE);
    let writer = match RotatingWriter::with_limits(
        path.clone(),
        RENDERER_LOG_MAX_BYTES,
        RENDERER_LOG_KEEP,
    ) {
        Ok(writer) => Some(writer),
        Err(error) => {
            tracing::warn!(
                "renderer diagnostics: cannot open {} ({error}); the renderer health line will go to the main log",
                path.display()
            );
            None
        }
    };
    if RENDERER_LOG.set(Mutex::new(writer)).is_err() {
        tracing::warn!(
            "renderer diagnostics: log directory installed twice; keeping the first sink"
        );
    }
}

/// Write one renderer health line.
///
/// Returns what happened to it so the caller can decide whether the line
/// still needs to reach the main log.
pub fn write_renderer_line(line: &str) -> RendererLineOutcome {
    if !super::diagnostics_enabled() {
        return RendererLineOutcome::Disabled;
    }
    let Some(slot) = RENDERER_LOG.get() else {
        return RendererLineOutcome::Unavailable;
    };
    let Ok(mut guard) = slot.lock() else {
        // A poisoned lock means a previous write panicked. The line is
        // reported as unavailable so the caller falls back to the main log
        // rather than dropping it.
        return RendererLineOutcome::Unavailable;
    };
    let Some(writer) = guard.as_mut() else {
        return RendererLineOutcome::Unavailable;
    };
    match writer.write_line_synced(line) {
        Ok(()) => RendererLineOutcome::Written,
        Err(error) => {
            tracing::warn!("renderer diagnostics: write failed: {error}");
            RendererLineOutcome::Unavailable
        }
    }
}

#[cfg(test)]
#[path = "renderer_tests.rs"]
mod renderer_tests;
