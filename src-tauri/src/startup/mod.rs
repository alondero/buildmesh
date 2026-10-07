//! Startup bootstrap: diagnostics that exist *before* the database does
//! (issue #1525).
//!
//! Why this module exists
//! ----------------------
//! The failures most likely to stop Buildmesh opening all happen early: the
//! app-data directory is unwritable, SQLite is corrupt, a migration fails. That
//! work used to run before any logging existed, so a user in exactly that
//! situation got no window, no message, and no log — while the Boot Error
//! Panel (which only renders after React has booted, which requires the
//! database) promised that details had been written to `buildmesh.log`.
//!
//! [`bootstrap`] runs first, inside Tauri `setup`, and does three things in
//! order: resolve the app-data profile, open the size-bounded `buildmesh.log`,
//! and install the tracing subscriber. Everything after it — `db::init`,
//! preferences, migrations, harness detection — is already inside a live
//! `tracing` pipeline, so its `warn!`/`error!` lines reach the log that the
//! error surface points the user at.
//!
//! One subscriber, installed once
//! ------------------------------
//! The old sequence installed tracing in `setup` *after* the database work,
//! which is exactly the gap this module closes. The fix is not a second,
//! earlier subscriber: it is **this** subscriber, installed as early as the
//! profile directory allows, and never replaced. So there is nothing to bridge
//! and nothing to rotate between two writers:
//!
//! * early records are already in the same bounded `buildmesh.log` the rest of
//!   the session appends to, so no early record is dropped;
//! * `try_init` is used rather than `init`, so a second attempt can never
//!   panic with "a global default subscriber has already been set";
//! * the writer is the same size-bounded, fixed-name
//!   [`RotatingWriter`](crate::diagnostics::RotatingWriter) the rest of the app
//!   uses, so healthy startup still produces exactly one bounded log file with
//!   the name the `/use`, `/verify`, and `/verify-ui` skills tail.
//!
//! Every byte that reaches that file goes through
//! [`SecretScrubber::scrub_for_persistence`](crate::secret_scrubber::SecretScrubber::scrub_for_persistence)
//! first — subscriber lines and the durable startup record alike. The frontend
//! command masks before it emits; this writer is the backstop, so a support
//! copy of `buildmesh.log` is not a second copy of a credential a trace
//! happened to include. The bootstrap stderr mirror receives the same masked
//! bytes.
//!
//! The one thing that *does* change at [`Bootstrap::promote`] is the stderr
//! mirror: during bootstrap the log is mirrored to stderr (a console launch, a
//! CI run, and a `panic = "abort"` build with no console all need the launcher
//! to see the reason), and after the database is open and the normal services
//! are starting the mirror is dropped so a long session does not spray
//! `debug`-level lines into a developer's terminal.
//!
//! Why the writer is synchronous
//! -----------------------------
//! The main log used to be wrapped in `tracing_appender::non_blocking`, which
//! moves the write onto a background thread so a log line never blocks the
//! async runtime. That wrapper cannot be used here: `tracing-appender` 0.2
//! exposes no way to force its queue to disk (`NonBlocking::flush` is a no-op
//! and `WorkerGuard` has no `flush` at all), and a fatal startup failure ends
//! the process moments later. A line still sitting in that queue is exactly the
//! record the user is about to be asked for, so the guarantee is not
//! negotiable.
//!
//! So the [`SharedLog`] is a `Mutex<RotatingWriter>` written inline. That costs
//! one uncontended mutex and one `write` syscall on the emitting thread, which
//! is what the panic hooks and the diagnostics sampler already do, and what
//! `tracing`'s own formatter does before handing a line to any writer. In
//! exchange, [`Bootstrap::record_durably`] can `sync_all` the line before the
//! error surface appears — the same durability argument the panic hooks make,
//! since `panic = "abort"` kills the process via `__fastfail` before the OS
//! file buffer would flush. A single file handle also means a single rotation
//! accounting, which two independent writers could not give.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tauri::Manager;
use tracing_subscriber::fmt::MakeWriter;

mod failure;
mod present;

pub use failure::StartupFailure;
pub(crate) use present::present_failure;

/// The resolved, opened, and installed diagnostics for this process.
#[derive(Debug)]
pub struct Bootstrap {
    profile_dir: PathBuf,
    log_dir: PathBuf,
    main_log: PathBuf,
    /// Flipped off by [`promote`](Self::promote) to end the stderr mirror.
    stderr_mirror: Arc<AtomicBool>,
    /// Shared with the tracing subscriber, so the fatal line and the session's
    /// log lines go through one handle and one rotation accounting.
    log: Arc<SharedLog>,
}

/// The path half of [`Bootstrap`], for callers that only need to tell a user
/// where their log is (issue #1525: the Boot Error Panel must show the
/// resolved absolute location, not a filename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePaths {
    pub profile_dir: PathBuf,
    pub log_dir: PathBuf,
    pub main_log: PathBuf,
}

impl Bootstrap {
    /// Absolute path of the app-data profile directory.
    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }

    /// Absolute path of the `logs` directory.
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// Absolute path of the size-bounded `buildmesh.log`.
    pub fn main_log(&self) -> &Path {
        &self.main_log
    }

    pub fn paths(&self) -> ProfilePaths {
        ProfilePaths {
            profile_dir: self.profile_dir.clone(),
            log_dir: self.log_dir.clone(),
            main_log: self.main_log.clone(),
        }
    }

    /// Leave the bootstrap phase: stop mirroring the log to stderr.
    ///
    /// Everything already recorded stays in `buildmesh.log` — the subscriber
    /// and its file handle are the same ones the bootstrap installed, so
    /// promotion is a flag, not a handover.
    pub fn promote(&self) {
        self.stderr_mirror.store(false, Ordering::Relaxed);
        tracing::info!(
            "Startup bootstrap complete — tracing is file-only from here ({})",
            self.main_log.display()
        );
    }

    /// Append one timestamped line to `buildmesh.log` and force it to disk.
    ///
    /// Separate from `tracing::error!` on purpose: a fatal failure must not
    /// depend on the subscriber pipeline it is about to outlive. This writes
    /// through the same shared handle, so it lands in the same file, in order,
    /// and counts against the same rotation budget.
    pub fn record_durably(&self, line: &str) {
        if let Err(error) = self.log.append_and_sync(line) {
            // The log is the sink the user is about to be asked for, so a
            // failure to write it must not pass silently. The line is still on
            // stderr via the caller.
            eprintln!("could not write to {}: {error}", self.main_log.display());
        }
    }
}

/// The process-wide bootstrap, once [`bootstrap`] has succeeded.
static INSTALLED: OnceLock<Bootstrap> = OnceLock::new();

/// The bootstrap for this process, or `None` if it never installed one
/// (startup failed first, or this is a test process that never booted).
pub fn installed() -> Option<&'static Bootstrap> {
    INSTALLED.get()
}

/// The copyable paths, for the diagnostic-locations command.
pub fn installed_paths() -> Option<ProfilePaths> {
    INSTALLED.get().map(Bootstrap::paths)
}

/// Resolve the app-data profile and install diagnostics, before any database
/// or preference work.
///
/// Ordering is the whole point, so it is worth stating what each step can and
/// cannot have touched on failure:
///
/// 1. `app_data_dir()` — resolves the profile. Nothing on disk has changed.
/// 2. `create_dir_all(profile)` — creates the profile if absent.
/// 3. `create_dir_all(logs)` — creates the log directory.
/// 4. open the bounded writer + `try_init` the subscriber.
///
/// Steps 1–3 are pure filesystem preparation: no process-global state is
/// installed, which is exactly why [`StartupFailure::is_retry_safe`] holds for
/// them and why the failure surface may offer "try again" for them. A failure
/// at any of them leaves nothing behind to unwind, so the returned failure is
/// safe to retry in place.
///
/// Errors are typed, not stringly, so the caller can present them without
/// re-deriving what went wrong; see [`StartupFailure::app_data`] and
/// [`StartupFailure::log_directory`].
pub fn bootstrap(handle: &tauri::AppHandle) -> Result<&'static Bootstrap, StartupFailure> {
    if let Some(existing) = INSTALLED.get() {
        return Ok(existing);
    }

    // --- 1. Resolve the profile ---------------------------------------
    let profile_dir = handle.path().app_data_dir().map_err(|error| {
        StartupFailure::app_data(
            "Buildmesh could not work out where to store its data.",
            error,
            None,
        )
    })?;

    // --- 2. Create the profile ----------------------------------------
    if let Err(error) = std::fs::create_dir_all(&profile_dir) {
        return Err(StartupFailure::app_data(
            "Buildmesh could not create its app data directory.",
            &error,
            Some(profile_dir),
        ));
    }

    // --- 3. Create the log directory -----------------------------------
    let log_dir = profile_dir.join("logs");
    if let Err(error) = std::fs::create_dir_all(&log_dir) {
        return Err(StartupFailure::log_directory(
            "Buildmesh could not create the folder it writes its log to.",
            &error,
            Some(log_dir),
            Some(profile_dir),
        ));
    }

    // --- 4. Open the bounded log and install the one subscriber -------
    //
    // `main_log_writer` is the same size-bounded, fixed-name writer the rest
    // of the app uses, so this is one file with one rotation policy, not a
    // second log competing for the same directory.
    let file_appender = crate::diagnostics::main_log_writer(&log_dir).map_err(|error| {
        StartupFailure::log_directory(
            "Buildmesh could not open its log file.",
            &error,
            Some(log_dir.clone()),
            Some(profile_dir.clone()),
        )
    })?;

    let stderr_mirror = Arc::new(AtomicBool::new(true));
    let log = Arc::new(SharedLog::new(file_appender));

    let bootstrap = Bootstrap {
        profile_dir: profile_dir.clone(),
        log_dir: log_dir.clone(),
        main_log: log_dir.join("buildmesh.log"),
        stderr_mirror: Arc::clone(&stderr_mirror),
        log: Arc::clone(&log),
    };

    // `try_init`, never `init`: a second attempt returns an error we can
    // ignore instead of panicking with "a global default subscriber has
    // already been set". A subscriber installed by something else still means
    // *somewhere* is collecting records, so this is not a startup failure.
    let installed_subscriber = tracing_subscriber::fmt()
        .with_writer(Tee {
            stderr: StderrMirror::new(Arc::clone(&stderr_mirror)),
            log: Arc::clone(&log),
        })
        .with_ansi(false)
        .with_env_filter(env_filter())
        .try_init()
        .is_ok();

    if !installed_subscriber {
        eprintln!(
            "buildmesh: a tracing subscriber was already installed; \
             bootstrap logging will go to stderr only"
        );
    }

    // `set` only fails if a racing caller installed first; either way there is
    // now exactly one bootstrap, and reading it back is the authoritative
    // answer for this process.
    let _already_installed = INSTALLED.set(bootstrap).is_err();
    let bootstrap = INSTALLED
        .get()
        .expect("bootstrap was set immediately above");

    // The first line in the file names the profile and the log the user will
    // be told about, so a report of "it didn't start" is answerable from the
    // log alone.
    tracing::info!(
        "Startup bootstrap: profile={} log={} stderr_mirror=on subscriber={}",
        bootstrap.profile_dir.display(),
        bootstrap.main_log.display(),
        if installed_subscriber {
            "installed"
        } else {
            "pre-existing"
        }
    );

    Ok(bootstrap)
}

/// Write a fatal failure to every sink that will still exist after the process
/// ends.
///
/// Two independent sinks, on purpose. The log is what the user is asked to
/// hand over and it is `sync_all`'d before this returns; stderr is what
/// survives when the log could not be opened at all, which is a real case — it
/// is one of the failures this module exists to report.
pub fn record(failure: &StartupFailure) {
    let line = failure.log_line();
    match installed() {
        Some(bootstrap) => {
            bootstrap.record_durably(&line);
            eprintln!("{line}");
        }
        None => eprintln!("{line}"),
    }
}

/// Report a fatal startup failure: record it, then put the native error surface
/// in front of the user.
///
/// Returns the failure as a `Box<dyn Error>` so a Tauri `setup` body can
/// `return Err(startup::report(...))` — the caller does not have to remember
/// to both show the surface and propagate the error.
///
/// The surface blocks, so "the app failed to open" always ends with the user
/// having seen why. There is no React, no webview, and no database involved:
/// this path is reached precisely when none of those are available.
pub fn report(failure: StartupFailure) -> Box<dyn std::error::Error> {
    record(&failure);
    let _ = present_failure(&failure, None);
    Box::new(failure)
}

/// [`report`], but with a "try again" affordance for a failure the user may be
/// able to fix from outside the app.
///
/// `retry` is only ever passed for a stage that failed before any
/// process-global state was installed ([`StartupFailure::is_retry_safe`]), so
/// calling it is a real second attempt rather than a no-op that would report
/// success for a still-broken database.
///
/// Returns `Err(failure)` when the user quit, or when a retry failed — in both
/// cases the outstanding failure has already been recorded.
pub fn report_with_retry(
    failure: StartupFailure,
    retry: &mut dyn FnMut() -> Result<(), StartupFailure>,
) -> Result<(), StartupFailure> {
    record(&failure);
    present_failure(&failure, Some(retry))
}

/// The one `buildmesh.log` handle, shared by the tracing subscriber and by
/// [`Bootstrap::record_durably`].
///
/// A single handle is what keeps rotation correct: two independent writers
/// would each track their own byte count against a cap, and whichever rotated
/// first would strand the other writing into a file the first had renamed.
pub(super) struct SharedLog(Mutex<crate::diagnostics::RotatingWriter>);

/// `RotatingWriter` is not `Debug` (it owns a live file handle), and printing
/// one would say nothing useful anyway. A `Mutex`'s own `Debug` would also try
/// to lock, which on a poisoned lock would panic inside a debug print.
impl std::fmt::Debug for SharedLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedLog(<buildmesh.log writer>)")
    }
}

impl SharedLog {
    fn new(writer: crate::diagnostics::RotatingWriter) -> Self {
        Self(Mutex::new(writer))
    }

    fn with<R>(&self, f: impl FnOnce(&mut crate::diagnostics::RotatingWriter) -> R) -> R {
        // A poisoned log mutex must not take down the app it exists to
        // diagnose: the writer itself has no invariant to violate.
        let mut writer = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut writer)
    }

    /// Append one timestamped line, forced to disk.
    fn append_and_sync(&self, line: &str) -> io::Result<()> {
        let scrubbed = crate::secret_scrubber::SecretScrubber::scrub_for_persistence(line);
        self.with(|writer| {
            writer.write_line(&scrubbed)?;
            writer.sync()
        })
    }

    /// Write already-formatted bytes into the shared log, in full.
    ///
    /// Inherent rather than an `io::Write` impl because the caller holds an
    /// `Arc<SharedLog>`, and an `Arc` only hands out `&T` — a trait method
    /// taking `&mut self` could not be reached through it. The interior
    /// `Mutex` is what makes `&self` sufficient.
    ///
    /// Secrets are masked **before** the lock. The fmt subscriber hands over
    /// one complete event per `write_all`, so the masker sees the whole line
    /// rather than a slice of a token. The lock is then taken **once, around
    /// the whole masked buffer**, and the standard library's `write_all` does
    /// the looping. An earlier version looped here and called `self.with(..)`
    /// per iteration, which released and re-took the mutex between chunks —
    /// letting another thread interleave its own line into the middle of this
    /// one.
    fn write_all_through(&self, buf: &[u8]) -> io::Result<()> {
        let scrubbed = scrub_log_bytes(buf);
        self.write_raw(&scrubbed)
    }

    fn write_raw(&self, buf: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        self.with(|writer| writer.write_all(buf))
    }

    fn flush_through(&self) -> io::Result<()> {
        use std::io::Write as _;
        self.with(|writer| writer.flush())
    }
}

/// Mask a subscriber buffer before it is persisted.
///
/// The fmt layer formats one event into a `String` and then `write_all`s
/// those bytes, so a valid UTF-8 buffer is one complete line. Invalid UTF-8
/// is written unchanged: there is no text to mask, and dropping the line
/// would hide the failure this log exists to record.
fn scrub_log_bytes(buf: &[u8]) -> Cow<'_, [u8]> {
    match std::str::from_utf8(buf) {
        Ok(text) => {
            let scrubbed = crate::secret_scrubber::SecretScrubber::scrub_for_persistence(text);
            if scrubbed == text {
                Cow::Borrowed(buf)
            } else {
                Cow::Owned(scrubbed.into_bytes())
            }
        }
        Err(_) => Cow::Borrowed(buf),
    }
}

/// Fans each formatted event out to the shared log and — while bootstrap is
/// still running — to stderr.
struct Tee {
    stderr: StderrMirror,
    log: Arc<SharedLog>,
}

impl io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Order matters, and it is the log first.
        //
        // The log is the durable sink — the thing a user is asked to hand over
        // — so it is written completely before the stderr mirror is even
        // consulted. Writing stderr first (and propagating its error) meant a
        // closed console or a full stderr buffer could abort the line before it
        // reached the file, which is the exact failure this whole module exists
        // to prevent.
        // The file path masks. The mirror must see that same mask, not the raw
        // event. Return the caller's length, not the masked length: `write_all`
        // treats a short result as "write the rest", and the rest would be the
        // unmasked tail.
        self.log.write_all_through(buf)?;
        // The mirror is best-effort by construction: `StderrMirror` already
        // no-ops once bootstrap promotes, and a genuine stderr error must not
        // turn into a lost log line. There is nowhere to report such an error
        // to, by definition.
        let _ = self.stderr.write_all(&scrub_log_bytes(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Same reasoning: a failing mirror must not skip the log's flush.
        self.log.flush_through()?;
        let _ = self.stderr.flush();
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Tee {
    type Writer = Tee;
    fn make_writer(&'a self) -> Tee {
        Tee {
            stderr: self.stderr.handle(),
            log: Arc::clone(&self.log),
        }
    }
}

/// The one filter both the bootstrap and any later logging uses.
///
/// `RUST_LOG` wins where it is set; the two `buildmesh*` directives keep our
/// own crates at `debug` (the level the skills and `scripts/*log*.ps1` expect
/// to find) and the default is `info` for everything else.
fn env_filter() -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("buildmesh_lib=debug".parse().expect("static directive"))
        .add_directive("buildmesh=debug".parse().expect("static directive"))
}

/// A `Write` that mirrors log output to stderr while bootstrap is still
/// running, and becomes a no-op once [`Bootstrap::promote`] flips the flag.
///
/// The no-op path reports the full length without writing: returning `Ok(0)`
/// would make callers treat the write as having done nothing, and `write_all`
/// would spin on it.
#[derive(Debug)]
struct StderrMirror {
    enabled: Arc<AtomicBool>,
    /// Injected console failure, test-only. See [`StderrMirror::broken`].
    #[cfg(test)]
    broken: bool,
}

impl StderrMirror {
    fn new(enabled: Arc<AtomicBool>) -> Self {
        Self {
            enabled,
            #[cfg(test)]
            broken: false,
        }
    }

    /// A mirror whose every write fails, standing in for a closed console.
    ///
    /// A real `windows_subsystem` launch has no console and a chatty parent can
    /// close the pipe, but neither can be reproduced reliably inside a test
    /// process — so the failure is injected here instead. Test-only, and it
    /// exists solely so "a broken console must not suppress the log line" is a
    /// locked property rather than a code-reading exercise.
    #[cfg(test)]
    fn broken(enabled: Arc<AtomicBool>) -> Self {
        Self {
            enabled,
            broken: true,
        }
    }

    /// A fresh handle onto the same flag, for a `MakeWriter`.
    fn handle(&self) -> StderrMirror {
        StderrMirror {
            enabled: Arc::clone(&self.enabled),
            #[cfg(test)]
            broken: self.broken,
        }
    }
}

impl io::Write for StderrMirror {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(test)]
        if self.broken {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected console failure",
            ));
        }
        if self.enabled.load(Ordering::Relaxed) {
            let mut stderr = io::stderr().lock();
            stderr.write_all(buf)?;
            stderr.flush()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        #[cfg(test)]
        if self.broken {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected console failure",
            ));
        }
        if self.enabled.load(Ordering::Relaxed) {
            io::stderr().lock().flush()?;
        }
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for StderrMirror {
    type Writer = StderrMirror;
    fn make_writer(&'a self) -> StderrMirror {
        self.handle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The stderr mirror must be able to turn itself off — that is the whole
    /// promote step, and if it leaked, every `debug` line of a long session
    /// would spray into the developer's terminal.
    ///
    /// The write is checked rather than the stream: a disabled mirror must
    /// still report the full length, because `write_all` on a short write
    /// would not advance.
    #[test]
    fn stderr_mirror_stops_writing_once_disabled() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut mirror = StderrMirror::new(Arc::clone(&flag));
        let payload = b"a line that must not reach stderr\n";

        assert!(!mirror.enabled.load(Ordering::Relaxed));
        assert_eq!(
            mirror.write(payload).unwrap(),
            payload.len(),
            "a disabled mirror must consume the write without emitting it"
        );
        mirror.flush().unwrap();

        // The check is on every write, not latched on first use, so a handle
        // created while the mirror was on also stops after promotion.
        flag.store(true, Ordering::Relaxed);
        let mut promoted = StderrMirror::new(Arc::clone(&flag));
        promoted.write_all(payload).unwrap();
        flag.store(false, Ordering::Relaxed);
        promoted.write_all(payload).unwrap();
        promoted.flush().unwrap();
    }

    /// `MakeWriter` hands out independent handles that share the flag, so a
    /// mirror created before promotion and one created after cannot disagree.
    #[test]
    fn stderr_mirror_handles_share_one_flag() {
        let flag = Arc::new(AtomicBool::new(true));
        let mirror = StderrMirror::new(Arc::clone(&flag));
        let before = mirror.make_writer();
        flag.store(false, Ordering::Relaxed);
        let after = mirror.make_writer();

        assert!(!before.enabled.load(Ordering::Relaxed));
        assert!(!after.enabled.load(Ordering::Relaxed));
    }

    /// The one-handle invariant, exercised end to end against a real
    /// size-bounded writer: a durable record and an ordinary subscriber line
    /// both land in the same file, in order, and the durable one is on disk
    /// before `append_and_sync` returns.
    ///
    /// Two independent writers would pass a naive "both lines present" check
    /// while each tracked its own byte count against the cap, so the byte
    /// accounting is what is actually being pinned here.
    #[test]
    fn durable_records_and_subscriber_lines_share_one_bounded_file() {
        let dir = std::env::temp_dir().join(format!("bm-startup-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let writer = crate::diagnostics::main_log_writer(&dir).unwrap();
        let log = SharedLog::new(writer);

        log.append_and_sync("STARTUP_FAILURE stage=test detail=boom")
            .unwrap();
        // Readable immediately: the sync is the whole reason `record` can
        // promise the log is on disk before the error surface appears.
        let after_durable = std::fs::read_to_string(dir.join("buildmesh.log")).unwrap();
        assert!(
            after_durable.contains("STARTUP_FAILURE stage=test"),
            "the durable line must be on disk when append_and_sync returns, got {after_durable:?}"
        );

        // An ordinary subscriber line through the same handle, which is how the
        // tracing layer reaches it.
        log.write_all_through(b"an ordinary subscriber line\n")
            .unwrap();
        log.flush_through().unwrap();

        let contents = std::fs::read_to_string(dir.join("buildmesh.log")).unwrap();
        assert!(contents.contains("STARTUP_FAILURE stage=test"));
        assert!(
            contents.contains("an ordinary subscriber line"),
            "both sinks must write the same file: {contents:?}"
        );
        // Durable first: the fatal line was written before the session line.
        assert!(
            contents.find("STARTUP_FAILURE").unwrap() < contents.find("an ordinary").unwrap(),
            "order must be preserved across both sinks: {contents:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A poisoned log mutex must not take down the app it exists to diagnose.
    /// The writer has no invariant a panic could break, so the next writer
    /// carries on with the same handle.
    #[test]
    fn a_poisoned_log_lock_does_not_fail_later_writes() {
        let dir = std::env::temp_dir().join(format!("bm-startup-poison-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = Arc::new(SharedLog::new(
            crate::diagnostics::main_log_writer(&dir).unwrap(),
        ));

        let panicking = Arc::clone(&log);
        let _ = std::thread::spawn(move || {
            let _guard = panicking.0.lock().unwrap();
            panic!("simulate a writer that panicked while holding the lock");
        })
        .join();

        // Still writable, and the line reaches the file.
        log.append_and_sync("STARTUP_FAILURE after=poisoned")
            .unwrap();
        let contents = std::fs::read_to_string(dir.join("buildmesh.log")).unwrap();
        assert!(
            contents.contains("STARTUP_FAILURE after=poisoned"),
            "a poisoned lock must not silence the failure log: {contents:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A mirror that always errors must not be able to stop the log line
    /// reaching the file.
    ///
    /// This is the failure the ordering in [`Tee::write`] exists to prevent:
    /// a closed console (common in a `windows_subsystem` launch) or a full
    /// stderr buffer used to abort the line before it was written, which is
    /// precisely the "no log explaining why" outcome this module fixes.
    /// A console that fails must not stop the log line reaching the file.
    ///
    /// This is the property the ordering in [`Tee::write`] exists to provide: a
    /// closed console (routine for a `windows_subsystem` launch) or a full
    /// stderr buffer used to abort the line before it was written, which is
    /// exactly the "app failed to open, no log explaining why" outcome this
    /// whole module exists to prevent.
    #[test]
    fn a_broken_stderr_mirror_never_suppresses_the_log_line() {
        let dir = std::env::temp_dir().join(format!("bm-startup-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut tee = Tee {
            stderr: StderrMirror::broken(Arc::new(AtomicBool::new(true))),
            log: Arc::new(SharedLog::new(
                crate::diagnostics::main_log_writer(&dir).unwrap(),
            )),
        };
        let payload = b"a line that must survive a broken console\n";

        // The mirror is definitely broken: prove the fixture is real before
        // relying on it, or this test could pass for the wrong reason.
        let mut lone = StderrMirror::broken(Arc::new(AtomicBool::new(true)));
        assert!(
            lone.write(payload).is_err(),
            "the injected failure must actually fail, or this test proves nothing"
        );

        let written = tee
            .write(payload)
            .expect("a broken console must not fail the durable write");
        assert_eq!(written, payload.len(), "the whole line must be consumed");
        tee.flush()
            .expect("flush must not depend on the mirror either");

        let contents = std::fs::read_to_string(dir.join("buildmesh.log")).unwrap();
        assert!(
            contents.contains("a line that must survive a broken console"),
            "the durable sink must not depend on the mirror: {contents:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A short write is legal, so `Tee` must not report the full length after
    /// a single partial `write` — that would truncate a log line while telling
    /// the formatter it was intact. `Write::write_all` handles the looping;
    /// what matters here is that it runs under a **single** lock acquisition, so
    /// a concurrent writer cannot interleave into the middle of this line.
    #[test]
    fn a_concurrent_write_cannot_interleave_into_the_middle_of_a_line() {
        let dir = std::env::temp_dir().join(format!("bm-startup-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = Arc::new(SharedLog::new(
            crate::diagnostics::main_log_writer(&dir).unwrap(),
        ));

        // A line long enough that a per-chunk lock would be visible.
        let line = format!("A{}\n", "x".repeat(200_000));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let writer = Arc::clone(&log);
        let thread_barrier = Arc::clone(&barrier);
        let racing = std::thread::spawn(move || {
            thread_barrier.wait();
            for _ in 0..50 {
                writer
                    .write_all_through(b"Z-interloper\n")
                    .expect("the racing writer must succeed");
            }
        });

        barrier.wait();
        for _ in 0..50 {
            log.write_all_through(line.as_bytes())
                .expect("the main writer must succeed");
        }
        racing.join().unwrap();

        let contents = std::fs::read_to_string(dir.join("buildmesh.log")).unwrap();
        // Every line is intact: no "A…Z-interloper…" splice, and no torn
        // interlopener.
        for candidate in contents.lines() {
            assert!(
                candidate == "Z-interloper" || candidate.starts_with('A'),
                "a line was spliced by a concurrent writer: {candidate:?}"
            );
            if candidate.starts_with('A') {
                assert_eq!(
                    candidate.len(),
                    line.trim_end_matches('\n').len(),
                    "the long line was truncated"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The filter is installed on the startup path, where a panic would be a
    /// regression to exactly the class of failure this module exists to make
    /// visible — so it must always build, and it must always carry the two
    /// `buildmesh*` directives that the skills and `scripts/*log*.ps1` expect
    /// to find in the log.
    ///
    /// Asserted through `Display` (the rendered directive list) rather than
    /// `Debug` (the internal `DirectiveSet`), so the test does not break when
    /// tracing-subscriber restructures its representation.
    #[test]
    fn env_filter_always_carries_the_buildmesh_directives() {
        let rendered = env_filter().to_string();
        assert!(
            rendered.contains("buildmesh_lib=debug"),
            "buildmesh_lib must stay at debug, got {rendered}"
        );
        assert!(
            rendered.contains("buildmesh=debug"),
            "buildmesh must stay at debug, got {rendered}"
        );
    }

    /// A subscriber line is one formatted event. The fmt layer writes that
    /// event in a single `write_all`, and this writer is what puts those bytes
    /// into `buildmesh.log`. A secret in the event must not survive that write.
    #[test]
    fn subscriber_line_is_scrubbed_before_it_reaches_the_log_file() {
        let dir = std::env::temp_dir().join(format!("bm-startup-scrub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = SharedLog::new(crate::diagnostics::main_log_writer(&dir).unwrap());

        let secret = "sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB";
        let line =
            format!("2026-10-07T00:00:00Z ERROR frontend: provider rejected {secret} session=42\n");
        log.write_all_through(line.as_bytes()).unwrap();
        log.flush_through().unwrap();

        let contents = std::fs::read_to_string(dir.join("buildmesh.log")).unwrap();
        assert!(
            !contents.contains(secret),
            "the log file must not keep the provider key: {contents}"
        );
        assert!(
            contents.contains("session=42"),
            "non-secret diagnostic text must remain: {contents}"
        );
        assert!(
            contents.contains("[REDACTED]"),
            "the masked token must be visible so the line is still diagnosable: {contents}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
